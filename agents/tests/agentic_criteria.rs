//! The four agentic acceptance criteria, proven offline.
//!
//! These tests exercise the reasoning pipeline and the blind-competition harness with the offline
//! reasoning double, so they need no network and no API key. They verify the *machinery* — that
//! the system surfaces reasoning, shifts with state, competes blind, and traces the order to the
//! reasoning transport — and clear two competing agents with the real uniform-price crossing rule
//! (`parclose_enclave::clear`), exactly what `CrossingEngine` settles on-chain.
//!
//! The genuinely non-formulaic reasoning that the track scores is delivered by the model behind
//! the real client; the offline double is a stand-in that makes the loop runnable, like the dev
//! attestation signer.

use odra::casper_types::account::AccountHash;
use odra::casper_types::U256;
use odra::prelude::Address;

use parclose_agents::{Agent, AgentPersona, OfflineHeuristicLLM, Perception, ScriptedLLM, Side};
use parclose_enclave::{clear, SubmittedOrder};
use parclose_shared::{Order, SIDE_REDEEM};

const TICK: u64 = 5;

fn account(n: u8) -> Address {
    Address::Account(AccountHash::new([n; 32]))
}

/// A subscriber (buyer) liquidity-agent persona.
fn buyer(name: &str, n: u8, cash: u64, max_size: u64, risk_bps: u32, style: &str) -> AgentPersona {
    AgentPersona {
        name: name.into(),
        account: account(n),
        mandate: Side::Subscribe,
        fund_inventory: U256::from(0u64),
        cash_inventory: U256::from(cash),
        max_size: U256::from(max_size),
        risk_appetite_bps: risk_bps,
        style: style.into(),
    }
}

fn perception(nav: u64, prior: Option<u64>) -> Perception {
    Perception {
        window_id: 1,
        attested_nav: nav,
        prior_clear_price: prior,
        fund_supply: U256::from(1_000_000u64),
        price_tick: TICK,
    }
}

/// A redeemer participant's sealed sell order (a human exiting the fund), for the harness book.
fn redeemer(n: u8, size: u64, limit: u64) -> SubmittedOrder {
    SubmittedOrder {
        order: Order {
            side: SIDE_REDEEM,
            size: U256::from(size),
            limit,
            window_id: 1,
            account: account(n),
        },
        id: [n; 32],
    }
}

fn submitted(order: Order, id: u8) -> SubmittedOrder {
    SubmittedOrder { order, id: [id; 32] }
}

// --- Criterion 1: visible reasoning -----------------------------------------------------------

#[test]
fn criterion_1_every_decision_carries_visible_reasoning() {
    let agent = Agent::new(
        buyer("Aria", 1, 10_000_000, 1000, 120, "patient inventory manager"),
        OfflineHeuristicLLM,
    );
    let d = agent.decide(&perception(1000, Some(990))).unwrap();

    // A judge can read why the agent priced as it did: a rationale plus all four factors.
    assert!(!d.rationale.is_empty());
    assert!(!d.factors.nav_signal.is_empty());
    assert!(!d.factors.inventory_risk.is_empty());
    assert!(!d.factors.fill_probability.is_empty());
    assert!(!d.factors.prior_context.is_empty());
}

// --- Criterion 2: state-dependent behaviour (the anti-formula test) ---------------------------

#[test]
fn criterion_2_a_nav_shift_moves_the_order() {
    let agent = Agent::new(
        buyer("Aria", 1, 10_000_000, 1000, 120, "patient inventory manager"),
        OfflineHeuristicLLM,
    );
    let low = agent.decide(&perception(1000, Some(990))).unwrap();
    let high = agent.decide(&perception(1100, Some(990))).unwrap();

    // Same agent, one input changed (NAV) -> a meaningfully different quote.
    assert_ne!(low.limit(), high.limit());
    assert!(high.limit() > low.limit());
}

#[test]
fn criterion_2_inventory_shift_moves_the_order_at_fixed_nav() {
    // The anti-formula bar: with NAV held fixed, changing only the agent's own inventory still
    // moves the order. A `bid = NAV * k` pricer could not produce this.
    let flush = Agent::new(
        buyer("Aria", 1, 10_000_000, 1000, 120, "deploying idle cash"),
        OfflineHeuristicLLM,
    );
    let thin = Agent::new(
        buyer("Aria", 1, 400_000, 1000, 120, "conserving limited cash"),
        OfflineHeuristicLLM,
    );
    let nav = perception(1000, Some(990));
    let flush_d = flush.decide(&nav).unwrap();
    let thin_d = thin.decide(&nav).unwrap();

    assert_eq!(nav.attested_nav, 1000, "NAV is identical across the two runs");
    assert_ne!(
        flush_d.limit(),
        thin_d.limit(),
        "inventory alone must move the quote when NAV is fixed"
    );
}

// --- Criterion 3: genuine sealed competition --------------------------------------------------

#[test]
fn criterion_3_two_agents_reach_different_orders_and_one_clearing_price() {
    // Two agents, different personas, the same public perception, each deciding blind: `decide`
    // is only ever handed the agent's own persona + the public perception, never a rival's order.
    let cautious = Agent::new(
        buyer("Aria", 1, 10_000_000, 1000, 100, "patient, fades volatility"),
        OfflineHeuristicLLM,
    );
    let aggressive = Agent::new(
        buyer("Boreas", 2, 3_000_000, 600, 350, "aggressive, chases fills"),
        OfflineHeuristicLLM,
    );
    let view = perception(1000, Some(990));
    let a = cautious.decide(&view).unwrap();
    let b = aggressive.decide(&view).unwrap();

    // Two distinct orders and two distinct rationales.
    assert_ne!(a.limit(), b.limit(), "different personas must reach different quotes");
    assert_ne!(a.rationale, b.rationale);
    assert_eq!(a.side(), Some(Side::Subscribe));
    assert_eq!(b.side(), Some(Side::Subscribe));

    // ...converging into one fair clearing price against exiting redeemers.
    let book = vec![
        submitted(a.order.clone(), 1),
        submitted(b.order.clone(), 2),
        redeemer(3, 800, 985),
        redeemer(4, 500, 995),
    ];
    let result = clear(1, &book, TICK);

    assert!(result.price > 0, "the window must cross");
    assert!(!result.fills.is_empty());

    // Token conservation holds (the same invariant CrossingEngine relies on).
    let (mut fund_spent, mut fund_credit) = (U256::zero(), U256::zero());
    let (mut cash_spent, mut cash_credit) = (U256::zero(), U256::zero());
    for f in &result.fills {
        fund_spent += f.fund_spent;
        fund_credit += f.fund_credit;
        cash_spent += f.cash_spent;
        cash_credit += f.cash_credit;
    }
    assert_eq!(fund_spent, fund_credit);
    assert_eq!(cash_spent, cash_credit);

    // Both agents took part in the cross.
    assert!(result.fills.iter().any(|f| f.account == account(1)));
    assert!(result.fills.iter().any(|f| f.account == account(2)));
}

// --- Criterion 4: model-in-the-loop, not decoration -------------------------------------------

#[test]
fn criterion_4_swapping_the_reasoning_transport_changes_behaviour() {
    // The order traces to the reasoning transport's output, not to a fixed transform of inputs:
    // hold the persona and perception constant and swap only the transport -> the order changes.
    let persona = buyer("Aria", 1, 10_000_000, 1000, 120, "patient inventory manager");
    let view = perception(1000, Some(990));

    let heuristic = Agent::new(persona.clone(), OfflineHeuristicLLM);
    let scripted = Agent::new(
        persona,
        ScriptedLLM::new(
            r#"{"side":"subscribe","size":250,"limit":880,"rationale":"a deliberately different read",
            "factors":{"nav_signal":"discounted hard vs NAV","inventory_risk":"holding back size",
            "fill_probability":"content to miss some fills","prior_context":"ignored the prior clear"}}"#,
        ),
    );

    let h = heuristic.decide(&view).unwrap();
    let s = scripted.decide(&view).unwrap();

    assert_ne!(
        (h.limit(), h.size()),
        (s.limit(), s.size()),
        "the order must follow the reasoning transport, not a fixed input transform"
    );
    assert_eq!(s.limit(), 880); // the scripted decision drove the order
    assert_eq!(s.size(), U256::from(250u64));
}

// --- The four-factor guard (R4.8 enforcement) -------------------------------------------------

#[test]
fn a_decision_that_skips_a_factor_is_rejected() {
    let agent = Agent::new(
        buyer("Aria", 1, 10_000_000, 1000, 120, "patient inventory manager"),
        ScriptedLLM::new(
            r#"{"side":"subscribe","size":100,"limit":990,"rationale":"r",
            "factors":{"nav_signal":"a","inventory_risk":"b","fill_probability":"c","prior_context":""}}"#,
        ),
    );
    // A reasoning reply that fails to weigh all four factors is not a valid decision.
    assert!(agent.decide(&perception(1000, Some(990))).is_err());
}

// --- Sanity: the offline double respects side/redeem geometry ---------------------------------

#[test]
fn redeemer_agent_quotes_above_fair() {
    let seller = Agent::new(
        AgentPersona {
            name: "Seraphina".into(),
            account: account(9),
            mandate: Side::Redeem,
            fund_inventory: U256::from(800u64),
            cash_inventory: U256::from(0u64),
            max_size: U256::from(1_000u64),
            risk_appetite_bps: 50,
            style: "exiting inventory patiently".into(),
        },
        OfflineHeuristicLLM,
    );
    let d = seller.decide(&perception(1000, Some(990))).unwrap();
    assert_eq!(d.order.side, SIDE_REDEEM);
    // A patient seller's ask sits above the blended fair value (~996 here).
    assert!(d.limit() > 996, "seller ask {} should be above fair", d.limit());
}
