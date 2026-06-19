//! Parclose confidential liquidity agents.
//!
//! A liquidity agent runs a real **perceive -> reason -> act** loop. It reads attested market
//! data and fund state, reasons about a pricing and sizing strategy *under uncertainty and blind
//! competition*, and submits a single **sealed order** into a crossing window. It cannot observe
//! any rival's order, size, price, or rationale — confidentiality is precisely what makes the
//! competition meaningful.
//!
//! This crate is the **reason** layer plus the local competition harness. It is deliberately
//! transport-agnostic:
//!
//! * The reasoning is driven by an [`LLMClient`] trait, so the strategy can run against the real
//!   Anthropic model in production or a deterministic test double offline. Removing the model
//!   measurably changes behaviour — the order traces to reasoning that cites the specific inputs
//!   perceived, not to a fixed formula.
//! * A [`Decision`] reduces that reasoning to a concrete [`parclose_shared::Order`] together with
//!   a short natural-language rationale and a per-factor trace, surfaced off-chain to the
//!   dashboard and logs (never posted on-chain, never visible to a rival during an open window).
//!
//! ## The four bars this layer must clear
//!
//! 1. **Visible reasoning** — every decision carries a rationale and a [`FactorTrace`].
//! 2. **State-dependent behaviour** — change one perceived input and the order shifts in a way a
//!    one-line transform of price could not explain.
//! 3. **Genuine sealed competition** — two agents with different personas reach different orders,
//!    each blind to the other.
//! 4. **Model-in-the-loop** — the reasoning step, not a closed-form rule, produces the order.
//!
//! A decision must weigh at least four factors — the attested NAV/market signal, the agent's own
//! inventory and risk limit, fill probability under competition, and the prior clearing context.
//! [`strategy::parse_decision`] rejects any reasoning output that fails to articulate all four, so
//! the multi-factor requirement is enforced structurally rather than assumed.

pub mod agent;
pub mod anthropic;
pub mod llm;
pub mod strategy;
pub mod types;

pub use agent::Agent;
pub use anthropic::AnthropicClient;
pub use llm::{LLMClient, LLMError, OfflineHeuristicLLM, Prompt, ScriptedLLM};
pub use strategy::{build_prompt, parse_decision, StrategyError};
pub use types::{AgentPersona, Decision, FactorTrace, Perception, Side};
