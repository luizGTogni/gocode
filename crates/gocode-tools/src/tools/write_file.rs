use serde::Deserialize;

use crate::{
    contract::{
        ChangeKind, FileChange, Tool, ToolContext, ToolDefinition, ToolError, ToolFuture,
        ToolMetadata, ToolName, ToolOutput, ToolResult, ToolStatus,
    },
    permissions::PermissionAction,
    tools::parse_args,
    workspace::{atomic_write_file, resolve_workspace_path},
};

/// Creates a new text file or fully replaces an existing one.
pub struct WriteFileTool;

#[derive(Debug, Deserialize)]
struct Input {
    path: String,
    content: String,
}

impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("write_file"),
            description: "Create a new text file or fully replace an existing file's content \
                inside the current project. Prefer apply_patch for targeted edits to an existing \
                file; use write_file for new files or intentional full replacement."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Project-relative file path"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    fn execute(&self, ctx: ToolContext, input: serde_json::Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: Input = parse_args(input)?;
            let resolved = resolve_workspace_path(&ctx.project_root, &args.path)?;

            ctx.permissions
                .authorize(PermissionAction::Write {
                    path: resolved.clone(),
                })
                .await
                .map_err(|reason| ToolError::PermissionDenied(reason.0))?;

            if resolved.is_dir() {
                return Err(ToolError::InvalidArguments(format!(
                    "{} is a directory, not a file",
                    args.path
                )));
            }

            let existed = resolved.exists();
            atomic_write_file(&resolved, &args.content)?;

            let line_count = args.content.lines().count();
            let content = if existed {
                format!("Replaced `{}` ({line_count} lines).", args.path)
            } else {
                format!("Created `{}` ({line_count} lines).", args.path)
            };

            let kind = if existed {
                ChangeKind::Modified
            } else {
                ChangeKind::Created
            };
            Ok(ToolResult {
                call_id: ctx.call_id.clone(),
                status: ToolStatus::Success,
                metadata: ToolMetadata {
                    affected_files: vec![FileChange {
                        path: std::path::PathBuf::from(&args.path),
                        kind,
                    }],
                    ..ToolMetadata::default()
                },
                output: ToolOutput::new(content),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::WriteFileTool;
    use crate::contract::{Tool, ToolCallId, ToolContext, ToolError, ToolStatus};
    use crate::permissions::{DefaultPermissionPolicy, PermissionContext};
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn fixture(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gocode-write-file-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn editing_ctx(root: &Path) -> ToolContext {
        ToolContext::new(
            ToolCallId::new("call-1"),
            root.to_path_buf(),
            PermissionContext::new(
                Arc::new(DefaultPermissionPolicy::editing()),
                Arc::new(crate::permissions::AlwaysDenyResolver),
            ),
        )
    }

    #[tokio::test]
    async fn creates_a_new_file() {
        let root = fixture("create");

        let result = WriteFileTool
            .execute(
                editing_ctx(&root),
                serde_json::json!({"path": "new.rs", "content": "fn main() {}\n"}),
            )
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.output.content.starts_with("Created"));
        assert_eq!(
            fs::read_to_string(root.join("new.rs")).unwrap(),
            "fn main() {}\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn replaces_an_existing_file() {
        let root = fixture("replace");
        fs::write(root.join("a.txt"), "old").unwrap();

        let result = WriteFileTool
            .execute(
                editing_ctx(&root),
                serde_json::json!({"path": "a.txt", "content": "new"}),
            )
            .await
            .unwrap();

        assert!(result.output.content.starts_with("Replaced"));
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "new");

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn denied_without_editing_intent() {
        let root = fixture("denied");
        let ctx = ToolContext::new(
            ToolCallId::new("call-1"),
            root.clone(),
            PermissionContext::read_only_default(),
        );

        let error = WriteFileTool
            .execute(ctx, serde_json::json!({"path": "a.txt", "content": "x"}))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::PermissionDenied(_)));
        assert!(!root.join("a.txt").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn workspace_escape_is_rejected() {
        let root = fixture("escape");

        let error = WriteFileTool
            .execute(
                editing_ctx(&root),
                serde_json::json!({"path": "../outside.txt", "content": "x"}),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::OutsideWorkspace(_)));

        fs::remove_dir_all(root).unwrap();
    }
}
