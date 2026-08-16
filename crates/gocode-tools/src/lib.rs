//! Deterministic, independently testable tool layer for the Gocode coding agent.
//!
//! Tools are the only supported mechanism through which the agent affects the local project and
//! operating system. Every call passes through [`workspace`] path validation and
//! [`permissions`] before it can touch the filesystem or spawn a process. See `docs/TOOLS.md`.

pub mod contract;
mod patch;
pub mod permissions;
pub mod process;
pub mod registry;
pub mod tools;
pub mod workspace;
pub mod worktree;

pub use contract::{
    ChangeKind, FileChange, NullEventSink, Tool, ToolCall, ToolCallId, ToolContext, ToolDefinition,
    ToolError, ToolEvent, ToolEventSink, ToolFuture, ToolMetadata, ToolName, ToolOutput,
    ToolResult, ToolStatus,
};
pub use registry::{ToolRegistry, builtin_registry};
