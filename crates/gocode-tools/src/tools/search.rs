use std::fmt::Write as _;

use serde::Deserialize;

use crate::{
    contract::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolMetadata, ToolName,
        ToolOutput, ToolResult, ToolStatus,
    },
    permissions::PermissionAction,
    tools::{check_cancelled, glob_match, parse_args},
    workspace::{discover, looks_binary, resolve_workspace_path},
};

/// Maximum bytes read per candidate file while searching for matches.
const MAX_SEARCHED_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Searches project text for a literal query before reading whole files.
pub struct SearchTool;

#[derive(Debug, Deserialize)]
struct Input {
    query: String,
    #[serde(default = "default_path")]
    path: String,
    glob: Option<String>,
    #[serde(default = "default_max_results")]
    max_results: usize,
}

fn default_path() -> String {
    ".".into()
}
const fn default_max_results() -> usize {
    50
}

impl Tool for SearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("search"),
            description: "Search project text for a literal query, respecting .gitignore and \
                skipping binary files. Returns file:line matches, not whole files. Prefer this \
                before read_file when you don't yet know which file holds the relevant code."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "path": {"type": "string", "description": "Project-relative directory to search, default \".\""},
                    "glob": {"type": "string", "description": "Optional filename filter, e.g. \"*.rs\""},
                    "max_results": {"type": "integer", "minimum": 1, "description": "Default 50"}
                },
                "required": ["query"],
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
            if args.query.is_empty() {
                return Err(ToolError::InvalidArguments(
                    "query must not be empty".into(),
                ));
            }

            let entries = discover(&ctx.project_root, &args.path, None, usize::MAX)?;
            let mut matches = Vec::new();

            for entry in entries {
                if entry.is_dir {
                    continue;
                }
                if let Some(glob) = &args.glob {
                    let file_name = entry
                        .relative_path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&entry.relative_path);
                    if !glob_match(glob, file_name) && !glob_match(glob, &entry.relative_path) {
                        continue;
                    }
                }

                check_cancelled(&ctx)?;

                let resolved = resolve_workspace_path(&ctx.project_root, &entry.relative_path)?;
                let Ok(metadata) = std::fs::metadata(&resolved) else {
                    continue;
                };
                if metadata.len() > MAX_SEARCHED_FILE_BYTES {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&resolved) else {
                    continue;
                };
                if looks_binary(&bytes) {
                    continue;
                }
                let Ok(text) = String::from_utf8(bytes) else {
                    continue;
                };

                for (line_index, line) in text.lines().enumerate() {
                    if line.contains(&args.query) {
                        matches.push(format!(
                            "{}:{}: {}",
                            entry.relative_path,
                            line_index + 1,
                            line.trim()
                        ));
                        if matches.len() >= args.max_results {
                            break;
                        }
                    }
                }
                if matches.len() >= args.max_results {
                    break;
                }
            }

            let truncated = matches.len() >= args.max_results;
            let content = if matches.is_empty() {
                format!("No matches found for `{}`.", args.query)
            } else {
                let mut content = matches.join("\n");
                if truncated {
                    let _ = write!(
                        content,
                        "\n[Results truncated at {} matches.]",
                        args.max_results
                    );
                }
                content
            };

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
    use super::SearchTool;
    use crate::contract::{Tool, ToolCallId, ToolContext, ToolStatus};
    use crate::permissions::PermissionContext;
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
            "gocode-search-{name}-{}-{nanos}",
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
    async fn finds_matches_with_file_and_line() {
        let root = fixture("basic");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/auth.rs"), "fn validate_token() {}\n").unwrap();

        let result = SearchTool
            .execute(ctx(&root), serde_json::json!({"query": "validate_token"}))
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.output.content.contains("src/auth.rs:1:"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn no_matches_is_still_a_success() {
        let root = fixture("no-match");
        fs::write(root.join("a.txt"), "nothing relevant\n").unwrap();

        let result = SearchTool
            .execute(ctx(&root), serde_json::json!({"query": "not_present"}))
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.output.content.starts_with("No matches"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn glob_filter_restricts_searched_files() {
        let root = fixture("glob");
        fs::write(root.join("a.rs"), "needle\n").unwrap();
        fs::write(root.join("a.py"), "needle\n").unwrap();

        let result = SearchTool
            .execute(
                ctx(&root),
                serde_json::json!({"query": "needle", "glob": "*.rs"}),
            )
            .await
            .unwrap();

        assert!(result.output.content.contains("a.rs"));
        assert!(!result.output.content.contains("a.py"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn binary_files_are_skipped() {
        let root = fixture("binary");
        fs::write(
            root.join("bin.dat"),
            [0u8, 1, b'n', b'e', b'e', b'd', b'l', b'e'],
        )
        .unwrap();
        fs::write(root.join("text.txt"), "needle\n").unwrap();

        let result = SearchTool
            .execute(ctx(&root), serde_json::json!({"query": "needle"}))
            .await
            .unwrap();

        assert!(!result.output.content.contains("bin.dat"));
        assert!(result.output.content.contains("text.txt"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn results_are_capped_at_max_results() {
        let root = fixture("cap");
        let content: String = (0..10).map(|_| "needle\n").collect();
        fs::write(root.join("a.txt"), content).unwrap();

        let result = SearchTool
            .execute(
                ctx(&root),
                serde_json::json!({"query": "needle", "max_results": 3}),
            )
            .await
            .unwrap();

        assert!(result.output.truncated);

        fs::remove_dir_all(root).unwrap();
    }
}
