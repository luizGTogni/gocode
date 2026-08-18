use std::{sync::Arc, time::Duration};

use serde::Deserialize;

use crate::{
    contract::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolMetadata, ToolName,
        ToolOutput, ToolResult, ToolStatus,
    },
    permissions::{PermissionAction, classify_command_risk},
    process::{
        CommandRequest, OutputSink, ProcessError, ProcessOutcome, ProcessRunner, ProcessStream,
        TokioProcessRunner,
    },
    tools::parse_args,
    workspace::resolve_workspace_path,
};

/// Default timeout applied when the caller does not specify one.
const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
/// Upper bound accepted for a caller-supplied timeout, to keep a single call bounded.
const MAX_TIMEOUT_SECONDS: u64 = 1800;

/// Executes a development command needed to inspect, build, test, lint, format, or validate the
/// project. The highest-risk MVP tool; every call is evaluated by the permission engine.
pub struct RunCommandTool {
    runner: Arc<dyn ProcessRunner>,
}

impl Default for RunCommandTool {
    fn default() -> Self {
        Self {
            runner: Arc::new(TokioProcessRunner),
        }
    }
}

impl RunCommandTool {
    /// Creates a tool backed by a custom [`ProcessRunner`], for tests and platform-specific
    /// process handling.
    #[must_use]
    pub fn with_runner(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

#[derive(Debug, Deserialize)]
struct Input {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_cwd")]
    cwd: String,
    #[serde(default)]
    shell: bool,
    timeout_seconds: Option<u64>,
}

fn default_cwd() -> String {
    ".".into()
}

struct EventSinkAdapter {
    events: Arc<dyn crate::contract::ToolEventSink>,
    call_id: crate::contract::ToolCallId,
}

impl OutputSink for EventSinkAdapter {
    fn on_chunk(&self, _stream: ProcessStream, chunk: &str) {
        self.events.emit(crate::contract::ToolEvent::OutputChunk {
            id: self.call_id.clone(),
            chunk: chunk.to_string(),
        });
    }
}

impl Tool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("run_command"),
            description: "Run a development command (build, test, lint, format, or similar) \
                inside the current project. Prefer program+args over shell=true. The working \
                directory stays inside the project; commands may require user confirmation or be \
                denied depending on their assessed risk."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "program": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "cwd": {"type": "string", "description": "Project-relative working directory, default \".\""},
                    "shell": {"type": "boolean", "description": "Interpret program as a shell command line"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECONDS}
                },
                "required": ["program"],
                "additionalProperties": false
            }),
        }
    }

    fn execute(&self, ctx: ToolContext, input: serde_json::Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: Input = parse_args(input)?;
            let cwd = resolve_workspace_path(&ctx.project_root, &args.cwd)?;
            if !cwd.is_dir() {
                return Err(ToolError::InvalidArguments(format!(
                    "{} is not a directory",
                    args.cwd
                )));
            }

            let risk = classify_command_risk(&args.program, &args.args, args.shell);
            let authorization = ctx
                .permissions
                .authorize(PermissionAction::Command {
                    program: args.program.clone(),
                    args: args.args.clone(),
                    cwd: cwd.clone(),
                    risk,
                })
                .await;
            if let Err(reason) = authorization {
                return Ok(ToolResult::denied(ctx.call_id.clone(), reason.0));
            }

            let timeout = Duration::from_secs(
                args.timeout_seconds
                    .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
                    .min(MAX_TIMEOUT_SECONDS),
            );
            let request = CommandRequest {
                program: args.program.clone(),
                args: args.args.clone(),
                cwd,
                shell: args.shell,
                timeout,
            };
            let sink: Arc<dyn OutputSink> = Arc::new(EventSinkAdapter {
                events: ctx.events.clone(),
                call_id: ctx.call_id.clone(),
            });

            let outcome = self
                .runner
                .run(request, ctx.cancellation.clone(), Some(sink))
                .await;

            match outcome {
                Err(ProcessError::SpawnFailed(message)) => Ok(ToolResult::failed(
                    ctx.call_id.clone(),
                    format!("could not run `{}`: {message}", args.program),
                )),
                Ok(process) if process.outcome == ProcessOutcome::Cancelled => {
                    Ok(ToolResult::cancelled(ctx.call_id.clone()))
                }
                Ok(process) if process.outcome == ProcessOutcome::TimedOut => {
                    Ok(ToolResult::failed(
                        ctx.call_id.clone(),
                        format!(
                            "command timed out after {} seconds",
                            request_timeout_secs(timeout)
                        ),
                    ))
                }
                Ok(process) => {
                    let mut content = format!(
                        "Process exited with code {}.\n",
                        process
                            .exit_code
                            .map_or_else(|| "unknown".to_string(), |code| code.to_string())
                    );
                    if !process.stdout.is_empty() {
                        content.push_str("--- stdout ---\n");
                        content.push_str(&process.stdout);
                        content.push('\n');
                    }
                    if !process.stderr.is_empty() {
                        content.push_str("--- stderr ---\n");
                        content.push_str(&process.stderr);
                        content.push('\n');
                    }

                    let truncated = process.stdout_truncated || process.stderr_truncated;
                    Ok(ToolResult {
                        call_id: ctx.call_id.clone(),
                        status: ToolStatus::Success,
                        metadata: ToolMetadata {
                            duration_ms: Some(process.duration_ms),
                            exit_code: process.exit_code,
                            truncated,
                            ..ToolMetadata::default()
                        },
                        output: ToolOutput { content, truncated },
                    })
                }
            }
        })
    }
}

fn request_timeout_secs(timeout: Duration) -> u64 {
    timeout.as_secs()
}

#[cfg(test)]
mod tests {
    use super::RunCommandTool;
    use crate::contract::{Tool, ToolCallId, ToolContext, ToolStatus};
    use crate::permissions::{DefaultPermissionPolicy, PermissionContext, PlanPermissionPolicy};
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
            "gocode-run-command-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct AlwaysApproveResolver;

    impl crate::permissions::PermissionResolver for AlwaysApproveResolver {
        fn resolve<'a>(
            &'a self,
            _request: &'a crate::permissions::PermissionRequest,
        ) -> crate::permissions::ResolveFuture<'a> {
            Box::pin(async { crate::permissions::PermissionResponse::AllowOnce })
        }
    }

    fn ctx(root: &Path) -> ToolContext {
        ToolContext::new(
            ToolCallId::new("call-1"),
            root.to_path_buf(),
            PermissionContext::new(
                Arc::new(DefaultPermissionPolicy::editing()),
                Arc::new(crate::permissions::AlwaysDenyResolver),
            ),
        )
    }

    fn plan_ctx(root: &Path) -> ToolContext {
        ToolContext::new(
            ToolCallId::new("call-1"),
            root.to_path_buf(),
            PermissionContext::new(
                Arc::new(PlanPermissionPolicy),
                Arc::new(crate::permissions::AlwaysDenyResolver),
            ),
        )
    }

    #[tokio::test]
    async fn low_risk_command_runs_and_reports_exit_code() {
        let root = fixture("low-risk");
        let input = serde_json::json!({"program": "cargo", "args": ["--version"]});

        let result = RunCommandTool::default()
            .execute(ctx(&root), input)
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.metadata.exit_code.is_some());

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn medium_risk_command_is_denied_when_the_resolver_rejects() {
        let root = fixture("medium-risk");
        let ctx = plan_ctx(&root);

        let result = RunCommandTool::default()
            .execute(
                ctx,
                serde_json::json!({"program": "npm", "args": ["install"]}),
            )
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Denied);

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn high_risk_command_is_denied_outright() {
        let root = fixture("high-risk");

        let result = RunCommandTool::default()
            .execute(
                plan_ctx(&root),
                serde_json::json!({"program": "git", "args": ["push"]}),
            )
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Denied);

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cwd_must_stay_inside_the_workspace() {
        let root = fixture("cwd-escape");

        let error = RunCommandTool::default()
            .execute(
                ctx(&root),
                serde_json::json!({"program": "echo", "cwd": "../"}),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            crate::contract::ToolError::OutsideWorkspace(_)
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn missing_executable_is_reported_as_failed() {
        let root = fixture("missing-exe");
        let ctx = ToolContext::new(
            ToolCallId::new("call-1"),
            root.clone(),
            PermissionContext::new(
                Arc::new(DefaultPermissionPolicy::editing()),
                Arc::new(AlwaysApproveResolver),
            ),
        );

        let result = RunCommandTool::default()
            .execute(
                ctx,
                serde_json::json!({"program": "definitely-not-a-real-executable-xyz"}),
            )
            .await
            .unwrap();

        assert_eq!(result.status, ToolStatus::Failed);

        fs::remove_dir_all(root).unwrap();
    }
}
