//! Transport abstraction MCP clients speak over: a stdio subprocess today, streamable HTTP
//! planned next (see the `/mcp` implementation plan).

pub mod stdio;

use std::{future::Future, pin::Pin};

use crate::{
    McpError,
    jsonrpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse},
};

/// A boxed, `Send` future — mirrors the hand-rolled `ToolFuture` convention in
/// `gocode-tools::contract` so this crate stays free of an `async-trait` dependency.
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, McpError>> + Send + 'a>>;

/// A live channel to a single MCP server, independent of how bytes actually move.
pub trait McpTransport: Send + Sync {
    /// Sends a request and waits for its matching response.
    fn send_request(&self, request: JsonRpcRequest) -> TransportFuture<'_, JsonRpcResponse>;

    /// Sends a notification; no response is expected.
    fn send_notification(&self, notification: JsonRpcNotification) -> TransportFuture<'_, ()>;

    /// Receives the next server-initiated notification (e.g. `notifications/progress`), or
    /// `None` once the transport has shut down.
    fn recv_notification(&self) -> TransportFuture<'_, Option<JsonRpcNotification>>;
}
