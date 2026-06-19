//! Core types for the reasoning layer: what an agent perceives, who the agent is, and the
//! decision it produces.

use odra::casper_types::U256;
use odra::prelude::Address;
use parclose_shared::{Order, SIDE_REDEEM, SIDE_SUBSCRIBE};

/// Which side of the crossing an order takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Buy fund tokens (provide exit liquidity to redeemers) at a price `<= limit`.
    Subscribe,
    /// Sell fund tokens (redeem) at a price `>= limit`.
    Redeem,
}

impl Side {
    /// The canonical on-chain encoding ([`parclose_shared::SIDE_SUBSCRIBE`] / `SIDE_REDEEM`).
    pub fn as_u8(self) -> u8 {
        match self {
            Side::Subscribe => SIDE_SUBSCRIBE,
            Side::Redeem => SIDE_REDEEM,
        }
    }

    /// The lowercase label used in prompts and the reasoning JSON.
    pub fn label(self) -> &'static str {
        match self {
            Side::Subscribe => "subscribe",
            Side::Redeem => "redeem",
        }
    }

    /// Parses the label produced by the reasoning layer; tolerant of surrounding whitespace/case.
    pub fn parse(s: &str) -> Option<Side> {
        match s.trim().to_ascii_lowercase().as_str() {
            "subscribe" | "buy" | "bid" => Some(Side::Subscribe),
            "redeem" | "sell" | "ask" => Some(Side::Redeem),
            _ => None,
        }
    }
}

/// The attested market and fund state an agent perceives for a window.
///
/// Every field here is *public, attested context* — the kind of signal an agent legitimately
/// reads through the Casper MCP server / CSPR.cloud (or a clearly-labelled stand-in NAV feed for
/// the demo). It contains nothing private to any participant and nothing about a rival's order.
#[derive(Clone, Debug)]
pub struct Perception {
    /// The open window this decision is for.
    pub window_id: u64,
    /// Attested net asset value per fund token, in cash-token units. The market anchor.
    pub attested_nav: u64,
    /// The price at which the previous window cleared, if any — the prior-clearing context.
    pub prior_clear_price: Option<u64>,
    /// Current fund-token supply / float, for context on depth.
    pub fund_supply: U256,
    /// The published price granularity; limits and the clearing price are multiples of it.
    pub price_tick: u64,
}

/// An agent's private configuration: its mandate, balance sheet, and risk appetite.
///
/// This is the agent's *own* state — its inventory and risk limits (a perceived input under
/// criterion 2) plus a free-form persona handed to the reasoning layer. Two agents with different
/// personas are expected to reach different orders even on identical market perception.
#[derive(Clone, Debug)]
pub struct AgentPersona {
    /// Human-readable agent name, for the dashboard and logs.
    pub name: String,
    /// The on-chain account the agent acts under; written into the order's `account` field so the
    /// enclave can bind the decrypted order to its on-chain submitter (D-15).
    pub account: Address,
    /// The side this agent is mandated to provide liquidity on.
    pub mandate: Side,
    /// Fund-token units the agent holds — its capacity to sell (redeem).
    pub fund_inventory: U256,
    /// Cash-token units the agent holds — its capacity to buy (subscribe).
    pub cash_inventory: U256,
    /// The largest order size the agent will place in a single window.
    pub max_size: U256,
    /// Risk appetite in basis points: how far from fair value the agent is willing to price.
    /// Larger = more aggressive (prices closer to, or through, fair value).
    pub risk_appetite_bps: u32,
    /// A natural-language persona handed verbatim to the reasoning layer (e.g. "patient
    /// inventory manager who fades volatility" vs "aggressive liquidity provider chasing fills").
    pub style: String,
}

/// A per-factor trace of how each of the four required factors moved the decision.
///
/// Every field must be a non-empty explanation; [`crate::strategy::parse_decision`] rejects a
/// decision that leaves any of them blank, which is how the "weighs at least four factors"
/// requirement is enforced rather than assumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactorTrace {
    /// How the attested NAV / market signal shaped the price.
    pub nav_signal: String,
    /// How the agent's own inventory and risk limit shaped size and price.
    pub inventory_risk: String,
    /// How the agent judged fill probability against unseen competing liquidity.
    pub fill_probability: String,
    /// How the prior clearing context shaped the decision.
    pub prior_context: String,
}

/// The output of the reasoning layer: a concrete order plus the reasoning behind it.
///
/// The [`Order`] is what gets sealed and submitted on-chain. The `rationale` and `factors` are
/// off-chain, demo-safe artifacts only — surfaced to the operator/judge and/or revealed
/// post-clearing, never posted on-chain and never visible to a rival during an open window.
#[derive(Clone, Debug)]
pub struct Decision {
    /// The order to seal and submit.
    pub order: Order,
    /// A short natural-language rationale ("widened my bid because attested NAV moved against my
    /// inventory and a competing bid is likely").
    pub rationale: String,
    /// The per-factor trace backing the rationale.
    pub factors: FactorTrace,
}

impl Decision {
    /// The decided side.
    pub fn side(&self) -> Option<Side> {
        match self.order.side {
            SIDE_SUBSCRIBE => Some(Side::Subscribe),
            SIDE_REDEEM => Some(Side::Redeem),
            _ => None,
        }
    }

    /// The decided limit price.
    pub fn limit(&self) -> u64 {
        self.order.limit
    }

    /// The decided size.
    pub fn size(&self) -> U256 {
        self.order.size
    }
}
