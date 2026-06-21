//! End-to-end (offline) sealed-order flow: agents decide blind, seal their orders to the enclave,
//! and the enclave opens the sealed set and clears it — only ciphertext crosses the boundary.
//!
//! This stitches the reasoning layer (`parclose-agents`), the encryption client (`parclose-seal`),
//! and the enclave open + crossing rule (`parclose-enclave`) into the path the on-chain flow will
//! follow, minus the Testnet submit. It also proves the D-15 mirror defense end to end.

use odra::casper_types::account::AccountHash;
use odra::casper_types::U256;
use odra::prelude::Address;

use parclose_agents::{seal_decision, Agent, AgentPersona, OfflineHeuristicLLM, Perception, Side};
use parclose_enclave::{clear, open_window, SealedSubmission};
use parclose_seal::{seal_order, EnclaveSecretKey};
use parclose_shared::{Order, SIDE_REDEEM};
use rand_core::OsRng;

const TICK: u64 = 5;
const WINDOW: u64 = 1;

fn account(n: u8) -> Address {
    Address::Account(AccountHash::new([n; 32]))
}

fn buyer(name: &str, n: u8, cash: u64, max_size: u64, risk_bps: u32) -> AgentPersona {
    AgentPersona {
        name: name.into(),
        account: account(n),
        mandate: Side::Subscribe,
        fund_inventory: U256::from(0u64),
        cash_inventory: U256::from(cash),
        max_size: U256::from(max_size),
        risk_appetite_bps: risk_bps,
        style: "liquidity provider".into(),
    }
}

fn perception() -> Perception {
    Perception {
        window_id: WINDOW,
        attested_nav: 1_000,
        prior_clear_price: Some(990),
        fund_supply: U256::from(1_000_000u64),
        price_tick: TICK,
    }
}

#[test]
fn agent_decisions_seal_open_and_clear() {
    let mut rng = OsRng;
    let sk = EnclaveSecretKey::generate(&mut rng);
    let pk = sk.public_key();

    // Two agents reason blind and reach their own orders.
    let aria = Agent::new(buyer("Aria", 1, 10_000_000, 1_000, 100), OfflineHeuristicLLM);
    let boreas = Agent::new(buyer("Boreas", 2, 3_000_000, 600, 350), OfflineHeuristicLLM);
    let view = perception();
    let da = aria.decide(&view).unwrap();
    let db = boreas.decide(&view).unwrap();

    // Each seals its own order; the submitter is the agent's own account.
    let mut submissions = vec![
        SealedSubmission {
            submitter: da.order.account,
            ciphertext: seal_decision(&da, &pk, &mut rng).unwrap(),
        },
        SealedSubmission {
            submitter: db.order.account,
            ciphertext: seal_decision(&db, &pk, &mut rng).unwrap(),
        },
    ];

    // A redeemer participant seals a sell order into the same window.
    let redeemer = Order {
        side: SIDE_REDEEM,
        size: U256::from(800u64),
        limit: 985,
        window_id: WINDOW,
        account: account(3),
    };
    submissions.push(SealedSubmission {
        submitter: account(3),
        ciphertext: seal_order(&redeemer, &pk, &mut rng).unwrap(),
    });

    // The enclave opens the sealed set and clears it.
    let opened = open_window(WINDOW, &submissions, &sk);
    assert_eq!(opened.rejected, 0);
    assert_eq!(opened.orders.len(), 3);

    let result = clear(WINDOW, &opened.orders, TICK);
    assert!(result.price > 0, "the window must cross");
    assert!(opened.orders.iter().any(|o| o.order.account == account(1)));
    assert!(opened.orders.iter().any(|o| o.order.account == account(2)));

    // Conservation holds (what CrossingEngine relies on).
    let (mut fund_spent, mut fund_credit) = (U256::zero(), U256::zero());
    for f in &result.fills {
        fund_spent += f.fund_spent;
        fund_credit += f.fund_credit;
    }
    assert_eq!(fund_spent, fund_credit);
}

#[test]
fn a_mirrored_agent_order_is_rejected_end_to_end() {
    // D-15 end to end: an adversary copies Aria's on-chain ciphertext and re-posts it under its
    // own account. The enclave opens it under the recorded (adversary) submitter and drops it.
    let mut rng = OsRng;
    let sk = EnclaveSecretKey::generate(&mut rng);
    let pk = sk.public_key();

    let aria = Agent::new(buyer("Aria", 1, 10_000_000, 1_000, 100), OfflineHeuristicLLM);
    let da = aria.decide(&perception()).unwrap();
    let ciphertext = seal_decision(&da, &pk, &mut rng).unwrap();

    let submissions = vec![
        SealedSubmission { submitter: da.order.account, ciphertext: ciphertext.clone() }, // legit
        SealedSubmission { submitter: account(9), ciphertext }, // mirrored copy
    ];
    let opened = open_window(WINDOW, &submissions, &sk);
    assert_eq!(opened.orders.len(), 1);
    assert_eq!(opened.rejected, 1);
    assert!(opened.orders.iter().all(|o| o.order.account == account(1)));
}
