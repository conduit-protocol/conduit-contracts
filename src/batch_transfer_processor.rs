use soroban_sdk::{contract, contractimpl, contracttype, Env};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    ProcessorLocked = 2001,
    CalculationOverflow = 2002,
    BatchTooLarge = 2003,
}

#[contract]
pub struct BatchTransferProcessor;

#[contractimpl]
impl BatchTransferProcessor {
    pub fn process_batch(env: Env, amounts: soroban_sdk::Vec<u64>) -> Result<u64, Error> {
        let lock_key = soroban_sdk::symbol_short!("B_Lock");
        let is_locked: bool = env.storage().instance().get(&lock_key).unwrap_or(false);

        if is_locked {
            // resolve gracefully rather than corrupting data
            return Err(Error::ProcessorLocked);
        }

        env.storage().instance().set(&lock_key, &true);

        // boundary checks
        if amounts.len() > 100 {
            env.storage().instance().set(&lock_key, &false);
            return Err(Error::BatchTooLarge);
        }

        let mut total: u64 = 0;
        for amount in amounts.iter() {
            // precision / error-boundary handlers
            match total.checked_add(amount) {
                Some(new_total) => total = new_total,
                None => {
                    env.storage().instance().set(&lock_key, &false);
                    return Err(Error::CalculationOverflow);
                }
            }
        }

        env.storage().instance().set(&lock_key, &false);
        Ok(total)
    }
}
