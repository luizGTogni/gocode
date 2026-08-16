//! JSON-RPC 2.0 message framing for the MCP wire protocol subset gocode speaks:
//! `initialize`, `notifications/initialized`, `tools/list`, `tools/call`, and progress
//! notifications. Deliberately hand-rolled (no external JSON-RPC crate) to match the
//! dependency-light convention used by the rest of the workspace.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version literal every message must carry.
pub const JSONRPC_VERSION: &str = "2.0";

/// Request/response correlation id. MCP servers may use either numbers or strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl From<i64> for RequestId {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
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

/// The error object carried by a failed [`JsonRpcResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A completed reply to a [`JsonRpcRequest`], either a result or an error.
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
    /// Returns [`crate::McpError::Rpc`] if the response carries a JSON-RPC error object.
    pub fn into_result(self, method: &str) -> Result<Value, crate::McpError> {
        if let Some(error) = self.error {
            return Err(crate::McpError::Rpc {
                method: method.to_string(),
                code: error.code,
                message: error.message,
            });
        }
        Ok(self.result.unwrap_or(Value::Null))
    }
}

/// Any single line of the newline-delimited JSON-RPC stream, before it's known whether it's a
/// response, a server-initiated request, or a notification.
#[derive(Debug, Clone)]
pub enum IncomingMessage {
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
    /// A server-to-client request (e.g. `sampling/createMessage`). gocode does not yet answer
    /// these; they are surfaced so a caller can at least log/ignore them explicitly.
    Request(JsonRpcRequest),
}

/// Parses one line of the newline-delimited MCP stdio stream into its message kind.
///
/// # Errors
/// Returns an error if `line` is not valid JSON or does not look like a JSON-RPC 2.0 message.
pub fn parse_incoming(line: &str) -> Result<IncomingMessage, crate::McpError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| crate::McpError::Protocol(format!("invalid JSON-RPC line: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| crate::McpError::Protocol("JSON-RPC message must be an object".into()))?;

    let has_id = object.contains_key("id");
    let has_method = object.contains_key("method");

    if has_id && !has_method {
        let response: JsonRpcResponse = serde_json::from_value(value).map_err(|error| {
            crate::McpError::Protocol(format!("invalid JSON-RPC response: {error}"))
        })?;
        return Ok(IncomingMessage::Response(response));
    }

    if has_method && has_id {
        let request: JsonRpcRequest = serde_json::from_value(value).map_err(|error| {
            crate::McpError::Protocol(format!("invalid JSON-RPC request: {error}"))
        })?;
        return Ok(IncomingMessage::Request(request));
    }

    if has_method {
        let notification: JsonRpcNotification = serde_json::from_value(value).map_err(|error| {
            crate::McpError::Protocol(format!("invalid JSON-RPC notification: {error}"))
        })?;
        return Ok(IncomingMessage::Notification(notification));
    }

    Err(crate::McpError::Protocol(
        "JSON-RPC message has neither method nor id".into(),
    ))
}

impl std::fmt::Display for IncomingMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IncomingMessage::Response(_) => write!(formatter, "response"),
            IncomingMessage::Notification(notification) => {
                write!(formatter, "notification({})", notification.method)
            }
            IncomingMessage::Request(request) => {
                write!(formatter, "request({})", request.method)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IncomingMessage, JsonRpcRequest, RequestId, parse_incoming};

    #[test]
    fn round_trips_a_request() {
        let request = JsonRpcRequest::new(
            RequestId::Number(1),
            "tools/list",
            Some(serde_json::json!({})),
        );
        let line = serde_json::to_string(&request).unwrap();
        assert_eq!(
            line,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#
        );
    }

    #[test]
    fn parses_a_success_response() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        match parse_incoming(line).unwrap() {
            IncomingMessage::Response(response) => {
                assert_eq!(response.id, RequestId::Number(1));
                assert!(response.error.is_none());
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn parses_an_error_response() {
        let line = r#"{"jsonrpc":"2.0","id":"a","error":{"code":-32601,"message":"not found"}}"#;
        match parse_incoming(line).unwrap() {
            IncomingMessage::Response(response) => {
                let error = response.error.expect("error present");
                assert_eq!(error.code, -32601);
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_notification() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#;
        match parse_incoming(line).unwrap() {
            IncomingMessage::Notification(notification) => {
                assert_eq!(notification.method, "notifications/progress");
            }
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_incoming("not json").is_err());
    }

    #[test]
    fn rejects_message_without_method_or_id() {
        assert!(parse_incoming(r#"{"jsonrpc":"2.0"}"#).is_err());
    }
}
