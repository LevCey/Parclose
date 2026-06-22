//! The dev attestation signer.
//!
//! This is a **testnet/dev attestation signer — not a hardware TEE.** It signs the same
//! domain-separated [`AttestationClaim`] the production enclave will sign, with a secp256k1 key
//! over the claim's canonical `bytesrepr` bytes, and computes the same `output_hash` the
//! `CrossingEngine` recomputes on-chain (`blake2b-256(ClearingResult.to_bytes())`). Because the
//! claim structure and signing bytes are identical to the production path, moving to the real
//! enclave is a configuration swap (a different key and code measurement), not a rewrite.
//!
//! It makes no trusted-hardware guarantee and must always be labelled as a development signer
//! wherever it is surfaced.

use odra::casper_types::bytesrepr::{Bytes, ToBytes};
use odra::casper_types::{crypto, PublicKey, SecretKey};
use odra::prelude::{Address, String};
use parclose_seal::blake2b_256;
use parclose_shared::{Attestation, AttestationClaim, ClearingResult};

/// The domain binding for an attestation, supplied by the caller. Everything here is checked by
/// `CrossingEngine.verify_attestation` against on-chain truth; `output_hash` is derived from the
/// clearing result by [`DevSigner::attest`] and is not part of this context.
#[derive(Clone, Debug)]
pub struct AttestationContext {
    /// Chain identity (must equal the engine's configured network).
    pub network: String,
    /// The `CrossingEngine` address this attestation authorizes.
    pub crossing_engine: Address,
    /// The published crossing-rule version the clearing applied (must match `WindowRegistry`).
    pub rule_version: u32,
    /// Label of the computation that produced the result.
    pub function: String,
    /// The enclave code measurement (must equal the engine's `expected_measurement`).
    pub code_hash: Bytes,
    /// Commitment to the cleared order set (must equal `SealedOrderBook.get_commitment(window_id)`).
    pub input_hash: Bytes,
    /// Commitment to the secret inputs the enclave decrypted.
    pub secrets_hash: Bytes,
    /// Signing time, in milliseconds since the Unix epoch (Casper block-time units).
    pub timestamp: u64,
    /// Single-use replay guard.
    pub nonce: u64,
}

/// Why constructing a signer failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignerError {
    /// The provided bytes were not a valid secp256k1 secret key.
    InvalidKey,
}

impl core::fmt::Display for SignerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SignerError::InvalidKey => write!(f, "invalid secp256k1 secret key bytes"),
        }
    }
}

impl std::error::Error for SignerError {}

/// A development attestation signer holding a secp256k1 keypair. Not a hardware TEE.
pub struct DevSigner {
    secret: SecretKey,
    public: PublicKey,
}

impl DevSigner {
    /// Builds a signer from 32 raw secp256k1 secret-key bytes (deterministic — handy for demos and
    /// tests where the same enclave key is configured on the `CrossingEngine`).
    pub fn from_secp256k1_bytes(bytes: [u8; 32]) -> Result<Self, SignerError> {
        let secret = SecretKey::secp256k1_from_bytes(bytes).map_err(|_| SignerError::InvalidKey)?;
        let public = PublicKey::from(&secret);
        Ok(DevSigner { secret, public })
    }

    /// The public key to configure as the `CrossingEngine`'s `enclave_pubkey`.
    pub fn public_key(&self) -> PublicKey {
        self.public.clone()
    }

    /// Builds and signs the attestation for a clearing result under the given domain context.
    ///
    /// `output_hash` is derived here as `blake2b-256(result.to_bytes())` — exactly what the
    /// `CrossingEngine` recomputes — so the engine settles precisely the result that was signed.
    pub fn attest(&self, result: &ClearingResult, context: &AttestationContext) -> Attestation {
        let result_bytes = result.to_bytes().expect("encoding a ClearingResult is infallible");
        let output_hash = Bytes::from(blake2b_256(&result_bytes).to_vec());

        let claim = AttestationClaim {
            network: context.network.clone(),
            crossing_engine: context.crossing_engine,
            window_id: result.window_id,
            rule_version: context.rule_version,
            function: context.function.clone(),
            code_hash: context.code_hash.clone(),
            input_hash: context.input_hash.clone(),
            secrets_hash: context.secrets_hash.clone(),
            output_hash,
            timestamp: context.timestamp,
            nonce: context.nonce,
        };

        let message = claim.to_bytes().expect("encoding an AttestationClaim is infallible");
        let signature = crypto::sign(message.as_slice(), &self.secret, &self.public);
        Attestation {
            claim,
            signature: Bytes::from(signature.to_bytes().expect("encoding a Signature is infallible")),
        }
    }
}
