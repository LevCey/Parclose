//! The agent: a persona plus a reasoning transport, producing one [`Decision`] per window.

use crate::llm::{LLMClient, LLMError};
use crate::strategy::{build_prompt, parse_decision, StrategyError};
use crate::types::{AgentPersona, Decision, Perception};

/// A failure to reach a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// The reasoning transport failed.
    Reasoning(LLMError),
    /// The reasoning reply could not be turned into a valid decision.
    Strategy(StrategyError),
}

impl core::fmt::Display for AgentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AgentError::Reasoning(e) => write!(f, "{e}"),
            AgentError::Strategy(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<LLMError> for AgentError {
    fn from(e: LLMError) -> Self {
        AgentError::Reasoning(e)
    }
}

impl From<StrategyError> for AgentError {
    fn from(e: StrategyError) -> Self {
        AgentError::Strategy(e)
    }
}

/// A confidential liquidity agent.
///
/// An agent owns a private [`AgentPersona`] and a reasoning transport. [`Agent::decide`] is the
/// reason step of the perceive -> reason -> act loop: it builds the prompt from the agent's own
/// persona and the shared public [`Perception`], asks the model, and parses the reply into a
/// validated [`Decision`].
///
/// Sealed competition is structural here: `decide` is given only this agent's own persona and the
/// public perception. It never receives a rival's persona, order, or rationale, so no agent can
/// condition on another's strategy — the blindness the thesis depends on is enforced by the
/// signature, not by convention.
pub struct Agent<L: LLMClient> {
    persona: AgentPersona,
    llm: L,
}

impl<L: LLMClient> Agent<L> {
    /// Creates an agent from a persona and a reasoning transport.
    pub fn new(persona: AgentPersona, llm: L) -> Self {
        Self { persona, llm }
    }

    /// The agent's persona (for the dashboard/logs).
    pub fn persona(&self) -> &AgentPersona {
        &self.persona
    }

    /// Runs the reason step: perceive (the supplied perception) -> reason (the model) -> a
    /// validated [`Decision`]. The decision's order is ready to be sealed and submitted; its
    /// rationale and factor trace are off-chain artifacts for the dashboard/logs only.
    pub fn decide(&self, perception: &Perception) -> Result<Decision, AgentError> {
        let prompt = build_prompt(perception, &self.persona);
        let raw = self.llm.complete(&prompt)?;
        let decision = parse_decision(&raw, perception, &self.persona)?;
        Ok(decision)
    }
}
