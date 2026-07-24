use soroban_sdk::{contracttype, Address};

/// A single price observation submitted by a whitelisted oracle for a
/// given asset.
///
/// `oracle` is recorded so a future TWAP variant can apply
/// per-oracle weighting or quorum rules without changing the wire shape.
/// For v1, the `oracle` field is also stored on the whitelist entry
/// itself; duplication here is intentional insurance for the v2 upgrade.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Observation {
    pub price: i128,
    /// Ledger timestamp at which this observation was recorded. The
    /// contract enforces monotonic increase at submit time so the buffer
    /// stays oldest → newest sorted when using the standard ring-buffer
    /// rollover (`remove(0)` then `push_back`).
    pub timestamp: u64,
    /// Address that submitted the observation. Acts as a witness that
    /// the relayer is genuinely a whitelisted oracle.
    pub oracle: Address,
}

/// Per-asset configuration set by the admin.
///
/// `decimals` is the precision exponent of the `price` field supplied by
/// oracles for this asset. Callers wanting a normalized value should
/// divide `get_twap()`'s `i128` price by `10^decimals`.
///
/// `window_size` is the look-back window (in seconds) for TWAP
/// computation, and `max_staleness` is the upper bound on the age of the
/// most-recent observation accepted by `get_twap()`. The contract
/// requires `window_size <= max_staleness` so a TWAP read is never
/// forced into a window the freshness check would reject.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetConfig {
    pub decimals: u32,
    pub window_size: u64,
    pub max_staleness: u64,
}

/// Storage keys for the oracle contract.
///
/// - **Instance storage** holds the admin address and per-asset
///   configs. Both scale with the number of assets (bounded).
/// - **Persistent storage** holds per-asset whitelists and the
///   observation ring buffer. Each entry's TTL must be extended.
///
/// Splits mirror the pattern in drip-factory so a future
/// cross-contract TTL walker can be reused.
#[contracttype]
pub enum DataKey {
    /// **Instance storage.** Admin address.
    /// Key: `DataKey::Admin` (no inner type, discriminant only)
    /// Value: `Address`
    Admin,

    /// **Instance storage.** Per-asset configuration.
    /// Key: `DataKey::AssetConfig(Address)` — the asset's Stellar address
    /// Value: `AssetConfig` — `decimals`, `window_size`, `max_staleness`
    /// TTL: instance — extended when admin configures the asset.
    AssetConfig(Address),

    /// **Persistent storage.** Per-asset oracle whitelist.
    /// Key: `DataKey::Oracles(Address)` — the asset
    /// Value: `Vec<Address>` — oracle addresses allowed to push for this
    /// asset, in insertion order
    /// TTL: extended to `ttl::EXTEND_TO` on each whitelist mutation and
    /// each accepted `submit_observation`.
    Oracles(Address),

    /// **Persistent storage.** Per-asset ring buffer of recent observations.
    /// Key: `DataKey::Observations(Address)` — the asset
    /// Value: `Vec<Observation>` — oldest → newest (assuming the contract's
    /// monotonic-timestamp invariant holds)
    /// Size cap: `BUFFER_MAX` entries (see `twap.rs`).
    /// TTL: extended on each accepted `submit_observation`.
    Observations(Address),
}
