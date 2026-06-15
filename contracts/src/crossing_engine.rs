use parclose_shared::{Attestation, AttestationClaim, ClearingResult};
use odra::casper_types::bytesrepr::{Bytes, ToBytes};
use odra::casper_types::{PublicKey, U256};
use odra::prelude::*;

use crate::cash_token::CashTokenContractRef;
use crate::fund_token::FundTokenContractRef;
use crate::sealed_order_book::SealedOrderBookContractRef;
use crate::window_registry::{WindowInfo, WindowRegistryContractRef};

#[odra::odra_error]
pub enum Error {
    /// A required trust-root parameter was never configured.
    NotConfigured = 0,
    /// Deposits are only accepted while the current window is open.
    WindowNotOpen = 1,
    /// Caller is not whitelisted on the fund token (required to escrow / be settled to).
    NotWhitelisted = 2,
    /// Arithmetic overflow on an escrow/credit balance.
    Overflow = 3,
    /// Attestation signature is not a valid signature by the configured enclave key.
    InvalidSignature = 4,
    /// `network` or `crossing_engine` in the claim does not bind to this deployment.
    DomainMismatch = 5,
    /// The window is not closed; settlement requires a closed window.
    WindowNotClosed = 6,
    /// This window has already been settled.
    WindowConsumed = 7,
    /// Claim `rule_version` does not match the registry's current rule version.
    RuleVersionMismatch = 8,
    /// Claim `code_hash` does not match the configured expected measurement.
    MeasurementMismatch = 9,
    /// Claim `input_hash` does not match the order book's commitment for the window.
    InputHashMismatch = 10,
    /// This nonce has already been consumed (replay).
    NonceUsed = 11,
    /// Claim timestamp is outside the freshness window.
    StaleAttestation = 12,
    /// The submitted result is for a different window than the claim.
    ResultWindowMismatch = 13,
    /// The submitted result does not hash to the claim's `output_hash`.
    OutputHashMismatch = 14,
    /// A fill spends more escrow than the account has deposited.
    InsufficientEscrow = 15,
    /// The result does not conserve value (sum of debits != sum of credits).
    ConservationViolated = 16,
    /// Escrow is locked while the current window is open or closed-but-unsettled.
    WithdrawLocked = 17,
    /// Nothing to withdraw for the caller.
    NothingToWithdraw = 18,
    /// Canonical encoding failed.
    Encoding = 19,
    /// The window has been expired (liveness escape); it can no longer be settled.
    WindowExpired = 20,
    /// The window's settlement deadline has not yet passed.
    DeadlineNotReached = 21,
}

#[odra::event]
pub struct EscrowDeposited {
    pub account: Address,
    /// true = fund token leg, false = cash token leg.
    pub is_fund: bool,
    pub amount: U256,
}

#[odra::event]
pub struct Settled {
    pub window_id: u64,
    pub price: u64,
    pub fill_count: u32,
}

#[odra::event]
pub struct Withdrawn {
    pub account: Address,
    pub fund_amount: U256,
    pub cash_amount: U256,
}

#[odra::event]
pub struct WindowExpired {
    pub window_id: u64,
}

/// CrossingEngine — verifies the enclave attestation and atomically settles a crossing from
/// pre-funded escrow.
///
/// Participants escrow their leg (`FundToken` for redeemers, `CashToken` for subscribers) while
/// the window is open; the fund-token whitelist is enforced at deposit (for both legs, since a
/// subscriber will *receive* fund tokens), establishing the compliance invariant before any
/// settlement can occur. After the window closes, anyone may submit the enclave's clearing
/// result + attestation to `settle` (permissionless — the attestation is self-authorizing).
///
/// `settle` performs no token transfers: it verifies the attestation (signature + full domain
/// binding + replay guard), confirms the result hashes to the attested `output_hash`, then moves
/// pre-funded escrow into withdrawable credit by internal bookkeeping only. Because no external
/// call can fail mid-settlement, the move is all-or-nothing. Tokens leave the contract only on
/// `withdraw`, pull-based, after the window is settled.
#[odra::module(
    errors = Error,
    events = [EscrowDeposited, Settled, Withdrawn, WindowExpired]
)]
pub struct CrossingEngine {
    registry: External<WindowRegistryContractRef>,
    order_book: External<SealedOrderBookContractRef>,
    fund_token: External<FundTokenContractRef>,
    cash_token: External<CashTokenContractRef>,
    // Configured trust root (set once at init, never inline).
    enclave_pubkey: Var<PublicKey>,
    expected_measurement: Var<Bytes>,
    network: Var<String>,
    freshness_window: Var<u64>,
    // How long after a window closes it may still be settled, in milliseconds. After this, anyone
    // may expire the window (liveness escape, I-7).
    settlement_deadline: Var<u64>,
    // Escrow ledger: committed maximum each account has deposited.
    escrow_fund: Mapping<Address, U256>,
    escrow_cash: Mapping<Address, U256>,
    // Settled-but-unwithdrawn proceeds.
    credit_fund: Mapping<Address, U256>,
    credit_cash: Mapping<Address, U256>,
    // Replay guards.
    consumed_window: Mapping<u64, bool>,
    used_nonce: Mapping<u64, bool>,
    // Windows expired via the liveness escape; mutually exclusive with consumed_window.
    expired_window: Mapping<u64, bool>,
}

#[odra::module]
impl CrossingEngine {
    /// Configures the engine. Trust-root parameters are set here, never inline at verify time.
    #[allow(clippy::too_many_arguments)]
    pub fn init(
        &mut self,
        registry: Address,
        order_book: Address,
        fund_token: Address,
        cash_token: Address,
        enclave_pubkey: PublicKey,
        expected_measurement: Bytes,
        network: String,
        freshness_window: u64,
        settlement_deadline: u64,
    ) {
        self.registry.set(registry);
        self.order_book.set(order_book);
        self.fund_token.set(fund_token);
        self.cash_token.set(cash_token);
        self.enclave_pubkey.set(enclave_pubkey);
        self.expected_measurement.set(expected_measurement);
        self.network.set(network);
        self.freshness_window.set(freshness_window);
        self.settlement_deadline.set(settlement_deadline);
    }

    /// Escrows fund tokens (the redeemer's leg). Requires the current window open and the caller
    /// whitelisted on the fund token. The caller must have approved this contract for `amount`.
    pub fn deposit_fund(&mut self, amount: U256) {
        self.require_window_open();
        let caller = self.env().caller();
        self.require_fund_whitelisted(&caller);
        let self_addr = self.env().self_address();
        self.fund_token.transfer_from(&caller, &self_addr, &amount);
        let new = self.checked_add(self.escrow_fund.get(&caller).unwrap_or_default(), amount);
        self.escrow_fund.set(&caller, new);
        self.env().emit_event(EscrowDeposited { account: caller, is_fund: true, amount });
    }

    /// Escrows cash tokens (the subscriber's leg). Requires the current window open and the caller
    /// whitelisted on the fund token (a subscriber receives fund tokens at settlement, so the
    /// compliance gate applies to this leg too). The caller must have approved this contract.
    pub fn deposit_cash(&mut self, amount: U256) {
        self.require_window_open();
        let caller = self.env().caller();
        self.require_fund_whitelisted(&caller);
        let self_addr = self.env().self_address();
        self.cash_token.transfer_from(&caller, &self_addr, &amount);
        let new = self.checked_add(self.escrow_cash.get(&caller).unwrap_or_default(), amount);
        self.escrow_cash.set(&caller, new);
        self.env().emit_event(EscrowDeposited { account: caller, is_fund: false, amount });
    }

    /// Settles a closed window from escrow. Permissionless: the attestation authorizes itself.
    pub fn settle(&mut self, result: ClearingResult, attestation: Attestation) {
        self.verify_attestation(&attestation);
        let claim = &attestation.claim;

        // The result must be for the attested window and hash to the attested output.
        if result.window_id != claim.window_id {
            self.env().revert(Error::ResultWindowMismatch);
        }
        let output_hash = Bytes::from(self.env().hash(self.encode(&result)).to_vec());
        if output_hash != claim.output_hash {
            self.env().revert(Error::OutputHashMismatch);
        }

        // Move escrow into withdrawable credit in a single pass. Each fill is checked against the
        // *running* escrow (decremented as we go), so two fills for the same account that
        // cumulatively exceed its escrow are caught with a named InsufficientEscrow rather than a
        // U256 underflow panic. A revert discards all writes, so settlement stays all-or-nothing.
        // The closing conservation check rejects any result that mints or burns value.
        let mut debit_fund = U256::zero();
        let mut debit_cash = U256::zero();
        let mut credit_fund = U256::zero();
        let mut credit_cash = U256::zero();
        for f in result.fills.iter() {
            let ef = self.escrow_fund.get(&f.account).unwrap_or_default();
            let ec = self.escrow_cash.get(&f.account).unwrap_or_default();
            if ef < f.fund_spent || ec < f.cash_spent {
                self.env().revert(Error::InsufficientEscrow);
            }
            self.escrow_fund.set(&f.account, ef - f.fund_spent);
            self.escrow_cash.set(&f.account, ec - f.cash_spent);
            let cf = self.credit_fund.get(&f.account).unwrap_or_default();
            let cc = self.credit_cash.get(&f.account).unwrap_or_default();
            self.credit_fund.set(&f.account, cf + f.fund_credit);
            self.credit_cash.set(&f.account, cc + f.cash_credit);
            debit_fund += f.fund_spent;
            debit_cash += f.cash_spent;
            credit_fund += f.fund_credit;
            credit_cash += f.cash_credit;
        }
        if debit_fund != credit_fund || debit_cash != credit_cash {
            self.env().revert(Error::ConservationViolated);
        }

        self.consumed_window.set(&claim.window_id, true);
        self.used_nonce.set(&claim.nonce, true);
        self.env().emit_event(Settled {
            window_id: claim.window_id,
            price: result.price,
            fill_count: result.fills.len() as u32,
        });
    }

    /// Liveness escape (I-7), permissionless: once a window has been closed for longer than the
    /// configured `settlement_deadline` without being settled, anyone may expire it. An expired
    /// window can no longer be settled, and its participants' escrow becomes withdrawable.
    /// `settle` and `expire_window` are mutually exclusive on the same window (a settled window
    /// cannot be expired, and an expired window cannot be settled), so escrow is never both paid
    /// out and refunded.
    pub fn expire_window(&mut self, window_id: u64) {
        if !self.registry.is_closed(window_id) {
            self.env().revert(Error::WindowNotClosed);
        }
        if self.consumed_window.get(&window_id).unwrap_or(false) {
            self.env().revert(Error::WindowConsumed);
        }
        if self.expired_window.get(&window_id).unwrap_or(false) {
            self.env().revert(Error::WindowExpired);
        }
        let info: WindowInfo = self
            .registry
            .get_window(window_id)
            .unwrap_or_else(|| self.env().revert(Error::WindowNotClosed));
        let deadline_at = info
            .closed_at
            .checked_add(self.settlement_deadline.get().unwrap_or_default())
            .unwrap_or_else(|| self.env().revert(Error::Overflow));
        if self.env().get_block_time() < deadline_at {
            self.env().revert(Error::DeadlineNotReached);
        }
        self.expired_window.set(&window_id, true);
        self.env().emit_event(WindowExpired { window_id });
    }

    /// Withdraws settled credit plus any unmatched escrow. Blocked while the current window is
    /// open or closed-but-unresolved; allowed once the current window is settled or expired (so
    /// escrow cannot be pulled out from under a pending clearing, but is always recoverable after).
    pub fn withdraw(&mut self) {
        let wid = self.registry.current_window_id();
        if wid != 0
            && !self.consumed_window.get(&wid).unwrap_or(false)
            && !self.expired_window.get(&wid).unwrap_or(false)
        {
            self.env().revert(Error::WithdrawLocked);
        }
        let caller = self.env().caller();
        let fund_out = self.escrow_fund.get(&caller).unwrap_or_default()
            + self.credit_fund.get(&caller).unwrap_or_default();
        let cash_out = self.escrow_cash.get(&caller).unwrap_or_default()
            + self.credit_cash.get(&caller).unwrap_or_default();
        if fund_out.is_zero() && cash_out.is_zero() {
            self.env().revert(Error::NothingToWithdraw);
        }
        // Zero balances before any external transfer (checks-effects-interactions).
        self.escrow_fund.set(&caller, U256::zero());
        self.credit_fund.set(&caller, U256::zero());
        self.escrow_cash.set(&caller, U256::zero());
        self.credit_cash.set(&caller, U256::zero());
        if !fund_out.is_zero() {
            self.fund_token.transfer(&caller, &fund_out);
        }
        if !cash_out.is_zero() {
            self.cash_token.transfer(&caller, &cash_out);
        }
        self.env().emit_event(Withdrawn {
            account: caller,
            fund_amount: fund_out,
            cash_amount: cash_out,
        });
    }

    /// The canonical commitment to a clearing result: `blake2b-256(result.to_bytes())`. The
    /// enclave sets `output_hash` to this; exposed so the off-chain signer commits to exactly
    /// what `settle` will check.
    pub fn compute_output_hash(&self, result: ClearingResult) -> Bytes {
        Bytes::from(self.env().hash(self.encode(&result)).to_vec())
    }

    pub fn escrow_fund_of(&self, account: Address) -> U256 {
        self.escrow_fund.get(&account).unwrap_or_default()
    }
    pub fn escrow_cash_of(&self, account: Address) -> U256 {
        self.escrow_cash.get(&account).unwrap_or_default()
    }
    pub fn credit_fund_of(&self, account: Address) -> U256 {
        self.credit_fund.get(&account).unwrap_or_default()
    }
    pub fn credit_cash_of(&self, account: Address) -> U256 {
        self.credit_cash.get(&account).unwrap_or_default()
    }
    pub fn is_window_consumed(&self, window_id: u64) -> bool {
        self.consumed_window.get(&window_id).unwrap_or(false)
    }
    pub fn is_window_expired(&self, window_id: u64) -> bool {
        self.expired_window.get(&window_id).unwrap_or(false)
    }
}

impl CrossingEngine {
    fn require_window_open(&self) {
        let wid = self.registry.current_window_id();
        if !self.registry.is_open(wid) {
            self.env().revert(Error::WindowNotOpen);
        }
    }

    fn require_fund_whitelisted(&self, account: &Address) {
        if !self.fund_token.is_whitelisted(account) {
            self.env().revert(Error::NotWhitelisted);
        }
    }

    /// Verifies the attestation signature and the full domain binding + replay guard. Reverts on
    /// any failure; returns normally only for a valid, bound, fresh, non-replayed attestation.
    fn verify_attestation(&self, attestation: &Attestation) {
        let claim: &AttestationClaim = &attestation.claim;

        let pubkey = self
            .enclave_pubkey
            .get()
            .unwrap_or_else(|| self.env().revert(Error::NotConfigured));
        let message = Bytes::from(self.encode(claim));
        if !self.env().verify_signature(&message, &attestation.signature, &pubkey) {
            self.env().revert(Error::InvalidSignature);
        }

        if claim.network != self.network.get().unwrap_or_default() {
            self.env().revert(Error::DomainMismatch);
        }
        if claim.crossing_engine != self.env().self_address() {
            self.env().revert(Error::DomainMismatch);
        }
        if !self.registry.is_closed(claim.window_id) {
            self.env().revert(Error::WindowNotClosed);
        }
        if self.consumed_window.get(&claim.window_id).unwrap_or(false) {
            self.env().revert(Error::WindowConsumed);
        }
        if self.expired_window.get(&claim.window_id).unwrap_or(false) {
            self.env().revert(Error::WindowExpired);
        }
        if claim.rule_version != self.registry.rule_version() {
            self.env().revert(Error::RuleVersionMismatch);
        }
        if claim.code_hash != self.expected_measurement.get().unwrap_or_default() {
            self.env().revert(Error::MeasurementMismatch);
        }
        if claim.input_hash != self.order_book.get_commitment(claim.window_id) {
            self.env().revert(Error::InputHashMismatch);
        }
        if self.used_nonce.get(&claim.nonce).unwrap_or(false) {
            self.env().revert(Error::NonceUsed);
        }

        let now = self.env().get_block_time();
        let skew = if now >= claim.timestamp {
            now - claim.timestamp
        } else {
            claim.timestamp - now
        };
        if skew > self.freshness_window.get().unwrap_or_default() {
            self.env().revert(Error::StaleAttestation);
        }
    }

    fn encode<T: ToBytes>(&self, value: &T) -> Vec<u8> {
        value
            .to_bytes()
            .unwrap_or_else(|_| self.env().revert(Error::Encoding))
    }

    fn checked_add(&self, a: U256, b: U256) -> U256 {
        a.checked_add(b)
            .unwrap_or_else(|| self.env().revert(Error::Overflow))
    }
}

#[cfg(test)]
mod tests {
    use super::{CrossingEngine, CrossingEngineHostRef, CrossingEngineInitArgs, Error};
    use crate::cash_token::{CashToken, CashTokenHostRef, CashTokenInitArgs};
    use crate::fund_token::{FundToken, FundTokenHostRef, FundTokenInitArgs};
    use crate::sealed_order_book::{SealedOrderBook, SealedOrderBookHostRef, SealedOrderBookInitArgs};
    use crate::window_registry::{WindowRegistry, WindowRegistryHostRef, WindowRegistryInitArgs};
    use parclose_shared::{Attestation, AttestationClaim, ClearingResult, Settlement};
    use odra::casper_types::bytesrepr::{Bytes, ToBytes};
    use odra::casper_types::{crypto, PublicKey, SecretKey, U256};
    use odra::host::{Deployer, HostEnv};
    use odra::prelude::*;

    const RULE: &str = "uniform-price crossing v1";
    const NETWORK: &str = "casper-test";
    const FUNCTION: &str = "uniform_price_crossing_v1";
    // Matched trade used across tests: redeemer sells 500 fund, subscriber buys at price 100.
    const QTY: u64 = 500;
    const PRICE: u64 = 100;
    const CASH: u64 = QTY * PRICE; // 50_000
    const DEADLINE_MS: u64 = 1_000; // settlement deadline for the liveness escape
    const FRESHNESS_MS: u64 = 3_600_000; // attestation freshness window (1 hour, ms)

    fn measurement() -> Bytes {
        Bytes::from(vec![0xCDu8; 32])
    }

    /// A deployed spine with the window open, both legs escrowed, and sealed orders submitted.
    /// Tests close the window (or not) and drive settlement themselves.
    struct Spine {
        env: HostEnv,
        registry: WindowRegistryHostRef,
        fund: FundTokenHostRef,
        cash: CashTokenHostRef,
        book: SealedOrderBookHostRef,
        engine: CrossingEngineHostRef,
        sk: SecretKey,
        pk: PublicKey,
        wid: u64,
        redeemer: Address,
        subscriber: Address,
    }

    fn setup() -> Spine {
        let env = odra_test::env();
        let admin = env.get_account(0);
        let redeemer = env.get_account(1);
        let subscriber = env.get_account(2);

        // Enclave key: a deterministic standalone secp256k1 keypair (not a Casper account).
        let sk = SecretKey::secp256k1_from_bytes([7u8; 32]).unwrap();
        let pk = PublicKey::from(&sk);

        let mut registry = WindowRegistry::deploy(
            &env,
            WindowRegistryInitArgs { initial_rule: RULE.to_string() },
        );
        let mut fund = FundToken::deploy(
            &env,
            FundTokenInitArgs {
                name: "Parclose Fund".to_string(),
                symbol: "APF".to_string(),
                decimals: 9,
                initial_supply: U256::from(1_000_000u64),
            },
        );
        let mut cash = CashToken::deploy(
            &env,
            CashTokenInitArgs {
                name: "Parclose Cash".to_string(),
                symbol: "APC".to_string(),
                decimals: 9,
                initial_supply: U256::from(1_000_000u64),
            },
        );
        let mut book = SealedOrderBook::deploy(
            &env,
            SealedOrderBookInitArgs { registry_address: registry.address() },
        );
        let mut engine = CrossingEngine::deploy(
            &env,
            CrossingEngineInitArgs {
                registry: registry.address(),
                order_book: book.address(),
                fund_token: fund.address(),
                cash_token: cash.address(),
                enclave_pubkey: pk.clone(),
                expected_measurement: measurement(),
                network: NETWORK.to_string(),
                freshness_window: FRESHNESS_MS,
                settlement_deadline: DEADLINE_MS,
            },
        );

        // Whitelist the custody endpoint and both participants; fund the participants.
        env.set_caller(admin);
        fund.set_whitelisted(engine.address(), true);
        fund.set_whitelisted(redeemer, true);
        fund.set_whitelisted(subscriber, true);
        fund.transfer(&redeemer, &U256::from(1_000u64));
        cash.transfer(&subscriber, &U256::from(100_000u64));
        // Close the registry↔engine deployment loop so the window-sequencing guard can read
        // settled/expired status (D-16).
        registry.set_crossing_engine(engine.address());

        let wid = registry.open_window();

        // Escrow both legs (each participant approves the engine, then deposits).
        env.set_caller(redeemer);
        fund.approve(&engine.address(), &U256::from(QTY));
        engine.deposit_fund(U256::from(QTY));

        env.set_caller(subscriber);
        cash.approve(&engine.address(), &U256::from(CASH));
        engine.deposit_cash(U256::from(CASH));

        // Sealed orders so the order book commitment (input_hash) is non-trivial.
        env.set_caller(redeemer);
        book.submit_sealed_order(wid, Bytes::from(b"redeem-order".to_vec()));
        env.set_caller(subscriber);
        book.submit_sealed_order(wid, Bytes::from(b"subscribe-order".to_vec()));

        Spine { env, registry, fund, cash, book, engine, sk, pk, wid, redeemer, subscriber }
    }

    /// The canonical clearing result for the matched trade.
    fn clearing_result(wid: u64, redeemer: Address, subscriber: Address) -> ClearingResult {
        ClearingResult {
            window_id: wid,
            price: PRICE,
            fills: vec![
                Settlement {
                    account: redeemer,
                    fund_spent: U256::from(QTY),
                    cash_spent: U256::zero(),
                    fund_credit: U256::zero(),
                    cash_credit: U256::from(CASH),
                },
                Settlement {
                    account: subscriber,
                    fund_spent: U256::zero(),
                    cash_spent: U256::from(CASH),
                    fund_credit: U256::from(QTY),
                    cash_credit: U256::zero(),
                },
            ],
        }
    }

    /// A fully valid claim for the matched trade. Negative tests mutate one field before signing.
    fn base_claim(
        engine_addr: Address,
        wid: u64,
        rule_version: u32,
        input_hash: Bytes,
        output_hash: Bytes,
        nonce: u64,
    ) -> AttestationClaim {
        AttestationClaim {
            network: NETWORK.to_string(),
            crossing_engine: engine_addr,
            window_id: wid,
            rule_version,
            function: FUNCTION.to_string(),
            code_hash: measurement(),
            input_hash: input_hash.clone(),
            secrets_hash: input_hash,
            output_hash,
            timestamp: 0,
            nonce,
        }
    }

    /// Signs a claim with the enclave key over its canonical bytes.
    fn sign_claim(sk: &SecretKey, pk: &PublicKey, claim: AttestationClaim) -> Attestation {
        let message = claim.to_bytes().unwrap();
        let signature = crypto::sign(message.as_slice(), sk, pk);
        Attestation {
            claim,
            signature: Bytes::from(signature.to_bytes().unwrap()),
        }
    }

    /// Closes the window and returns a valid `(input_hash, output_hash)` for the canonical result.
    fn close_and_commit(s: &mut Spine) -> (Bytes, Bytes) {
        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(s.wid);
        let output_hash = s
            .engine
            .compute_output_hash(clearing_result(s.wid, s.redeemer, s.subscriber));
        let input_hash = s.book.get_commitment(s.wid);
        (input_hash, output_hash)
    }

    #[test]
    fn full_spine_settles_and_withdraws() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);

        // Permissionless: a non-participant submits the settlement.
        s.env.set_caller(s.env.get_account(3));
        s.engine
            .settle(clearing_result(s.wid, s.redeemer, s.subscriber), attestation);

        assert!(s.engine.is_window_consumed(s.wid));
        assert_eq!(s.engine.credit_cash_of(s.redeemer), U256::from(CASH));
        assert_eq!(s.engine.credit_fund_of(s.subscriber), U256::from(QTY));
        assert_eq!(s.engine.escrow_fund_of(s.redeemer), U256::zero());
        assert_eq!(s.engine.escrow_cash_of(s.subscriber), U256::zero());

        // Redeemer withdraws cash proceeds; subscriber withdraws fund tokens.
        s.env.set_caller(s.redeemer);
        s.engine.withdraw();
        s.env.set_caller(s.subscriber);
        s.engine.withdraw();

        // Redeemer: started 1000 fund, escrowed 500 (kept 500), received 50_000 cash.
        assert_eq!(s.fund.balance_of(&s.redeemer), U256::from(500u64));
        assert_eq!(s.cash.balance_of(&s.redeemer), U256::from(CASH));
        // Subscriber: started 0 fund / 100_000 cash, escrowed 50_000 cash, received 500 fund.
        assert_eq!(s.fund.balance_of(&s.subscriber), U256::from(QTY));
        assert_eq!(s.cash.balance_of(&s.subscriber), U256::from(50_000u64));
    }

    // --- verify / binding failure tests (I-2, W1.6.2): each tampers exactly one thing and
    // asserts settlement reverts for the specific reason; the happy path proves these guards
    // are otherwise satisfied, so each revert isolates one guard. ---

    #[test]
    fn settle_before_close_reverts() {
        let mut s = setup();
        // Build a fully valid attestation but never close the window.
        let output_hash = s
            .engine
            .compute_output_hash(clearing_result(s.wid, s.redeemer, s.subscriber));
        let input_hash = s.book.get_commitment(s.wid);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);

        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::WindowNotClosed.into()));
    }

    #[test]
    fn tampered_signature_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let mut attestation = sign_claim(&s.sk, &s.pk, claim);
        // Corrupt one signature byte.
        let mut sig = attestation.signature.inner_bytes().to_vec();
        sig[5] ^= 0xFF;
        attestation.signature = Bytes::from(sig);

        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::InvalidSignature.into()));
    }

    #[test]
    fn wrong_network_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let mut claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        claim.network = "casper-mainnet".to_string(); // signed, but binds to the wrong chain
        let attestation = sign_claim(&s.sk, &s.pk, claim);

        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::DomainMismatch.into()));
    }

    #[test]
    fn wrong_measurement_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let mut claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        claim.code_hash = Bytes::from(vec![0xEEu8; 32]); // not the configured measurement
        let attestation = sign_claim(&s.sk, &s.pk, claim);

        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::MeasurementMismatch.into()));
    }

    #[test]
    fn wrong_input_hash_reverts() {
        let mut s = setup();
        let (_input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let bad_input = Bytes::from(vec![0x01u8; 32]); // not the order-book commitment
        let claim = base_claim(s.engine.address(), s.wid, rule_version, bad_input, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);

        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::InputHashMismatch.into()));
    }

    #[test]
    fn output_hash_mismatch_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);

        // Submit a result that does not match the attested output_hash (price tampered).
        let mut tampered = clearing_result(s.wid, s.redeemer, s.subscriber);
        tampered.price = 999;
        assert_eq!(s.engine.try_settle(tampered, attestation), Err(Error::OutputHashMismatch.into()));
    }

    #[test]
    fn replayed_window_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(
            s.engine.address(),
            s.wid,
            rule_version,
            input_hash.clone(),
            output_hash.clone(),
            1,
        );
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        s.engine
            .settle(clearing_result(s.wid, s.redeemer, s.subscriber), attestation);

        // A second, independently valid attestation (fresh nonce) for the same window is rejected.
        let claim2 = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 2);
        let attestation2 = sign_claim(&s.sk, &s.pk, claim2);
        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation2), Err(Error::WindowConsumed.into()));
    }

    // --- liveness escape (I-7, D-14): a closed-but-never-settled window can always be expired
    // after the deadline, refunding escrow; settle and expire are mutually exclusive. ---

    #[test]
    fn expire_after_deadline_unlocks_escrow() {
        let mut s = setup();
        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(s.wid);
        s.env.advance_block_time(DEADLINE_MS + 1);

        // Permissionless.
        s.env.set_caller(s.env.get_account(3));
        s.engine.expire_window(s.wid);
        assert!(s.engine.is_window_expired(s.wid));
        assert!(!s.engine.is_window_consumed(s.wid));

        // Both participants recover their full escrow (nothing was settled).
        s.env.set_caller(s.redeemer);
        s.engine.withdraw();
        s.env.set_caller(s.subscriber);
        s.engine.withdraw();
        assert_eq!(s.fund.balance_of(&s.redeemer), U256::from(1_000u64));
        assert_eq!(s.cash.balance_of(&s.subscriber), U256::from(100_000u64));
    }

    #[test]
    fn expire_before_deadline_reverts() {
        let mut s = setup();
        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(s.wid);
        // No time advanced: the deadline has not passed.
        assert_eq!(s.engine.try_expire_window(s.wid), Err(Error::DeadlineNotReached.into()));
    }

    #[test]
    fn expire_open_window_reverts() {
        let mut s = setup();
        // The window is still open.
        assert_eq!(s.engine.try_expire_window(s.wid), Err(Error::WindowNotClosed.into()));
    }

    #[test]
    fn settle_after_expire_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        s.env.advance_block_time(DEADLINE_MS + 1);
        s.engine.expire_window(s.wid);

        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::WindowExpired.into()));
    }

    #[test]
    fn expire_after_settle_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        s.engine
            .settle(clearing_result(s.wid, s.redeemer, s.subscriber), attestation);

        // The window is settled; it can never be expired (escrow is never both paid and refunded).
        s.env.advance_block_time(DEADLINE_MS + 1);
        assert_eq!(s.engine.try_expire_window(s.wid), Err(Error::WindowConsumed.into()));
    }

    #[test]
    fn expire_twice_reverts() {
        let mut s = setup();
        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(s.wid);
        s.env.advance_block_time(DEADLINE_MS + 1);
        s.engine.expire_window(s.wid);
        assert_eq!(s.engine.try_expire_window(s.wid), Err(Error::WindowExpired.into()));
    }

    // --- window sequencing guard (D-16): a new window may not open until the previous one is
    // resolved (settled or expired); the registry reads that status from the engine. ---

    #[test]
    fn open_next_window_after_settle_succeeds() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        s.engine
            .settle(clearing_result(s.wid, s.redeemer, s.subscriber), attestation);

        s.env.set_caller(s.env.get_account(0));
        let w2 = s.registry.open_window();
        assert_eq!(w2, s.wid + 1);
        assert!(s.registry.is_open(w2));
    }

    #[test]
    fn open_next_window_after_expire_succeeds() {
        let mut s = setup();
        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(s.wid);
        s.env.advance_block_time(DEADLINE_MS + 1);
        s.engine.expire_window(s.wid);

        s.env.set_caller(s.env.get_account(0));
        let w2 = s.registry.open_window();
        assert_eq!(w2, s.wid + 1);
        assert!(s.registry.is_open(w2));
    }

    #[test]
    fn open_next_window_before_resolve_reverts() {
        let mut s = setup();
        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(s.wid);
        // Previous window is closed but neither settled nor expired.
        assert_eq!(
            s.registry.try_open_window(),
            Err(crate::window_registry::Error::PreviousWindowUnresolved.into())
        );
    }

    /// Window-binding of the order commitment: the same orders under a different window_id yield a
    /// different commitment (window_id is part of the commitment preimage).
    #[test]
    fn commitment_differs_across_windows() {
        let mut s = setup();
        let c1 = s.book.get_commitment(s.wid);

        // Resolve window 1 (expire) so a second window may open.
        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(s.wid);
        s.env.advance_block_time(DEADLINE_MS + 1);
        s.engine.expire_window(s.wid);
        let w2 = s.registry.open_window();

        // Replay the identical orders (same submitters, same ciphertexts, same order) into w2.
        s.env.set_caller(s.redeemer);
        s.book.submit_sealed_order(w2, Bytes::from(b"redeem-order".to_vec()));
        s.env.set_caller(s.subscriber);
        s.book.submit_sealed_order(w2, Bytes::from(b"subscribe-order".to_vec()));
        let c2 = s.book.get_commitment(w2);

        assert_ne!(s.wid, w2);
        assert_ne!(c1, c2);
    }

    // --- remaining verify/binding + accounting tests (W1.6.2 completion) ---

    #[test]
    fn stale_attestation_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        // Advance well past the freshness window; the claim's timestamp (0) is now stale.
        s.env.advance_block_time(FRESHNESS_MS + 1);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::StaleAttestation.into()));
    }

    #[test]
    fn wrong_rule_version_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let mut claim = base_claim(s.engine.address(), s.wid, 999, input_hash, output_hash, 1);
        claim.rule_version = 999; // not the registry's current version
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::RuleVersionMismatch.into()));
    }

    #[test]
    fn wrong_crossing_engine_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        // Bind the claim to a different engine address.
        let other = s.env.get_account(7);
        let claim = base_claim(other, s.wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        let result = clearing_result(s.wid, s.redeemer, s.subscriber);
        assert_eq!(s.engine.try_settle(result, attestation), Err(Error::DomainMismatch.into()));
    }

    #[test]
    fn nonce_reuse_across_windows_reverts() {
        let mut s = setup();
        let (input_hash, output_hash) = close_and_commit(&mut s);
        let rule_version = s.registry.rule_version();
        let claim1 = base_claim(s.engine.address(), s.wid, rule_version, input_hash, output_hash, 1);
        let att1 = sign_claim(&s.sk, &s.pk, claim1);
        s.engine
            .settle(clearing_result(s.wid, s.redeemer, s.subscriber), att1);

        // Open and close a second (empty) window, then try to settle it reusing nonce 1.
        s.env.set_caller(s.env.get_account(0));
        let w2 = s.registry.open_window();
        s.registry.close_window(w2);
        let empty = || ClearingResult { window_id: w2, price: 0, fills: vec![] };
        let oh2 = s.engine.compute_output_hash(empty());
        let ih2 = s.book.get_commitment(w2);
        let claim2 = base_claim(s.engine.address(), w2, rule_version, ih2, oh2, 1); // nonce 1 reused
        let att2 = sign_claim(&s.sk, &s.pk, claim2);
        assert_eq!(s.engine.try_settle(empty(), att2), Err(Error::NonceUsed.into()));
    }

    #[test]
    fn partial_fill_refunds_unmatched_escrow() {
        let mut s = setup();
        let (red, sub, wid) = (s.redeemer, s.subscriber, s.wid);
        let part_qty = 300u64;
        let part_cash = part_qty * PRICE; // 30_000
        // Only 300 of the redeemer's 500 fund escrow and 30_000 of the subscriber's 50_000 cash
        // escrow are matched; the remainders stay refundable.
        let make = || ClearingResult {
            window_id: wid,
            price: PRICE,
            fills: vec![
                Settlement {
                    account: red,
                    fund_spent: U256::from(part_qty),
                    cash_spent: U256::zero(),
                    fund_credit: U256::zero(),
                    cash_credit: U256::from(part_cash),
                },
                Settlement {
                    account: sub,
                    fund_spent: U256::zero(),
                    cash_spent: U256::from(part_cash),
                    fund_credit: U256::from(part_qty),
                    cash_credit: U256::zero(),
                },
            ],
        };

        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(wid);
        let output_hash = s.engine.compute_output_hash(make());
        let input_hash = s.book.get_commitment(wid);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        s.engine.settle(make(), attestation);

        s.env.set_caller(red);
        s.engine.withdraw();
        s.env.set_caller(sub);
        s.engine.withdraw();

        // Redeemer: 1000 - 500 escrowed + 200 unmatched refund = 700 fund; 30_000 cash proceeds.
        assert_eq!(s.fund.balance_of(&red), U256::from(700u64));
        assert_eq!(s.cash.balance_of(&red), U256::from(part_cash));
        // Subscriber: 300 fund proceeds; 100_000 - 50_000 + 20_000 unmatched refund = 70_000 cash.
        assert_eq!(s.fund.balance_of(&sub), U256::from(part_qty));
        assert_eq!(s.cash.balance_of(&sub), U256::from(70_000u64));
    }

    #[test]
    fn duplicate_account_fill_reverts() {
        let mut s = setup();
        let (red, wid) = (s.redeemer, s.wid);
        // Two fills for the redeemer spending 300 + 300 = 600 fund > 500 escrowed: the second
        // fill must revert with a named InsufficientEscrow, not a U256 underflow panic.
        let make = || ClearingResult {
            window_id: wid,
            price: PRICE,
            fills: vec![
                Settlement {
                    account: red,
                    fund_spent: U256::from(300u64),
                    cash_spent: U256::zero(),
                    fund_credit: U256::zero(),
                    cash_credit: U256::from(30_000u64),
                },
                Settlement {
                    account: red,
                    fund_spent: U256::from(300u64),
                    cash_spent: U256::zero(),
                    fund_credit: U256::zero(),
                    cash_credit: U256::from(30_000u64),
                },
            ],
        };

        s.env.set_caller(s.env.get_account(0));
        s.registry.close_window(wid);
        let output_hash = s.engine.compute_output_hash(make());
        let input_hash = s.book.get_commitment(wid);
        let rule_version = s.registry.rule_version();
        let claim = base_claim(s.engine.address(), wid, rule_version, input_hash, output_hash, 1);
        let attestation = sign_claim(&s.sk, &s.pk, claim);
        assert_eq!(s.engine.try_settle(make(), attestation), Err(Error::InsufficientEscrow.into()));
    }
}
