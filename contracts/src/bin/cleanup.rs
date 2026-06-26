//! Resolves a stuck open/closed crossing window by settling it with an empty (no-cross) clearing
//! result, so the window-sequencing guard lets a fresh window open and any escrow becomes
//! withdrawable. Operates on the registry's current window.
//!
//! ```text
//! ODRA_CASPER_LIVENET_* env set; cargo run --bin cleanup --features livenet
//! ```

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use odra::casper_types::bytesrepr::Bytes;
use odra::host::HostRefLoader;
use odra::prelude::{Address, Vec};

use parclose_enclave::{AttestationContext, DevSigner};
use parclose_shared::ClearingResult;

use contracts::crossing_engine::CrossingEngine;
use contracts::sealed_order_book::SealedOrderBook;
use contracts::window_registry::WindowRegistry;

const WINDOW_REGISTRY: &str =
    "hash-66f68780c36d3646415170125503198128965e369e0132719f42af26bece7190";
const SEALED_ORDER_BOOK: &str =
    "hash-2895f3852fc8e070ff1b7fa74ededd46587c1d7e43badcd51b965d0a93b42a9d";
const CROSSING_ENGINE: &str =
    "hash-ead50d4643379c2b7d82f872d59449164501de13d8d0d42f35d0cd5dc93c9150";

const CALL_GAS: u64 = 100_000_000_000;
const HEAVY_GAS: u64 = 250_000_000_000;

fn address(s: &str) -> Address {
    Address::from_str(s).expect("valid contract address")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn main() {
    let env = odra_casper_livenet_env::env();
    let engine_addr = address(CROSSING_ENGINE);
    let mut registry = WindowRegistry::load(&env, address(WINDOW_REGISTRY));
    let book = SealedOrderBook::load(&env, address(SEALED_ORDER_BOOK));
    let mut engine = CrossingEngine::load(&env, engine_addr);

    let wid = registry.current_window_id();
    println!("resolving window #{wid} (consumed={})", engine.is_window_consumed(wid));

    if registry.is_open(wid) {
        env.set_gas(CALL_GAS);
        registry.close_window(wid);
        println!("closed window #{wid}");
    }

    // Empty no-cross clearing: nothing matched, so all escrow stays withdrawable.
    let result = ClearingResult { window_id: wid, price: 0, fills: Vec::new() };
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

    env.set_gas(HEAVY_GAS);
    engine.settle(result, attestation);
    println!("settled window #{wid} with an empty clearing (consumed)");

    env.set_gas(HEAVY_GAS);
    engine.withdraw();
    println!("withdrew unconsumed escrow");
}
