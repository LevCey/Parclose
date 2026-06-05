use odra::casper_types::U256;
use odra::prelude::*;
use odra_modules::cep18_token::Cep18;

/// CashToken — a valueless CEP-18 test token used as the cash leg of settlement.
///
/// Testnet-only stand-in: it represents no real asset. Unlike the fund token it
/// is freely transferable.
#[odra::module]
pub struct CashToken {
    token: SubModule<Cep18>,
}

#[odra::module]
impl CashToken {
    /// Deploys the token and mints `initial_supply` to the deployer.
    pub fn init(&mut self, name: String, symbol: String, decimals: u8, initial_supply: U256) {
        self.token.init(symbol, name, decimals, initial_supply);
    }

    delegate! {
        to self.token {
            fn name(&self) -> String;
            fn symbol(&self) -> String;
            fn decimals(&self) -> u8;
            fn total_supply(&self) -> U256;
            fn balance_of(&self, address: &Address) -> U256;
            fn allowance(&self, owner: &Address, spender: &Address) -> U256;
            fn approve(&mut self, spender: &Address, amount: &U256);
            fn transfer(&mut self, recipient: &Address, amount: &U256);
            fn transfer_from(&mut self, owner: &Address, recipient: &Address, amount: &U256);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CashToken, CashTokenInitArgs};
    use odra::casper_types::U256;
    use odra::host::Deployer;
    use odra::prelude::*;

    #[test]
    fn free_transfer() {
        let env = odra_test::env();
        let deployer = env.get_account(0);
        let alice = env.get_account(1);
        let mut token = CashToken::deploy(
            &env,
            CashTokenInitArgs {
                name: "Aperture Cash".to_string(),
                symbol: "APC".to_string(),
                decimals: 9,
                initial_supply: U256::from(1_000_000u64),
            },
        );

        env.set_caller(deployer);
        token.transfer(&alice, &U256::from(250u64));

        assert_eq!(token.balance_of(&alice), U256::from(250u64));
        assert_eq!(token.balance_of(&deployer), U256::from(999_750u64));
    }
}
