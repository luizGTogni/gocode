//! Connects provider inference to validated local tools through a cancellable, multi-turn
//! Agent Runtime. See `docs/AGENT.md`.
//!
//! The Agent owns the inference → tool → inference loop, conversation history, tool-call
//! assembly, permission integration, and run limits. It renders nothing and knows nothing about
//! a specific provider's wire format: both arrive through [`gocode_core::Provider`] and
//! [`gocode_tools::Tool`].

mod agent;
mod context;
mod error;
mod events;
mod ids;
mod limits;
mod state;
mod subagent_manager;
mod toolcalls;

pub use agent::{Agent, AgentRequest};
pub use error::AgentError;
pub use events::{AgentCompletion, AgentEvent, AgentRunStats, AgentWarning};
pub use ids::AgentRunId;
pub use limits::{AgentLimit, AgentLimits};
pub use state::AgentState;
pub use subagent_manager::{
    SpawnRequest, SubagentError, SubagentEvent, SubagentLimits, SubagentManager,
    effective_subagent_mode, parse_result,
};

#[cfg(test)]
mod tests;
