//! TWAP oracle integration for fiat-denominated streams.
//!
//! NOTE ON BUILD STATUS: this module is not currently part of any compiled
//! target. The workspace root package (`conduit-integration-tests`) declares no
//! `mod oracle;` in `src/lib.rs`, and `soroban-sdk` is only a dev-dependency
//! there, so nothing below is type-checked by `cargo build`. Promoting it to a
//! real `contracts/oracle` crate (or deleting it) is a separate decision — see
//! the note at the bottom of this file.
//!
//! The defects fixed here were found while triaging issue #80, which described
//! an "async race condition" that does not apply to Soroban's synchronous,
//! single-shot execution model. These are the real problems the file had.

#![no_std]
use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env};

#[contracttype]
pub enum DataKey {
    /// Address permitted to reconfigure the feed and submit prices.
    Admin,
    /// Feed configuration (`OracleConfig`).
    Config,
    /// Most recently submitted price observation (`PriceData`).
    Price,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleConfig {
    pub oracle_address: Address,
    pub decimals: u32,
    pub asset_peg: u32,
    pub max_staleness: u64,
}

/// A single price observation, timestamped at submission.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    pub price: u64,
    /// Ledger timestamp at which this observation was recorded.
    pub updated_at: u64,
}

// Was `#[contracttype]`, which does not produce a valid contract error type.
// Only compiled dead code hid the mistake.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    OracleStalePrice = 1001,
    OracleNotConfigured = 1002,
    InvalidPrice = 1003,
    OracleLocked = 1004,
    CalculationOverflow = 1005,
    NotAuthorized = 1004,
    AlreadyInitialized = 1005,
    /// A price has been configured but never submitted.
    NoPriceAvailable = 1006,
    /// Fiat conversion overflowed `u64`.
    ArithmeticOverflow = 1007,
    /// `decimals` is too large to compute `10^decimals` in `u128`.
    InvalidDecimals = 1008,
}

#[contract]
pub struct TwapOracleIntegration;

#[contractimpl]
impl TwapOracleIntegration {
    /// Sets the account allowed to configure the feed and submit prices.
    ///
    /// Callable once. Without this, `configure_oracle` had no owner to check
    /// against and every setter below was world-writable.
    pub fn initialize(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    /// Stores the oracle configuration.
    ///
    /// Previously unauthenticated: any account could overwrite
    /// `oracle_address` and `max_staleness`, repointing the feed at a
    /// contract of their choosing or disabling the staleness guard outright
    /// by setting it to `u64::MAX`.
    pub fn configure_oracle(env: Env, caller: Address, config: OracleConfig) -> Result<(), Error> {
        require_admin(&env, &caller)?;

        // `calculate_fiat_stream_payout` computes `10^decimals` in `u128`;
        // anything past 38 overflows that. Reject at write time rather than
        // panicking on every later read.
        if config.decimals > 38 {
            return Err(Error::InvalidDecimals);
        }

        env.storage().instance().set(&DataKey::Config, &config);
        Ok(())
    }

    /// Records a price observation, stamped with the current ledger time.
    ///
    /// Replaces the previous hardcoded `mock_price` / `current_time - 30`
    /// pair, which made `get_twap_price` return a fabricated 50_000_000 and
    /// made the staleness comparison tautological — it compared a constant 30
    /// against `max_staleness` and so could only fail if the feed was
    /// configured with `max_staleness < 30`.
    pub fn submit_price(env: Env, caller: Address, price: u64) -> Result<(), Error> {
        require_admin(&env, &caller)?;

        if price == 0 {
            return Err(Error::InvalidPrice);
        }

        let data = PriceData {
            price,
            updated_at: env.ledger().timestamp(),
        };
        env.storage().instance().set(&DataKey::Price, &data);
        Ok(())
    }

    /// Returns the latest price, rejecting observations older than
    /// `max_staleness` seconds.
    pub fn get_twap_price(env: Env) -> Result<u64, Error> {
        let lock_key = soroban_sdk::symbol_short!("O_Lock");
        let is_locked: bool = env.storage().instance().get(&lock_key).unwrap_or(false);
        if is_locked {
            return Err(Error::OracleLocked);
        }

        env.storage().instance().set(&lock_key, &true);

        let config: OracleConfig = match env
            .storage()
            .instance()
            .get(&DataKey::Config)
        {
            Some(cfg) => cfg,
            None => {
                env.storage().instance().set(&lock_key, &false);
                return Err(Error::OracleNotConfigured);
            }
        };

        let data: PriceData = match env
            .storage()
            .instance()
            .get(&DataKey::Price)
        {
            Some(d) => d,
            None => {
                env.storage().instance().set(&lock_key, &false);
                return Err(Error::NoPriceAvailable);
            }
        };

        let age = env.ledger().timestamp().saturating_sub(data.updated_at);
        if age > config.max_staleness {
            env.storage().instance().set(&lock_key, &false);
            return Err(Error::OracleStalePrice);
        }

        if data.price == 0 {
            env.storage().instance().set(&lock_key, &false);
            return Err(Error::InvalidPrice);
        }

        env.storage().instance().set(&lock_key, &false);
        Ok(data.price)
    }

    /// Converts a nominal token amount into its fiat equivalent.
    pub fn calculate_fiat_stream_payout(env: Env, token_amount: u64) -> Result<u64, Error> {
        let current_price = Self::get_twap_price(env.clone())?;

        let config: OracleConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(Error::OracleNotConfigured)?;

        let precision = 10u128
            .checked_pow(config.decimals)
            .ok_or(Error::InvalidDecimals)?;

        let value = (token_amount as u128)
            .checked_mul(current_price as u128)
            .ok_or(Error::ArithmeticOverflow)?
            / precision;

        if value > u64::MAX as u128 {
            return Err(Error::ArithmeticOverflow);
        }

        Ok(value as u64)
    }
}

/// Requires that `caller` is the stored admin and authorized the transaction.
///
/// A free function rather than an associated one so it stays clearly outside
/// the exported contract surface generated by `#[contractimpl]`.
fn require_admin(env: &Env, caller: &Address) -> Result<(), Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::OracleNotConfigured)?;
    if *caller != admin {
        return Err(Error::NotAuthorized);
    }
    caller.require_auth();
    Ok(())
}

// REMAINING WORK (not done here — out of the /contracts scope this pass):
//
//  1. This module is still not compiled. Either promote it to a
//     `contracts/oracle` crate and add it to the workspace `members` list, or
//     delete it. Leaving it here means these fixes are inert.
//  2. `submit_price` is admin-push. The original intent (per the commented-out
//     `env.invoke_contract` call) was a pull from an external TWAP oracle at
//     `config.oracle_address`. That still needs to be written against a real
//     oracle interface, with `oracle_address` whitelisted.
//  3. `asset_peg` is stored but never read by any code path.
