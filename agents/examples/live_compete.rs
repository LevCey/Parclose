//! The same blind-competition window as `compete`, but driven by the real Anthropic model.
//!
//! Requires `ANTHROPIC_API_KEY` in the environment, network access, and the `curl` binary. The
//! perceive -> reason -> act loop is byte-for-byte the one the offline example uses; only the
//! reasoning transport differs.
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-... cargo run --example live_compete
//! ```

use odra::casper_types::account::AccountHash;
use odra::casper_types::U256;
use odra::prelude::Address;

use parclose_agents::{Agent, AgentPersona, AnthropicClient, Perception, Side};
use parclose_enclave::{clear, SubmittedOrder};
use parclose_shared::{Order, SIDE_REDEEM};

const TICK: u64 = 5;
const WINDOW: u64 = 1;

fn account(n: u8) -> Address {
    Address::Account(AccountHash::new([n; 32]))
}

fn main() {
    let client = match AnthropicClient::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Live reasoning unavailable: {e}.");
            eprintln!("Set ANTHROPIC_API_KEY (and optionally ANTHROPIC_MODEL) to run this example.");
            eprintln!("For an offline run with the deterministic stand-in: cargo run --example compete");
            return;
        }
    };
    println!("Reasoning with model: {}\n", client.model());

    let perception = Perception {
        window_id: WINDOW,
        attested_nav: 1_000,
        prior_clear_price: Some(990),
        fund_supply: U256::from(1_000_000u64),
        price_tick: TICK,
    };

    let personas = [
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
    ];

    let mut book: Vec<SubmittedOrder> = Vec::new();
    let mut next_id: u8 = 1;

    for persona in personas {
        let name = persona.name.clone();
        let agent = Agent::new(persona, client.clone());
        match agent.decide(&perception) {
            Ok(d) => {
                let side = d.side().map(Side::label).unwrap_or("?");
                println!("Agent {name}: {side} {} @ limit {}", d.size(), d.limit());
                println!("  rationale: {}", d.rationale);
                println!("    · nav signal     : {}", d.factors.nav_signal);
                println!("    · inventory/risk  : {}", d.factors.inventory_risk);
                println!("    · fill probability: {}", d.factors.fill_probability);
                println!("    · prior context   : {}\n", d.factors.prior_context);
                book.push(SubmittedOrder { order: d.order, id: [next_id; 32] });
                next_id += 1;
            }
            Err(e) => {
                eprintln!("Agent {name} could not decide: {e}");
                return;
            }
        }
    }

    for (n, size, limit) in [(3u8, 800u64, 985u64), (4u8, 500u64, 995u64)] {
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

    let result = clear(WINDOW, &book, TICK);
    if result.price == 0 {
        println!("No cross this window.");
        return;
    }
    println!("== Uniform clearing price: {} ==", result.price);
    println!("private inputs -> verified fair clearing -> atomic on-chain settlement");
}
