//! Sealed-order encryption for Parclose.
//!
//! An order is encrypted to the enclave's long-lived X25519 key and posted on-chain as opaque
//! ciphertext; only the enclave can open it. The scheme is a standard hybrid seal:
//!
//! 1. the client generates an ephemeral X25519 keypair and performs ECDH against the enclave's
//!    public key;
//! 2. the shared secret is run through a SHA-256 KDF to a 32-byte AEAD key;
//! 3. the order bytes are sealed with XChaCha20-Poly1305 under **associated data that binds the
//!    window id and the submitter's account** (D-15).
//!
//! The associated-data binding is what defeats order mirroring: a ciphertext sealed by account A
//! carries A in its AAD, so re-posting it from account B (whom `SealedOrderBook` records as the
//! submitter) makes the enclave open it under B's AAD — the AEAD tag check fails and the order is
//! rejected. The enclave additionally checks the decrypted `Order.account` against the recorded
//! submitter, and because the submitter is folded into the on-chain order commitment, that check
//! is rooted in the attestation's `input_hash` rather than in an operator's word.
//!
//! Ciphertext layout: `ephemeral_public_key (32) || nonce (24) || AEAD(ciphertext || tag)`.
//!
//! The canonical order encoding is the shared `bytesrepr` form, so what the agent seals is exactly
//! what the enclave clears and the `CrossingEngine` settles.

use blake2::Blake2b;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use odra::casper_types::bytesrepr::{FromBytes, ToBytes};
use odra::prelude::Address;
use parclose_shared::Order;
use rand_core::{CryptoRng, RngCore};
use sha2::{Digest, Sha256};
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

/// Domain tag for the key-derivation hash.
const KDF_DOMAIN: &[u8] = b"PARCLOSE_SEAL_KDF_V1";
/// Domain tag for the associated data.
const AAD_DOMAIN: &[u8] = b"PARCLOSE_SEAL_AAD_V1";

/// Length of an X25519 public key.
pub const EPK_LEN: usize = 32;
/// Length of the XChaCha20-Poly1305 nonce.
pub const NONCE_LEN: usize = 24;
/// Length of the Poly1305 authentication tag.
pub const TAG_LEN: usize = 16;
/// Minimum length of a well-formed ciphertext (header + tag, empty body).
pub const MIN_CIPHERTEXT_LEN: usize = EPK_LEN + NONCE_LEN + TAG_LEN;

/// blake2b with a 32-byte digest, matching `SealedOrderBook`'s on-chain ciphertext hash.
type Blake2b256 = Blake2b<blake2::digest::consts::U32>;

/// The enclave's long-lived X25519 secret key. Stays inside the enclave; never leaves it.
#[derive(Clone)]
pub struct EnclaveSecretKey(StaticSecret);

/// The enclave's X25519 public key. Distributed to clients so they can seal orders to the enclave.
#[derive(Clone)]
pub struct EnclavePublicKey(PublicKey);

impl EnclaveSecretKey {
    /// Generates a fresh enclave key.
    pub fn generate<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        EnclaveSecretKey(StaticSecret::random_from_rng(&mut *rng))
    }

    /// Reconstructs the key from its 32 raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        EnclaveSecretKey(StaticSecret::from(bytes))
    }

    /// The raw 32-byte secret scalar.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// The matching public key.
    pub fn public_key(&self) -> EnclavePublicKey {
        EnclavePublicKey(PublicKey::from(&self.0))
    }
}

impl EnclavePublicKey {
    /// Reconstructs the public key from its 32 raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        EnclavePublicKey(PublicKey::from(bytes))
    }

    /// The raw 32-byte public key.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
}

/// Why a seal or open operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    /// The order (or submitter) could not be serialized.
    Encode,
    /// The decrypted bytes were not a well-formed order.
    Decode,
    /// AEAD encryption or decryption failed — for `open`, this means tampering, a wrong key, or a
    /// submitter/window mismatch in the associated data (the D-15 copy/replay defense).
    Crypto,
    /// The ciphertext was too short to be valid.
    MalformedCiphertext,
    /// The decrypted order was bound to a different window.
    WindowMismatch,
    /// The decrypted order's account did not match the recorded on-chain submitter.
    SubmitterMismatch,
}

impl core::fmt::Display for SealError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SealError::Encode => write!(f, "could not serialize the order"),
            SealError::Decode => write!(f, "decrypted bytes were not a valid order"),
            SealError::Crypto => write!(f, "AEAD open/seal failed (tamper, wrong key, or AAD mismatch)"),
            SealError::MalformedCiphertext => write!(f, "ciphertext too short"),
            SealError::WindowMismatch => write!(f, "order bound to a different window"),
            SealError::SubmitterMismatch => write!(f, "order account does not match the submitter"),
        }
    }
}

impl std::error::Error for SealError {}

/// Seals an order to the enclave. The submitter and window are taken from the order itself and
/// bound into the associated data, so the ciphertext can only be opened under that same identity.
pub fn seal_order<R: RngCore + CryptoRng>(
    order: &Order,
    enclave_pk: &EnclavePublicKey,
    rng: &mut R,
) -> Result<Vec<u8>, SealError> {
    let plaintext = order.to_bytes().map_err(|_| SealError::Encode)?;
    let aad = associated_data(order.window_id, &order.account)?;

    let ephemeral = EphemeralSecret::random_from_rng(&mut *rng);
    let ephemeral_public = PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&enclave_pk.0);
    let key = derive_key(shared.as_bytes(), ephemeral_public.as_bytes(), &enclave_pk.to_bytes());

    let mut nonce = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce);

    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|_| SealError::Crypto)?;
    let body = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: &plaintext, aad: &aad })
        .map_err(|_| SealError::Crypto)?;

    let mut out = Vec::with_capacity(EPK_LEN + NONCE_LEN + body.len());
    out.extend_from_slice(ephemeral_public.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Opens a ciphertext under a given window and submitter. Fails if the ciphertext was tampered
/// with, sealed to a different enclave key, or sealed by a different submitter/window than the
/// ones supplied (the AEAD tag covers the submitter- and window-bound associated data).
pub fn open_order(
    ciphertext: &[u8],
    enclave_sk: &EnclaveSecretKey,
    window_id: u64,
    submitter: &Address,
) -> Result<Order, SealError> {
    if ciphertext.len() < MIN_CIPHERTEXT_LEN {
        return Err(SealError::MalformedCiphertext);
    }
    let mut epk_bytes = [0u8; EPK_LEN];
    epk_bytes.copy_from_slice(&ciphertext[..EPK_LEN]);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&ciphertext[EPK_LEN..EPK_LEN + NONCE_LEN]);
    let body = &ciphertext[EPK_LEN + NONCE_LEN..];

    let ephemeral_public = PublicKey::from(epk_bytes);
    let shared = enclave_sk.0.diffie_hellman(&ephemeral_public);
    let key = derive_key(shared.as_bytes(), &epk_bytes, &enclave_sk.public_key().to_bytes());

    let aad = associated_data(window_id, submitter)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|_| SealError::Crypto)?;
    let plaintext = cipher
        .decrypt(&XNonce::from(nonce), Payload { msg: body, aad: &aad })
        .map_err(|_| SealError::Crypto)?;

    let (order, rest) = Order::from_bytes(&plaintext).map_err(|_| SealError::Decode)?;
    if !rest.is_empty() {
        return Err(SealError::Decode);
    }
    if order.window_id != window_id {
        return Err(SealError::WindowMismatch);
    }
    if order.account != *submitter {
        return Err(SealError::SubmitterMismatch);
    }
    Ok(order)
}

/// The canonical on-chain ciphertext hash (blake2b-256), matching `SealedOrderBook`'s derivation.
pub fn ciphertext_hash(ciphertext: &[u8]) -> [u8; 32] {
    blake2b_256(ciphertext)
}

/// blake2b-256 of arbitrary bytes — the same hash Casper's on-chain `env().hash` computes. Used
/// both for the on-chain ciphertext hash and for the attestation's `output_hash` commitment, so
/// off-chain components produce digests the contracts agree with.
pub fn blake2b_256(data: &[u8]) -> [u8; 32] {
    let digest = Blake2b256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// `SHA-256(domain || shared_secret || ephemeral_pub || enclave_pub)` -> 32-byte AEAD key.
fn derive_key(shared: &[u8], ephemeral_pub: &[u8], enclave_pub: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(KDF_DOMAIN);
    hasher.update(shared);
    hasher.update(ephemeral_pub);
    hasher.update(enclave_pub);
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// `domain || window_id (LE) || submitter_bytes` — the AEAD associated data (D-15).
fn associated_data(window_id: u64, submitter: &Address) -> Result<Vec<u8>, SealError> {
    let submitter_bytes = submitter.to_bytes().map_err(|_| SealError::Encode)?;
    let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + 8 + submitter_bytes.len());
    aad.extend_from_slice(AAD_DOMAIN);
    aad.extend_from_slice(&window_id.to_le_bytes());
    aad.extend_from_slice(&submitter_bytes);
    Ok(aad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use odra::casper_types::account::AccountHash;
    use odra::casper_types::U256;
    use parclose_shared::{SIDE_SUBSCRIBE};

    /// A tiny deterministic RNG for reproducible tests. Not for production use.
    struct TestRng(u64);
    impl RngCore for TestRng {
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }
        fn next_u64(&mut self) -> u64 {
            // SplitMix64 step.
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(8) {
                let v = self.next_u64().to_le_bytes();
                chunk.copy_from_slice(&v[..chunk.len()]);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }
    impl CryptoRng for TestRng {}

    fn account(n: u8) -> Address {
        Address::Account(AccountHash::new([n; 32]))
    }

    fn order(account_byte: u8, window_id: u64) -> Order {
        Order {
            side: SIDE_SUBSCRIBE,
            size: U256::from(500u64),
            limit: 995,
            window_id,
            account: account(account_byte),
        }
    }

    #[test]
    fn round_trip_recovers_the_order() {
        let mut rng = TestRng(1);
        let sk = EnclaveSecretKey::generate(&mut rng);
        let pk = sk.public_key();

        let original = order(7, 42);
        let ct = seal_order(&original, &pk, &mut rng).unwrap();
        let opened = open_order(&ct, &sk, 42, &account(7)).unwrap();

        assert_eq!(opened.side, original.side);
        assert_eq!(opened.size, original.size);
        assert_eq!(opened.limit, original.limit);
        assert_eq!(opened.window_id, original.window_id);
        assert_eq!(opened.account, original.account);
    }

    #[test]
    fn only_ciphertext_no_plaintext_leaks() {
        let mut rng = TestRng(2);
        let sk = EnclaveSecretKey::generate(&mut rng);
        let original = order(7, 42);
        let ct = seal_order(&original, &sk.public_key(), &mut rng).unwrap();
        // The limit 995 little-endian must not appear verbatim in the ciphertext body.
        assert!(!ct.windows(2).any(|w| w == 995u16.to_le_bytes()));
        assert!(ct.len() >= MIN_CIPHERTEXT_LEN);
    }

    #[test]
    fn tampering_is_detected() {
        let mut rng = TestRng(3);
        let sk = EnclaveSecretKey::generate(&mut rng);
        let mut ct = seal_order(&order(7, 42), &sk.public_key(), &mut rng).unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 0x01; // flip a tag bit
        assert_eq!(open_order(&ct, &sk, 42, &account(7)), Err(SealError::Crypto));
    }

    #[test]
    fn copied_ciphertext_from_another_submitter_is_rejected() {
        // D-15: account A seals; account B re-posts the same ciphertext. The enclave opens it
        // under B (the recorded submitter), the AAD no longer matches, and the open fails.
        let mut rng = TestRng(4);
        let sk = EnclaveSecretKey::generate(&mut rng);
        let ct = seal_order(&order(7, 42), &sk.public_key(), &mut rng).unwrap();
        assert_eq!(open_order(&ct, &sk, 42, &account(8)), Err(SealError::Crypto));
    }

    #[test]
    fn wrong_window_is_rejected() {
        let mut rng = TestRng(5);
        let sk = EnclaveSecretKey::generate(&mut rng);
        let ct = seal_order(&order(7, 42), &sk.public_key(), &mut rng).unwrap();
        assert_eq!(open_order(&ct, &sk, 99, &account(7)), Err(SealError::Crypto));
    }

    #[test]
    fn wrong_enclave_key_cannot_open() {
        let mut rng = TestRng(6);
        let sk = EnclaveSecretKey::generate(&mut rng);
        let other = EnclaveSecretKey::generate(&mut rng);
        let ct = seal_order(&order(7, 42), &sk.public_key(), &mut rng).unwrap();
        assert_eq!(open_order(&ct, &other, 42, &account(7)), Err(SealError::Crypto));
    }

    #[test]
    fn key_serialization_round_trips() {
        let mut rng = TestRng(7);
        let sk = EnclaveSecretKey::generate(&mut rng);
        let sk2 = EnclaveSecretKey::from_bytes(sk.to_bytes());
        let pk2 = EnclavePublicKey::from_bytes(sk.public_key().to_bytes());

        let ct = seal_order(&order(3, 1), &pk2, &mut rng).unwrap();
        // Opening with the reconstructed secret key still works.
        assert!(open_order(&ct, &sk2, 1, &account(3)).is_ok());
    }

    #[test]
    fn ciphertext_hash_is_deterministic_and_sensitive() {
        let mut rng = TestRng(8);
        let sk = EnclaveSecretKey::generate(&mut rng);
        let ct = seal_order(&order(7, 42), &sk.public_key(), &mut rng).unwrap();
        assert_eq!(ciphertext_hash(&ct), ciphertext_hash(&ct));
        let mut other = ct.clone();
        other[0] ^= 0xFF;
        assert_ne!(ciphertext_hash(&ct), ciphertext_hash(&other));
    }
}
