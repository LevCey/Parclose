//! Testnet deployment for Parclose, via the Odra livenet backend.
//!
//! Prerequisites: build the optimized wasm first (`cargo odra build`, with `wasm-opt` on PATH),
//! then provide the `ODRA_CASPER_LIVENET_*` environment variables (node address, chain name,
//! secret-key path, events url) and run:
//!
//! ```text
//! cargo run --bin deploy --features livenet
//! ```
//!
//! It deploys the five contracts, wires the registry to the engine (admin-once), whitelists the
//! custody endpoint and the deployer on the fund token, and prints every address for `.env`.
//!
//! Gas budgets below are starting estimates; large installs cost the most. Tune them if a deploy
//! reverts for insufficient gas, and watch the deployer balance on cspr.live.

use odra::casper_types::bytesrepr::{Bytes, ToBytes};
use odra::casper_types::{PublicKey, SecretKey, U256};
use odra::host::Deployer;
use odra::prelude::Addressable;

use contracts::cash_token::{CashToken, CashTokenInitArgs};
use contracts::crossing_engine::{CrossingEngine, CrossingEngineInitArgs};
use contracts::fund_token::{FundToken, FundTokenInitArgs};
use contracts::sealed_order_book::{SealedOrderBook, SealedOrderBookInitArgs};
use contracts::window_registry::{WindowRegistry, WindowRegistryInitArgs};

/// Gas for a contract install (motes). Must stay under the network block_gas_limit of
/// 812_500_000_000 (812.5 CSPR); the chainspec refunds 75% of unused gas, so headroom is cheap.
const GAS_DEPLOY: u64 = 800_000_000_000;
/// Gas for the largest install (CrossingEngine), still under the block gas limit.
const GAS_DEPLOY_ENGINE: u64 = 800_000_000_000;
/// Gas for a state-changing entry-point call.
const GAS_CALL: u64 = 20_000_000_000;

const NETWORK: &str = "casper-test";
const RULE: &str = "uniform-price crossing v1";
const FRESHNESS_MS: u64 = 3_600_000; // 1 hour
const SETTLEMENT_DEADLINE_MS: u64 = 86_400_000; // 24 hours

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let env = odra_casper_livenet_env::env();
    let deployer = env.caller();

    // The enclave attestation key: the same deterministic secp256k1 key the dev signer uses
    // (`DevSigner::from_secp256k1_bytes([7u8; 32])`), so its attestations verify on-chain.
    let enclave_sk = SecretKey::secp256k1_from_bytes([7u8; 32]).expect("valid secp256k1 key");
    let enclave_pubkey = PublicKey::from(&enclave_sk);
    let enclave_pubkey_hex = hex(&enclave_pubkey.to_bytes().expect("encode pubkey"));
    // Placeholder enclave code measurement; the dev signer signs with this same code_hash.
    let measurement = Bytes::from(vec![0u8; 32]);

    println!("Deployer: {}", deployer.to_string());
    println!("Network:  {NETWORK}");
    println!("Enclave pubkey (secp256k1): {enclave_pubkey_hex}\n");

    // 1. WindowRegistry
    env.set_gas(GAS_DEPLOY);
    let mut registry =
        WindowRegistry::deploy(&env, WindowRegistryInitArgs { initial_rule: RULE.to_string() });
    println!("WindowRegistry:  {}", registry.address().to_string());

    // 2. FundToken (compliant, transfer-restricted)
    env.set_gas(GAS_DEPLOY);
    let mut fund = FundToken::deploy(
        &env,
        FundTokenInitArgs {
            name: "Parclose Fund".to_string(),
            symbol: "PCF".to_string(),
            decimals: 9,
            initial_supply: U256::from(1_000_000u64),
        },
    );
    println!("FundToken:       {}", fund.address().to_string());

    // 3. CashToken (valueless test cash leg)
    env.set_gas(GAS_DEPLOY);
    let cash = CashToken::deploy(
        &env,
        CashTokenInitArgs {
            name: "Parclose Cash".to_string(),
            symbol: "PCC".to_string(),
            decimals: 9,
            initial_supply: U256::from(1_000_000u64),
        },
    );
    println!("CashToken:       {}", cash.address().to_string());

    // 4. SealedOrderBook
    env.set_gas(GAS_DEPLOY);
    let mut book = SealedOrderBook::deploy(
        &env,
        SealedOrderBookInitArgs { registry_address: registry.address() },
    );
    println!("SealedOrderBook: {}", book.address().to_string());

    // 5. CrossingEngine
    env.set_gas(GAS_DEPLOY_ENGINE);
    let engine = CrossingEngine::deploy(
        &env,
        CrossingEngineInitArgs {
            registry: registry.address(),
            order_book: book.address(),
            fund_token: fund.address(),
            cash_token: cash.address(),
            enclave_pubkey: enclave_pubkey.clone(),
            expected_measurement: measurement,
            network: NETWORK.to_string(),
            freshness_window: FRESHNESS_MS,
            settlement_deadline: SETTLEMENT_DEADLINE_MS,
        },
    );
    println!("CrossingEngine:  {}", engine.address().to_string());

    // Wire registry -> engine (admin-once), so window sequencing can read settled/expired status.
    env.set_gas(GAS_CALL);
    registry.set_crossing_engine(engine.address());
    println!("set_crossing_engine: ok");

    // Wire book -> engine (one-time), activating the escrow-backing submission gate (#2).
    book.set_crossing_engine(engine.address());
    println!("book set_crossing_engine: ok");

    // Whitelist the custody endpoint (engine) and the deployer on the fund token.
    env.set_gas(GAS_CALL);
    fund.set_whitelisted(engine.address(), true);
    env.set_gas(GAS_CALL);
    fund.set_whitelisted(deployer, true);
    println!("whitelisted engine + deployer");

    println!("\n=== addresses (copy into .env) ===");
    println!("WINDOW_REGISTRY_ADDRESS={}", registry.address().to_string());
    println!("FUND_TOKEN_ADDRESS={}", fund.address().to_string());
    println!("CASH_TOKEN_ADDRESS={}", cash.address().to_string());
    println!("SEALED_ORDER_BOOK_ADDRESS={}", book.address().to_string());
    println!("CROSSING_ENGINE_ADDRESS={}", engine.address().to_string());
    println!("ENCLAVE_PUBKEY={enclave_pubkey_hex}");
}
