//! `McpClient`: the `initialize` handshake plus `tools/list` and `tools/call`, over any
//! [`McpTransport`].

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    McpError,
    jsonrpc::{JsonRpcNotification, JsonRpcRequest, RequestId},
    transport::McpTransport,
};

/// MCP protocol revision gocode implements the client side of.
const PROTOCOL_VERSION: &str = "2025-03-26";

/// gocode's self-identification sent during `initialize`.
const CLIENT_NAME: &str = "gocode";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One tool a connected MCP server advertises.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// The content of a completed `tools/call`.
#[derive(Debug, Clone)]
pub struct ToolCallOutcome {
    /// Human/model-readable content blocks the server returned (already-serialized JSON, since
    /// MCP content blocks vary in shape — text, image, embedded resource).
    pub content: Vec<Value>,
    /// Whether the server flagged this as an error result (still a successful RPC call).
    pub is_error: bool,
}

/// A connected MCP server: owns the transport, has completed the `initialize` handshake.
pub struct McpClient<T: McpTransport> {
    transport: T,
    server_name: Option<String>,
}

impl<T: McpTransport> McpClient<T> {
    /// Performs the `initialize` handshake over `transport` and returns a ready client.
    ///
    /// # Errors
    /// Returns an error if the transport fails or the server's `initialize` response can't be
    /// parsed.
    pub async fn connect(transport: T) -> Result<Self, McpError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": CLIENT_NAME,
                "version": CLIENT_VERSION,
            },
        });
        let request = JsonRpcRequest::new(RequestId::Number(0), "initialize", Some(params));
        let response = transport.send_request(request).await?;
        let result = response.into_result("initialize")?;

        let server_name = result
            .get("serverInfo")
            .and_then(|info| info.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string);

        transport
            .send_notification(JsonRpcNotification::new(
                "notifications/initialized",
                Some(json!({})),
            ))
            .await?;

        Ok(Self {
            transport,
            server_name,
        })
    }

    /// The server-reported name from its `initialize` response, if it sent one.
    #[must_use]
    pub fn server_name(&self) -> Option<&str> {
        self.server_name.as_deref()
    }

    /// Lists every tool the server currently advertises.
    ///
    /// # Errors
    /// Returns an error if the transport fails or the server's response is malformed.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let request = JsonRpcRequest::new(RequestId::Number(0), "tools/list", Some(json!({})));
        let response = self.transport.send_request(request).await?;
        let result = response.into_result("tools/list")?;

        let tools = result
            .get("tools")
            .cloned()
            .ok_or_else(|| McpError::Protocol("tools/list response missing 'tools'".into()))?;
        serde_json::from_value(tools)
            .map_err(|error| McpError::Protocol(format!("invalid tools/list payload: {error}")))
    }

    /// Invokes `tool_name` with `arguments` and returns its result content.
    ///
    /// # Errors
    /// Returns an error if the transport fails or the response can't be parsed.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolCallOutcome, McpError> {
        let params = json!({
            "name": tool_name,
            "arguments": arguments,
        });
        let request = JsonRpcRequest::new(RequestId::Number(0), "tools/call", Some(params));
        let response = self.transport.send_request(request).await?;
        let result = response.into_result("tools/call")?;

        let content = result
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Ok(ToolCallOutcome { content, is_error })
    }
}

#[cfg(test)]
mod tests {
    use super::McpClient;
    use crate::transport::stdio::StdioTransport;

    /// A minimal `sh` server: answers `initialize` with a server name, then `tools/list` with
    /// one tool, ignoring the `notifications/initialized` line it receives in between (no
    /// response is expected for it).
    const FAKE_SERVER: &str = r#"
read _init
printf '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"fake-server"}}}\n'
read _initialized_notification
read _list
printf '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}\n'
"#;

    #[tokio::test]
    async fn connects_and_lists_tools() {
        let transport =
            StdioTransport::spawn("sh", &["-c".to_string(), FAKE_SERVER.to_string()], &[])
                .expect("spawn sh");
        let client = McpClient::connect(transport).await.expect("initialize");
        assert_eq!(client.server_name(), Some("fake-server"));

        let tools = client.list_tools().await.expect("tools/list");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }
}
