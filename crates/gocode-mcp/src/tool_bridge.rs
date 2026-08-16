//! Bridges a remote MCP tool into gocode's [`Tool`] trait, so the model can call it exactly
//! like a built-in tool.

use std::sync::Arc;

use gocode_tools::contract::{
    Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolMetadata, ToolName, ToolOutput,
    ToolResult, ToolStatus,
};
use serde_json::Value;

use crate::{McpClient, McpToolInfo, transport::McpTransport};

/// One MCP server tool, registered under `mcp__<server>__<tool>` to keep every server's tools
/// disjoint from the built-ins and from each other.
pub struct McpTool<T: McpTransport + 'static> {
    client: Arc<McpClient<T>>,
    server_name: String,
    info: McpToolInfo,
}

impl<T: McpTransport + 'static> McpTool<T> {
    #[must_use]
    pub fn new(
        client: Arc<McpClient<T>>,
        server_name: impl Into<String>,
        info: McpToolInfo,
    ) -> Self {
        Self {
            client,
            server_name: server_name.into(),
            info,
        }
    }

    fn qualified_name(&self) -> String {
        format!("mcp__{}__{}", self.server_name, self.info.name)
    }
}

impl<T: McpTransport + 'static> Tool for McpTool<T> {
    fn definition(&self) -> ToolDefinition {
        let description = self.info.description.clone().unwrap_or_else(|| {
            format!(
                "Tool '{}' provided by the '{}' MCP server.",
                self.info.name, self.server_name
            )
        });
        ToolDefinition {
            name: ToolName::new(self.qualified_name()),
            description: format!("[MCP:{}] {description}", self.server_name),
            input_schema: self.info.input_schema.clone(),
        }
    }

    fn execute(&self, ctx: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let outcome = self
                .client
                .call_tool(&self.info.name, input)
                .await
                .map_err(|error| ToolError::Internal(error.to_string()))?;

            let mut content = render_content_blocks(&outcome.content);
            if outcome.is_error {
                content = format!("MCP tool reported an error:\n{content}");
            }

            Ok(ToolResult {
                call_id: ctx.call_id,
                status: ToolStatus::Success,
                metadata: ToolMetadata::default(),
                output: ToolOutput::new(content),
            })
        })
    }
}

/// Renders MCP `tools/call` content blocks as model-facing text. Text blocks are taken
/// verbatim; every other block kind (image, embedded resource, ...) falls back to its raw JSON,
/// since gocode's tool output channel is text-only today.
fn render_content_blocks(blocks: &[Value]) -> String {
    let mut rendered = String::new();
    for block in blocks {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        match block.get("text").and_then(Value::as_str) {
            Some(text) => rendered.push_str(text),
            None => rendered.push_str(&block.to_string()),
        }
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::render_content_blocks;
    use serde_json::json;

    #[test]
    fn renders_text_blocks_verbatim() {
        let blocks = vec![json!({"type": "text", "text": "hello"})];
        assert_eq!(render_content_blocks(&blocks), "hello");
    }

    #[test]
    fn joins_multiple_blocks_with_newlines() {
        let blocks = vec![
            json!({"type": "text", "text": "first"}),
            json!({"type": "text", "text": "second"}),
        ];
        assert_eq!(render_content_blocks(&blocks), "first\nsecond");
    }

    #[test]
    fn falls_back_to_raw_json_for_non_text_blocks() {
        let blocks = vec![json!({"type": "image", "data": "base64=="})];
        assert_eq!(
            render_content_blocks(&blocks),
            r#"{"data":"base64==","type":"image"}"#
        );
    }
}
