//! Events emitted by an [`crate::Agent`] while it drives a run.

use gocode_core::ChatMessage;
use gocode_tools::{FileChange, FileSnapshot, ToolCall, ToolCallId, ToolResult};

use crate::{ids::AgentRunId, state::AgentState};

/// A fact emitted while an [`crate::Agent`] drives one run.
///
/// The TUI (or any other observer) consumes these to show progress without knowing the Agent's
/// internal implementation. See `docs/AGENT.md` §9–10 and §102 for ordering guarantees: a
/// `ToolFinished` is never emitted before the matching `ToolStarted`, and `Completed`/`Cancelled`
/// is always the last event for a run.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// The run has started.
    Started,
    /// The run's lifecycle state changed.
    StateChanged(AgentState),
    /// Incremental assistant text produced this turn.
    TextDelta(String),
    /// Incremental reasoning/"thinking" text produced this turn, separate from the final answer.
    ReasoningDelta(String),
    /// The model requested a tool call.
    ToolRequested(ToolCall),
    /// Execution began for the given call.
    ToolStarted(ToolCallId),
    /// An incremental chunk of tool output, typically from `run_command`.
    ToolOutputChunk {
        /// Call the chunk belongs to.
        id: ToolCallId,
        /// Raw chunk content.
        chunk: String,
    },
    /// Execution finished for one tool call.
    ToolFinished(ToolResult),
    /// A tool call affected a workspace file.
    FileChanged(FileChange),
    /// A `write_file` or `apply_patch` call's before/after content for one file, used to build
    /// undo history. Emitted alongside [`AgentEvent::FileChanged`] for the same file, before any
    /// subsequent tool call — including one that then fails or cancels the run — so undo history
    /// is never lost to an early return.
    FileSnapshot(FileSnapshot),
    /// A non-fatal condition worth surfacing to the user.
    Warning(AgentWarning),
    /// The run finished normally.
    Completed(AgentCompletion),
    /// The run was cancelled.
    Cancelled,
}

/// A non-fatal condition observed during a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWarning {
    /// The model requested a tool name that is not registered.
    UnknownTool(String),
    /// The same tool call repeated with the same arguments and was stopped.
    LoopDetected(String),
    /// The provider cut the turn off at its output token limit (`finish_reason: "length"`)
    /// before the model produced any visible text or tool call. Common with large reasoning
    /// models whose chain-of-thought alone can consume the entire budget, leaving nothing for
    /// the actual answer — without this warning that turn looks like a silent no-op.
    TruncatedBeforeContent,
}

/// Counters describing one run's shape, useful for diagnostics and the final response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentRunStats {
    /// Number of inference turns consumed.
    pub turns: usize,
    /// Number of tool calls attempted.
    pub tool_calls: usize,
    /// Number of tool calls that did not succeed.
    pub failed_tool_calls: usize,
    /// Input tokens reported for the run's last turn, when the provider reports them. Used as an
    /// approximate signal for automatic context compaction.
    pub last_input_tokens: Option<u64>,
}

/// The successful outcome of one agent run.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentCompletion {
    /// Identifier of the run that completed.
    pub run_id: AgentRunId,
    /// The model's final visible response.
    pub final_text: String,
    /// Counters for the completed run.
    pub stats: AgentRunStats,
    /// The full conversation transcript after this run, ready to seed the next turn's request.
    pub history: Vec<ChatMessage>,
}
