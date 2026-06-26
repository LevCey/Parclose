//! Drives a full crossing window on the deployed Parclose contracts (Casper Testnet) and settles
//! it on-chain: open a window, escrow both legs, submit two sealed orders, close, clear + sign
//! off-chain with the dev attestation signer, settle on `CrossingEngine`, and withdraw.
//!
//! The deployer plays both sides (a redeem and a subscribe order) so the whole on-chain settle
//! path runs end to end with a single funded account. Every state-changing call prints its
//! cspr.live transaction link.
//!
//! ```text
//! ODRA_CASPER_LIVENET_* env set; cargo run --bin demo_window --features livenet
//! ```

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use odra::casper_types::bytesrepr::Bytes;
use odra::casper_types::U256;
use odra::host::HostRefLoader;
use odra::prelude::Address;
use rand_core::OsRng;

use parclose_enclave::{clear, open_window, AttestationContext, DevSigner, SealedSubmission};
use parclose_seal::{seal_order, EnclaveSecretKey};
use parclose_shared::{Order, SIDE_REDEEM, SIDE_SUBSCRIBE};

use contracts::cash_token::CashToken;
use contracts::crossing_engine::CrossingEngine;
use contracts::fund_token::FundToken;
use contracts::sealed_order_book::SealedOrderBook;
use contracts::window_registry::WindowRegistry;

// Deployed on Casper Testnet (casper-test), 2026-06-26.
const WINDOW_REGISTRY: &str =
    "hash-66f68780c36d3646415170125503198128965e369e0132719f42af26bece7190";
const FUND_TOKEN: &str = "hash-4922ed8af46bb36a5d5ab3507107c86d775e535ee58e9bd69ca25097024de39e";
const CASH_TOKEN: &str = "hash-0c9507ca709d750f99fcd4b9c69eddd93598f6323a9b2c73f28e5590d64f01eb";
const SEALED_ORDER_BOOK: &str =
    "hash-2895f3852fc8e070ff1b7fa74ededd46587c1d7e43badcd51b965d0a93b42a9d";
const CROSSING_ENGINE: &str =
    "hash-ead50d4643379c2b7d82f872d59449164501de13d8d0d42f35d0cd5dc93c9150";

const QTY: u64 = 100;
const PRICE: u64 = 100;
const CASH: u64 = QTY * PRICE; // cash leg = size * price
const TICK: u64 = 1;

// Gas budgets (motes), all under the block_gas_limit of 812.5 CSPR; 75% of unused is refunded.
const CALL_GAS: u64 = 100_000_000_000; // 100 CSPR per entrypoint call
const HEAVY_GAS: u64 = 250_000_000_000; // 250 CSPR for settle / withdraw / deposits

fn address(s: &str) -> Address {
    Address::from_str(s).expect("valid contract address")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn main() {
    let env = odra_casper_livenet_env::env();
    let me = env.caller();
    let engine_addr = address(CROSSING_ENGINE);

    let mut registry = WindowRegistry::load(&env, address(WINDOW_REGISTRY));
    let mut fund = FundToken::load(&env, address(FUND_TOKEN));
    let mut cash = CashToken::load(&env, address(CASH_TOKEN));
    let mut book = SealedOrderBook::load(&env, address(SEALED_ORDER_BOOK));
    let mut engine = CrossingEngine::load(&env, engine_addr);

    println!("== Parclose live crossing window (Casper Testnet) ==");
    println!("participant: {}", me.to_string());
    println!("fund/cash before: {} / {}\n", fund.balance_of(&me), cash.balance_of(&me));

    // 1. Open a window.
    env.set_gas(CALL_GAS);
    let wid = registry.open_window();
    println!("opened window #{wid}");

    // 2. Escrow both legs (the participant provides both the redeem and the subscribe side).
    env.set_gas(CALL_GAS);
    fund.approve(&engine_addr, &U256::from(QTY));
    env.set_gas(HEAVY_GAS);
    engine.deposit_fund(U256::from(QTY));
    env.set_gas(CALL_GAS);
    cash.approve(&engine_addr, &U256::from(CASH));
    env.set_gas(HEAVY_GAS);
    engine.deposit_cash(U256::from(CASH));
    println!("escrowed {QTY} fund + {CASH} cash");

    // 3. Seal two orders to a throwaway enclave key and post the ciphertext on-chain.
    let mut rng = OsRng;
    let enclave_sk = EnclaveSecretKey::generate(&mut rng);
    let enclave_pk = enclave_sk.public_key();
    let redeem = Order {
        side: SIDE_REDEEM,
        size: U256::from(QTY),
        limit: PRICE,
        window_id: wid,
        account: me,
    };
    let subscribe = Order {
        side: SIDE_SUBSCRIBE,
        size: U256::from(QTY),
        limit: PRICE,
        window_id: wid,
        account: me,
    };
    let ct_redeem = seal_order(&redeem, &enclave_pk, &mut rng).expect("seal redeem");
    let ct_subscribe = seal_order(&subscribe, &enclave_pk, &mut rng).expect("seal subscribe");

    env.set_gas(CALL_GAS);
    book.submit_sealed_order(wid, Bytes::from(ct_redeem.clone()));
    env.set_gas(CALL_GAS);
    book.submit_sealed_order(wid, Bytes::from(ct_subscribe.clone()));
    println!("submitted 2 sealed orders (ciphertext only)");

    // 4. Close the window.
    env.set_gas(CALL_GAS);
    registry.close_window(wid);
    println!("closed window #{wid}");

    // 5. Open the sealed set + clear, off-chain inside the (dev) enclave.
    let submissions = vec![
        SealedSubmission { submitter: me, ciphertext: ct_redeem },
        SealedSubmission { submitter: me, ciphertext: ct_subscribe },
    ];
    let opened = open_window(wid, &submissions, &enclave_sk);
    let result = clear(wid, &opened.orders, TICK);
    println!("cleared {} order(s) at uniform price {}", opened.orders.len(), result.price);

    // 6. Sign the attestation, binding input_hash to the on-chain order-book commitment.
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

    // 7. Settle on-chain (permissionless: a valid attestation is self-authorizing).
    env.set_gas(HEAVY_GAS);
    engine.settle(result, attestation);
    println!("settled window #{wid} on-chain");

    // 8. Withdraw settled credit + any unconsumed escrow.
    env.set_gas(HEAVY_GAS);
    engine.withdraw();

    println!("\nfund/cash after: {} / {}", fund.balance_of(&me), cash.balance_of(&me));
    println!("window consumed: {}", engine.is_window_consumed(wid));
    println!("private inputs -> verified fair clearing -> atomic on-chain settlement");
}
