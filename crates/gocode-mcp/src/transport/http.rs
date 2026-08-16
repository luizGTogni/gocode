//! Streamable HTTP transport (MCP's 2025-03-26 transport spec): POST JSON-RPC requests to a
//! single endpoint URL and read back either a single `application/json` response or a
//! `text/event-stream` of JSON-RPC messages, the last of which is the matching response.

use std::sync::atomic::{AtomicI64, Ordering};

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use super::{McpTransport, TransportFuture};
use crate::{
    McpError,
    jsonrpc::{
        IncomingMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
        parse_incoming,
    },
};

/// Session header MCP servers may return on `initialize` and expect echoed back on every
/// subsequent request, per the streamable-HTTP transport spec.
const SESSION_HEADER: &str = "Mcp-Session-Id";

/// A live streamable-HTTP MCP server connection.
pub struct HttpTransport {
    client: reqwest::Client,
    url: reqwest::Url,
    headers: HeaderMap,
    session_id: AsyncMutex<Option<String>>,
    next_id: AtomicI64,
    notification_tx: mpsc::UnboundedSender<JsonRpcNotification>,
    notification_rx: AsyncMutex<mpsc::UnboundedReceiver<JsonRpcNotification>>,
}

impl HttpTransport {
    /// Builds a transport for the MCP endpoint at `url`, sending `headers` (e.g. an
    /// `Authorization` bearer token) on every request.
    ///
    /// # Errors
    /// Returns an error if `url` does not parse, or any header name/value is invalid.
    pub fn new(url: &str, headers: &[(String, String)]) -> Result<Self, McpError> {
        let url = reqwest::Url::parse(url).map_err(|error| {
            McpError::Transport(format!("invalid MCP server URL '{url}': {error}"))
        })?;

        let mut header_map = HeaderMap::new();
        for (name, value) in headers {
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                McpError::Transport(format!("invalid header name '{name}': {error}"))
            })?;
            let header_value = HeaderValue::from_str(value).map_err(|error| {
                McpError::Transport(format!("invalid header value for '{name}': {error}"))
            })?;
            header_map.insert(header_name, header_value);
        }

        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        Ok(Self {
            client: reqwest::Client::new(),
            url,
            headers: header_map,
            session_id: AsyncMutex::new(None),
            next_id: AtomicI64::new(1),
            notification_tx,
            notification_rx: AsyncMutex::new(notification_rx),
        })
    }

    fn next_request_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// POSTs `body` and, when `target_id` is set, waits for the JSON-RPC response matching it
    /// (parsing either a direct JSON body or an SSE event stream). Notifications observed along
    /// the way are forwarded to [`Self::recv_notification`] instead of being discarded.
    async fn post(
        &self,
        body: Vec<u8>,
        target_id: Option<&RequestId>,
    ) -> Result<Option<JsonRpcResponse>, McpError> {
        let mut request = self
            .client
            .post(self.url.clone())
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(body);

        if let Some(session_id) = self.session_id.lock().await.clone() {
            request = request.header(SESSION_HEADER, session_id);
        }

        let response = request
            .send()
            .await
            .map_err(|error| McpError::Transport(format!("HTTP request failed: {error}")))?;

        if let Some(session_id) = response.headers().get(SESSION_HEADER)
            && let Ok(session_id) = session_id.to_str()
        {
            *self.session_id.lock().await = Some(session_id.to_string());
        }

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!(
                "MCP server responded with {status}: {text}"
            )));
        }

        let Some(target_id) = target_id else {
            return Ok(None);
        };

        let is_event_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));

        if is_event_stream {
            self.read_sse_for_response(response, target_id).await
        } else {
            let bytes = response.bytes().await.map_err(|error| {
                McpError::Transport(format!("failed to read response body: {error}"))
            })?;
            let text = String::from_utf8_lossy(&bytes);
            match parse_incoming(text.trim())? {
                IncomingMessage::Response(response) => Ok(Some(response)),
                other => Err(McpError::Protocol(format!(
                    "expected a JSON-RPC response, got a {other}"
                ))),
            }
        }
    }

    /// Reads an SSE response body event-by-event, forwarding any notification and returning
    /// once the response matching `target_id` arrives.
    async fn read_sse_for_response(
        &self,
        response: reqwest::Response,
        target_id: &RequestId,
    ) -> Result<Option<JsonRpcResponse>, McpError> {
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|error| McpError::Transport(format!("SSE stream error: {error}")))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(boundary) = buffer.find("\n\n") {
                let event: String = buffer.drain(..=boundary + 1).collect();
                for line in event.lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    match parse_incoming(data.trim())? {
                        IncomingMessage::Response(response) if &response.id == target_id => {
                            return Ok(Some(response));
                        }
                        IncomingMessage::Response(_) | IncomingMessage::Request(_) => {}
                        IncomingMessage::Notification(notification) => {
                            let _ = self.notification_tx.send(notification);
                        }
                    }
                }
            }
        }
        Err(McpError::Transport(
            "SSE stream ended before the matching response arrived".into(),
        ))
    }
}

impl McpTransport for HttpTransport {
    fn send_request(&self, mut request: JsonRpcRequest) -> TransportFuture<'_, JsonRpcResponse> {
        Box::pin(async move {
            request.id = self.next_request_id();
            let target_id = request.id.clone();
            let body = serde_json::to_vec(&request).map_err(|error| {
                McpError::Protocol(format!("failed to encode request: {error}"))
            })?;
            self.post(body, Some(&target_id)).await?.ok_or_else(|| {
                McpError::Transport("server accepted the request but returned no response".into())
            })
        })
    }

    fn send_notification(&self, notification: JsonRpcNotification) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            let body = serde_json::to_vec(&notification).map_err(|error| {
                McpError::Protocol(format!("failed to encode notification: {error}"))
            })?;
            self.post(body, None).await?;
            Ok(())
        })
    }

    fn recv_notification(&self) -> TransportFuture<'_, Option<JsonRpcNotification>> {
        Box::pin(async move { Ok(self.notification_rx.lock().await.recv().await) })
    }
}

#[cfg(test)]
mod tests {
    use super::HttpTransport;
    use crate::{
        jsonrpc::{JsonRpcRequest, RequestId},
        transport::McpTransport,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    /// Spawns a minimal single-request HTTP/1.1 server that always replies with `body` as a
    /// `200 application/json` response, and returns the URL to reach it. Good enough to test
    /// request framing and response parsing without a real MCP server or a mocking crate.
    async fn spawn_json_server(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0_u8; 4096];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}/mcp")
    }

    #[tokio::test]
    async fn sends_a_request_and_parses_a_direct_json_response() {
        let url = spawn_json_server(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).await;
        let transport = HttpTransport::new(&url, &[]).expect("build transport");

        let request = JsonRpcRequest::new(RequestId::Number(0), "ping", None);
        let response = transport.send_request(request).await.expect("response");

        assert_eq!(response.result, Some(serde_json::json!({"ok": true})));
    }

    #[tokio::test]
    async fn rejects_an_invalid_url() {
        assert!(HttpTransport::new("not a url", &[]).is_err());
    }

    #[tokio::test]
    async fn rejects_an_invalid_header_name() {
        let result = HttpTransport::new(
            "http://127.0.0.1:1/mcp",
            &[("bad header".to_string(), "value".to_string())],
        );
        assert!(result.is_err());
    }
}
