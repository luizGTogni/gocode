//! Language Server Protocol client support: gives the agent semantic code navigation (hover,
//! go-to-definition, find-references, document symbols, diagnostics) instead of relying solely
//! on grep. Each configured language server runs as a long-lived stdio subprocess for the
//! duration of the session, managed by [`manager::LspManager`].

pub mod client;
pub mod jsonrpc;
pub mod manager;
pub mod tool;
pub mod transport;

pub use client::{Diagnostic, LspClient};
pub use manager::{
    ArcLspManagerObserver, FileChangeKind, LspManager, LspServerSpec, is_command_available,
};
pub use tool::LspTool;
pub use transport::{LspTransport, stdio::StdioTransport};

/// Everything that can go wrong talking to a language server.
#[derive(Debug, Clone)]
pub enum LspError {
    /// The underlying transport (process spawn, pipe write, connection) failed.
    Transport(String),
    /// A message didn't parse as valid JSON-RPC or valid LSP payload shape.
    Protocol(String),
    /// The server answered a call with a JSON-RPC error object.
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
    /// No language server is configured (or found on PATH) for the file's language.
    Unsupported(String),
}

impl std::fmt::Display for LspError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LspError::Transport(message) => write!(formatter, "LSP transport error: {message}"),
            LspError::Protocol(message) => write!(formatter, "LSP protocol error: {message}"),
            LspError::Rpc {
                method,
                code,
                message,
            } => write!(
                formatter,
                "language server rejected '{method}' ({code}): {message}"
            ),
            LspError::Unsupported(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for LspError {}
