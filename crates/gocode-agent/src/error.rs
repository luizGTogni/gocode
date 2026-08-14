//! Errors that terminate an agent run.

use std::fmt;

use gocode_core::ProviderError;

use crate::limits::AgentLimit;

/// An error that ends the current [`crate::AgentRun`] outright.
///
/// Recoverable failures (invalid tool arguments, a missing file, a non-zero command exit, an
/// unknown tool) are not represented here: they become a [`gocode_tools::ToolResult`] returned to
/// the model so the run can continue. See `docs/AGENT.md` §69–70.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentError {
    /// The provider could not start or continue the request.
    Provider(ProviderError),
    /// The request context could not be constructed.
    Context(String),
    /// A configured limit stopped the run.
    LimitReached(AgentLimit),
    /// The user cancelled the run.
    Cancelled,
    /// An unexpected internal failure occurred.
    Internal(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => write!(formatter, "provider error: {error}"),
            Self::Context(message) => write!(formatter, "context error: {message}"),
            Self::LimitReached(limit) => write!(formatter, "run stopped: {limit}"),
            Self::Cancelled => formatter.write_str("the run was cancelled"),
            Self::Internal(message) => write!(formatter, "internal error: {message}"),
        }
    }
}

impl std::error::Error for AgentError {}
