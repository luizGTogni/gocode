use std::fmt::Write as _;

use serde::Deserialize;

use crate::{
    contract::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolMetadata, ToolName,
        ToolOutput, ToolResult, ToolStatus,
    },
    permissions::PermissionAction,
    tools::parse_args,
    workspace::discover,
};

/// Discovers project files and directories without loading their contents.
pub struct ListFilesTool;

#[derive(Debug, Deserialize)]
struct Input {
    #[serde(default = "default_path")]
    path: String,
    #[serde(default = "default_depth")]
    depth: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_path() -> String {
    ".".into()
}
const fn default_depth() -> usize {
    2
}
const fn default_limit() -> usize {
    200
}

impl Tool for ListFilesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("list_files"),
            description: "List files and directories inside the current project, respecting \
                .gitignore. Use this to orient before reading or searching. Does not load file \
                contents."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Project-relative directory to list, default \".\""},
                    "depth": {"type": "integer", "minimum": 1, "description": "Maximum directory depth, default 2"},
                    "limit": {"type": "integer", "minimum": 1, "description": "Maximum entries returned, default 200"}
                },
                "additionalProperties": false
            }),
        }
    }

    fn execute(&self, ctx: ToolContext, input: serde_json::Value) -> ToolFuture<'_> {
        Box::pin(async move {
            ctx.permissions
                .authorize(PermissionAction::ReadOnly)
                .await
                .map_err(|reason| ToolError::PermissionDenied(reason.0))?;

            let args: Input = parse_args(input)?;
            let entries = discover(&ctx.project_root, &args.path, Some(args.depth), args.limit)?;

            let truncated = entries.len() >= args.limit;
            let mut content = String::new();
            for entry in &entries {
                if entry.is_dir {
                    content.push_str(&entry.relative_path);
                    content.push('/');
                } else {
                    content.push_str(&entry.relative_path);
                }
                content.push('\n');
            }
            if content.is_empty() {
                content.push_str("(no entries)\n");
            }
            if truncated {
                let _ = writeln!(content, "[Output truncated at {} entries.]", args.limit);
            }

            let output = ToolOutput { content, truncated };
            Ok(ToolResult {
                call_id: ctx.call_id.clone(),
                status: ToolStatus::Success,
                metadata: ToolMetadata {
                    truncated,
                    ..ToolMetadata::default()
                },
                output,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ListFilesTool;
    use crate::contract::{Tool, ToolCallId, ToolContext, ToolStatus};
    use crate::permissions::PermissionContext;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn fixture(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gocode-list-files-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn lists_project_relative_entries() {
        let root = fixture("basic");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();

        let ctx = ToolContext::new(
            ToolCallId::new("call-1"),
            root.clone(),
            PermissionContext::read_only_default(),
        );
        let result = ListFilesTool
            .execute(ctx, serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.output.content.contains("Cargo.toml"));
        assert!(result.output.content.contains("src/"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_invalid_argument_types() {
        let root = fixture("invalid-args");
        let ctx = ToolContext::new(
            ToolCallId::new("call-1"),
            root.clone(),
            PermissionContext::read_only_default(),
        );

        let error = ListFilesTool
            .execute(ctx, serde_json::json!({"depth": "not-a-number"}))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            crate::contract::ToolError::InvalidArguments(_)
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
