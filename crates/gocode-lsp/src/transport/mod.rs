//! Transport abstraction LSP clients speak over: a stdio subprocess (the only transport gocode
//! needs — every language server it spawns talks stdio).

pub mod stdio;

use std::{future::Future, pin::Pin};

use crate::{
    LspError,
    jsonrpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse},
};

/// A boxed, `Send` future — mirrors the hand-rolled `ToolFuture`/`TransportFuture` convention
/// used elsewhere in the workspace so this crate stays free of an `async-trait` dependency.
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LspError>> + Send + 'a>>;

/// A live channel to a single language server process.
pub trait LspTransport: Send + Sync {
    /// Sends a request and waits for its matching response.
    fn send_request(&self, request: JsonRpcRequest) -> TransportFuture<'_, JsonRpcResponse>;

    /// Sends a notification; no response is expected.
    fn send_notification(&self, notification: JsonRpcNotification) -> TransportFuture<'_, ()>;

    /// Receives the next server-initiated notification (e.g. `textDocument/publishDiagnostics`),
    /// or `None` once the transport has shut down.
    fn recv_notification(&self) -> TransportFuture<'_, Option<JsonRpcNotification>>;
}
