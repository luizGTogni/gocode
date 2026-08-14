use std::fmt::Write as _;

use serde::Deserialize;

use crate::{
    contract::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolMetadata, ToolName,
        ToolOutput, ToolResult, ToolStatus,
    },
    permissions::PermissionAction,
    tools::parse_args,
    workspace::{looks_binary, resolve_workspace_path},
};

/// Maximum lines returned when the model does not request a specific range.
const DEFAULT_MAX_LINES: usize = 300;

/// Reads UTF-8 text content from a specific project file.
pub struct ReadFileTool;

#[derive(Debug, Deserialize)]
struct Input {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("read_file"),
            description: "Read UTF-8 text from a file inside the current project. Use \
                start_line/end_line (1-based) for large files. Do not use this tool for binary \
                files; it will reject them."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Project-relative file path"},
                    "start_line": {"type": "integer", "minimum": 1},
                    "end_line": {"type": "integer", "minimum": 1}
                },
                "required": ["path"],
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
            let resolved = resolve_workspace_path(&ctx.project_root, &args.path)?;

            if resolved.is_dir() {
                return Err(ToolError::InvalidArguments(format!(
                    "{} is a directory, not a file",
                    args.path
                )));
            }
            if !resolved.exists() {
                return Err(ToolError::NotFound(resolved));
            }

            let bytes = std::fs::read(&resolved).map_err(|error| {
                ToolError::Io(format!("could not read {}: {error}", resolved.display()))
            })?;
            if looks_binary(&bytes) {
                return Err(ToolError::UnsupportedFileType(format!(
                    "cannot read `{}` as text because it appears to be a binary file",
                    args.path
                )));
            }
            let text = String::from_utf8(bytes).map_err(|_| {
                ToolError::UnsupportedFileType(format!(
                    "`{}` does not contain valid UTF-8 text",
                    args.path
                ))
            })?;

            let lines: Vec<&str> = text.lines().collect();
            let total = lines.len();
            let start = args.start_line.unwrap_or(1).max(1);
            let has_explicit_range = args.start_line.is_some() || args.end_line.is_some();
            let end = args
                .end_line
                .unwrap_or_else(|| {
                    if has_explicit_range {
                        total
                    } else {
                        (start + DEFAULT_MAX_LINES - 1).min(total)
                    }
                })
                .min(total);

            let mut content = String::new();
            if start > total {
                content.push_str("(the requested range is beyond the end of the file)\n");
            } else {
                for (offset, line) in lines[start - 1..end].iter().enumerate() {
                    let _ = writeln!(content, "{} | {line}", start + offset);
                }
            }

            let truncated = !has_explicit_range && end < total;
            if truncated {
                let _ = writeln!(
                    content,
                    "[Showing lines {start}-{end} of {total}. Request another range to continue.]"
                );
            }

            Ok(ToolResult {
                call_id: ctx.call_id.clone(),
                status: ToolStatus::Success,
                metadata: ToolMetadata {
                    truncated,
                    ..ToolMetadata::default()
                },
                output: ToolOutput { content, truncated },
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ReadFileTool;
    use crate::contract::{Tool, ToolCallId, ToolContext, ToolError, ToolStatus};
    use crate::permissions::PermissionContext;
    use std::fmt::Write as _;
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn fixture(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gocode-read-file-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx(root: &Path) -> ToolContext {
        ToolContext::new(
            ToolCallId::new("call-1"),
            root.to_path_buf(),
            PermissionContext::read_only_default(),
        )
    }

    #[tokio::test]
    async fn reads_a_file_with_line_numbers() {
        let root = fixture("basic");
        fs::write(root.join("a.rs"), "fn main() {}\n").unwrap();

        let result = ReadFileTool
            .execute(ctx(&root), serde_json::json!({"path": "a.rs"}))
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.output.content.starts_with("1 | fn main"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn respects_an_explicit_line_range() {
        let root = fixture("range");
        fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();

        let result = ReadFileTool
            .execute(
                ctx(&root),
                serde_json::json!({"path": "a.txt", "start_line": 2, "end_line": 3}),
            )
            .await
            .unwrap();

        assert!(result.output.content.contains("2 | two"));
        assert!(result.output.content.contains("3 | three"));
        assert!(!result.output.content.contains("four"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_binary_content() {
        let root = fixture("binary");
        fs::write(root.join("bin.dat"), [0u8, 1, 2, 0, 3]).unwrap();

        let error = ReadFileTool
            .execute(ctx(&root), serde_json::json!({"path": "bin.dat"}))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::UnsupportedFileType(_)));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_a_directory_path() {
        let root = fixture("directory");
        fs::create_dir_all(root.join("src")).unwrap();

        let error = ReadFileTool
            .execute(ctx(&root), serde_json::json!({"path": "src"}))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn missing_file_is_reported_as_not_found() {
        let root = fixture("missing");

        let error = ReadFileTool
            .execute(ctx(&root), serde_json::json!({"path": "nope.txt"}))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::NotFound(_)));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn workspace_escape_is_rejected() {
        let root = fixture("escape");

        let error = ReadFileTool
            .execute(ctx(&root), serde_json::json!({"path": "../outside.txt"}))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::OutsideWorkspace(_)));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn large_files_without_a_range_are_truncated_with_guidance() {
        let root = fixture("large");
        let content: String = (1..=500).fold(String::new(), |mut acc, n| {
            let _ = writeln!(acc, "line {n}");
            acc
        });
        fs::write(root.join("big.txt"), content).unwrap();

        let result = ReadFileTool
            .execute(ctx(&root), serde_json::json!({"path": "big.txt"}))
            .await
            .unwrap();

        assert!(result.output.truncated);
        assert!(result.output.content.contains("Showing lines 1-300 of 500"));

        fs::remove_dir_all(root).unwrap();
    }
}
