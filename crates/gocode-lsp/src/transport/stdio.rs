//! Stdio transport: spawns a language server as a subprocess and speaks
//! `Content-Length`-framed JSON-RPC over its stdin/stdout, per the LSP base protocol.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex as AsyncMutex, mpsc, oneshot},
    task::JoinHandle,
};

use super::{LspTransport, TransportFuture};
use crate::{
    LspError,
    jsonrpc::{
        IncomingMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
        encode_framed, parse_incoming,
    },
};

type PendingMap = Arc<Mutex<HashMap<RequestId, oneshot::Sender<JsonRpcResponse>>>>;

/// A live stdio-backed language server connection.
pub struct StdioTransport {
    stdin: AsyncMutex<ChildStdin>,
    pending: PendingMap,
    notifications: AsyncMutex<mpsc::UnboundedReceiver<JsonRpcNotification>>,
    next_id: AtomicI64,
    _child: Child,
    reader_task: JoinHandle<()>,
}

/// Reads one `Content-Length`-framed message body from `reader`, or `None` at EOF.
async fn read_framed_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Option<String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await.ok()?;
        if bytes_read == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().ok();
        }
    }
    let length = content_length?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await.ok()?;
    String::from_utf8(body).ok()
}

impl StdioTransport {
    /// Spawns `command` with `args`/`env` and starts reading its framed JSON-RPC stdout in the
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
    ) -> Result<Self, LspError> {
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
            LspError::Transport(format!("failed to spawn '{command}': {error}"))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Transport("child process has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Transport("child process has no stdout".into()))?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();

        let reader_pending = Arc::clone(&pending);
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let Some(body) = read_framed_message(&mut reader).await else {
                    break;
                };
                match parse_incoming(&body) {
                    Ok(IncomingMessage::Response(response)) => {
                        let sender = reader_pending.lock().unwrap().remove(&response.id);
                        if let Some(sender) = sender {
                            let _ = sender.send(response);
                        }
                    }
                    Ok(IncomingMessage::Notification(notification)) => {
                        let _ = notification_tx.send(notification);
                    }
                    // Server-initiated requests (e.g. `workspace/configuration`) aren't
                    // answered yet; nothing in this codebase issues them today.
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

impl LspTransport for StdioTransport {
    fn send_request(&self, mut request: JsonRpcRequest) -> TransportFuture<'_, JsonRpcResponse> {
        Box::pin(async move {
            request.id = self.next_request_id();
            let (response_tx, response_rx) = oneshot::channel();
            self.pending
                .lock()
                .unwrap()
                .insert(request.id.clone(), response_tx);

            let framed = encode_framed(&request)?;

            {
                let mut stdin = self.stdin.lock().await;
                if let Err(error) = stdin.write_all(&framed).await {
                    self.pending.lock().unwrap().remove(&request.id);
                    return Err(LspError::Transport(format!(
                        "failed to write request: {error}"
                    )));
                }
            }

            response_rx.await.map_err(|_| {
                LspError::Transport("server closed the connection before responding".into())
            })
        })
    }

    fn send_notification(&self, notification: JsonRpcNotification) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            let framed = encode_framed(&notification)?;
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(&framed).await.map_err(|error| {
                LspError::Transport(format!("failed to write notification: {error}"))
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
        transport::LspTransport,
    };

    /// A tiny `sh` one-liner that plays language server for a single request: it ignores
    /// whatever the client writes (the request is small enough to fit in the stdin pipe buffer,
    /// so not draining it doesn't deadlock this single-round-trip test) and replies with a
    /// canned `Content-Length`-framed success response. Good enough to exercise framing without
    /// a real language server dependency.
    const ECHO_SCRIPT: &str = r#"body='{"jsonrpc":"2.0","id":1,"result":{"ok":true}}'; len=${#body}; printf 'Content-Length: %s\r\n\r\n%s' "$len" "$body""#;

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
