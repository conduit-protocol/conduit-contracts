//! TWAP computation + buffer-cap and TTL constants.
//!
//! The TWAP algorithm is the rolling-window variant: every accepted
//! `submit_observation` appends a new entry; once the buffer is full
//! (`BUFFER_MAX`), the oldest is dropped (`remove(0)`) so the buffer
//! stays newest-aligned and bounded in size. Time-weighted averaging is
//! done at read time, not write time, so the same buffer can serve
//! multiple `window_size` requests.

use soroban_sdk::{Env, Vec};

use crate::errors::Error;
use crate::storage::{AssetConfig, DataKey, Observation};

/// Maximum number of observations retained per asset. Sized so that a
/// single `Vec<Observation>` stays well under Soroban's per-entry
/// payload limit (~64KB) and so `get_twap` is `O(BUFFER_MAX)` — bounded
/// CPU regardless of how aggressively a feed pushes. With a worst-case
/// 1 observation/second push this caps each asset at ~5 minutes of
/// history; longer windows rely on `window_size` filtering at read
/// time, not on retaining more samples.
pub const BUFFER_MAX: u32 = 32;

/// Threshold below which we extend the persistent TTL on
/// `DataKey::Oracles(_)` and `DataKey::Observations(_)` so they do not
/// silently archive during an idle period.
pub const TTL_THRESHOLD: u32 = 100_000;

/// Target TTL on the persistent entries above after extension.
pub const TTL_EXTEND_TO: u32 = 200_000;

/// Maximum `decimals` allowed in `AssetConfig`. Mirrors u128's
/// `10^38` ceiling so an oracle cannot push a precision that overflows
/// the type when callers normalize the price.
pub const MAX_DECIMALS: u32 = 38;

/// Tolerance for an oracle pushing a "now" timestamp that is slightly
/// ahead of the host's ledger clock (e.g. multi-relayer propagation
/// delay or batched timestamp submission). Submissions further into
/// the future than this are rejected with `TimestampInvalid`.
pub const CLOCK_DRIFT_TOLERANCE: u64 = 30;

pub fn bump_persistent(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, TTL_THRESHOLD, TTL_EXTEND_TO);
}

/// Time-weighted average price over the configured `window_size`,
/// rejecting reads where the newest observation is older than
/// `max_staleness`. Returns the numerator/denominator sum as a single
/// `i128` division so the caller can apply any rounding policy on top.
///
/// Algorithm:
///   1. Reject empty buffer.
///   2. Find the first observation within `window_start = now -
///      window_size`. Reject if none fall in-window.
///   3. Staleness check: `age_of_newest > max_staleness` → PriceStale.
///   4. If only one in-window sample, return it directly.
///   5. Otherwise: sum `obs[i].price * dt_i` over each in-window pair
///      where `dt_i` is the gap until the next observation, plus
///      `obs[last].price * (now - obs[last].ts)` (the live "current"
///      segment extending from the most recent push to now). Divide
///      by the matching `sum(dt_i)`.
///
/// All multiplications use `checked_mul` and additions use
/// `checked_add` so an overflow surfaces as `ArithmeticOverflow`
/// rather than a panic.
pub fn compute(
    env: &Env,
    observations: &Vec<Observation>,
    config: &AssetConfig,
) -> Result<i128, Error> {
    let _ = env; // env kept for symmetry with future "now"-depending helpers; reads happen via env.ledger() in caller
    let window_size = config.window_size;
    let max_staleness = config.max_staleness;

    if observations.is_empty() {
        return Err(Error::InsufficientSamples);
    }

    let now = env.ledger().timestamp();
    let window_start = now.saturating_sub(window_size);

    // Search for first in-window observation. Defensive: contract
    // invariant says timestamps are append-only monotonic, so once
    // one is in-window, all of its successors are too.
    let mut first_in_window: Option<u32> = None;
    let mut i: u32 = 0;
    while i < observations.len() {
        let ts = observations.get_unchecked_ts(i);
        if ts >= window_start {
            first_in_window = Some(i);
            break;
        }
        i += 1;
    }
    let first_in_window = match first_in_window {
        Some(i) => i,
        None => return Err(Error::InsufficientSamples),
    };

    // Staleness check on the very newest observation, regardless of
    // whether it falls in the TWAP window.
    let len = observations.len();
    let newest_ts = observations.get_unchecked_ts(len - 1);
    let age_of_newest = now.saturating_sub(newest_ts);
    if age_of_newest > max_staleness {
        return Err(Error::PriceStale);
    }

    // Single in-window sample: just emit its price (no weighting).
    if len - first_in_window == 1 {
        return Ok(observations.get_unchecked_price(first_in_window));
    }

    // Walk the in-window samples, accumulating the time-weighted sum
    // of "price stayed at obs[i] until obs[i+1]".
    let mut weighted_sum: i128 = 0;
    let mut weight_total: i128 = 0;
    let mut j: u32 = first_in_window;
    while j < len - 1 {
        let price_j = observations.get_unchecked_price(j);
        let ts_j = observations.get_unchecked_ts(j);
        let ts_next = observations.get_unchecked_ts(j + 1);
        let dt = (ts_next - ts_j) as i128;
        let contribution = price_j.checked_mul(dt).ok_or(Error::ArithmeticOverflow)?;
        weighted_sum = weighted_sum
            .checked_add(contribution)
            .ok_or(Error::ArithmeticOverflow)?;
        weight_total = weight_total
            .checked_add(dt)
            .ok_or(Error::ArithmeticOverflow)?;
        j += 1;
    }

    // Last in-window sample's "weight" extends from its timestamp to
    // *now* (the current price segment).
    let last_idx = len - 1;
    let last_price = observations.get_unchecked_price(last_idx);
    let last_ts = observations.get_unchecked_ts(last_idx);
    let last_dt = (now - last_ts) as i128;
    let last_contribution = last_price
        .checked_mul(last_dt)
        .ok_or(Error::ArithmeticOverflow)?;
    weighted_sum = weighted_sum
        .checked_add(last_contribution)
        .ok_or(Error::ArithmeticOverflow)?;
    weight_total = weight_total
        .checked_add(last_dt)
        .ok_or(Error::ArithmeticOverflow)?;

    weighted_sum
        .checked_div(weight_total)
        .ok_or(Error::ArithmeticOverflow)
}

/// Tiny helper extensions on `Vec<Observation>` that read without
/// unwrap. `soroban_sdk::Vec::get()` returns `Option<T>`; centralising
/// the indexing keeps the math in `compute` readable.
trait ObservationVecExt {
    fn get_unchecked_ts(&self, idx: u32) -> u64;
    fn get_unchecked_price(&self, idx: u32) -> i128;
}

impl ObservationVecExt for Vec<Observation> {
    fn get_unchecked_ts(&self, idx: u32) -> u64 {
        self.get(idx).expect("idx bounded by len").timestamp
    }
    fn get_unchecked_price(&self, idx: u32) -> i128 {
        self.get(idx).expect("idx bounded by len").price
    }
}

// Belt-and-braces: ensure BUFFER_MAX fits in the xl determinism
// envelope (Soroban compile-time size guard). 32 << 64KB Vec cap.
const _: () = assert!(BUFFER_MAX <= 256, "BUFFER_MAX must fit Soroban Vec cap");
