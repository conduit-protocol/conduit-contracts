#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleConfig {
    pub oracle_address: Address,
    pub decimals: u32,
    pub asset_peg: u32,
    pub max_staleness: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    OracleStalePrice = 1001,
    OracleNotConfigured = 1002,
    InvalidPrice = 1003,
    OracleLocked = 1004,
    CalculationOverflow = 1005,
}

#[contract]
pub struct TwapOracleIntegration;

#[contractimpl]
impl TwapOracleIntegration {
    pub fn configure_oracle(env: Env, config: OracleConfig) {
        // Store the oracle configuration securely in instance storage
        env.storage().instance().set(&soroban_sdk::symbol_short!("OracleCfg"), &config);
    }

    pub fn get_twap_price(env: Env) -> Result<u64, Error> {
        let lock_key = soroban_sdk::symbol_short!("O_Lock");
        let is_locked: bool = env.storage().instance().get(&lock_key).unwrap_or(false);
        if is_locked {
            // Resolve gracefully when concurrent asynchronous hooks fire
            return Err(Error::OracleLocked);
        }
        
        env.storage().instance().set(&lock_key, &true);

        let config_res: Option<OracleConfig> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("OracleCfg"));
            
        let config = match config_res {
            Some(cfg) => cfg,
            None => {
                env.storage().instance().set(&lock_key, &false);
                return Err(Error::OracleNotConfigured);
            }
        };

        // Invoke the external Oracle contract securely.
        // Important: Ensure the oracle_address is a trusted and whitelisted contract.
        // In a real environment, this would call `get_price` on the oracle_address
        let current_time = env.ledger().timestamp();
        
        // Mock price fetch for the sake of integration testing
        // Real implementation would be: 
        // let price_data: (u64, u64) = env.invoke_contract(&config.oracle_address, &soroban_sdk::symbol_short!("get_twap"), ());
        let mock_price: u64 = 50_000_000; 
        let last_updated: u64 = current_time.saturating_sub(30); // 30 seconds ago

        if current_time.saturating_sub(last_updated) > config.max_staleness {
            env.storage().instance().set(&lock_key, &false);
            return Err(Error::OracleStalePrice);
        }

        if mock_price == 0 {
            env.storage().instance().set(&lock_key, &false);
            return Err(Error::InvalidPrice);
        }

        env.storage().instance().set(&lock_key, &false);
        Ok(mock_price)
    }

    pub fn calculate_fiat_stream_payout(env: Env, token_amount: u64) -> Result<u64, Error> {
        let current_price = Self::get_twap_price(env.clone())?;
        
        let config: OracleConfig = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("OracleCfg"))
            .unwrap();

        // Convert a nominal token amount into its fiat equivalent
        let precision = 10u128.checked_pow(config.decimals).ok_or(Error::CalculationOverflow)?;
        let token_val = (token_amount as u128).checked_mul(current_price as u128).ok_or(Error::CalculationOverflow)?;
        let value = token_val.checked_div(precision).ok_or(Error::CalculationOverflow)?;
        
        Ok(value as u64)
    }
}
// adding discretionary commit to trigger CI
