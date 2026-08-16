use std::fmt::Write as _;

use serde::Deserialize;

use crate::{
    contract::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolMetadata, ToolName,
        ToolOutput, ToolResult, ToolStatus,
    },
    patch,
    permissions::PermissionAction,
    tools::parse_args,
};

/// Applies minimal, targeted edits to existing (or new) text files using a `*** Begin Patch`
/// document. This is the preferred editing tool over `write_file` for existing files.
pub struct ApplyPatchTool;

#[derive(Debug, Deserialize)]
struct Input {
    patch: String,
}

impl Tool for ApplyPatchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("apply_patch"),
            description: "Apply a minimal, targeted patch to one or more files inside the \
                current project. Preferred over write_file for editing an existing file. The \
                patch must use `*** Begin Patch` / `*** Update File:` / `*** Add File:` / \
                `*** Delete File:` / `*** End Patch` sections with ' ', '-', and '+' prefixed \
                lines. Fails safely if the file no longer matches the expected context; read the \
                file again and retry in that case."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": {"type": "string"}
                },
                "required": ["patch"],
                "additionalProperties": false
            }),
        }
    }

    fn execute(&self, ctx: ToolContext, input: serde_json::Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: Input = parse_args(input)?;

            ctx.permissions
                .authorize(PermissionAction::Write {
                    path: ctx.project_root.clone(),
                })
                .await
                .map_err(|reason| ToolError::PermissionDenied(reason.0))?;

            let changes = patch::apply_patch(&ctx.project_root, &args.patch)?;

            let mut content = String::from("Applied patch:\n");
            for change in &changes {
                let _ = writeln!(content, "  {:?} {}", change.kind, change.path.display());
            }

            Ok(ToolResult {
                call_id: ctx.call_id.clone(),
                status: ToolStatus::Success,
                metadata: ToolMetadata {
                    affected_files: changes.clone(),
                    ..ToolMetadata::default()
                },
                output: ToolOutput::new(content),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ApplyPatchTool;
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
            "gocode-apply-patch-tool-{name}-{}-{nanos}",
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
    async fn applies_a_valid_patch_and_reports_the_change() {
        let root = fixture("apply");
        fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: a.txt\n one\n-two\n+TWO\n*** End Patch";

        let result = ApplyPatchTool
            .execute(editing_ctx(&root), serde_json::json!({"patch": patch}))
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.output.content.contains("Modified"));
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn denied_without_editing_intent() {
        let root = fixture("denied");
        fs::write(root.join("a.txt"), "one\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: a.txt\n-one\n+ONE\n*** End Patch";

        let ctx = ToolContext::new(
            ToolCallId::new("call-1"),
            root.clone(),
            PermissionContext::read_only_default(),
        );
        let error = ApplyPatchTool
            .execute(ctx, serde_json::json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::PermissionDenied(_)));
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn invalid_patch_is_reported_as_invalid_arguments() {
        let root = fixture("invalid");

        let error = ApplyPatchTool
            .execute(editing_ctx(&root), serde_json::json!({"patch": "garbage"}))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));

        fs::remove_dir_all(root).unwrap();
    }
}
