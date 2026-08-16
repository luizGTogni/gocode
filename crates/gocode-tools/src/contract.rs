use std::{fmt, future::Future, path::PathBuf, pin::Pin, sync::Arc};

use crate::permissions::PermissionContext;

/// Stable identifier for a built-in tool, used as the registry key and in provider schemas.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolName(String);

impl ToolName {
    /// Creates a tool name from its stable string form.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the stable tool name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Correlation identifier shared by a provider tool call, its execution, and its result.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// Wraps an existing correlation identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the correlation identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Metadata a tool exposes to the model and the registry.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Stable, model-facing tool name.
    pub name: ToolName,
    /// Concise description that teaches the model when to use, and not use, the tool.
    pub description: String,
    /// JSON schema describing accepted arguments.
    pub input_schema: serde_json::Value,
}

/// A normalized tool call, independent of the originating provider's wire format.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Correlation identifier for this call.
    pub id: ToolCallId,
    /// Name of the requested tool.
    pub name: ToolName,
    /// Raw arguments, validated against the tool's schema before execution.
    pub arguments: serde_json::Value,
}

/// Outcome of one tool execution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// The tool executed; a non-zero process exit code is still `Success`.
    Success,
    /// The tool could not complete the requested action.
    Failed,
    /// Execution was cancelled before completion.
    Cancelled,
    /// The user or permission policy denied the action.
    Denied,
}

/// Human- and model-facing tool output.
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    /// Rendered content returned to the model.
    pub content: String,
    /// Whether `content` omits part of the full result.
    pub truncated: bool,
}

impl ToolOutput {
    /// Creates output that was not truncated.
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            truncated: false,
        }
    }

    /// Creates output explicitly marked as truncated.
    #[must_use]
    pub fn truncated(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            truncated: true,
        }
    }
}

/// Small, common metadata attached to every tool result.
#[derive(Debug, Clone, Default)]
pub struct ToolMetadata {
    /// Wall-clock execution duration.
    pub duration_ms: Option<u64>,
    /// Files created, modified, or deleted by this call.
    pub affected_files: Vec<FileChange>,
    /// Process exit code, when the tool ran an external process.
    pub exit_code: Option<i32>,
    /// Whether the output was truncated (mirrors [`ToolOutput::truncated`] for convenience).
    pub truncated: bool,
}

/// Normalized result returned from a tool execution.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Correlation identifier copied from the originating [`ToolCall`].
    pub call_id: ToolCallId,
    /// Outcome classification.
    pub status: ToolStatus,
    /// Content returned to the model.
    pub output: ToolOutput,
    /// Additional structured metadata.
    pub metadata: ToolMetadata,
}

impl ToolResult {
    /// Builds a successful result with no extra metadata.
    #[must_use]
    pub fn success(call_id: ToolCallId, output: ToolOutput) -> Self {
        Self {
            call_id,
            status: ToolStatus::Success,
            metadata: ToolMetadata {
                truncated: output.truncated,
                ..ToolMetadata::default()
            },
            output,
        }
    }

    /// Builds a failed result carrying a model-actionable explanation.
    #[must_use]
    pub fn failed(call_id: ToolCallId, message: impl Into<String>) -> Self {
        Self {
            call_id,
            status: ToolStatus::Failed,
            output: ToolOutput::new(message),
            metadata: ToolMetadata::default(),
        }
    }

    /// Builds a result for a call cancelled before or during execution.
    #[must_use]
    pub fn cancelled(call_id: ToolCallId) -> Self {
        Self {
            call_id,
            status: ToolStatus::Cancelled,
            output: ToolOutput::new("The operation was cancelled."),
            metadata: ToolMetadata::default(),
        }
    }

    /// Builds a result for a call the permission policy or user denied.
    #[must_use]
    pub fn denied(call_id: ToolCallId, reason: impl Into<String>) -> Self {
        Self {
            call_id,
            status: ToolStatus::Denied,
            output: ToolOutput::new(reason),
            metadata: ToolMetadata::default(),
        }
    }
}

/// Errors that prevent a tool call from producing a normal [`ToolResult`].
///
/// Most of these are still returned to the model as a [`ToolResult`] with
/// [`ToolStatus::Failed`]; they exist as a typed error so tool implementations do not need to
/// hand-format every failure path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// Arguments did not parse or did not satisfy the tool's schema.
    InvalidArguments(String),
    /// A resolved path fell outside the workspace boundary.
    OutsideWorkspace(PathBuf),
    /// The requested path does not exist.
    NotFound(PathBuf),
    /// The permission policy or user denied the action.
    PermissionDenied(String),
    /// The tool cannot operate on the file's detected content type.
    UnsupportedFileType(String),
    /// An underlying filesystem or process I/O operation failed.
    Io(String),
    /// Execution was cancelled.
    Cancelled,
    /// Execution exceeded its allotted time.
    Timeout,
    /// An unexpected internal failure occurred.
    Internal(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(message) => write!(formatter, "invalid arguments: {message}"),
            Self::OutsideWorkspace(path) => {
                write!(formatter, "{} is outside the workspace", path.display())
            }
            Self::NotFound(path) => write!(formatter, "{} was not found", path.display()),
            Self::PermissionDenied(message) => write!(formatter, "permission denied: {message}"),
            Self::UnsupportedFileType(message) => write!(formatter, "unsupported file: {message}"),
            Self::Io(message) => write!(formatter, "I/O error: {message}"),
            Self::Cancelled => write!(formatter, "the operation was cancelled"),
            Self::Timeout => write!(formatter, "the operation timed out"),
            Self::Internal(message) => write!(formatter, "internal error: {message}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// Kind of change a write tool applied to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A new file was created.
    Created,
    /// An existing file's content changed.
    Modified,
    /// A file was removed.
    Deleted,
}

/// Record of one file affected by a write tool, suitable for session persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Workspace-relative path of the affected file.
    pub path: PathBuf,
    /// What kind of change occurred.
    pub kind: ChangeKind,
}

/// Progress reported by a long-running tool while it executes.
#[derive(Debug, Clone)]
pub enum ToolEvent {
    /// Execution began for the given call.
    Started(ToolCallId),
    /// An incremental chunk of process or search output.
    OutputChunk {
        /// Call the chunk belongs to.
        id: ToolCallId,
        /// Raw chunk content.
        chunk: String,
    },
    /// Execution finished; carries the final result.
    Finished(ToolResult),
}

/// Sink that tools use to report progress without depending on a concrete transport.
pub trait ToolEventSink: Send + Sync {
    /// Records one tool lifecycle event. Implementations must not block or panic.
    fn emit(&self, event: ToolEvent);
}

/// A [`ToolEventSink`] that discards every event, used when no observer is attached.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullEventSink;

impl ToolEventSink for NullEventSink {
    fn emit(&self, _event: ToolEvent) {}
}

/// Controlled runtime context passed to every tool execution.
#[derive(Clone)]
pub struct ToolContext {
    /// Correlation identifier for the call this context serves, echoed on every [`ToolResult`].
    pub call_id: ToolCallId,
    /// Root directory that bounds every filesystem and process operation.
    pub project_root: PathBuf,
    /// Cooperative cancellation shared with the caller.
    pub cancellation: tokio_util::sync::CancellationToken,
    /// Permission policy and interactive resolver for this run.
    pub permissions: PermissionContext,
    /// Optional progress observer.
    pub events: Arc<dyn ToolEventSink>,
}

impl ToolContext {
    /// Creates a context with no progress observer.
    #[must_use]
    pub fn new(call_id: ToolCallId, project_root: PathBuf, permissions: PermissionContext) -> Self {
        Self {
            call_id,
            project_root,
            cancellation: tokio_util::sync::CancellationToken::new(),
            permissions,
            events: Arc::new(NullEventSink),
        }
    }

    /// Attaches a progress observer to this context.
    #[must_use]
    pub fn with_events(mut self, events: Arc<dyn ToolEventSink>) -> Self {
        self.events = events;
        self
    }

    /// Attaches an explicit cancellation token to this context.
    #[must_use]
    pub fn with_cancellation(mut self, cancellation: tokio_util::sync::CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

/// Future type returned by [`Tool::execute`].
///
/// Hand-written instead of relying on an `async-trait`-style macro so `dyn Tool` stays a
/// dependency-free trait object.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>>;

/// The capability boundary through which the agent affects the local project and OS.
pub trait Tool: Send + Sync {
    /// Metadata exposed to the model and the registry.
    fn definition(&self) -> ToolDefinition;

    /// Validates arguments, enforces boundaries and permissions, and executes the action.
    fn execute(&self, ctx: ToolContext, input: serde_json::Value) -> ToolFuture<'_>;
}
