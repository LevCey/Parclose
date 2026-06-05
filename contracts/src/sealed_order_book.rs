use odra::casper_types::bytesrepr::Bytes;
use odra::prelude::*;
use crate::window_registry::WindowRegistryContractRef;

#[odra::odra_error]
pub enum Error {
    /// The target window is not open; submissions are rejected.
    WindowNotOpen = 0,
}

/// Emitted for each accepted sealed order so the dashboard can show ciphertext arriving.
#[odra::event]
pub struct OrderSubmitted {
    pub window_id: u64,
    pub order_idx: u64,
    /// The 32-byte hash of the submitted ciphertext (not the plaintext).
    pub ciphertext_hash: Bytes,
}

/// Domain-separation tag for the per-window order commitment. Bumping the version
/// invalidates cross-version reuse of a commitment preimage.
const COMMITMENT_DOMAIN: &[u8] = b"APERTURE_ORDER_COMMITMENT_V1";

/// Sealed-order intake for a crossing window.
///
/// Only ciphertext is ever stored; no plaintext order field (side/size/limit) is accepted.
///
/// **Per-window commitment.** Each window carries a running commitment that binds the exact
/// ordered sequence of submitted ciphertext hashes. It is an ordered, domain-separated hash
/// chain over blake2b-256 (`env().hash`):
///
/// ```text
/// commitment_0   = 0^32
/// commitment_{n} = H( COMMITMENT_DOMAIN || window_id_be8 || n_be8
///                     || commitment_{n-1} || ciphertext_hash_n )
/// ```
///
/// where `n` is the **zero-based index** of the order being added. The chain binds order,
/// position, count, and window: reordering, dropping, duplicating, or moving an order to a
/// different window all change the final commitment. (A plain XOR/sum would let duplicate
/// hashes cancel and would not bind order — hence the chain.)
///
/// The enclave reads the stored ciphertexts for the window, recomputes the identical chain to
/// produce its attestation `input_hash`, and `CrossingEngine` checks
/// `get_commitment(window_id) == attestation.input_hash` at settle time. The big-endian,
/// fixed-width field encoding above is the canonical encoding the enclave must mirror exactly.
///
/// **`ciphertext_hash` is derived on-chain**, never taken from the caller:
/// `ciphertext_hash = blake2b-256(ciphertext)` (`env().hash`). The commitment therefore binds
/// the actual ciphertext bytes, not a caller-asserted digest — a caller cannot decouple the
/// committed hash from the stored ciphertext to grief the window's settlement. The enclave
/// derives the same hash from the same bytes with the same function.
#[odra::module(errors = Error)]
pub struct SealedOrderBook {
    registry: External<WindowRegistryContractRef>,
    // window_id → count of accepted orders
    order_count: Mapping<u64, u64>,
    // (window_id, order_idx) → ciphertext bytes
    ciphertexts: Mapping<(u64, u64), Bytes>,
    // (window_id, order_idx) → blake2b-256(ciphertext), derived on submit (32 bytes)
    cipher_hashes: Mapping<(u64, u64), Bytes>,
    // window_id → running commitment hash chain over accepted ciphertext_hashes
    commitments: Mapping<u64, Bytes>,
}

#[odra::module]
impl SealedOrderBook {
    /// Deploys the order book pointing at an already-deployed WindowRegistry.
    pub fn init(&mut self, registry_address: Address) {
        self.registry.set(registry_address);
    }

    /// Submits a sealed order for an open window. The window must be open (checked via
    /// WindowRegistry). The order's hash is derived on-chain as `blake2b-256(ciphertext)`;
    /// the caller does not supply it.
    pub fn submit_sealed_order(&mut self, window_id: u64, ciphertext: Bytes) {
        if !self.registry.is_open(window_id) {
            self.env().revert(Error::WindowNotOpen);
        }

        // Derive the hash from the actual ciphertext bytes; never trust a caller-supplied value.
        let ciphertext_hash = Bytes::from(self.env().hash(ciphertext.inner_bytes()).to_vec());

        let idx = self.order_count.get(&window_id).unwrap_or(0);
        self.ciphertexts.set(&(window_id, idx), ciphertext);
        self.cipher_hashes.set(&(window_id, idx), ciphertext_hash.clone());
        self.order_count.set(&window_id, idx + 1);

        // Extend the per-window commitment hash chain with this order.
        let prev = self
            .commitments
            .get(&window_id)
            .unwrap_or_else(zero_commitment);
        let next = self.extend_commitment(window_id, idx, &prev, &ciphertext_hash);
        self.commitments.set(&window_id, next);

        self.env().emit_event(OrderSubmitted {
            window_id,
            order_idx: idx,
            ciphertext_hash,
        });
    }

    /// Returns the per-window commitment (32 zero bytes if no orders submitted yet).
    pub fn get_commitment(&self, window_id: u64) -> Bytes {
        self.commitments
            .get(&window_id)
            .unwrap_or_else(zero_commitment)
    }

    /// Returns the number of sealed orders accepted for a window.
    pub fn get_order_count(&self, window_id: u64) -> u64 {
        self.order_count.get(&window_id).unwrap_or(0)
    }

    /// Returns the stored ciphertext for a given order index (None if out of range).
    pub fn get_order_ciphertext(&self, window_id: u64, order_idx: u64) -> Option<Bytes> {
        self.ciphertexts.get(&(window_id, order_idx))
    }

    /// Returns the stored ciphertext_hash for a given order index.
    pub fn get_order_hash(&self, window_id: u64, order_idx: u64) -> Option<Bytes> {
        self.cipher_hashes.get(&(window_id, order_idx))
    }
}

impl SealedOrderBook {
    /// Computes `commitment_{idx+1}` from the previous commitment and the new order hash.
    /// See the module docs for the canonical preimage encoding the enclave must mirror.
    fn extend_commitment(
        &self,
        window_id: u64,
        idx: u64,
        prev: &Bytes,
        ciphertext_hash: &Bytes,
    ) -> Bytes {
        let mut preimage =
            Vec::with_capacity(COMMITMENT_DOMAIN.len() + 8 + 8 + 32 + 32);
        preimage.extend_from_slice(COMMITMENT_DOMAIN);
        preimage.extend_from_slice(&window_id.to_be_bytes());
        preimage.extend_from_slice(&idx.to_be_bytes());
        preimage.extend_from_slice(prev.inner_bytes());
        preimage.extend_from_slice(ciphertext_hash.inner_bytes());
        Bytes::from(self.env().hash(&preimage).to_vec())
    }
}

/// The empty-window commitment: 32 zero bytes (`commitment_0`).
fn zero_commitment() -> Bytes {
    Bytes::from(vec![0u8; 32])
}

#[cfg(test)]
mod tests {
    use super::{zero_commitment, SealedOrderBook, SealedOrderBookHostRef, SealedOrderBookInitArgs};
    use crate::window_registry::{
        WindowRegistry, WindowRegistryHostRef, WindowRegistryInitArgs,
    };
    use odra::casper_types::bytesrepr::Bytes;
    use odra::host::{Deployer, HostEnv};
    use odra::prelude::*;

    const RULE: &str = "uniform-price crossing";

    fn bytes(data: &[u8]) -> Bytes {
        Bytes::from(data.to_vec())
    }

    /// Deploys a fresh registry + order book. Each registry starts its window ids at 1.
    fn setup(env: &HostEnv) -> (WindowRegistryHostRef, SealedOrderBookHostRef) {
        let registry = WindowRegistry::deploy(
            env,
            WindowRegistryInitArgs { initial_rule: RULE.to_string() },
        );
        let book = SealedOrderBook::deploy(
            env,
            SealedOrderBookInitArgs { registry_address: registry.address() },
        );
        (registry, book)
    }

    #[test]
    fn submit_to_open_window_succeeds() {
        let env = odra_test::env();
        let (mut registry, mut book) = setup(&env);
        let wid = registry.open_window();

        book.submit_sealed_order(wid, bytes(b"encrypted-order-data"));

        assert_eq!(book.get_order_count(wid), 1);
        assert!(book.get_order_ciphertext(wid, 0).is_some());
        // The derived order hash is stored and is 32 bytes.
        assert_eq!(book.get_order_hash(wid, 0).unwrap().inner_bytes().len(), 32);
        // A submitted order moves the commitment off the empty value.
        assert_ne!(book.get_commitment(wid), zero_commitment());
    }

    #[test]
    fn submit_to_closed_window_reverts() {
        let env = odra_test::env();
        let (mut registry, mut book) = setup(&env);
        let wid = registry.open_window();
        registry.close_window(wid);

        let result = book.try_submit_sealed_order(wid, bytes(b"ciphertext"));
        assert!(result.is_err());
        assert_eq!(book.get_order_count(wid), 0);
    }

    /// The stored order hash is derived from the ciphertext bytes, not supplied by the caller:
    /// identical ciphertext ⇒ identical hash; different ciphertext ⇒ different hash. This is the
    /// property that prevents a caller from decoupling the committed hash from the stored bytes.
    #[test]
    fn order_hash_is_derived_from_ciphertext() {
        let env = odra_test::env();

        let (mut reg_a, mut book_a) = setup(&env);
        let wa = reg_a.open_window();
        book_a.submit_sealed_order(wa, bytes(b"same-ciphertext"));

        let (mut reg_b, mut book_b) = setup(&env);
        let wb = reg_b.open_window();
        book_b.submit_sealed_order(wb, bytes(b"same-ciphertext"));
        book_b.submit_sealed_order(wb, bytes(b"other-ciphertext"));

        // Same ciphertext bytes -> identical derived hash across independent deployments.
        assert_eq!(book_a.get_order_hash(wa, 0), book_b.get_order_hash(wb, 0));
        // Different ciphertext bytes -> different derived hash.
        assert_ne!(book_b.get_order_hash(wb, 0), book_b.get_order_hash(wb, 1));
    }

    /// Regression against the old XOR scheme: two orders with the *same* ciphertext (hence the
    /// same derived hash) must NOT cancel back to the empty commitment.
    #[test]
    fn duplicate_orders_do_not_cancel() {
        let env = odra_test::env();
        let (mut registry, mut book) = setup(&env);
        let wid = registry.open_window();

        book.submit_sealed_order(wid, bytes(b"dup"));
        let c1 = book.get_commitment(wid);
        book.submit_sealed_order(wid, bytes(b"dup"));
        let c2 = book.get_commitment(wid);

        assert_ne!(c2, zero_commitment()); // XOR would have cancelled to zero here
        assert_ne!(c2, c1);
        assert_eq!(book.get_order_count(wid), 2);
    }

    /// Regression against XOR/sum: order of submission must change the commitment.
    #[test]
    fn commitment_is_order_sensitive() {
        let env = odra_test::env();

        let (mut reg_a, mut book_a) = setup(&env);
        let wa = reg_a.open_window();
        book_a.submit_sealed_order(wa, bytes(b"o1"));
        book_a.submit_sealed_order(wa, bytes(b"o2"));

        let (mut reg_b, mut book_b) = setup(&env);
        let wb = reg_b.open_window();
        book_b.submit_sealed_order(wb, bytes(b"o2"));
        book_b.submit_sealed_order(wb, bytes(b"o1"));

        // Same window id (1) and same ciphertext multiset, reversed order -> different commitment.
        assert_eq!(wa, wb);
        assert_ne!(book_a.get_commitment(wa), book_b.get_commitment(wb));
    }

    /// Identical window id + identical ordered ciphertext sequence -> identical commitment.
    #[test]
    fn commitment_is_deterministic() {
        let env = odra_test::env();

        let (mut reg_a, mut book_a) = setup(&env);
        let wa = reg_a.open_window();
        book_a.submit_sealed_order(wa, bytes(b"o1"));
        book_a.submit_sealed_order(wa, bytes(b"o2"));

        let (mut reg_b, mut book_b) = setup(&env);
        let wb = reg_b.open_window();
        book_b.submit_sealed_order(wb, bytes(b"o1"));
        book_b.submit_sealed_order(wb, bytes(b"o2"));

        assert_eq!(wa, wb);
        assert_eq!(book_a.get_commitment(wa), book_b.get_commitment(wb));
    }

    /// The same first order under different window ids yields different commitments.
    #[test]
    fn commitment_is_window_bound() {
        let env = odra_test::env();
        let (mut registry, mut book) = setup(&env);

        let w1 = registry.open_window();
        book.submit_sealed_order(w1, bytes(b"o1"));
        let c1 = book.get_commitment(w1);

        registry.close_window(w1);
        let w2 = registry.open_window();
        book.submit_sealed_order(w2, bytes(b"o1"));
        let c2 = book.get_commitment(w2);

        assert_ne!(w1, w2);
        assert_ne!(c1, c2);
    }
}
