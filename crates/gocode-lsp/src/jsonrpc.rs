//! JSON-RPC 2.0 message types for the Language Server Protocol. Distinct from
//! `gocode-mcp::jsonrpc` because LSP frames messages with `Content-Length` headers rather than
//! newline-delimited JSON; the message shapes are otherwise the same JSON-RPC 2.0 envelope.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC_VERSION: &str = "2.0";

/// Request/response correlation id. LSP servers may use either numbers or strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// An outgoing or incoming JSON-RPC call awaiting a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    #[must_use]
    pub fn new(id: RequestId, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A fire-and-forget JSON-RPC message; carries no id and expects no response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    #[must_use]
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Converts this response into a `Result`, using the request's method name in the error
    /// message so a caller can tell which call failed.
    ///
    /// # Errors
    /// Returns [`crate::LspError::Rpc`] if the response carries a JSON-RPC error object.
    pub fn into_result(self, method: &str) -> Result<Value, crate::LspError> {
        if let Some(error) = self.error {
            return Err(crate::LspError::Rpc {
                method: method.to_string(),
                code: error.code,
                message: error.message,
            });
        }
        Ok(self.result.unwrap_or(Value::Null))
    }
}

/// Any single decoded message, before it's known whether it's a response, a server-initiated
/// request, or a notification.
#[derive(Debug, Clone)]
pub enum IncomingMessage {
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
    /// A server-to-client request (e.g. `workspace/configuration`). gocode does not yet answer
    /// these; they are surfaced so a caller can at least log/ignore them explicitly.
    Request(JsonRpcRequest),
}

/// Parses one decoded JSON-RPC body (already stripped of `Content-Length` framing) into its
/// message kind.
///
/// # Errors
/// Returns an error if `body` is not valid JSON or does not look like a JSON-RPC 2.0 message.
pub fn parse_incoming(body: &str) -> Result<IncomingMessage, crate::LspError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| crate::LspError::Protocol(format!("invalid JSON-RPC body: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| crate::LspError::Protocol("JSON-RPC message must be an object".into()))?;

    let has_id = object.contains_key("id");
    let has_method = object.contains_key("method");

    if has_id && !has_method {
        let response: JsonRpcResponse = serde_json::from_value(value).map_err(|error| {
            crate::LspError::Protocol(format!("invalid JSON-RPC response: {error}"))
        })?;
        return Ok(IncomingMessage::Response(response));
    }

    if has_method && has_id {
        let request: JsonRpcRequest = serde_json::from_value(value).map_err(|error| {
            crate::LspError::Protocol(format!("invalid JSON-RPC request: {error}"))
        })?;
        return Ok(IncomingMessage::Request(request));
    }

    if has_method {
        let notification: JsonRpcNotification = serde_json::from_value(value).map_err(|error| {
            crate::LspError::Protocol(format!("invalid JSON-RPC notification: {error}"))
        })?;
        return Ok(IncomingMessage::Notification(notification));
    }

    Err(crate::LspError::Protocol(
        "JSON-RPC message has neither method nor id".into(),
    ))
}

/// Encodes a serializable JSON-RPC message with the `Content-Length` header LSP requires.
///
/// # Errors
/// Returns an error if `message` cannot be serialized to JSON.
pub fn encode_framed(message: &impl Serialize) -> Result<Vec<u8>, crate::LspError> {
    let body = serde_json::to_vec(message)
        .map_err(|error| crate::LspError::Protocol(format!("failed to encode message: {error}")))?;
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(&body);
    Ok(framed)
}

#[cfg(test)]
mod tests {
    use super::{IncomingMessage, JsonRpcRequest, RequestId, encode_framed, parse_incoming};

    #[test]
    fn round_trips_a_request() {
        let request = JsonRpcRequest::new(
            RequestId::Number(1),
            "initialize",
            Some(serde_json::json!({})),
        );
        let body = serde_json::to_string(&request).unwrap();
        match parse_incoming(&body).unwrap() {
            IncomingMessage::Request(parsed) => assert_eq!(parsed.method, "initialize"),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_success_response() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#;
        match parse_incoming(body).unwrap() {
            IncomingMessage::Response(response) => {
                assert_eq!(response.id, RequestId::Number(1));
                assert!(response.error.is_none());
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_notification() {
        let body = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{}}"#;
        match parse_incoming(body).unwrap() {
            IncomingMessage::Notification(notification) => {
                assert_eq!(notification.method, "textDocument/publishDiagnostics");
            }
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_incoming("not json").is_err());
    }

    #[test]
    fn encodes_content_length_header() {
        let request = JsonRpcRequest::new(RequestId::Number(0), "ping", None);
        let framed = encode_framed(&request).unwrap();
        let text = String::from_utf8(framed).unwrap();
        assert!(text.starts_with("Content-Length: "));
        assert!(text.contains("\r\n\r\n"));
    }
}
