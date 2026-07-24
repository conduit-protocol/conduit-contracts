//! Admin authorization helpers.
//!
//! Exposed as a free-function module so the `#[contractimpl]` block stays
//! short. Each admin-gated entry point calls `require_admin` first;
//! non-admin callers fail fast with `Error::NotAuthorized` while
//! `require_auth()` panics on missing signatures fire only on the
//! truly-unauthenticated path.

use soroban_sdk::{Address, Env};

use crate::errors::Error;
use crate::storage::DataKey;

/// Requires `caller` to equal the stored admin and to have signed the
/// transaction.
///
/// Order: read admin first (fail fast if storage has none), compare
/// addresses (fail fast on mismatch), then call `caller.require_auth()`
/// so a missing signature also traps cleanly. Reversing these would
/// burn the auth check on a non-admin caller.
pub fn require_admin(env: &Env, caller: &Address) -> Result<(), Error> {
    let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
    let admin = admin.ok_or(Error::NotInitialized)?;
    if *caller != admin {
        return Err(Error::NotAuthorized);
    }
    caller.require_auth();
    Ok(())
}
