//! MCP (Model Context Protocol) client support: JSON-RPC framing, transports (stdio today,
//! streamable HTTP planned), and the `initialize`/`tools/list`/`tools/call` client.

pub mod client;
pub mod jsonrpc;
pub mod manager;
pub mod tool_bridge;
pub mod transport;

pub use client::{McpClient, McpToolInfo, ToolCallOutcome};
pub use manager::{McpConnectOutcome, connect_configured_servers};
pub use tool_bridge::McpTool;
pub use transport::{McpTransport, stdio::StdioTransport};

/// Everything that can go wrong talking to an MCP server.
#[derive(Debug, Clone)]
pub enum McpError {
    /// The underlying transport (process spawn, pipe write, connection) failed.
    Transport(String),
    /// A message didn't parse as valid JSON-RPC or valid MCP payload shape.
    Protocol(String),
    /// The server answered a call with a JSON-RPC error object.
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
}

impl std::fmt::Display for McpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Transport(message) => write!(formatter, "MCP transport error: {message}"),
            McpError::Protocol(message) => write!(formatter, "MCP protocol error: {message}"),
            McpError::Rpc {
                method,
                code,
                message,
            } => write!(
                formatter,
                "MCP server rejected '{method}' ({code}): {message}"
            ),
        }
    }
}

impl std::error::Error for McpError {}
