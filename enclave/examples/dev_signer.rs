//! A runnable walk through the enclave's off-chain pipeline with the dev attestation signer.
//!
//! testnet/dev attestation signer — not a hardware TEE. It seals two orders to the enclave,
//! opens the sealed set, clears it, and signs the result into an attestation byte-compatible with
//! what `CrossingEngine` verifies on-chain.
//!
//! ```text
//! cargo run --example dev_signer
//! ```

use odra::casper_types::account::AccountHash;
use odra::casper_types::bytesrepr::{Bytes, ToBytes};
use odra::casper_types::U256;
use odra::prelude::Address;

use parclose_enclave::{clear, open_window, AttestationContext, DevSigner, SealedSubmission};
use parclose_seal::{blake2b_256, seal_order, EnclaveSecretKey};
use parclose_shared::{Order, SIDE_REDEEM, SIDE_SUBSCRIBE};
use rand_core::OsRng;

const WINDOW: u64 = 1;
const TICK: u64 = 1;
const PRICE: u64 = 100;
const QTY: u64 = 500;

fn account(n: u8) -> Address {
    Address::Account(AccountHash::new([n; 32]))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn order(side: u8, account_byte: u8) -> Order {
    Order {
        side,
        size: U256::from(QTY),
        limit: PRICE,
        window_id: WINDOW,
        account: account(account_byte),
    }
}

fn main() {
    let mut rng = OsRng;

    // The enclave holds two keys: an X25519 key for sealing/decryption and a secp256k1 key for
    // attestation signing. (The signing key is fixed here only to make the demo reproducible.)
    let seal_sk = EnclaveSecretKey::generate(&mut rng);
    let seal_pk = seal_sk.public_key();
    let signer = DevSigner::from_secp256k1_bytes([7u8; 32]).expect("valid key");

    println!("== Parclose dev attestation signer (testnet/dev — not a hardware TEE) ==");
    println!("enclave sealing pubkey (X25519): {}", hex(&seal_pk.to_bytes()));
    println!(
        "enclave signing pubkey (secp256k1): {}\n",
        hex(&signer.public_key().to_bytes().expect("encode pubkey"))
    );

    // A redeemer and a subscriber each seal an order to the enclave; only ciphertext would be
    // posted on-chain.
    let redeemer = order(SIDE_REDEEM, 1);
    let subscriber = order(SIDE_SUBSCRIBE, 2);
    let submissions = vec![
        SealedSubmission {
            submitter: redeemer.account,
            ciphertext: seal_order(&redeemer, &seal_pk, &mut rng).expect("seal"),
        },
        SealedSubmission {
            submitter: subscriber.account,
            ciphertext: seal_order(&subscriber, &seal_pk, &mut rng).expect("seal"),
        },
    ];
    println!("sealed {} orders (ciphertext only)", submissions.len());

    // The enclave opens the sealed set and clears it.
    let opened = open_window(WINDOW, &submissions, &seal_sk);
    println!("opened {} orders, {} rejected", opened.orders.len(), opened.rejected);
    let result = clear(WINDOW, &opened.orders, TICK);
    println!("uniform clearing price: {}", result.price);

    // Sign the result. The on-chain commitment (input_hash) would be SealedOrderBook's per-window
    // commitment; here we derive a stand-in over the posted ciphertext hashes for the demo.
    let mut commitment_preimage = Vec::new();
    for s in &submissions {
        commitment_preimage.extend_from_slice(&parclose_seal::ciphertext_hash(&s.ciphertext));
    }
    let input_hash = Bytes::from(blake2b_256(&commitment_preimage).to_vec());

    let context = AttestationContext {
        network: "casper-test".to_string(),
        crossing_engine: account(0xCE), // placeholder; the real engine address on Testnet
        rule_version: 1,
        function: "uniform_price_crossing_v1".to_string(),
        code_hash: Bytes::from(vec![0u8; 32]), // placeholder enclave measurement for the demo
        input_hash,
        secrets_hash: Bytes::from(blake2b_256(b"secrets").to_vec()),
        timestamp: 0,
        nonce: 1,
    };
    let attestation = signer.attest(&result, &context);

    println!("\n== signed attestation ==");
    println!("window_id     : {}", attestation.claim.window_id);
    println!("output_hash   : {}", hex(attestation.claim.output_hash.as_slice()));
    println!("nonce         : {}", attestation.claim.nonce);
    println!("signature len : {} bytes", attestation.signature.len());
    println!(
        "\nConfigure CrossingEngine.enclave_pubkey with the secp256k1 key above to verify and settle this."
    );
}
