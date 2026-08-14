use std::{fmt::Write as _, time::Duration};

use serde::Deserialize;

use crate::{
    contract::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolMetadata, ToolName,
        ToolOutput, ToolResult, ToolStatus,
    },
    permissions::PermissionAction,
    process::{CommandRequest, ProcessError, ProcessRunner, TokioProcessRunner},
    tools::{cap_chars, parse_args},
};

/// Maximum characters retained from a `git diff` before truncation.
const MAX_DIFF_CHARS: usize = 60_000;

fn is_git_repository(project_root: &std::path::Path) -> bool {
    project_root.join(".git").exists()
}

async fn run_git(
    runner: &dyn ProcessRunner,
    project_root: &std::path::Path,
    args: Vec<String>,
) -> Result<crate::process::ProcessResult, ToolError> {
    let request = CommandRequest {
        program: "git".into(),
        args,
        cwd: project_root.to_path_buf(),
        shell: false,
        timeout: Duration::from_secs(30),
    };
    runner
        .run(request, tokio_util::sync::CancellationToken::new(), None)
        .await
        .map_err(|ProcessError::SpawnFailed(message)| {
            ToolError::Internal(format!("could not run git: {message}"))
        })
}

/// Inspects the current Git working tree state without modifying it.
pub struct GitStatusTool;

impl Tool for GitStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("git_status"),
            description: "Show the current Git working tree status (staged, modified, \
                untracked, and deleted files) for the project. Read-only."
                .into(),
            input_schema: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
        }
    }

    fn execute(&self, ctx: ToolContext, input: serde_json::Value) -> ToolFuture<'_> {
        Box::pin(async move {
            ctx.permissions
                .authorize(PermissionAction::ReadOnly)
                .await
                .map_err(|reason| ToolError::PermissionDenied(reason.0))?;
            let _: serde_json::Value = input;

            if !is_git_repository(&ctx.project_root) {
                return Ok(ToolResult::success(
                    ctx.call_id.clone(),
                    ToolOutput::new("This project is not inside a Git repository."),
                ));
            }

            let runner = TokioProcessRunner;
            let process = run_git(
                &runner,
                &ctx.project_root,
                vec!["status".into(), "--porcelain".into()],
            )
            .await?;

            Ok(ToolResult {
                call_id: ctx.call_id.clone(),
                status: ToolStatus::Success,
                metadata: ToolMetadata::default(),
                output: ToolOutput::new(format_status(&process.stdout)),
            })
        })
    }
}

fn format_status(porcelain: &str) -> String {
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();
    let mut untracked = Vec::new();

    for line in porcelain.lines() {
        if line.len() < 3 {
            continue;
        }
        let index_status = line.as_bytes()[0] as char;
        let worktree_status = line.as_bytes()[1] as char;
        let path = &line[3..];

        if index_status == '?' && worktree_status == '?' {
            untracked.push(path);
        } else if index_status == 'D' || worktree_status == 'D' {
            deleted.push(path);
        } else if worktree_status == 'M' {
            modified.push(path);
        } else if index_status != ' ' {
            staged.push(path);
        }
    }

    let mut sections = Vec::new();
    for (label, paths) in [
        ("Staged", &staged),
        ("Modified", &modified),
        ("Deleted", &deleted),
        ("Untracked", &untracked),
    ] {
        if !paths.is_empty() {
            let mut section = format!("{label}:\n");
            for path in paths {
                let _ = writeln!(section, "  {path}");
            }
            sections.push(section);
        }
    }

    if sections.is_empty() {
        "The working tree is clean.".to_string()
    } else {
        sections.join("\n").trim_end().to_string()
    }
}

/// Inspects current uncommitted changes without modifying the repository.
pub struct GitDiffTool;

#[derive(Debug, Deserialize, Default)]
struct DiffInput {
    path: Option<String>,
    #[serde(default)]
    staged: bool,
}

impl Tool for GitDiffTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("git_diff"),
            description: "Show the current Git diff for the project, optionally scoped to one \
                path and optionally staged instead of the working tree. Read-only; output is \
                bounded and marked when truncated."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "staged": {"type": "boolean"}
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

            let args: DiffInput = parse_args(input)?;

            if !is_git_repository(&ctx.project_root) {
                return Ok(ToolResult::success(
                    ctx.call_id.clone(),
                    ToolOutput::new("This project is not inside a Git repository."),
                ));
            }

            let mut git_args = vec!["diff".to_string()];
            if args.staged {
                git_args.push("--staged".into());
            }
            if let Some(path) = &args.path {
                git_args.push("--".into());
                git_args.push(path.clone());
            }

            let runner = TokioProcessRunner;
            let process = run_git(&runner, &ctx.project_root, git_args).await?;

            let (content, truncated) = cap_chars(process.stdout, MAX_DIFF_CHARS);
            let content = if content.is_empty() {
                "No changes.".to_string()
            } else if truncated {
                format!("{content}\n[Diff truncated at {MAX_DIFF_CHARS} characters.]")
            } else {
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
    use super::{GitDiffTool, GitStatusTool};
    use crate::contract::{Tool, ToolCallId, ToolContext, ToolStatus};
    use crate::permissions::PermissionContext;
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn fixture(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gocode-git-tool-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn init_repo(root: &Path) {
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .expect("git should be installed for this test");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
    }

    fn ctx(root: &Path) -> ToolContext {
        ToolContext::new(
            ToolCallId::new("call-1"),
            root.to_path_buf(),
            PermissionContext::read_only_default(),
        )
    }

    #[tokio::test]
    async fn non_git_project_reports_a_friendly_message() {
        let root = fixture("non-git");

        let result = GitStatusTool
            .execute(ctx(&root), serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Success);
        assert!(
            result
                .output
                .content
                .contains("not inside a Git repository")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn status_reports_untracked_and_modified_files() {
        let root = fixture("status");
        init_repo(&root);
        fs::write(root.join("tracked.txt"), "one\n").unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&root)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&root)
            .output()
            .unwrap();
        fs::write(root.join("tracked.txt"), "one\ntwo\n").unwrap();
        fs::write(root.join("untracked.txt"), "new\n").unwrap();

        let result = GitStatusTool
            .execute(ctx(&root), serde_json::json!({}))
            .await
            .unwrap();

        assert!(result.output.content.contains("Modified"));
        assert!(result.output.content.contains("tracked.txt"));
        assert!(result.output.content.contains("Untracked"));
        assert!(result.output.content.contains("untracked.txt"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn diff_reports_no_changes_on_a_clean_tree() {
        let root = fixture("diff-clean");
        init_repo(&root);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        Command::new("git")
            .args(["add", "a.txt"])
            .current_dir(&root)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&root)
            .output()
            .unwrap();

        let result = GitDiffTool
            .execute(ctx(&root), serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result.output.content, "No changes.");

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn diff_can_be_scoped_to_one_path() {
        let root = fixture("diff-scoped");
        init_repo(&root);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::write(root.join("b.txt"), "one\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&root)
            .output()
            .unwrap();
        fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        fs::write(root.join("b.txt"), "one\ntwo\n").unwrap();

        let result = GitDiffTool
            .execute(ctx(&root), serde_json::json!({"path": "a.txt"}))
            .await
            .unwrap();

        assert!(result.output.content.contains("a.txt"));
        assert!(!result.output.content.contains("b.txt"));

        fs::remove_dir_all(root).unwrap();
    }
}
