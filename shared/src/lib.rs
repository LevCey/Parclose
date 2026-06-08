#![no_std]

//! Canonical cross-component encodings for Aperture.
//!
//! These types are the single source of truth shared by the contracts, the enclave (or its
//! mock-signer stub), and the agents. Two encodings must never diverge across components, or
//! verification breaks silently:
//!
//! 1. [`AttestationClaim`] — the message the enclave signs and `CrossingEngine` verifies. The
//!    canonical signing bytes are `claim.to_bytes()` (casper `bytesrepr` serialization). Both
//!    the signer and the contract serialize the *same* struct the *same* way, so the signed
//!    bytes match by construction.
//! 2. [`ClearingResult`] — the settlement instructions. `AttestationClaim::output_hash` commits
//!    to `blake2b-256(result.to_bytes())`; `CrossingEngine` settles exactly what that hash binds.
//!
//! All multi-byte integers in the `bytesrepr` encoding are little-endian fixed width (casper
//! convention); `String`/`Vec`/`Bytes` are length-prefixed. A non-Rust component reproducing
//! these encodings must follow casper `bytesrepr` rules.

extern crate alloc;

use odra::casper_types::bytesrepr::Bytes;
use odra::casper_types::U256;
use odra::prelude::*;

/// Order side: the submitter buys fund tokens (subscribes) at a price `<= limit`.
pub const SIDE_SUBSCRIBE: u8 = 0;
/// Order side: the submitter sells fund tokens (redeems) at a price `>= limit`.
pub const SIDE_REDEEM: u8 = 1;

/// A sealed order's plaintext. Encrypted to the enclave; only its ciphertext is ever on-chain
/// (in `SealedOrderBook`). Represented here as the canonical encoding the enclave decrypts to.
#[odra::odra_type]
pub struct Order {
    /// [`SIDE_SUBSCRIBE`] or [`SIDE_REDEEM`].
    pub side: u8,
    /// Order size in fund-token base units.
    pub size: U256,
    /// Limit price in cash-token units per fund token.
    pub limit: u64,
    /// The window this order is bound to.
    pub window_id: u64,
}

/// The domain-separated attestation claim (R5.3). The enclave signs `claim.to_bytes()`; the
/// `CrossingEngine` reconstructs the identical struct from the submitted attestation, verifies
/// the signature over those bytes, then checks each binding field against on-chain truth.
#[odra::odra_type]
pub struct AttestationClaim {
    /// Chain identity (e.g. "casper-test"); bound so an attestation cannot be replayed cross-chain.
    pub network: String,
    /// The `CrossingEngine` address this attestation authorizes; bound to prevent cross-contract reuse.
    pub crossing_engine: Address,
    /// The (closed) window being settled.
    pub window_id: u64,
    /// The published crossing-rule version the enclave applied; must match `WindowRegistry`.
    pub rule_version: u32,
    /// Label of the computation that produced the result (e.g. "uniform_price_crossing_v1").
    pub function: String,
    /// The enclave code measurement; must match the configured `expected_measurement`.
    pub code_hash: Bytes,
    /// Commitment to the exact order set cleared; must equal `SealedOrderBook.get_commitment(window_id)`.
    pub input_hash: Bytes,
    /// The enclave's commitment to the secret inputs (ciphertext set) it decrypted.
    pub secrets_hash: Bytes,
    /// Commitment to the settlement instructions: `blake2b-256(ClearingResult.to_bytes())`.
    pub output_hash: Bytes,
    /// Enclave clock at signing; must be within the configured freshness window.
    pub timestamp: u64,
    /// Single-use replay guard; rejected if already consumed.
    pub nonce: u64,
}

/// One participant's net effect from a clearing, expressed as deltas against their escrow.
/// `*_spent` is debited from the participant's escrow; `*_credit` is added to their withdrawable
/// credit. Unspent escrow remains fully withdrawable (refund of unmatched/over-committed amounts).
#[odra::odra_type]
pub struct Settlement {
    pub account: Address,
    /// Fund-token base units consumed from this account's fund escrow.
    pub fund_spent: U256,
    /// Cash-token base units consumed from this account's cash escrow.
    pub cash_spent: U256,
    /// Fund-token base units credited to this account (withdrawable).
    pub fund_credit: U256,
    /// Cash-token base units credited to this account (withdrawable).
    pub cash_credit: U256,
}

/// The enclave's clearing output for a window. `AttestationClaim::output_hash` commits to this.
#[odra::odra_type]
pub struct ClearingResult {
    pub window_id: u64,
    /// The uniform clearing price `P*` (cash units per fund token), for events/dashboard.
    pub price: u64,
    /// Per-account net settlements. Token conservation holds across the set:
    /// `sum(fund_spent) == sum(fund_credit)` and `sum(cash_spent) == sum(cash_credit)`.
    pub fills: Vec<Settlement>,
}

/// What is submitted to `CrossingEngine.settle`: the claim plus the signature over `claim.to_bytes()`.
#[odra::odra_type]
pub struct Attestation {
    pub claim: AttestationClaim,
    /// A casper `Signature` (`bytesrepr`-serialized) by the configured enclave key.
    pub signature: Bytes,
}
