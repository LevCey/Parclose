//! A runnable blind-competition window.
//!
//! Two liquidity agents reason independently — each blind to the other — over the same public,
//! attested market view, reach two different sealed orders, and converge into one uniform clearing
//! price against exiting redeemers. The clearing uses the real `parclose_enclave::clear` rule,
//! exactly what `CrossingEngine` settles on-chain.
//!
//! This runs offline with the deterministic reasoning stand-in (no network, no API key):
//!
//! ```text
//! cargo run --example compete
//! ```
//!
//! The production demo swaps the stand-in for the real Anthropic-backed client; the loop is
//! identical.

use odra::casper_types::account::AccountHash;
use odra::casper_types::U256;
use odra::prelude::Address;

use parclose_agents::{Agent, AgentPersona, Decision, OfflineHeuristicLLM, Perception, Side};
use parclose_enclave::{clear, SubmittedOrder};
use parclose_shared::{Order, SIDE_REDEEM};

const TICK: u64 = 5;
const WINDOW: u64 = 1;

fn account(n: u8) -> Address {
    Address::Account(AccountHash::new([n; 32]))
}

fn main() {
    let perception = Perception {
        window_id: WINDOW,
        attested_nav: 1_000,
        prior_clear_price: Some(990),
        fund_supply: U256::from(1_000_000u64),
        price_tick: TICK,
    };

    println!("== Parclose crossing window #{WINDOW} ==");
    println!(
        "attested NAV {}  ·  prior clear {}  ·  tick {}\n",
        perception.attested_nav,
        perception.prior_clear_price.unwrap(),
        TICK
    );

    // Two liquidity agents with different personas, each deciding blind.
    let agents = [
        Agent::new(
            AgentPersona {
                name: "Aria".into(),
                account: account(1),
                mandate: Side::Subscribe,
                fund_inventory: U256::zero(),
                cash_inventory: U256::from(10_000_000u64),
                max_size: U256::from(1_000u64),
                risk_appetite_bps: 100,
                style: "patient liquidity provider; fades volatility and protects inventory".into(),
            },
            OfflineHeuristicLLM,
        ),
        Agent::new(
            AgentPersona {
                name: "Boreas".into(),
                account: account(2),
                mandate: Side::Subscribe,
                fund_inventory: U256::zero(),
                cash_inventory: U256::from(3_000_000u64),
                max_size: U256::from(600u64),
                risk_appetite_bps: 350,
                style: "aggressive liquidity provider; chases fills and prices through fair".into(),
            },
            OfflineHeuristicLLM,
        ),
    ];

    let mut book: Vec<SubmittedOrder> = Vec::new();
    let mut next_id: u8 = 1;

    for agent in &agents {
        let decision = agent.decide(&perception).expect("agent reached a decision");
        print_decision(agent.persona().name.as_str(), &decision);
        book.push(SubmittedOrder { order: decision.order.clone(), id: [next_id; 32] });
        next_id += 1;
    }

    // Exiting redeemers (human participants) submitting sealed sell orders into the same window.
    let redeemers = [(3u8, 800u64, 985u64), (4u8, 500u64, 995u64)];
    println!("Redeemers exiting this window:");
    for (n, size, limit) in redeemers {
        println!("  participant #{n}: redeem {size} @ limit {limit}");
        book.push(SubmittedOrder {
            order: Order {
                side: SIDE_REDEEM,
                size: U256::from(size),
                limit,
                window_id: WINDOW,
                account: account(n),
            },
            id: [n; 32],
        });
    }
    println!();

    // Clear with the real uniform-price crossing rule.
    let result = clear(WINDOW, &book, TICK);
    if result.price == 0 {
        println!("No cross this window.");
        return;
    }

    println!("== Uniform clearing price: {} ==", result.price);
    for f in &result.fills {
        if !f.fund_credit.is_zero() {
            println!(
                "  {} bought {} fund for {} cash",
                short(&f.account),
                f.fund_credit,
                f.cash_spent
            );
        }
        if !f.fund_spent.is_zero() {
            println!(
                "  {} redeemed {} fund for {} cash",
                short(&f.account),
                f.fund_spent,
                f.cash_credit
            );
        }
    }
    println!(
        "\nprivate inputs -> verified fair clearing at {} -> atomic on-chain settlement",
        result.price
    );
}

fn print_decision(name: &str, d: &Decision) {
    let side = d.side().map(Side::label).unwrap_or("?");
    println!("Agent {name}: {side} {} @ limit {}", d.size(), d.limit());
    println!("  rationale: {}", d.rationale);
    println!("    · nav signal     : {}", d.factors.nav_signal);
    println!("    · inventory/risk  : {}", d.factors.inventory_risk);
    println!("    · fill probability: {}", d.factors.fill_probability);
    println!("    · prior context   : {}", d.factors.prior_context);
    println!();
}

fn short(a: &Address) -> String {
    let s = format!("{a:?}");
    s.chars().take(20).collect::<String>() + "…"
}
