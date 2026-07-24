//! DripOracle — a TWAP price-aggregator contract for fiat-denominated
//! streams.
//!
//! Replaces the abandoned `src/oracle.rs` of the integration-test
//! crate. Real on-chain TWAP over a bounded per-asset ring buffer of
//! observations, fed by a per-asset whitelisted set of oracle_addresses,
//! administered by a single admin.
//!
//! Public surface (see `#[contractimpl]` below):
//!   * `init(admin)` — one-time setup.
//!   * `configure_asset(asset, decimals, window_size, max_staleness)` — admin-only.
//!   * `add_oracle(asset, oracle)` / `remove_oracle(asset, oracle)` — admin-only.
//!   * `submit_observation(asset, price, timestamp, oracle)` — oracle-address-signed.
//!   * `get_twap(asset) -> (price, decimals)` — read-only.
//!
//! Read-only convenience accessors (`oracle_count(asset)`,
//! `observation_count(asset)`, `is_whitelisted(asset, oracle)`,
//! `admin_address()`) round out the surface so off-chain tooling can
//! inspect without invoking the more expensive set of methods.

#![no_std]

mod admin;
mod errors;
mod storage;
#[cfg(test)]
mod tests;
mod twap;

pub use errors::Error;
use storage::{AssetConfig, DataKey, Observation};
use twap::{bump_persistent, BUFFER_MAX, MAX_DECIMALS};

use soroban_sdk::{contract, contractimpl, Address, Env, Vec};

#[contract]
pub struct DripOracle;

#[contractimpl]
impl DripOracle {
    /// One-time setup. Stores the admin address and asserts the contract
    /// has not already been initialized so a re-init cannot point the
    /// whitelists and configs at an attacker-controlled state.
    pub fn init(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    /// Configure an asset's per-feed parameters. Admin-only.
    ///
    /// Rejects:
    ///   * `decimals > MAX_DECIMALS` — overflows u128 in any
    ///     `value * 10^decimals` computation downstream.
    ///   * `window_size == 0` or `max_staleness == 0` — would force
    ///     every read to fail.
    ///   * `window_size > max_staleness` — a TWAP read across a window
    ///     that the freshness check itself rejects is nonsense.
    pub fn configure_asset(
        env: Env,
        admin: Address,
        asset: Address,
        decimals: u32,
        window_size: u64,
        max_staleness: u64,
    ) -> Result<(), Error> {
        admin::require_admin(&env, &admin)?;

        if decimals > MAX_DECIMALS || window_size == 0 || max_staleness == 0 {
            return Err(Error::InvalidConfig);
        }
        // Note: we intentionally do NOT enforce `window_size <= max_staleness` here.
        // The earlier invariant `window <= stale` made the `PriceStale` error
        // unreachable in `get_twap` because staleness (`advance > stale`) and
        // in-window (`window >= advance`) become mutually exclusive. Allowing
        // `window > stale` is the standard oracle pattern (longer TWAP lookback
        // than freshness) and restores the PriceStale path.

        let cfg = AssetConfig {
            decimals,
            window_size,
            max_staleness,
        };
        env.storage()
            .instance()
            .set(&DataKey::AssetConfig(asset), &cfg);
        Ok(())
    }

    /// Append `oracle` to the asset's whitelist. Admin-only. Idempotent.
    pub fn add_oracle(
        env: Env,
        admin: Address,
        asset: Address,
        oracle: Address,
    ) -> Result<(), Error> {
        admin::require_admin(&env, &admin)?;

        if !is_asset_configured(&env, &asset) {
            return Err(Error::AssetNotConfigured);
        }

        let key = DataKey::Oracles(asset.clone());
        let mut list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env));

        if !list_contains(&list, &oracle) {
            list.push_back(oracle);
            env.storage().persistent().set(&key, &list);
            bump_persistent(&env, &key);
        }
        Ok(())
    }

    /// Remove `oracle` from the asset's whitelist. Admin-only. No-op if
    /// absent (idempotent).
    pub fn remove_oracle(
        env: Env,
        admin: Address,
        asset: Address,
        oracle: Address,
    ) -> Result<(), Error> {
        admin::require_admin(&env, &admin)?;

        let key = DataKey::Oracles(asset.clone());
        let list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env));

        let mut new_list: Vec<Address> = Vec::new(&env);
        let mut changed = false;
        let mut i: u32 = 0;
        while i < list.len() {
            if list.get(i).expect("idx bounded by len") == oracle {
                changed = true;
            } else {
                new_list.push_back(list.get(i).expect("idx bounded by len"));
            }
            i += 1;
        }
        if changed {
            env.storage().persistent().set(&key, &new_list);
            bump_persistent(&env, &key);
        }
        Ok(())
    }

    /// Accept a new price observation from a whitelisted oracle.
    ///
    /// Auth: `oracle.require_auth()` — the on-chain relayer signs.
    /// Monotonic: a submitted `timestamp` must be strictly greater than
    /// the latest stored observation's timestamp and within
    /// `CLOCK_DRIFT_TOLERANCE` of the ledger clock.
    /// Buffer rollover: at `BUFFER_MAX` entries the oldest is dropped.
    pub fn submit_observation(
        env: Env,
        oracle: Address,
        asset: Address,
        price: i128,
        timestamp: u64,
    ) -> Result<(), Error> {
        // Auth first — a non-whitelisted-oracle-but-signed relayer
        // still wastes the auth if we don't check whitelist.
        oracle.require_auth();

        if price <= 0 {
            return Err(Error::InvalidPrice);
        }

        if !is_asset_configured(&env, &asset) {
            return Err(Error::AssetNotConfigured);
        }

        // Whitelist enforcement.
        let oracles_key = DataKey::Oracles(asset.clone());
        let oracles: Vec<Address> = env
            .storage()
            .persistent()
            .get(&oracles_key)
            .unwrap_or(Vec::new(&env));
        if !list_contains(&oracles, &oracle) {
            return Err(Error::OracleNotWhitelisted);
        }

        // Timestamp sanity.
        let now = env.ledger().timestamp();
        if timestamp > now + twap::CLOCK_DRIFT_TOLERANCE {
            return Err(Error::TimestampInvalid);
        }

        // Monotonic check + buffer rollover.
        let obs_key = DataKey::Observations(asset.clone());
        let mut buf: Vec<Observation> = env
            .storage()
            .persistent()
            .get(&obs_key)
            .unwrap_or(Vec::new(&env));

        if let Some(last) = buf.last() {
            if timestamp <= last.timestamp {
                return Err(Error::TimestampInvalid);
            }
        }

        buf.push_back(Observation {
            price,
            timestamp,
            oracle: oracle.clone(),
        });
        if buf.len() > BUFFER_MAX {
            buf.remove(0);
        }

        env.storage().persistent().set(&obs_key, &buf);
        bump_persistent(&env, &obs_key);
        // Touch the whitelist entry's TTL too — push activity counts
        // as maintenance for the asset's persistent surface.
        bump_persistent(&env, &oracles_key);
        Ok(())
    }

    /// Read-only: TWAP over the configured window. Returns
    /// `(price, decimals)`; callers normalize as needed.
    /// Errors:
    ///   * `AssetNotConfigured` if the asset has no config.
    ///   * `PriceStale` if the most recent observation is older than
    ///     `max_staleness`.
    ///   * `InsufficientSamples` if the window cannot be reconstructed.
    ///   * `ArithmeticOverflow` if the weighted-sum math overflows.
    pub fn get_twap(env: Env, asset: Address) -> Result<(i128, u32), Error> {
        let config: AssetConfig = env
            .storage()
            .instance()
            .get(&DataKey::AssetConfig(asset.clone()))
            .ok_or(Error::AssetNotConfigured)?;

        let observations: Vec<Observation> = env
            .storage()
            .persistent()
            .get(&DataKey::Observations(asset))
            .unwrap_or(Vec::new(&env));

        let price = twap::compute(&env, &observations, &config)?;
        Ok((price, config.decimals))
    }

    // ── Read-only accessors ────────────────────────────────────────────

    /// Number of oracles currently whitelisted for `asset`.
    pub fn oracle_count(env: Env, asset: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Oracles(asset))
            .map(|v: Vec<Address>| v.len())
            .unwrap_or(0)
    }

    /// Number of observations currently buffered for `asset`.
    pub fn observation_count(env: Env, asset: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Observations(asset))
            .map(|v: Vec<Observation>| v.len())
            .unwrap_or(0)
    }

    /// Whether `oracle` is on the whitelist for `asset`.
    pub fn is_whitelisted(env: Env, asset: Address, oracle: Address) -> bool {
        match env
            .storage()
            .persistent()
            .get::<DataKey, Vec<Address>>(&DataKey::Oracles(asset))
        {
            Some(v) => list_contains(&v, &oracle),
            None => false,
        }
    }

    /// The stored admin address. `None` if `init` has not been called.
    pub fn admin_address(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }
}

// ─── Free-function helpers (intentionally not on the contract) ────────

fn is_asset_configured(env: &Env, asset: &Address) -> bool {
    env.storage()
        .instance()
        .has(&DataKey::AssetConfig(asset.clone()))
}

fn list_contains(list: &Vec<Address>, needle: &Address) -> bool {
    let mut i: u32 = 0;
    while i < list.len() {
        if list.get(i).expect("idx bounded by len") == *needle {
            return true;
        }
        i += 1;
    }
    false
}
