//! Exposes the language server queries the agent needs — `hover`, `definition`, `references`,
//! `document_symbol`, `diagnostics` — as a single model-facing tool, so the agent can navigate
//! code semantically instead of relying solely on `search`/grep.

use std::sync::Arc;

use gocode_tools::{
    contract::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolName, ToolOutput, ToolResult,
    },
    permissions::PermissionAction,
    workspace::resolve_workspace_path,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{LspError, LspManager};

/// The `lsp` tool: a thin, read-only façade over [`LspManager`].
pub struct LspTool {
    manager: Arc<LspManager>,
}

impl LspTool {
    #[must_use]
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Command {
    Hover,
    Definition,
    References,
    DocumentSymbol,
    Diagnostics,
}

#[derive(Debug, Deserialize)]
struct Input {
    command: Command,
    path: String,
    line: Option<u32>,
    character: Option<u32>,
}

fn lsp_error_to_tool_error(error: LspError) -> ToolError {
    match error {
        LspError::Unsupported(message) => ToolError::UnsupportedFileType(message),
        other => ToolError::Internal(other.to_string()),
    }
}

impl Tool for LspTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("lsp"),
            description: "Query semantic information about a source file from its language \
                server: hover (type/doc info at a position), definition (where a symbol is \
                declared), references (every use of a symbol), document_symbol (outline of a \
                file), or diagnostics (compiler/linter errors and warnings currently known for \
                the file). Prefer this over grep when you need to know what a symbol actually \
                resolves to, or whether an edit introduced a type error. line/character are \
                0-based and required for hover/definition/references; not needed for \
                document_symbol/diagnostics."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": ["hover", "definition", "references", "document_symbol", "diagnostics"]
                    },
                    "path": {"type": "string", "description": "Project-relative file path"},
                    "line": {"type": "integer", "minimum": 0},
                    "character": {"type": "integer", "minimum": 0}
                },
                "required": ["command", "path"],
                "additionalProperties": false
            }),
        }
    }

    fn execute(&self, ctx: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            ctx.permissions
                .authorize(PermissionAction::ReadOnly)
                .await
                .map_err(|reason| ToolError::PermissionDenied(reason.0))?;

            let args: Input = serde_json::from_value(input)
                .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
            let resolved = resolve_workspace_path(&ctx.project_root, &args.path)?;
            let relative = args.path.as_str();

            let content = match args.command {
                Command::Diagnostics => {
                    let client = self
                        .manager
                        .client_for(std::path::Path::new(relative))
                        .await
                        .map_err(lsp_error_to_tool_error)?;
                    let diagnostics = client.cached_diagnostics(&resolved);
                    if diagnostics.is_empty() {
                        "No diagnostics currently known for this file.".to_string()
                    } else {
                        diagnostics
                            .iter()
                            .map(|d| {
                                format!(
                                    "{}:{} [{}] {}{}",
                                    d.line + 1,
                                    d.character + 1,
                                    d.severity.as_deref().unwrap_or("info"),
                                    d.message,
                                    d.source
                                        .as_ref()
                                        .map(|s| format!(" ({s})"))
                                        .unwrap_or_default()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                }
                Command::DocumentSymbol => {
                    let client = self
                        .manager
                        .client_for(std::path::Path::new(relative))
                        .await
                        .map_err(lsp_error_to_tool_error)?;
                    let result = client
                        .document_symbol(&resolved)
                        .await
                        .map_err(lsp_error_to_tool_error)?;
                    serde_json::to_string_pretty(&result)
                        .map_err(|error| ToolError::Internal(error.to_string()))?
                }
                Command::Hover | Command::Definition | Command::References => {
                    let (Some(line), Some(character)) = (args.line, args.character) else {
                        return Err(ToolError::InvalidArguments(
                            "line and character are required for hover/definition/references"
                                .into(),
                        ));
                    };
                    let client = self
                        .manager
                        .client_for(std::path::Path::new(relative))
                        .await
                        .map_err(lsp_error_to_tool_error)?;
                    let result = match args.command {
                        Command::Hover => client.hover(&resolved, line, character).await,
                        Command::Definition => client.definition(&resolved, line, character).await,
                        Command::References => client.references(&resolved, line, character).await,
                        Command::DocumentSymbol | Command::Diagnostics => unreachable!(),
                    }
                    .map_err(lsp_error_to_tool_error)?;
                    serde_json::to_string_pretty(&result)
                        .map_err(|error| ToolError::Internal(error.to_string()))?
                }
            };

            Ok(ToolResult::success(ctx.call_id, ToolOutput::new(content)))
        })
    }
}
