//! Bounds that keep one agent run finite.

use std::fmt;

/// Prevents an agent run from looping indefinitely or exhausting resources.
///
/// See `docs/AGENT.md` §53–57. The defaults are starting guidelines, not tuned values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentLimits {
    /// Maximum number of UTF-8 scalar values retained from one provider response.
    pub max_response_chars: usize,
    /// Maximum number of inference turns before the run stops.
    pub max_turns: usize,
    /// Maximum number of tool calls across the whole run.
    pub max_total_tool_calls: usize,
    /// Maximum number of tool calls the model may request in a single turn.
    pub max_tool_calls_per_turn: usize,
    /// Maximum number of consecutive failed tool calls before the run stops.
    pub max_consecutive_failures: usize,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_response_chars: 64 * 1024,
            max_turns: 100,
            max_total_tool_calls: 150,
            max_tool_calls_per_turn: 10,
            max_consecutive_failures: 3,
        }
    }
}

/// The specific limit an agent run stopped against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLimit {
    /// Provider stream text exceeded the response budget.
    ResponseTooLarge,
    /// [`AgentLimits::max_turns`] was reached.
    MaxTurns,
    /// [`AgentLimits::max_total_tool_calls`] was reached.
    MaxTotalToolCalls,
    /// [`AgentLimits::max_tool_calls_per_turn`] was exceeded in one turn.
    MaxToolCallsPerTurn,
    /// [`AgentLimits::max_consecutive_failures`] was reached.
    ConsecutiveFailures,
    /// The same tool call failed with the same arguments repeatedly.
    RepeatedToolCall,
    /// Provider stream data exceeded the bounded tool-call assembly budget.
    ToolCallPayloadTooLarge,
}

impl fmt::Display for AgentLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResponseTooLarge => {
                formatter.write_str("the provider response exceeded the maximum size")
            }
            Self::MaxTurns => formatter.write_str("the maximum number of turns was reached"),
            Self::MaxTotalToolCalls => {
                formatter.write_str("the maximum number of tool calls was reached")
            }
            Self::MaxToolCallsPerTurn => {
                formatter.write_str("too many tool calls were requested in one turn")
            }
            Self::ConsecutiveFailures => {
                formatter.write_str("too many tool calls failed consecutively")
            }
            Self::RepeatedToolCall => {
                formatter.write_str("the same tool call repeated with the same arguments")
            }
            Self::ToolCallPayloadTooLarge => {
                formatter.write_str("the provider sent an oversized tool-call payload")
            }
        }
    }
}
