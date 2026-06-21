//! Opening a window's sealed orders inside the enclave.
//!
//! `SealedOrderBook` posts, per accepted order, the on-chain submitter and the opaque ciphertext.
//! The enclave opens each ciphertext under that recorded submitter (and the window id) and turns
//! the survivors into [`SubmittedOrder`]s for the crossing rule. Two things are enforced here:
//!
//! * **D-15 submitter binding.** A ciphertext is opened under the submitter `SealedOrderBook`
//!   recorded for it. Because the submitter is part of the AEAD associated data, a ciphertext
//!   copied from another account fails to open — order mirroring is rejected cryptographically,
//!   and [`open_order`] additionally checks the decrypted `account` against that submitter.
//! * **Robustness.** A submission that cannot be opened (tampered, copied, malformed) is dropped,
//!   not fatal: it simply does not enter the clearing. The count is reported for logging.
//!
//! Each survivor's id is the canonical `blake2b-256(ciphertext)` — the same hash `SealedOrderBook`
//! derives on-chain — so the crossing rule's residual tiebreak matches the on-chain identity.

use odra::prelude::Address;
use parclose_seal::{ciphertext_hash, open_order, EnclaveSecretKey};

use crate::SubmittedOrder;

/// One sealed order as recorded on-chain by `SealedOrderBook`: the submitter and the ciphertext.
#[derive(Clone)]
pub struct SealedSubmission {
    /// The account `SealedOrderBook` recorded as the submitter of this ciphertext.
    pub submitter: Address,
    /// The opaque ciphertext bytes posted on-chain.
    pub ciphertext: Vec<u8>,
}

/// The result of opening a window's sealed set.
pub struct OpenedWindow {
    /// The orders that opened successfully, ready for the crossing rule.
    pub orders: Vec<SubmittedOrder>,
    /// How many submissions were dropped because they could not be opened (tampered, copied to a
    /// different submitter, malformed, or sealed to a different key/window).
    pub rejected: usize,
}

/// Opens every sealed submission for `window_id`, dropping any that fail to open, and returns the
/// clearable orders plus a count of the dropped submissions.
pub fn open_window(
    window_id: u64,
    submissions: &[SealedSubmission],
    enclave_sk: &EnclaveSecretKey,
) -> OpenedWindow {
    let mut orders = Vec::with_capacity(submissions.len());
    let mut rejected = 0;
    for submission in submissions {
        match open_order(&submission.ciphertext, enclave_sk, window_id, &submission.submitter) {
            Ok(order) => orders.push(SubmittedOrder {
                order,
                id: ciphertext_hash(&submission.ciphertext),
            }),
            Err(_) => rejected += 1,
        }
    }
    OpenedWindow { orders, rejected }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clear;
    use odra::casper_types::account::AccountHash;
    use odra::casper_types::U256;
    use parclose_seal::seal_order;
    use parclose_shared::{Order, SIDE_REDEEM, SIDE_SUBSCRIBE};
    use rand_core::OsRng;

    const TICK: u64 = 5;
    const WINDOW: u64 = 7;

    fn account(n: u8) -> Address {
        Address::Account(AccountHash::new([n; 32]))
    }

    fn order(side: u8, size: u64, limit: u64, account_byte: u8) -> Order {
        Order {
            side,
            size: U256::from(size),
            limit,
            window_id: WINDOW,
            account: account(account_byte),
        }
    }

    fn seal(o: &Order, pk: &parclose_seal::EnclavePublicKey) -> Vec<u8> {
        let mut rng = OsRng;
        seal_order(o, pk, &mut rng).expect("seal")
    }

    #[test]
    fn opens_and_clears_two_sealed_orders() {
        let mut rng = OsRng;
        let sk = EnclaveSecretKey::generate(&mut rng);
        let pk = sk.public_key();

        let buy = order(SIDE_SUBSCRIBE, 100, 1_000, 1);
        let sell = order(SIDE_REDEEM, 100, 1_000, 2);
        let submissions = vec![
            SealedSubmission { submitter: account(1), ciphertext: seal(&buy, &pk) },
            SealedSubmission { submitter: account(2), ciphertext: seal(&sell, &pk) },
        ];

        let opened = open_window(WINDOW, &submissions, &sk);
        assert_eq!(opened.rejected, 0);
        assert_eq!(opened.orders.len(), 2);

        let result = clear(WINDOW, &opened.orders, TICK);
        assert_eq!(result.price, 1_000);
        assert!(!result.fills.is_empty());
    }

    #[test]
    fn ids_match_on_chain_ciphertext_hash() {
        let mut rng = OsRng;
        let sk = EnclaveSecretKey::generate(&mut rng);
        let buy = order(SIDE_SUBSCRIBE, 100, 1_000, 1);
        let ct = seal(&buy, &sk.public_key());
        let opened = open_window(
            WINDOW,
            &[SealedSubmission { submitter: account(1), ciphertext: ct.clone() }],
            &sk,
        );
        assert_eq!(opened.orders.len(), 1);
        assert_eq!(opened.orders[0].id, ciphertext_hash(&ct));
    }

    #[test]
    fn a_copied_submission_is_dropped() {
        // D-15: account 1 sealed this; account 9 re-posts the same bytes. It cannot open under 9.
        let mut rng = OsRng;
        let sk = EnclaveSecretKey::generate(&mut rng);
        let buy = order(SIDE_SUBSCRIBE, 100, 1_000, 1);
        let ct = seal(&buy, &sk.public_key());

        let submissions = vec![
            SealedSubmission { submitter: account(1), ciphertext: ct.clone() }, // legit
            SealedSubmission { submitter: account(9), ciphertext: ct },         // copied
        ];
        let opened = open_window(WINDOW, &submissions, &sk);
        assert_eq!(opened.orders.len(), 1);
        assert_eq!(opened.rejected, 1);
        // Only the legitimate submitter's order survives.
        assert!(opened.orders.iter().all(|o| o.order.account == account(1)));
    }

    #[test]
    fn a_tampered_submission_is_dropped() {
        let mut rng = OsRng;
        let sk = EnclaveSecretKey::generate(&mut rng);
        let buy = order(SIDE_SUBSCRIBE, 100, 1_000, 1);
        let mut ct = seal(&buy, &sk.public_key());
        let last = ct.len() - 1;
        ct[last] ^= 0x01;
        let opened =
            open_window(WINDOW, &[SealedSubmission { submitter: account(1), ciphertext: ct }], &sk);
        assert_eq!(opened.orders.len(), 0);
        assert_eq!(opened.rejected, 1);
    }
}
