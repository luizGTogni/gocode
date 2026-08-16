//! Stdio transport: spawns the MCP server as a subprocess and speaks newline-delimited
//! JSON-RPC over its stdin/stdout, per the MCP stdio transport spec.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex as AsyncMutex, mpsc, oneshot},
    task::JoinHandle,
};

use super::{McpTransport, TransportFuture};
use crate::{
    McpError,
    jsonrpc::{
        IncomingMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
        parse_incoming,
    },
};

type PendingMap = Arc<Mutex<HashMap<RequestId, oneshot::Sender<JsonRpcResponse>>>>;

/// A live stdio-backed MCP server connection.
pub struct StdioTransport {
    stdin: AsyncMutex<ChildStdin>,
    pending: PendingMap,
    notifications: AsyncMutex<mpsc::UnboundedReceiver<JsonRpcNotification>>,
    next_id: AtomicI64,
    _child: Child,
    reader_task: JoinHandle<()>,
}

impl StdioTransport {
    /// Spawns `command` with `args`/`env` and starts reading its JSON-RPC stdout in the
    /// background. The child is killed when the returned transport is dropped.
    ///
    /// # Errors
    /// Returns an error if the process cannot be spawned or its stdio pipes are unavailable.
    ///
    /// # Panics
    /// Panics only if the internal pending-requests mutex is poisoned by an earlier panic.
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .envs(
                env.iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|error| {
            McpError::Transport(format!("failed to spawn '{command}': {error}"))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child process has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child process has no stdout".into()))?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();

        let reader_pending = Arc::clone(&pending);
        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let Ok(Some(line)) = lines.next_line().await else {
                    break;
                };
                if line.trim().is_empty() {
                    continue;
                }
                match parse_incoming(&line) {
                    Ok(IncomingMessage::Response(response)) => {
                        let sender = reader_pending.lock().unwrap().remove(&response.id);
                        if let Some(sender) = sender {
                            let _ = sender.send(response);
                        }
                    }
                    Ok(IncomingMessage::Notification(notification)) => {
                        let _ = notification_tx.send(notification);
                    }
                    // Server-initiated requests (e.g. `sampling/createMessage`) aren't answered
                    // yet; nothing in this codebase issues them today.
                    Ok(IncomingMessage::Request(_)) | Err(_) => {}
                }
            }
        });

        Ok(Self {
            stdin: AsyncMutex::new(stdin),
            pending,
            notifications: AsyncMutex::new(notification_rx),
            next_id: AtomicI64::new(1),
            _child: child,
            reader_task,
        })
    }

    fn next_request_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

impl McpTransport for StdioTransport {
    fn send_request(&self, mut request: JsonRpcRequest) -> TransportFuture<'_, JsonRpcResponse> {
        Box::pin(async move {
            request.id = self.next_request_id();
            let (response_tx, response_rx) = oneshot::channel();
            self.pending
                .lock()
                .unwrap()
                .insert(request.id.clone(), response_tx);

            let mut line = serde_json::to_string(&request).map_err(|error| {
                McpError::Protocol(format!("failed to encode request: {error}"))
            })?;
            line.push('\n');

            {
                let mut stdin = self.stdin.lock().await;
                if let Err(error) = stdin.write_all(line.as_bytes()).await {
                    self.pending.lock().unwrap().remove(&request.id);
                    return Err(McpError::Transport(format!(
                        "failed to write request: {error}"
                    )));
                }
            }

            response_rx.await.map_err(|_| {
                McpError::Transport("server closed the connection before responding".into())
            })
        })
    }

    fn send_notification(&self, notification: JsonRpcNotification) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            let mut line = serde_json::to_string(&notification).map_err(|error| {
                McpError::Protocol(format!("failed to encode notification: {error}"))
            })?;
            line.push('\n');
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await.map_err(|error| {
                McpError::Transport(format!("failed to write notification: {error}"))
            })
        })
    }

    fn recv_notification(&self) -> TransportFuture<'_, Option<JsonRpcNotification>> {
        Box::pin(async move { Ok(self.notifications.lock().await.recv().await) })
    }
}

#[cfg(test)]
mod tests {
    use super::StdioTransport;
    use crate::{
        jsonrpc::{JsonRpcRequest, RequestId},
        transport::McpTransport,
    };

    /// A tiny `sh` one-liner that plays MCP server for a single `ping` call: it reads one
    /// JSON-RPC line and echoes back a canned success response, ignoring the request's actual
    /// content. Good enough to exercise framing without a real MCP server dependency.
    const ECHO_SCRIPT: &str =
        r#"read line; printf '{"jsonrpc":"2.0","id":1,"result":{"ok":true}}\n'"#;

    #[tokio::test]
    async fn round_trips_a_request_over_a_subprocess() {
        let transport =
            StdioTransport::spawn("sh", &["-c".to_string(), ECHO_SCRIPT.to_string()], &[])
                .expect("spawn sh");

        let request = JsonRpcRequest::new(RequestId::Number(0), "ping", None);
        let response = transport.send_request(request).await.expect("response");

        assert_eq!(response.result, Some(serde_json::json!({"ok": true})));
    }

    #[tokio::test]
    async fn surfaces_a_transport_error_for_a_missing_command() {
        let result = StdioTransport::spawn("definitely-not-a-real-binary", &[], &[]);
        assert!(result.is_err());
    }
}
