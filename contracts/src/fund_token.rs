use odra::casper_types::U256;
use odra::prelude::*;
use odra_modules::cep18_token::Cep18;

/// Errors raised by the transfer-restricted fund token.
#[odra::odra_error]
pub enum Error {
    /// Caller is not the token admin.
    NotAdmin = 0,
    /// A party to the transfer is not whitelisted.
    NotWhitelisted = 1,
}

/// Emitted whenever the transfer whitelist changes, so the compliance control is
/// auditable on-chain.
#[odra::event]
pub struct Whitelisted {
    pub address: Address,
    pub allowed: bool,
}

/// FundToken — a compliant, transfer-restricted CEP-18 token.
///
/// Transfers (including escrow and settlement) are permitted only between
/// whitelisted holders. This is a stand-in for an ERC-3643-style compliant
/// security token; the whitelist stands in for a compliance registry.
#[odra::module(errors = Error, events = [Whitelisted])]
pub struct FundToken {
    token: SubModule<Cep18>,
    admin: Var<Address>,
    whitelist: Mapping<Address, bool>,
}

#[odra::module]
impl FundToken {
    /// Deploys the token, mints `initial_supply` to the deployer, and records
    /// the deployer as admin and as the first whitelisted holder.
    pub fn init(&mut self, name: String, symbol: String, decimals: u8, initial_supply: U256) {
        let deployer = self.env().caller();
        self.token.init(symbol, name, decimals, initial_supply);
        self.admin.set(deployer);
        // The deployer receives the initial supply, so it must be whitelisted to move it.
        self.whitelist.set(&deployer, true);
    }

    /// Adds or removes an address from the transfer whitelist. Admin only.
    pub fn set_whitelisted(&mut self, address: Address, allowed: bool) {
        self.assert_admin();
        self.whitelist.set(&address, allowed);
        self.env().emit_event(Whitelisted { address, allowed });
    }

    /// Returns whether `address` is permitted to send or receive the token.
    pub fn is_whitelisted(&self, address: &Address) -> bool {
        self.whitelist.get(address).unwrap_or(false)
    }

    /// Transfer-restricted: both the caller and the recipient must be whitelisted.
    pub fn transfer(&mut self, recipient: &Address, amount: &U256) {
        let caller = self.env().caller();
        self.assert_whitelisted(&caller);
        self.assert_whitelisted(recipient);
        self.token.transfer(recipient, amount);
    }

    /// Transfer-restricted: both the owner and the recipient must be whitelisted.
    pub fn transfer_from(&mut self, owner: &Address, recipient: &Address, amount: &U256) {
        self.assert_whitelisted(owner);
        self.assert_whitelisted(recipient);
        self.token.transfer_from(owner, recipient, amount);
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
        }
    }
}

impl FundToken {
    fn assert_admin(&self) {
        let caller = self.env().caller();
        let is_admin = self.admin.get().map(|a| a == caller).unwrap_or(false);
        if !is_admin {
            self.env().revert(Error::NotAdmin);
        }
    }

    fn assert_whitelisted(&self, address: &Address) {
        if !self.whitelist.get(address).unwrap_or(false) {
            self.env().revert(Error::NotWhitelisted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FundToken, FundTokenInitArgs};
    use odra::casper_types::U256;
    use odra::host::Deployer;
    use odra::prelude::*;

    fn args() -> FundTokenInitArgs {
        FundTokenInitArgs {
            name: "Parclose Fund".to_string(),
            symbol: "APF".to_string(),
            decimals: 9,
            initial_supply: U256::from(1_000_000u64),
        }
    }

    #[test]
    fn whitelisted_transfer_succeeds() {
        let env = odra_test::env();
        let deployer = env.get_account(0);
        let alice = env.get_account(1);
        let mut token = FundToken::deploy(&env, args());

        token.set_whitelisted(alice, true);

        env.set_caller(deployer);
        token.transfer(&alice, &U256::from(100u64));

        assert_eq!(token.balance_of(&alice), U256::from(100u64));
        assert_eq!(token.balance_of(&deployer), U256::from(999_900u64));
    }

    #[test]
    fn transfer_to_non_whitelisted_reverts() {
        let env = odra_test::env();
        let deployer = env.get_account(0);
        let bob = env.get_account(2); // never whitelisted
        let mut token = FundToken::deploy(&env, args());

        env.set_caller(deployer);
        let result = token.try_transfer(&bob, &U256::from(100u64));

        assert!(result.is_err());
        assert_eq!(token.balance_of(&bob), U256::zero());
    }

    #[test]
    fn non_admin_cannot_whitelist() {
        let env = odra_test::env();
        let mallory = env.get_account(3);
        let mut token = FundToken::deploy(&env, args());

        env.set_caller(mallory);
        let result = token.try_set_whitelisted(mallory, true);

        assert!(result.is_err());
        assert!(!token.is_whitelisted(&mallory));
    }
}
