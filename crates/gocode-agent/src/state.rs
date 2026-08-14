//! Lifecycle state of one agent run.

/// Where one [`crate::AgentRun`] currently is in its lifecycle.
///
/// See `docs/AGENT.md` §4–5 for the full transition diagram. Any active state may move to
/// [`Self::Cancelled`] when the user cancels, and unrecoverable errors move to [`Self::Failed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// No run is active.
    Idle,
    /// The run has started but inference has not begun.
    Preparing,
    /// The provider is streaming a response.
    Inference,
    /// A tool call is waiting for the user to approve or deny it.
    WaitingForPermission,
    /// One or more tool calls are executing.
    ExecutingTools,
    /// The model produced a final response with no pending tool calls.
    Finalizing,
    /// The run finished normally.
    Completed,
    /// The user cancelled the run.
    Cancelled,
    /// The run ended in an unrecoverable error.
    Failed,
}
