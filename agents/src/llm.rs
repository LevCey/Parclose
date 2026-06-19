//! The reasoning transport: an [`LLMClient`] trait with offline doubles for tests and demos.
//!
//! The trait is text-in, text-out so the production Anthropic client and the offline doubles are
//! interchangeable. The real perceive -> reason -> act path uses the model; the doubles here let
//! the competition harness, parsing, and the acceptance-criteria tests run with no network and no
//! API key — the same "mock-first" discipline the rest of the system uses for the attestation
//! signer.
//!
//! **The offline doubles are test scaffolding, not an agent strategy.** [`OfflineHeuristicLLM`]
//! stands in for the model so the pipeline can be exercised deterministically; it must never be
//! the shipped pricer. Genuine multi-factor reasoning — the behaviour that is actually scored —
//! is delivered by the model behind the real client.

use crate::strategy::{DecisionDto, FactorsDto, PromptInputs};
use crate::types::Side;

/// A prompt: a system message framing the task and a user message carrying the perceived inputs.
#[derive(Clone, Debug)]
pub struct Prompt {
    /// The framing/instructions (see [`crate::strategy::SYSTEM_PROMPT`]).
    pub system: String,
    /// The per-window context and mandate.
    pub user: String,
}

/// Errors a reasoning transport can raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LLMError {
    /// A configuration problem (e.g. a missing API key or malformed prompt).
    Config(String),
    /// The transport failed (network, TLS, timeout).
    Transport(String),
    /// The provider returned an error status or body.
    Provider(String),
    /// The provider returned an empty completion.
    EmptyCompletion,
}

impl core::fmt::Display for LLMError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LLMError::Config(m) => write!(f, "reasoning client misconfigured: {m}"),
            LLMError::Transport(m) => write!(f, "reasoning transport failed: {m}"),
            LLMError::Provider(m) => write!(f, "reasoning provider error: {m}"),
            LLMError::EmptyCompletion => write!(f, "reasoning provider returned no completion"),
        }
    }
}

impl std::error::Error for LLMError {}

/// The reasoning transport an [`crate::Agent`] calls to turn a [`Prompt`] into a reply.
pub trait LLMClient {
    /// Returns the model's reply text (expected to contain the decision JSON).
    fn complete(&self, prompt: &Prompt) -> Result<String, LLMError>;
}

/// A fixed-reply double: returns the same string regardless of prompt. Useful for testing the
/// parser/validator and for injecting a contrasting decision in model-in-the-loop tests.
#[derive(Clone, Debug)]
pub struct ScriptedLLM {
    /// The exact reply to return.
    pub response: String,
}

impl ScriptedLLM {
    /// Creates a scripted client that always replies with `response`.
    pub fn new(response: impl Into<String>) -> Self {
        Self { response: response.into() }
    }
}

impl LLMClient for ScriptedLLM {
    fn complete(&self, _prompt: &Prompt) -> Result<String, LLMError> {
        Ok(self.response.clone())
    }
}

/// A deterministic stand-in for the model, used only offline.
///
/// It reads the `<inputs>` block embedded in the prompt and returns a well-formed decision whose
/// quote moves with every input — NAV, prior clear, inventory, risk appetite, and a competition
/// shade. That state-dependence lets the harness and the criteria tests run without a key, but it
/// is explicitly **not** the agent's strategy and must never ship as the pricer (a closed-form or
/// fixed-rule pricer is forbidden). Treat it exactly like the dev attestation signer: a stand-in
/// that makes the full loop runnable while the real reasoning path is wired.
#[derive(Clone, Debug, Default)]
pub struct OfflineHeuristicLLM;

impl LLMClient for OfflineHeuristicLLM {
    fn complete(&self, prompt: &Prompt) -> Result<String, LLMError> {
        let inputs = extract_inputs(&prompt.user)
            .ok_or_else(|| LLMError::Config("prompt carried no <inputs> block".into()))?;
        let dto = offline_decision(&inputs);
        serde_json::to_string(&dto).map_err(|e| LLMError::Provider(e.to_string()))
    }
}

/// Extracts and parses the `<inputs>` JSON block embedded by [`crate::strategy::build_prompt`].
fn extract_inputs(user: &str) -> Option<PromptInputs> {
    let start = user.find("<inputs>")? + "<inputs>".len();
    let end = user[start..].find("</inputs>")? + start;
    serde_json::from_str(user[start..end].trim()).ok()
}

/// Computes the offline stand-in decision. Quote = fair value adjusted by a signed offset that
/// combines four interacting terms (NAV-anchored fair, risk appetite, inventory pressure, and a
/// competition shade), so changing any single input moves the order non-trivially.
fn offline_decision(inp: &PromptInputs) -> DecisionDto {
    let nav = inp.attested_nav.max(1);
    let fair = match inp.prior_clear_price {
        Some(prior) if prior > 0 => (2 * nav + prior) / 3, // blend NAV with the prior clear
        _ => nav,
    };
    let side = Side::parse(&inp.mandate).unwrap_or(Side::Subscribe);

    // Factor terms, in basis points.
    let base: i128 = 200; // baseline distance a quote sits from fair
    let risk_term: i128 = inp.risk_appetite_bps as i128; // appetite pulls the quote toward/through fair
    let notional: u128 = (fair as u128).saturating_mul(inp.max_size.max(1) as u128).max(1);
    let balance: u128 = match side {
        Side::Subscribe => inp.cash_inventory as u128,
        Side::Redeem => (inp.fund_inventory as u128).saturating_mul(fair as u128),
    };
    let inv_term: i128 = if balance >= 2 * notional {
        60
    } else if balance >= notional {
        30
    } else {
        0
    };
    let comp_term: i128 = 40; // shade toward fair to win fills under blind competition

    // Buyers sit below fair (negative offset), sellers above (positive); appetite, inventory
    // pressure, and the competition shade all pull the quote toward — or through — fair.
    let pull = risk_term + inv_term + comp_term;
    let offset_bps = match side {
        Side::Subscribe => -base + pull,
        Side::Redeem => base - pull,
    }
    .clamp(-5000, 3000);

    let raw_limit = (((fair as i128) * (10_000 + offset_bps)) / 10_000).max(1) as u64;
    // Snap to the published tick grid so the narrated quote matches the order that ships.
    let tick = inp.price_tick.max(1);
    let limit = if tick <= 1 {
        raw_limit
    } else {
        let snapped = ((raw_limit + tick / 2) / tick) * tick;
        if snapped == 0 {
            tick
        } else {
            snapped
        }
    };

    let capacity: u64 = match side {
        Side::Subscribe => inp.cash_inventory / limit.max(1),
        Side::Redeem => inp.fund_inventory,
    };
    let size = capacity.min(inp.max_size).max(1);

    let prior_clause = match inp.prior_clear_price {
        Some(p) => format!(" blended with the prior clear {p}"),
        None => " with no prior clear to lean on".to_string(),
    };
    let stance = if offset_bps.abs() < 50 { "tight" } else { "measured" };

    DecisionDto {
        side: side.label().to_string(),
        size,
        limit,
        rationale: format!(
            "Quoting {limit} against a fair value of {fair}; a {stance} {} stance under blind competition.",
            side.label()
        ),
        factors: FactorsDto {
            nav_signal: format!(
                "Anchored fair value at {fair} from attested NAV {nav}{prior_clause}."
            ),
            inventory_risk: format!(
                "Risk appetite {}bps and an inventory pressure term of {inv_term}bps set how far from fair I sit.",
                inp.risk_appetite_bps
            ),
            fill_probability: format!(
                "Shaded {comp_term}bps toward fair to lift fill odds against unseen competing liquidity."
            ),
            prior_context: match inp.prior_clear_price {
                Some(p) => format!("Leaned toward the prior clearing price {p} as a recent reference."),
                None => "No prior clearing price, so I weighted current NAV more heavily.".to_string(),
            },
        },
    }
}
