//! Regression tests for
//! `conduit_integration_tests::batch_transfer_processor::BatchTransferProcessor`,
//! locking in the `audit-round-2-v2` fix that switched `Error` from
//! `#[contracttype]` to `#[contracterror]` + the matching
//! `Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord` derive set.
//!
//! A future regression to `#[contracttype]` will either:
//!   (1) fail to compile (the `#[contractimpl]` macro still requires
//!   `Copy` + `TryFrom<soroban_sdk::Error>` + `From<&Error>`, none of
//!   which `#[contracttype]` produces), or
//!   (2) silently mask the `Result<Result<T, Error>, _>` shape clients
//!   observe, breaking the discriminant assertions below.
//!
//! The contract-semantics tests additionally pin the locked-flag
//! release invariant: the lock must be released after every exit path
//! except `Error::ProcessorLocked` itself (which short-circuits before
//! touching the lock).

#![cfg(test)]

use conduit_integration_tests::batch_transfer_processor::{
    BatchTransferProcessor, BatchTransferProcessorClient, Error,
};
use soroban_sdk::{symbol_short, Env, Vec};

const LOCK_KEY: soroban_sdk::Symbol = symbol_short!("B_Lock");

fn deploy_processor(env: &Env) -> BatchTransferProcessorClient<'_> {
    let id = env.register_contract(None, BatchTransferProcessor);
    BatchTransferProcessorClient::new(env, &id)
}

/// Reads the processor's lock state from instance storage. Returns
/// `false` when the entry was never written, matching the contract's
/// own default of "unlocked".
fn lock_state(env: &Env, client: &BatchTransferProcessorClient<'_>) -> bool {
    env.as_contract(&client.address, || {
        env.storage().instance().get(&LOCK_KEY).unwrap_or(false)
    })
}

#[test]
fn process_batch_sums_amounts_and_releases_lock() {
    let env = Env::default();
    let client = deploy_processor(&env);
    let amounts = Vec::from_array(&env, [10u64, 20, 30]);
    assert_eq!(client.try_process_batch(&amounts), Ok(Ok(60)));
    assert!(
        !lock_state(&env, &client),
        "lock must be released after a successful call",
    );
}

#[test]
fn process_batch_empty_input_returns_zero() {
    let env = Env::default();
    let client = deploy_processor(&env);
    let amounts: Vec<u64> = Vec::new(&env);
    assert_eq!(client.try_process_batch(&amounts), Ok(Ok(0)));
    assert!(
        !lock_state(&env, &client),
        "lock must be released after an empty-input call",
    );
}

#[test]
fn process_batch_accepts_exactly_100_entries() {
    let env = Env::default();
    let client = deploy_processor(&env);
    let amounts = Vec::from_array(&env, [1u64; 100]);
    assert_eq!(client.try_process_batch(&amounts), Ok(Ok(100)));
    assert!(
        !lock_state(&env, &client),
        "lock must be released after a max-sized successful call",
    );
}

#[test]
fn process_batch_rejects_101_entries_and_releases_lock() {
    let env = Env::default();
    let client = deploy_processor(&env);
    let amounts = Vec::from_array(&env, [1u64; 101]);
    assert_eq!(
        client.try_process_batch(&amounts),
        Err(Ok(Error::BatchTooLarge)),
    );
    assert!(
        !lock_state(&env, &client),
        "BatchTooLarge must release the lock so the next caller is not \
         fooled by a stale flag",
    );
}

#[test]
fn process_batch_detects_overflow_and_releases_lock() {
    let env = Env::default();
    let client = deploy_processor(&env);
    // `u64::MAX + 1` is the smallest pair that overflows `checked_add`.
    let amounts = Vec::from_array(&env, [u64::MAX, 1u64]);
    assert_eq!(
        client.try_process_batch(&amounts),
        Err(Ok(Error::CalculationOverflow)),
    );
    assert!(
        !lock_state(&env, &client),
        "CalculationOverflow must release the lock",
    );
}

#[test]
fn process_batch_rejects_when_lock_is_held() {
    let env = Env::default();
    let client = deploy_processor(&env);

    // Simulate a previous call that exited before clearing the lock
    // (e.g. a host panic). The contract must reject the next call
    // gracefully and not corrupt the externally-imposed lock state.
    env.as_contract(&client.address, || {
        env.storage().instance().set(&LOCK_KEY, &true);
    });

    let amounts = Vec::from_array(&env, [42u64]);
    assert_eq!(
        client.try_process_batch(&amounts),
        Err(Ok(Error::ProcessorLocked)),
    );
    // The ProcessorLocked path is an early return BEFORE the contract
    // touches the lock. Lock in that invariant here: a future refactor
    // that adds a stray `set(lock_key, false)` (or any storage write)
    // adjacent to the early return would silently change a no-op-on-error
    // contract into a state-mutating one — this assertion catches it.
    assert!(
        lock_state(&env, &client),
        "ProcessorLocked must short-circuit without touching the lock",
    );
}

#[test]
fn error_type_carries_required_traits_and_named_discriminants() {
    fn assert_traits<T: Copy + Clone + core::fmt::Debug + Eq + PartialEq + PartialOrd + Ord>() {}
    assert_traits::<Error>();
    // Lock in the discriminant values so client integrators (and
    // downstream error handling in tests) cannot silently drift.
    assert_eq!(Error::ProcessorLocked as u32, 2001);
    assert_eq!(Error::CalculationOverflow as u32, 2002);
    assert_eq!(Error::BatchTooLarge as u32, 2003);
}
