//! The encryption client: sealing a decided order to the enclave before it is posted on-chain.
//!
//! This is the first step of the agent's *act* phase. The agent seals its own [`Decision`] to the
//! enclave's public key; the submitter bound into the ciphertext is the order's own account, so
//! only that account can post it without the enclave rejecting it (D-15). The opaque bytes
//! returned here are what gets submitted to `SealedOrderBook` — nothing readable ever leaves the
//! agent.

use parclose_seal::{seal_order, EnclavePublicKey, SealError};
use rand_core::{CryptoRng, RngCore};

use crate::Decision;

/// Seals a decision's order to the enclave, returning the ciphertext to post on-chain.
///
/// The window and submitter are taken from the order itself (the agent's own account), so the
/// ciphertext is bound to this agent and this window.
pub fn seal_decision<R: RngCore + CryptoRng>(
    decision: &Decision,
    enclave_pk: &EnclavePublicKey,
    rng: &mut R,
) -> Result<Vec<u8>, SealError> {
    seal_order(&decision.order, enclave_pk, rng)
}
