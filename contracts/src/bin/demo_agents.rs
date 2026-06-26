//! Two autonomous liquidity agents drive a real economic crossing window on the deployed Parclose
//! contracts (Casper Testnet) and settle it on-chain.
//!
//! Account 0 (deployer) is a **redeem** agent holding fund tokens; account 1 is a **subscribe**
//! agent holding cash. Each reasons blind over the same public market view, reaches its own sealed
//! order, escrows its leg, and submits the ciphertext under its own key. The window is then
//! cleared off-chain (uniform price), the result is signed by the dev attestation signer, and
//! `CrossingEngine` verifies + settles it on-chain. Both agents withdraw their proceeds.
//!
//! Requires two funded accounts:
//! ```text
//! ODRA_CASPER_LIVENET_SECRET_KEY_PATH=<deployer.pem>
//! ODRA_CASPER_LIVENET_KEY_1=<subscriber.pem>
//! ... other ODRA_CASPER_LIVENET_* ...
//! cargo run --bin demo_agents --features livenet
//! ```

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use odra::casper_types::bytesrepr::Bytes;
use odra::casper_types::U256;
use odra::host::HostRefLoader;
use odra::prelude::Address;
use rand_core::OsRng;

use parclose_agents::{seal_decision, Agent, AgentPersona, Decision, OfflineHeuristicLLM, Perception, Side};
use parclose_enclave::{clear, open_window, AttestationContext, DevSigner, SealedSubmission};
use parclose_seal::EnclaveSecretKey;

use contracts::cash_token::CashToken;
use contracts::crossing_engine::CrossingEngine;
use contracts::fund_token::FundToken;
use contracts::sealed_order_book::SealedOrderBook;
use contracts::window_registry::WindowRegistry;

const WINDOW_REGISTRY: &str =
    "hash-66f68780c36d3646415170125503198128965e369e0132719f42af26bece7190";
const FUND_TOKEN: &str = "hash-4922ed8af46bb36a5d5ab3507107c86d775e535ee58e9bd69ca25097024de39e";
const CASH_TOKEN: &str = "hash-0c9507ca709d750f99fcd4b9c69eddd93598f6323a9b2c73f28e5590d64f01eb";
const SEALED_ORDER_BOOK: &str =
    "hash-2895f3852fc8e070ff1b7fa74ededd46587c1d7e43badcd51b965d0a93b42a9d";
const CROSSING_ENGINE: &str =
    "hash-ead50d4643379c2b7d82f872d59449164501de13d8d0d42f35d0cd5dc93c9150";

const NAV: u64 = 1_000;
const PRIOR_CLEAR: u64 = 990;
const TICK: u64 = 1;
const MAX_SIZE: u64 = 100;
const CASH_TO_SUBSCRIBER: u64 = 100_000; // seeded to account 1 so it can escrow the cash leg

const CALL_GAS: u64 = 100_000_000_000;
const HEAVY_GAS: u64 = 250_000_000_000;

fn address(s: &str) -> Address {
    Address::from_str(s).expect("valid contract address")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn print_decision(name: &str, d: &Decision) {
    let side = d.side().map(Side::label).unwrap_or("?");
    println!("  {name}: {side} {} @ limit {}", d.size(), d.limit());
    println!("    rationale: {}", d.rationale);
}

fn main() {
    let env = odra_casper_livenet_env::env();
    let redeemer = env.get_account(0); // deployer, holds fund
    let subscriber = env.get_account(1); // account 1, holds cash
    let engine_addr = address(CROSSING_ENGINE);

    let mut registry = WindowRegistry::load(&env, address(WINDOW_REGISTRY));
    let mut fund = FundToken::load(&env, address(FUND_TOKEN));
    let mut cash = CashToken::load(&env, address(CASH_TOKEN));
    let mut book = SealedOrderBook::load(&env, address(SEALED_ORDER_BOOK));
    let mut engine = CrossingEngine::load(&env, engine_addr);

    println!("== Parclose: two agents, one live crossing window (Casper Testnet) ==");
    println!("redeemer (acct 0):   {}", redeemer.to_string());
    println!("subscriber (acct 1): {}\n", subscriber.to_string());

    // --- Admin setup (deployer): whitelist the subscriber and seed it with cash. ---
    env.set_caller(redeemer);
    env.set_gas(CALL_GAS);
    fund.set_whitelisted(subscriber, true);
    env.set_gas(CALL_GAS);
    cash.transfer(&subscriber, &U256::from(CASH_TO_SUBSCRIBER));

    // --- Open a window. ---
    env.set_gas(CALL_GAS);
    let wid = registry.open_window();
    println!("opened window #{wid}");

    // --- Each agent perceives the same public view and reasons blind. ---
    let perception = Perception {
        window_id: wid,
        attested_nav: NAV,
        prior_clear_price: Some(PRIOR_CLEAR),
        fund_supply: U256::from(1_000_000u64),
        price_tick: TICK,
    };
    let aria = Agent::new(
        AgentPersona {
            name: "Aria".into(),
            account: redeemer,
            mandate: Side::Redeem,
            fund_inventory: U256::from(MAX_SIZE),
            cash_inventory: U256::zero(),
            max_size: U256::from(MAX_SIZE),
            risk_appetite_bps: 300,
            style: "exiting inventory; prices to get filled".into(),
        },
        OfflineHeuristicLLM,
    );
    let boreas = Agent::new(
        AgentPersona {
            name: "Boreas".into(),
            account: subscriber,
            mandate: Side::Subscribe,
            fund_inventory: U256::zero(),
            cash_inventory: U256::from(200_000u64),
            max_size: U256::from(MAX_SIZE),
            risk_appetite_bps: 300,
            style: "accumulating; bids through fair to win the cross".into(),
        },
        OfflineHeuristicLLM,
    );

    let da = aria.decide(&perception).expect("Aria decides");
    let db = boreas.decide(&perception).expect("Boreas decides");
    println!("agents reasoned blind:");
    print_decision("Aria", &da);
    print_decision("Boreas", &db);

    // --- Seal each order to a throwaway enclave key. ---
    let mut rng = OsRng;
    let enclave_sk = EnclaveSecretKey::generate(&mut rng);
    let enclave_pk = enclave_sk.public_key();
    let ct_a = seal_decision(&da, &enclave_pk, &mut rng).expect("seal Aria");
    let ct_b = seal_decision(&db, &enclave_pk, &mut rng).expect("seal Boreas");

    // --- Aria (redeemer) escrows fund + submits, under account 0. ---
    env.set_caller(redeemer);
    env.set_gas(CALL_GAS);
    fund.approve(&engine_addr, &da.size());
    env.set_gas(HEAVY_GAS);
    engine.deposit_fund(da.size());
    env.set_gas(CALL_GAS);
    book.submit_sealed_order(wid, Bytes::from(ct_a.clone()));

    // --- Boreas (subscriber) escrows cash + submits, under account 1. ---
    env.set_caller(subscriber);
    let cash_escrow = U256::from(CASH_TO_SUBSCRIBER);
    env.set_gas(CALL_GAS);
    cash.approve(&engine_addr, &cash_escrow);
    env.set_gas(HEAVY_GAS);
    engine.deposit_cash(cash_escrow);
    env.set_gas(CALL_GAS);
    book.submit_sealed_order(wid, Bytes::from(ct_b.clone()));
    println!("both legs escrowed; both sealed orders submitted (ciphertext only)");

    // --- Close the window (admin). ---
    env.set_caller(redeemer);
    env.set_gas(CALL_GAS);
    registry.close_window(wid);

    // --- Clear off-chain (uniform price) and sign the attestation. ---
    let submissions = vec![
        SealedSubmission { submitter: redeemer, ciphertext: ct_a },
        SealedSubmission { submitter: subscriber, ciphertext: ct_b },
    ];
    let opened = open_window(wid, &submissions, &enclave_sk);
    let result = clear(wid, &opened.orders, TICK);
    println!("cleared {} orders at uniform price {}", opened.orders.len(), result.price);

    let input_hash = book.get_commitment(wid);
    let signer = DevSigner::from_secp256k1_bytes([7u8; 32]).expect("signer");
    let ctx = AttestationContext {
        network: "casper-test".to_string(),
        crossing_engine: engine_addr,
        rule_version: registry.rule_version(),
        function: "uniform_price_crossing_v1".to_string(),
        code_hash: Bytes::from(vec![0u8; 32]),
        input_hash: input_hash.clone(),
        secrets_hash: input_hash,
        timestamp: now_ms(),
        nonce: now_ms(),
    };
    let attestation = signer.attest(&result, &ctx);

    // --- Settle on-chain, then each agent withdraws. ---
    env.set_caller(redeemer);
    env.set_gas(HEAVY_GAS);
    engine.settle(result, attestation);
    println!("settled window #{wid} on-chain");

    env.set_caller(redeemer);
    env.set_gas(HEAVY_GAS);
    engine.withdraw();
    env.set_caller(subscriber);
    env.set_gas(HEAVY_GAS);
    engine.withdraw();

    println!("\n-- final balances --");
    println!("redeemer fund/cash:   {} / {}", fund.balance_of(&redeemer), cash.balance_of(&redeemer));
    println!("subscriber fund/cash: {} / {}", fund.balance_of(&subscriber), cash.balance_of(&subscriber));
    println!("window consumed: {}", engine.is_window_consumed(wid));
}
