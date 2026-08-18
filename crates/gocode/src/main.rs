//! Gocode application bootstrap.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gocode_agent::{Agent, AgentEvent, AgentRequest, SpawnRequest, SubagentEvent, SubagentManager};
use gocode_core::{
    AUTO_COMPACT_TOKEN_THRESHOLD, AppCommand, AppError, EnvironmentPaths, Platform, PlatformPaths,
    ProjectContext, RuntimeChannels, bootstrap_with_paths,
};
use gocode_credentials::{CredentialStore, NativeCredentialStore, SecretString};
use gocode_provider_nvidia::NvidiaProvider;
use gocode_tools::{
    ChangeKind, ToolRegistry, ToolStatus, builtin_registry,
    permissions::{
        ApproveEverythingPolicy, DefaultPermissionPolicy, PermissionContext, PermissionPolicy,
        PermissionRequest, PermissionResolver, PlanPermissionPolicy, ResolveFuture,
    },
    process::{CommandRequest, ProcessRunner, TokioProcessRunner},
    worktree,
};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing_subscriber::prelude::*;

/// Slot for the single active permission prompt, shared between the resolver and the command
/// loop so a user response (or a run cancellation) can reach the waiting resolver future.
type PendingPermission = Arc<Mutex<Option<oneshot::Sender<bool>>>>;

/// [`PermissionResolver`] that shows the prompt in the TUI and waits for the user's answer.
struct TuiPermissionResolver {
    event_tx: mpsc::Sender<gocode_core::AppEvent>,
    pending: PendingPermission,
}

impl PermissionResolver for TuiPermissionResolver {
    fn resolve<'a>(&'a self, request: &'a PermissionRequest) -> ResolveFuture<'a> {
        Box::pin(async move {
            let (response_tx, response_rx) = oneshot::channel();
            *self.pending.lock().await = Some(response_tx);

            if self
                .event_tx
                .send(gocode_core::AppEvent::PermissionRequested {
                    summary: request.summary.clone(),
                    working_directory: request.working_directory.display().to_string(),
                })
                .await
                .is_err()
            {
                return false;
            }

            response_rx.await.unwrap_or(false)
        })
    }
}

/// Shortens a subagent id to the prefix `/agents` displays and every `/agent <cmd> <id>` accepts.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Forwards every subagent's lifecycle facts to the interface as short, human-readable notices —
/// spawn confirmation, status changes, progress lines, and completion — translating them to the
/// provider-neutral [`gocode_core::AppEvent`] contract. Runs for the lifetime of the application;
/// one task serves every subagent, since [`SubagentManager`] multiplexes their events onto a
/// single channel.
async fn bridge_subagent_events(
    mut events: mpsc::Receiver<SubagentEvent>,
    event_tx: mpsc::Sender<gocode_core::AppEvent>,
) {
    use std::fmt::Write as _;
    while let Some(event) = events.recv().await {
        let notice = match event {
            SubagentEvent::Spawned(record) => Some(format!(
                "Subagent {} created — mode: {}, {}, location: {}.",
                short_id(&record.id),
                record.mode.label(),
                if record.read_only {
                    "read-only"
                } else {
                    "editing"
                },
                record.worktree_path.as_ref().map_or_else(
                    || "main workspace (read-only)".to_string(),
                    |path| path.display().to_string()
                ),
            )),
            SubagentEvent::StatusChanged { .. } => None,
            SubagentEvent::Progress { id, line } => {
                let _ = event_tx
                    .send(gocode_core::AppEvent::AgentProgress { id, line })
                    .await;
                None
            }
            SubagentEvent::Finished(record) => {
                let mut notice = format!(
                    "Subagent {} finished: {}.{}",
                    short_id(&record.id),
                    record.status.label(),
                    record
                        .result
                        .as_ref()
                        .map(|result| format!(" {}", result.summary))
                        .unwrap_or_default(),
                );
                if let Some(next_steps) = record
                    .result
                    .as_ref()
                    .map(|result| result.next_steps.len())
                    .filter(|count| *count > 0)
                {
                    let _ = write!(
                        notice,
                        " Suggests {next_steps} next step(s) — open /agents to review and spawn \
                         any of them."
                    );
                }
                Some(notice)
            }
        };
        if let Some(notice) = notice {
            let _ = event_tx
                .send(gocode_core::AppEvent::AgentNotice(notice))
                .await;
        }
    }
}

/// Either a ready-to-show diff or a plain notice explaining why there isn't one.
enum AgentDiffOutcome {
    Diff(String),
    Notice(String),
}

/// Computes `git diff base...branch` for `/agent apply <id>`, from the main workspace root (a
/// linked worktree shares its branches with the main one, so this works without changing
/// directories into the subagent's worktree).
async fn compute_subagent_diff(
    runner: &TokioProcessRunner,
    project_root: &Path,
    id: &str,
    base: &str,
    branch: &str,
) -> AgentDiffOutcome {
    let request = CommandRequest {
        program: "git".into(),
        args: vec!["diff".into(), format!("{base}...{branch}")],
        cwd: project_root.to_path_buf(),
        shell: false,
        timeout: Duration::from_secs(30),
    };
    match runner
        .run(request, tokio_util::sync::CancellationToken::new(), None)
        .await
    {
        Ok(result) if result.exit_code == Some(0) => {
            let diff = if result.stdout.trim().is_empty() {
                "(no changes)".to_string()
            } else {
                result.stdout
            };
            AgentDiffOutcome::Diff(format!(
                "Subagent {} (branch {branch}), merging into {base}:\n\n{diff}",
                short_id(id),
            ))
        }
        _ => AgentDiffOutcome::Notice("Could not compute the diff for that subagent.".into()),
    }
}

/// Outcome of attempting to merge a subagent's worktree branch, for `/agent apply <id> confirm`.
enum MergeAttempt {
    /// The merge completed cleanly.
    Applied(String),
    /// The merge conflicted; left in progress in the main workspace (never aborted) so the guided
    /// resolver can walk the user through each file in `files`.
    Conflict(Vec<String>),
    /// The merge could not even be attempted, or its output didn't match the expected conflict
    /// shape; the merge was aborted defensively since there is nothing to guide the user through.
    Error(String),
}

async fn git(
    runner: &TokioProcessRunner,
    project_root: &Path,
    args: Vec<String>,
    timeout: Duration,
) -> Result<gocode_tools::process::ProcessResult, gocode_tools::process::ProcessError> {
    runner
        .run(
            CommandRequest {
                program: "git".into(),
                args,
                cwd: project_root.to_path_buf(),
                shell: false,
                timeout,
            },
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .await
}

/// Attempts to merge `branch` into the current branch of the main workspace. On a conflict, the
/// merge is deliberately left in progress (not aborted) so `/agent apply`'s guided resolver
/// (`AppCommand::AgentResolveConflict`/`AgentFinishMerge`/`AgentAbortMerge`) can walk the user
/// through it file by file.
async fn attempt_merge(
    runner: &TokioProcessRunner,
    project_root: &Path,
    id: &str,
    branch: &str,
) -> MergeAttempt {
    match git(
        runner,
        project_root,
        vec!["merge".into(), "--no-ff".into(), branch.to_string()],
        Duration::from_secs(60),
    )
    .await
    {
        Ok(result) if result.exit_code == Some(0) => MergeAttempt::Applied(format!(
            "Applied subagent {}'s changes (merged branch {branch}).",
            short_id(id)
        )),
        Ok(result) => {
            let conflicts = parse_conflicting_files(&result.stdout);
            if conflicts.is_empty() {
                // Unexpected output shape: nothing to guide the user through, so fall back to
                // aborting and reporting git's own text rather than leaving an unexplained
                // conflict in progress.
                let _ = git(
                    runner,
                    project_root,
                    vec!["merge".into(), "--abort".into()],
                    Duration::from_secs(30),
                )
                .await;
                MergeAttempt::Error(format!(
                    "Could not apply subagent {}'s changes; the merge was aborted so nothing was \
                     left half-applied.\n\n{}\n{}",
                    short_id(id),
                    result.stdout.trim(),
                    result.stderr.trim(),
                ))
            } else {
                MergeAttempt::Conflict(conflicts)
            }
        }
        Err(gocode_tools::process::ProcessError::SpawnFailed(message)) => {
            MergeAttempt::Error(format!("Could not run git merge: {message}"))
        }
    }
}

/// Resolves one conflicting file from an in-progress merge by checking out either side and
/// staging it, for `AppCommand::AgentResolveConflict`. Returns `true` once the file is staged.
async fn resolve_conflict_file(
    runner: &TokioProcessRunner,
    project_root: &Path,
    file: &str,
    ours: bool,
) -> bool {
    let flag = if ours { "--ours" } else { "--theirs" };
    let checked_out = matches!(
        git(
            runner,
            project_root,
            vec!["checkout".into(), flag.into(), "--".into(), file.to_string()],
            Duration::from_secs(30),
        )
        .await,
        Ok(result) if result.exit_code == Some(0)
    );
    if !checked_out {
        return false;
    }
    matches!(
        git(
            runner,
            project_root,
            vec!["add".into(), "--".into(), file.to_string()],
            Duration::from_secs(30),
        )
        .await,
        Ok(result) if result.exit_code == Some(0)
    )
}

/// Completes an in-progress merge for `AppCommand::AgentFinishMerge`, once every conflicting file
/// has been resolved and staged.
async fn finish_merge(
    runner: &TokioProcessRunner,
    project_root: &Path,
    id: &str,
) -> (bool, String) {
    match git(
        runner,
        project_root,
        vec!["commit".into(), "--no-edit".into()],
        Duration::from_secs(30),
    )
    .await
    {
        Ok(result) if result.exit_code == Some(0) => (
            true,
            format!("Applied subagent {}'s changes.", short_id(id)),
        ),
        Ok(result) => (
            false,
            format!(
                "Could not finish the merge; files may still be unresolved.\n\n{}\n{}",
                result.stdout.trim(),
                result.stderr.trim(),
            ),
        ),
        Err(gocode_tools::process::ProcessError::SpawnFailed(message)) => {
            (false, format!("Could not run git commit: {message}"))
        }
    }
}

/// Aborts an in-progress merge for `AppCommand::AgentAbortMerge`, discarding every resolution
/// made so far.
async fn abort_merge(runner: &TokioProcessRunner, project_root: &Path) -> String {
    let _ = git(
        runner,
        project_root,
        vec!["merge".into(), "--abort".into()],
        Duration::from_secs(30),
    )
    .await;
    "Merge aborted; no changes were applied.".into()
}

/// Extracts the file paths git reports as conflicting from `git merge`'s stdout, e.g. lines like
/// `CONFLICT (content): Merge conflict in src/foo.rs`. Returns an empty vec (never an error) when
/// the output doesn't match the expected shape, so callers can fall back to showing it raw.
fn parse_conflicting_files(merge_stdout: &str) -> Vec<String> {
    merge_stdout
        .lines()
        .filter_map(|line| line.rsplit_once("Merge conflict in "))
        .map(|(_, path)| path.trim().to_string())
        .collect()
}

/// The warning shown before a confirmed `/agent cleanup <id>` removes a subagent's worktree.
fn cleanup_warning(record: &gocode_core::SubagentRecord) -> String {
    match &record.worktree_path {
        Some(path) => format!(
            "This removes the worktree at {} (branch {}). Any changes not already applied via \
             `/agent apply {}` will be discarded.",
            path.display(),
            record.branch.as_deref().unwrap_or("?"),
            short_id(&record.id),
        ),
        None => format!("This removes subagent {}'s metadata.", short_id(&record.id)),
    }
}

/// Removes a subagent's worktree (if any) and its persisted metadata for
/// `/agent cleanup <id> confirm`. Metadata is only removed once the worktree removal (if
/// applicable) has actually succeeded, so a failed `git worktree remove` never leaves an orphaned
/// worktree with no record pointing at it.
async fn cleanup_subagent(
    manager: &SubagentManager,
    project_root: &Path,
    record: gocode_core::SubagentRecord,
) -> String {
    if let Some(path) = &record.worktree_path {
        let runner = TokioProcessRunner;
        if let Err(error) =
            worktree::remove_worktree(&runner, project_root, &path.display().to_string()).await
        {
            return format!("Could not remove worktree: {error}");
        }
    }
    match manager.delete(&record.id).await {
        Ok(()) => format!("Removed subagent {}.", short_id(&record.id)),
        Err(error) => format!("Could not remove subagent metadata: {error}"),
    }
}

/// Forwards one agent run's events to the interface, translating them to the provider-neutral
/// [`gocode_core::AppEvent`] contract.
#[allow(
    clippy::too_many_lines,
    reason = "a flat one-variant-per-line translation match is clearer here than splitting it"
)]
async fn bridge_agent_events(
    mut agent_events: mpsc::Receiver<AgentEvent>,
    event_tx: mpsc::Sender<gocode_core::AppEvent>,
    project_root: PathBuf,
    prompt: String,
    undo: UndoRegistry,
    undo_dir: PathBuf,
) {
    let mut streamed_any_text = false;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut tools_with_output: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut file_snapshots: Vec<gocode_tools::FileSnapshot> = Vec::new();

    while let Some(event) = agent_events.recv().await {
        let is_run_end = matches!(event, AgentEvent::Completed(_) | AgentEvent::Cancelled);
        let mapped = match event {
            AgentEvent::FileSnapshot(snapshot) => {
                file_snapshots.push(snapshot);
                None
            }
            AgentEvent::Started | AgentEvent::ToolStarted(_) => None,
            AgentEvent::StateChanged(state) => match state {
                gocode_agent::AgentState::Inference => {
                    Some(gocode_core::AppEvent::AgentStateChanged(
                        gocode_core::AgentActivityState::Thinking,
                    ))
                }
                gocode_agent::AgentState::ExecutingTools
                | gocode_agent::AgentState::WaitingForPermission => {
                    Some(gocode_core::AppEvent::AgentStateChanged(
                        gocode_core::AgentActivityState::RunningTools,
                    ))
                }
                gocode_agent::AgentState::Idle
                | gocode_agent::AgentState::Preparing
                | gocode_agent::AgentState::Finalizing
                | gocode_agent::AgentState::Completed
                | gocode_agent::AgentState::Cancelled
                | gocode_agent::AgentState::Failed => None,
            },
            AgentEvent::TextDelta(delta) => {
                streamed_any_text = true;
                Some(gocode_core::AppEvent::AssistantTextDelta(delta))
            }
            AgentEvent::ReasoningDelta(delta) => {
                Some(gocode_core::AppEvent::AssistantReasoningDelta(delta))
            }
            AgentEvent::ToolRequested(call) => {
                let id = call.id.as_str().to_string();
                let name = call.name.as_str().to_string();
                tool_names.insert(id.clone(), name.clone());
                Some(gocode_core::AppEvent::ToolActivity {
                    id,
                    name,
                    status: gocode_core::ToolActivityStatus::Started,
                    detail: "running".into(),
                })
            }
            AgentEvent::ToolOutputChunk { id, chunk } => {
                let id = id.as_str().to_string();
                tools_with_output.insert(id.clone());
                Some(gocode_core::AppEvent::ToolOutputChunk { id, chunk })
            }
            AgentEvent::ToolFinished(result) => {
                let id = result.call_id.as_str().to_string();
                let name = tool_names.get(&id).cloned().unwrap_or_default();
                let status = match result.status {
                    ToolStatus::Success => gocode_core::ToolActivityStatus::Succeeded,
                    ToolStatus::Failed => gocode_core::ToolActivityStatus::Failed,
                    ToolStatus::Cancelled => gocode_core::ToolActivityStatus::Cancelled,
                    ToolStatus::Denied => gocode_core::ToolActivityStatus::Denied,
                };
                if !tools_with_output.contains(&id) && !result.output.content.is_empty() {
                    let _ = event_tx
                        .send(gocode_core::AppEvent::ToolOutputChunk {
                            id: id.clone(),
                            chunk: result.output.content.clone(),
                        })
                        .await;
                }
                Some(gocode_core::AppEvent::ToolActivity {
                    id,
                    name,
                    status,
                    detail: first_line(&result.output.content, 96),
                })
            }
            AgentEvent::FileChanged(change) => Some(gocode_core::AppEvent::FileChanged {
                path: change.path.display().to_string(),
                kind: match change.kind {
                    ChangeKind::Created => "created",
                    ChangeKind::Modified => "modified",
                    ChangeKind::Deleted => "deleted",
                }
                .to_string(),
            }),
            AgentEvent::Warning(warning) => Some(gocode_core::AppEvent::AgentWarning(
                describe_warning(&warning),
            )),
            AgentEvent::Completed(completion) => Some(gocode_core::AppEvent::AgentCompleted {
                final_text: (!streamed_any_text).then_some(completion.final_text),
                turns: completion.stats.turns,
                tool_calls: completion.stats.tool_calls,
                failed_tool_calls: completion.stats.failed_tool_calls,
                last_input_tokens: completion.stats.last_input_tokens,
                partial: !matches!(
                    completion.termination,
                    gocode_agent::TerminationReason::Normal
                ),
            }),
            AgentEvent::Cancelled => Some(gocode_core::AppEvent::AgentCancelled),
        };

        if let Some(mapped) = mapped
            && event_tx.send(mapped).await.is_err()
        {
            return;
        }

        if is_run_end && !file_snapshots.is_empty() {
            commit_undo_transaction(
                &event_tx,
                &undo,
                &undo_dir,
                &project_root,
                &prompt,
                std::mem::take(&mut file_snapshots),
            )
            .await;
        }
    }
}

/// Records one agent run's file edits as a single undo transaction and reports the new stack
/// sizes, once the run has completed or been cancelled.
async fn commit_undo_transaction(
    event_tx: &mpsc::Sender<gocode_core::AppEvent>,
    undo: &UndoRegistry,
    undo_dir: &Path,
    project_root: &Path,
    prompt: &str,
    files: Vec<gocode_tools::FileSnapshot>,
) {
    let transaction = gocode_tools::UndoTransaction {
        id: uuid::Uuid::new_v4().to_string(),
        created_at_unix: unix_now(),
        description: first_line(prompt, 60),
        files,
    };
    let counts = {
        let mut store = undo.lock().await;
        let entry = store.entry(project_root.to_path_buf()).or_insert_with(|| {
            gocode_tools::load_undo_store(
                undo_dir,
                project_root,
                gocode_tools::undo::DEFAULT_MAX_ENTRIES,
            )
        });
        entry.commit(transaction);
        let _ = gocode_tools::save_undo_store(undo_dir, project_root, entry);
        (entry.undo_count(), entry.redo_count())
    };
    let _ = event_tx
        .send(gocode_core::AppEvent::UndoStackChanged {
            undo_count: counts.0,
            redo_count: counts.1,
        })
        .await;
}

/// Shared undo/redo history, one [`gocode_tools::UndoStore`] per worktree (keyed by its
/// `project_root`), mirrored to disk under `undo_dir` so it survives a restart.
type UndoRegistry = Arc<Mutex<HashMap<PathBuf, gocode_tools::UndoStore>>>;

/// Converts one applied transaction's file outcomes into the plain-string form
/// [`gocode_core::AppEvent`] carries, so `gocode-core` need not depend on `gocode-tools`.
fn describe_applied_transaction(
    transaction: &gocode_tools::AppliedTransaction,
) -> gocode_core::UndoTransactionResult {
    gocode_core::UndoTransactionResult {
        description: transaction.description.clone(),
        files: transaction
            .files
            .iter()
            .map(|file| {
                let action = match file.action {
                    gocode_tools::FileAction::Restored => "restored",
                    gocode_tools::FileAction::Removed => "removed",
                    gocode_tools::FileAction::Recreated => "recreated",
                };
                (file.path.display().to_string(), action.to_string())
            })
            .collect(),
    }
}

/// Runs `/undo` or `/redo` against the current worktree's history and reports the outcome.
async fn apply_undo_redo(
    event_tx: &mpsc::Sender<gocode_core::AppEvent>,
    undo: &UndoRegistry,
    undo_dir: &Path,
    project_root: &Path,
    n: usize,
    force: bool,
    redo: bool,
) {
    let direction = if redo { "redo" } else { "undo" };
    let (result, undo_count, redo_count) = {
        let mut store = undo.lock().await;
        let entry = store.entry(project_root.to_path_buf()).or_insert_with(|| {
            gocode_tools::load_undo_store(
                undo_dir,
                project_root,
                gocode_tools::undo::DEFAULT_MAX_ENTRIES,
            )
        });
        let result = if redo {
            entry.redo(n, project_root, force)
        } else {
            entry.undo(n, project_root, force)
        };
        if result
            .as_ref()
            .is_ok_and(|outcome| !outcome.applied.is_empty())
        {
            let _ = gocode_tools::save_undo_store(undo_dir, project_root, entry);
        }
        (result, entry.undo_count(), entry.redo_count())
    };

    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = event_tx
                .send(gocode_core::AppEvent::UndoOperationFailed {
                    direction: direction.to_string(),
                    message: error.to_string(),
                })
                .await;
            return;
        }
    };

    if let Some(conflict) = outcome.conflict {
        let _ = event_tx
            .send(gocode_core::AppEvent::UndoConflict {
                direction: direction.to_string(),
                requested: n,
                applied: outcome
                    .applied
                    .iter()
                    .map(describe_applied_transaction)
                    .collect(),
                conflicting_files: conflict
                    .files
                    .into_iter()
                    .map(|file| gocode_core::UndoConflictFile {
                        path: file.path.display().to_string(),
                        expected: file.expected,
                        actual: file.actual,
                    })
                    .collect(),
            })
            .await;
    } else if outcome.applied.is_empty() {
        let _ = event_tx
            .send(gocode_core::AppEvent::UndoUnavailable {
                direction: direction.to_string(),
            })
            .await;
    } else {
        let _ = event_tx
            .send(gocode_core::AppEvent::UndoApplied {
                direction: direction.to_string(),
                transactions: outcome
                    .applied
                    .iter()
                    .map(describe_applied_transaction)
                    .collect(),
            })
            .await;
    }

    let _ = event_tx
        .send(gocode_core::AppEvent::UndoStackChanged {
            undo_count,
            redo_count,
        })
        .await;
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn describe_warning(warning: &gocode_agent::AgentWarning) -> String {
    match warning {
        gocode_agent::AgentWarning::UnknownTool(name) => {
            format!("The model requested an unavailable tool: {name}")
        }
        gocode_agent::AgentWarning::LoopDetected(name) => {
            format!("Stopped a repeating tool call: {name}")
        }
        gocode_agent::AgentWarning::TruncatedBeforeContent => {
            "The model ran out of output tokens before producing any answer or tool call \
             (often happens on reasoning models whose thinking alone fills the budget). Try \
             again, or lower the reasoning effort."
                .to_string()
        }
        gocode_agent::AgentWarning::BudgetExhausted(limit) => {
            format!("Run limit reached ({limit}); wrapping up with a summary of progress so far.")
        }
        gocode_agent::AgentWarning::NoProgress => {
            "Stopped exploration because repeated tool calls produced no new evidence.".to_string()
        }
    }
}

/// Configured MCP servers and their live connection state, owned by the runtime loop. Rebuilt
/// into a fresh [`ToolRegistry`] after every connect/disconnect; `/mcp` never mutates the tool
/// registry directly, only this state.
struct McpRuntime {
    /// Every configured server (global + project, merged), in file order.
    servers: Vec<gocode_core::McpServerEntry>,
    /// Tools discovered from each currently-connected server, keyed by server name.
    connected: HashMap<String, Vec<Arc<dyn gocode_tools::contract::Tool>>>,
    /// The most recent connection error for a server, keyed by server name. Cleared on connect.
    errors: HashMap<String, String>,
}

impl McpRuntime {
    /// Loads the merged global+project MCP server configuration and connects every enabled
    /// server. Best-effort: a server that fails to connect contributes no tools and is recorded
    /// in `errors`, rather than preventing startup.
    async fn bootstrap(paths: &PlatformPaths, project: &ProjectContext) -> Self {
        let load_layer =
            |path: &Path, layer: &str| match gocode_core::load_or_default_mcp_config(path) {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!("could not load {layer} mcp.toml: {error}");
                    gocode_core::McpConfig::default()
                }
            };
        let global_mcp = load_layer(&paths.mcp_config_path(), "global");
        let project_mcp = load_layer(&project.mcp_config_path(), "project");
        let servers = gocode_core::merge_mcp_servers(&global_mcp, &project_mcp);

        let mut runtime = Self {
            servers,
            connected: HashMap::new(),
            errors: HashMap::new(),
        };

        let enabled: Vec<_> = runtime
            .servers
            .iter()
            .filter(|server| server.enabled)
            .cloned()
            .collect();
        if !enabled.is_empty() {
            let outcome = gocode_mcp::connect_configured_servers(&enabled).await;
            for connection in outcome.connections {
                runtime.connected.insert(connection.name, connection.tools);
            }
            for (server_name, error) in outcome.failures {
                runtime.errors.insert(server_name, error.to_string());
            }
        }

        runtime
    }

    /// Connects one configured server by name, regardless of its persisted `enabled` flag.
    async fn connect(&mut self, name: &str) -> Result<(), String> {
        let Some(server) = self.servers.iter().find(|server| server.name == name) else {
            return Err(format!("no MCP server named '{name}' is configured"));
        };
        match gocode_mcp::connect_server(server).await {
            Ok(connection) => {
                self.connected.insert(connection.name, connection.tools);
                self.errors.remove(name);
                Ok(())
            }
            Err(error) => {
                self.errors.insert(name.to_string(), error.to_string());
                Err(error.to_string())
            }
        }
    }

    /// Disconnects one currently-connected server by name, dropping its tools (and, once the
    /// last reference to its client is gone, its subprocess).
    fn disconnect(&mut self, name: &str) -> bool {
        self.errors.remove(name);
        self.connected.remove(name).is_some()
    }

    /// Adds a newly configured server (or replaces one of the same name) to the in-memory
    /// server list. Callers persist it to `mcp.toml` separately.
    fn add_or_replace_server(&mut self, entry: gocode_core::McpServerEntry) {
        if let Some(existing) = self.servers.iter_mut().find(|s| s.name == entry.name) {
            *existing = entry;
        } else {
            self.servers.push(entry);
        }
    }

    /// One status per configured server, for [`gocode_core::AppEvent::McpServersAvailable`].
    fn statuses(&self) -> Vec<gocode_core::McpServerStatus> {
        self.servers
            .iter()
            .map(|server| {
                let tools = self.connected.get(&server.name);
                let tool_names = tools.map_or_else(Vec::new, |tools| {
                    tools
                        .iter()
                        .map(|tool| tool.definition().name.as_str().to_string())
                        .collect()
                });
                gocode_core::McpServerStatus {
                    name: server.name.clone(),
                    transport: server.transport.label(),
                    connected: tools.is_some(),
                    tool_count: tools.map_or(0, Vec::len),
                    tool_names,
                    error: self.errors.get(&server.name).cloned(),
                    needs_authorization: matches!(
                        server.auth,
                        gocode_core::McpAuthConfig::OAuth { .. }
                    ),
                }
            })
            .collect()
    }

    /// Rebuilds a tool registry from the built-ins plus every currently-connected server's
    /// tools.
    fn build_registry(&self) -> ToolRegistry {
        let mut registry = builtin_registry();
        for tools in self.connected.values() {
            for tool in tools {
                registry.register(Arc::clone(tool));
            }
        }
        registry
    }
}

/// Builds a language server manager from the merged global+project `lsp.toml`, falling back to
/// built-in defaults (rust-analyzer, typescript-language-server, pyright, gopls) for any
/// extension the user hasn't configured, filtered to commands actually found on `PATH`.
fn build_lsp_manager(paths: &PlatformPaths, project: &ProjectContext) -> gocode_lsp::LspManager {
    let load_layer = |path: &Path, layer: &str| match gocode_core::load_or_default_lsp_config(path)
    {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!("could not load {layer} lsp.toml: {error}");
            gocode_core::LspConfig::default()
        }
    };
    let global_lsp = load_layer(&paths.lsp_config_path(), "global");
    let project_lsp = load_layer(&project.lsp_config_path(), "project");
    let mut configured = gocode_core::merge_lsp_servers(&global_lsp, &project_lsp);

    let configured_extensions: std::collections::HashSet<String> = configured
        .iter()
        .flat_map(|entry| entry.extensions.iter().cloned())
        .collect();
    for default_entry in gocode_core::builtin_lsp_defaults() {
        let covers_new_extension = default_entry
            .extensions
            .iter()
            .any(|extension| !configured_extensions.contains(extension));
        if covers_new_extension && gocode_lsp::is_command_available(&default_entry.command) {
            configured.push(default_entry);
        }
    }

    let specs = configured
        .into_iter()
        .filter(|entry| entry.enabled)
        .map(|entry| gocode_lsp::LspServerSpec {
            name: entry.name,
            extensions: entry.extensions,
            command: entry.command,
            args: entry.args,
            env: entry.env.into_iter().collect(),
        })
        .collect();

    gocode_lsp::LspManager::new(project.root.clone(), specs)
}

/// Rebuilds the full tool registry (built-ins + connected MCP servers + the `lsp` tool) after
/// any change to MCP server connections. The LSP manager itself is long-lived across rebuilds.
fn build_full_registry(
    mcp_runtime: &McpRuntime,
    lsp_manager: &Arc<gocode_lsp::LspManager>,
) -> ToolRegistry {
    let mut registry = mcp_runtime.build_registry();
    registry.register(Arc::new(gocode_lsp::LspTool::new(Arc::clone(lsp_manager))));
    registry
}

fn first_line(text: &str, max_chars: usize) -> String {
    let first = text.lines().next().unwrap_or_default();
    if first.chars().count() > max_chars {
        format!("{}…", first.chars().take(max_chars).collect::<String>())
    } else if first.is_empty() {
        "done".into()
    } else {
        first.into()
    }
}

fn debug_agent_prompt(debug: &gocode_core::DebugInvestigation) -> String {
    let description = debug
        .description
        .as_deref()
        .unwrap_or("Problema descrito no fluxo guiado");
    let answers = debug.answers.join("\n");
    format!(
        "Você está no fluxo /debug. Mostre explicitamente as etapas: Triagem, Reproduzindo, Investigando, Hipótese, Corrigindo e Validando.\n\
Problema: {description}\n\
Informações coletadas:\n{answers}\n\n\
Primeiro resuma apenas fatos recebidos e lacunas, classifique o tipo provável e investigue com leitura/busca e comandos diagnósticos seguros. \n\
Não invente erros, logs ou resultados. Apresente hipóteses ordenadas, com evidência e próximo teste. \n\
Não edite antes de ter evidência suficiente de causa provável. Em modo Plan, não edite. Em modo Approve, apresente causa, arquivos, estratégia e risco antes de solicitar a edição. \n\
Em Auto, ainda pare para ações destrutivas, rede, banco, deploy ou fora do workspace. Use a menor correção, valide a reprodução e as verificações proporcionais. \n\
Ao concluir, entregue: Debug concluído; Causa raiz; Correção; Arquivos alterados; Validação executada; Resultado; Próximo passo quando necessário."
    )
}

/// Everything a compaction (automatic or `/compact`) needs to read and update the active session.
struct CompactionContext<'a> {
    provider: &'a NvidiaProvider,
    model: &'a str,
    session: &'a Arc<Mutex<gocode_core::SessionRecord>>,
    sessions_dir: &'a Path,
}

/// Persists a completed run's history onto its session, then triggers automatic compaction when
/// the reported input-token usage crosses [`AUTO_COMPACT_TOKEN_THRESHOLD`].
async fn handle_completed_run(
    ctx: &CompactionContext<'_>,
    prompt: &str,
    completion: gocode_agent::AgentCompletion,
    auto_compact_enabled: bool,
    event_tx: &mpsc::Sender<gocode_core::AppEvent>,
) {
    let last_input_tokens = completion.stats.last_input_tokens;
    {
        let mut session = ctx.session.lock().await;
        session.history = completion.history;
        session.record_turn(prompt, &completion.final_text);
        let _ = gocode_core::save_session(ctx.sessions_dir, &session);
    }

    if auto_compact_enabled
        && last_input_tokens.is_some_and(|tokens| tokens >= AUTO_COMPACT_TOKEN_THRESHOLD)
    {
        let history_snapshot = ctx.session.lock().await.history.clone();
        compact_and_report(ctx, history_snapshot, true, event_tx).await;
    }
}

/// Summarizes `history` down to one condensed message and stores it on the session, reporting
/// the outcome either way.
async fn compact_and_report(
    ctx: &CompactionContext<'_>,
    history: Vec<gocode_core::ChatMessage>,
    automatic: bool,
    event_tx: &mpsc::Sender<gocode_core::AppEvent>,
) {
    match compact_conversation(ctx.provider, ctx.model, history).await {
        Ok(compacted) => {
            {
                let mut session = ctx.session.lock().await;
                session.history = compacted;
                let _ = gocode_core::save_session(ctx.sessions_dir, &session);
            }
            let _ = event_tx
                .send(gocode_core::AppEvent::ContextCompacted { automatic })
                .await;
        }
        Err(error) => {
            let _ = event_tx
                .send(gocode_core::AppEvent::ContextCompactionFailed(
                    error.to_string(),
                ))
                .await;
        }
    }
}

/// Number of most-recent user turns compaction always keeps verbatim. Summarizing the *entire*
/// history (the previous behavior) loses exact recent tool outputs and decisions right when
/// they matter most — mid-task, on a long refactor. Only the older portion gets condensed; the
/// tail survives untouched so the agent doesn't lose track of what it was just doing.
const COMPACT_KEEP_RECENT_TURNS: usize = 2;

/// Splits `history` at a `User`-message boundary so the most recent `keep_recent_turns` user
/// turns — and everything that belongs to them (assistant replies, tool calls, tool results) —
/// are kept intact, separate from everything older. Splitting only ever happens right before a
/// `User` message because a fresh user turn never continues a pending tool-call sequence, so
/// this boundary is always safe to cut at regardless of provider.
///
/// Returns `(older, recent)`. `older` is empty when `history` has at most `keep_recent_turns`
/// user turns total, meaning there is nothing old enough to summarize.
fn split_for_compaction(
    history: &[gocode_core::ChatMessage],
    keep_recent_turns: usize,
) -> (Vec<gocode_core::ChatMessage>, Vec<gocode_core::ChatMessage>) {
    let user_indices: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, message)| matches!(message, gocode_core::ChatMessage::User(_)))
        .map(|(index, _)| index)
        .collect();

    if user_indices.len() <= keep_recent_turns {
        return (Vec::new(), history.to_vec());
    }

    let split_at = user_indices[user_indices.len() - keep_recent_turns];
    (history[..split_at].to_vec(), history[split_at..].to_vec())
}

/// Asks the model to summarize the older portion of `history`, replacing it with a single
/// condensed message while keeping the most recent [`COMPACT_KEEP_RECENT_TURNS`] user turns
/// verbatim. Returns `history` unchanged if there's nothing old enough to summarize.
///
/// # Errors
///
/// Returns the provider error when the summarization request fails.
async fn compact_conversation(
    provider: &NvidiaProvider,
    model: &str,
    history: Vec<gocode_core::ChatMessage>,
) -> Result<Vec<gocode_core::ChatMessage>, gocode_core::ProviderError> {
    let (older, recent) = split_for_compaction(&history, COMPACT_KEEP_RECENT_TURNS);
    if older.is_empty() {
        return Ok(history);
    }

    let mut messages = older;
    messages.push(gocode_core::ChatMessage::User(
        "Summarize the conversation so far in a concise paragraph, preserving important facts, \
         decisions, and any unfinished tasks. This summary will be prepended to the most recent \
         messages, which are kept in full and shown separately \u{2014} do not restate them, \
         focus on what happened before them. Respond with only the summary, no preamble."
            .into(),
    ));
    let request = gocode_core::ChatRequest {
        model: gocode_core::ModelId::new(model),
        messages,
        tools: Vec::new(),
        reasoning_effort: None,
    };
    let cancellation = gocode_core::CancellationToken::new();
    let mut stream = provider.stream_chat(request, cancellation).await?;

    let mut summary = String::new();
    while let Some(event) = stream.recv().await {
        if let gocode_core::ChatStreamEvent::TextDelta(delta) = event? {
            summary.push_str(&delta);
        }
    }
    let summary = summary.trim();
    let summary = if summary.is_empty() {
        "(no summary was produced)"
    } else {
        summary
    };

    let mut compacted = vec![gocode_core::ChatMessage::User(format!(
        "(Earlier conversation was compacted to save context. Summary of what happened before \
         the messages below:)\n{summary}"
    ))];
    compacted.extend(recent);
    Ok(compacted)
}

#[tokio::main]
async fn main() {
    gocode_tui::install_panic_hook();

    match Box::pin(run_application()).await {
        Ok(()) => {}
        Err(error) => {
            eprintln!("Gocode could not start: {error}");
            std::process::exit(1);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the bootstrap composition root deliberately keeps runtime ownership visible"
)]
async fn run_application() -> Result<(), AppError> {
    let paths = application_paths(current_platform(), process_environment())?;
    let _log_guard = init_logging(&paths.state_dir)?;
    let bootstrap = bootstrap_with_paths(&paths)?;
    let preferences_path = paths.config_dir.join("preferences.toml");
    let loaded_preferences = gocode_core::load_or_default_preferences(&preferences_path);
    tracing::info!("application bootstrapped");

    let environment_credential = std::env::var("NVIDIA_API_KEY").ok();

    let (client, mut driver) = RuntimeChannels::create();
    driver
        .event_tx
        .send(bootstrap.event)
        .await
        .map_err(|error| AppError::Initialization(format!("could not send boot event: {error}")))?;
    driver
        .event_tx
        .send(gocode_core::AppEvent::PreferencesLoaded {
            preferences: loaded_preferences.preferences.clone(),
            recovery: loaded_preferences.recovered_from_error.clone(),
        })
        .await
        .map_err(|error| {
            AppError::Initialization(format!("could not send preferences: {error}"))
        })?;
    start_update_check(driver.event_tx.clone());
    let tui = gocode_tui::run(client.event_rx, client.command_tx);
    let runtime = async move {
        let credential_store = NativeCredentialStore::new();
        let mut provider = None;
        let mut selected_model = None;
        let mut model_catalog: Vec<gocode_core::Model> = Vec::new();
        let mut reasoning_effort: Option<String> =
            bootstrap.resolved_config.reasoning_effort.clone();
        let mut permission_mode = gocode_core::PermissionMode::default();
        let mut auto_compact_enabled = true;
        let mut staged_update: Option<StagedUpdate> = None;
        // Set once `RestartForUpdate` is handled; installed only after the TUI's final `Exit`
        // command confirms it has torn down (see the `AppCommand::Exit` arm below).
        let mut restart_after_exit: Option<StagedUpdate> = None;
        let sessions_dir = gocode_core::sessions_dir(&paths.state_dir);
        let subagents_dir = gocode_core::subagents_dir(&paths.state_dir);
        let (subagent_event_tx, subagent_event_rx) = mpsc::channel::<SubagentEvent>(128);
        let lsp_manager = Arc::new(build_lsp_manager(&paths, &bootstrap.project));
        let file_change_observer: Arc<dyn gocode_tools::contract::FileChangeObserver> =
            Arc::new(gocode_lsp::ArcLspManagerObserver(Arc::clone(&lsp_manager)));
        let subagent_manager = Arc::new(
            SubagentManager::new(
                subagents_dir.clone(),
                gocode_agent::SubagentLimits::default(),
                subagent_event_tx,
            )
            .with_file_change_observer(Arc::clone(&file_change_observer)),
        );
        match gocode_core::recover_interrupted(&subagents_dir) {
            Ok(recovered) if !recovered.is_empty() => {
                driver
                    .event_tx
                    .send(gocode_core::AppEvent::AgentNotice(format!(
                        "{} subagent(s) were interrupted by a restart; use /agents to review \
                         and /agent cleanup <id> to discard.",
                        recovered.len()
                    )))
                    .await
                    .map_err(|error| {
                        AppError::Initialization(format!(
                            "could not report interrupted subagents: {error}"
                        ))
                    })?;
            }
            Ok(_) => {}
            Err(error) => tracing::warn!("could not recover subagent metadata: {error}"),
        }
        tokio::spawn(bridge_subagent_events(
            subagent_event_rx,
            driver.event_tx.clone(),
        ));
        let current_session: Arc<Mutex<gocode_core::SessionRecord>> =
            Arc::new(Mutex::new(gocode_core::SessionRecord::new()));
        // The session's active working directory. Starts at the detected project root and moves
        // to a Git worktree on `/worktree` create/switch, without ever touching the original
        // project root's files or checked-out branch.
        let mut project_root = bootstrap.project.root.clone();
        let undo: UndoRegistry = Arc::new(Mutex::new(HashMap::new()));
        let undo_dir = gocode_tools::undo_dir(&paths.state_dir);
        let mut active_cancellation: Option<gocode_core::CancellationToken> = None;
        let mut active_personality = loaded_preferences.preferences.personality;
        let config_path = paths.config_dir.join("config.toml");
        let mut mcp_runtime = McpRuntime::bootstrap(&paths, &bootstrap.project).await;
        let mut tool_registry: Arc<ToolRegistry> =
            Arc::new(build_full_registry(&mcp_runtime, &lsp_manager));
        driver
            .event_tx
            .send(gocode_core::AppEvent::McpServersAvailable(
                mcp_runtime.statuses(),
            ))
            .await
            .map_err(|error| {
                AppError::Initialization(format!("could not send configured MCP servers: {error}"))
            })?;
        let permission_pending: PendingPermission = Arc::new(Mutex::new(None));
        let instructions =
            std::fs::read_to_string(bootstrap.project.gocode_dir.join("instructions.md")).ok();
        let project_overview =
            std::fs::read_to_string(bootstrap.project.root.join("AGENTS.md")).ok();

        let custom_commands =
            gocode_core::load_custom_commands(&bootstrap.project.gocode_dir.join("commands"));
        driver
            .event_tx
            .send(gocode_core::AppEvent::CustomCommandsAvailable(
                custom_commands,
            ))
            .await
            .map_err(|error| {
                AppError::Initialization(format!(
                    "could not send discovered custom commands: {error}"
                ))
            })?;

        let global_skills_dir = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| Path::new(&home).join(".agents").join("skills"));
        let project_agents_skills_dir = bootstrap.project.root.join(".agents").join("skills");
        let project_skills_dir = if project_agents_skills_dir.is_dir() {
            project_agents_skills_dir
        } else {
            bootstrap.project.gocode_dir.join("skills")
        };
        let mut skills =
            gocode_core::load_skills(global_skills_dir.as_deref(), &project_skills_dir);
        let disabled_skills = gocode_core::load_disabled_skills(&bootstrap.project.gocode_dir);
        gocode_core::apply_disabled_skills(&mut skills, &disabled_skills);
        let skills_summary = skills.iter().any(|skill| skill.enabled).then(|| {
            skills
                .iter()
                .filter(|skill| skill.enabled)
                .map(|skill| {
                    format!(
                        "- {}: {} (read {} to use)",
                        skill.name,
                        skill.description,
                        skill.path.display()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        driver
            .event_tx
            .send(gocode_core::AppEvent::SkillsAvailable(skills))
            .await
            .map_err(|error| {
                AppError::Initialization(format!("could not send discovered skills: {error}"))
            })?;

        driver
            .event_tx
            .send(gocode_core::AppEvent::ProjectContextAvailable {
                working_directory: bootstrap.project.root.display().to_string(),
            })
            .await
            .map_err(|error| {
                AppError::Initialization(format!(
                    "could not send the project working directory: {error}"
                ))
            })?;
        driver
            .event_tx
            .send(gocode_core::AppEvent::SessionSwitched {
                id: current_session.lock().await.id.clone(),
                name: "New session".into(),
                transition: gocode_core::SessionTransition::New,
                history: Vec::new(),
            })
            .await
            .map_err(|error| {
                AppError::Initialization(format!("could not confirm the initial session: {error}"))
            })?;
        driver
            .event_tx
            .send(gocode_core::AppEvent::DebugStateUpdated(
                current_session.lock().await.debug.clone(),
            ))
            .await
            .map_err(|error| {
                AppError::Initialization(format!("could not publish initial debug state: {error}"))
            })?;

        if let Some(effort) = reasoning_effort.clone() {
            driver
                .event_tx
                .send(gocode_core::AppEvent::ReasoningEffortChanged {
                    effort: Some(effort),
                    announce: false,
                })
                .await
                .map_err(|error| {
                    AppError::Initialization(format!(
                        "could not restore the saved reasoning effort: {error}"
                    ))
                })?;
        }

        let stored_credential = environment_credential
            .map(SecretString::new)
            .or_else(|| credential_store.get_nvidia().ok().flatten());

        if let Some(secret) = stored_credential {
            let candidate = NvidiaProvider::hosted(secret);
            match candidate.list_models().await {
                Ok(models) => {
                    let model_ids: Vec<String> = models
                        .iter()
                        .map(|model| model.id.as_str().into())
                        .collect();
                    model_catalog = models;
                    provider = Some(candidate);

                    let remembered_model = bootstrap
                        .resolved_config
                        .model
                        .as_deref()
                        .filter(|model| model_ids.iter().any(|id| id == model))
                        .map(str::to_string);

                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::ModelsAvailable(model_ids))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not send discovered models: {error}"
                            ))
                        })?;

                    if let Some(model) = remembered_model {
                        selected_model = Some(model.clone());
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::ModelSelected(model))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not confirm remembered model: {error}"
                                ))
                            })?;
                    }
                }
                Err(error) => driver
                    .event_tx
                    .send(gocode_core::AppEvent::CredentialValidationFailed(
                        error.to_string(),
                    ))
                    .await
                    .map_err(|send_error| {
                        AppError::Initialization(format!(
                            "could not report provider failure: {send_error}"
                        ))
                    })?,
            }
        } else {
            driver
                .event_tx
                .send(gocode_core::AppEvent::CredentialRequired)
                .await
                .map_err(|error| {
                    AppError::Initialization(format!(
                        "could not start credential onboarding: {error}"
                    ))
                })?;
        }

        while let Some(command) = driver.command_rx.recv().await {
            match command {
                AppCommand::Exit => return Ok(restart_after_exit.take()),
                AppCommand::Resize { columns, rows } => driver
                    .event_tx
                    .send(gocode_core::AppEvent::TerminalResized { columns, rows })
                    .await
                    .map_err(|error| {
                        AppError::Initialization(format!(
                            "could not send resize event to interface: {error}"
                        ))
                    })?,
                AppCommand::SubmitCredential(credential) => {
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::CredentialValidationStarted)
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not show credential validation: {error}"
                            ))
                        })?;
                    let secret = SecretString::new(credential);
                    let candidate = NvidiaProvider::hosted(secret.clone());
                    match candidate.list_models().await {
                        Ok(models) => {
                            if let Err(error) = credential_store.save_nvidia(&secret) {
                                driver.event_tx.send(gocode_core::AppEvent::ProviderFailed(format!("Credential validated but could not be saved securely: {error:?}"))).await.map_err(|send_error| AppError::Initialization(format!("could not report credential-store failure: {send_error}")))?;
                            }
                            let model_ids = models
                                .iter()
                                .map(|model| model.id.as_str().into())
                                .collect();
                            model_catalog = models;
                            provider = Some(candidate);
                            driver
                                .event_tx
                                .send(gocode_core::AppEvent::ModelsAvailable(model_ids))
                                .await
                                .map_err(|error| {
                                    AppError::Initialization(format!(
                                        "could not send discovered models: {error}"
                                    ))
                                })?;
                        }
                        Err(error) => driver
                            .event_tx
                            .send(gocode_core::AppEvent::CredentialValidationFailed(
                                error.to_string(),
                            ))
                            .await
                            .map_err(|send_error| {
                                AppError::Initialization(format!(
                                    "could not report credential failure: {send_error}"
                                ))
                            })?,
                    }
                }
                AppCommand::SelectModel(model) => {
                    gocode_core::save_global_config(
                        &config_path,
                        Some("nvidia"),
                        Some(&model),
                        reasoning_effort.as_deref(),
                    )?;
                    selected_model = Some(model.clone());
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::ModelSelected(model))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not confirm model selection: {error}"
                            ))
                        })?;
                }
                AppCommand::DebugStart(description) => {
                    let (debug, ready) = {
                        let mut session = current_session.lock().await;
                        session.debug = gocode_core::DebugInvestigation {
                            started: true,
                            description: description.filter(|value| !value.trim().is_empty()),
                            ..Default::default()
                        };
                        let debug = session.debug.clone();
                        let _ = gocode_core::save_session(&sessions_dir, &session);
                        (debug.clone(), debug.ready_for_investigation())
                    };
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::DebugStateUpdated(debug.clone()))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not update debug state: {error}"
                            ))
                        })?;
                    if ready {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::DebugInvestigationReady(
                                debug_agent_prompt(&debug),
                            ))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not start debug investigation: {error}"
                                ))
                            })?;
                    } else if let Some(question) = debug.next_question() {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentWarning(format!(
                                "Triagem\n\n{question}"
                            )))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not request debug detail: {error}"
                                ))
                            })?;
                    }
                }
                AppCommand::DebugAnswer(answer) => {
                    let (debug, ready) = {
                        let mut session = current_session.lock().await;
                        if session.debug.next_question().is_some() {
                            session
                                .debug
                                .answers
                                .push(gocode_tools::process::redact_secrets(&answer));
                        }
                        let debug = session.debug.clone();
                        let _ = gocode_core::save_session(&sessions_dir, &session);
                        (debug.clone(), debug.ready_for_investigation())
                    };
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::DebugStateUpdated(debug.clone()))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not update debug answer: {error}"
                            ))
                        })?;
                    if ready {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::DebugInvestigationReady(
                                debug_agent_prompt(&debug),
                            ))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not begin debug investigation: {error}"
                                ))
                            })?;
                    } else if let Some(question) = debug.next_question() {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentWarning(format!(
                                "Triagem\n\n{question}"
                            )))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not request next debug detail: {error}"
                                ))
                            })?;
                    }
                }
                AppCommand::DebugStop => {
                    let debug = {
                        let mut session = current_session.lock().await;
                        session.debug.stopped = true;
                        let debug = session.debug.clone();
                        let _ = gocode_core::save_session(&sessions_dir, &session);
                        debug
                    };
                    if let Some(cancellation) = active_cancellation.take() {
                        cancellation.cancel();
                    }
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::DebugStateUpdated(debug))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not stop debug investigation: {error}"
                            ))
                        })?;
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::AgentWarning(
                            "Investigação interrompida; evidências preservadas.".into(),
                        ))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not report debug stop: {error}"
                            ))
                        })?;
                }
                AppCommand::SubmitChat(message) => {
                    let (Some(provider), Some(model)) = (&provider, &selected_model) else {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::ProviderFailed(
                                "Select an NVIDIA model before chatting.".into(),
                            ))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not report provider state: {error}"
                                ))
                            })?;
                        continue;
                    };
                    let tools_enabled = model_catalog
                        .iter()
                        .find(|candidate| candidate.id.as_str() == model)
                        .is_some_and(|candidate| {
                            candidate.capabilities.tools == gocode_core::ToolCapability::Supported
                        });

                    let cancellation = gocode_core::CancellationToken::new();
                    active_cancellation = Some(cancellation.clone());
                    let resolver = Arc::new(TuiPermissionResolver {
                        event_tx: driver.event_tx.clone(),
                        pending: permission_pending.clone(),
                    });
                    let policy: Arc<dyn PermissionPolicy> = match permission_mode {
                        gocode_core::PermissionMode::Auto => {
                            Arc::new(DefaultPermissionPolicy::editing())
                        }
                        gocode_core::PermissionMode::Plan => Arc::new(PlanPermissionPolicy),
                        gocode_core::PermissionMode::Approve => Arc::new(ApproveEverythingPolicy),
                    };
                    let permissions = PermissionContext::new(policy, resolver);
                    let agent = Agent::new(
                        Arc::new(provider.clone()),
                        tool_registry.clone(),
                        permissions,
                        gocode_agent::AgentLimits::default(),
                    )
                    .with_file_change_observer(Arc::clone(&file_change_observer));
                    let history_snapshot = current_session.lock().await.history.clone();
                    let prompt = message.clone();
                    let request = AgentRequest {
                        prompt: message,
                        model: gocode_core::ModelId::new(model.clone()),
                        project_root: project_root.clone(),
                        instructions: instructions.clone(),
                        project_overview: project_overview.clone(),
                        skills_summary: skills_summary.clone(),
                        tools_enabled,
                        reasoning_effort: reasoning_effort.clone(),
                        personality: active_personality,
                        history: history_snapshot,
                    };
                    let event_tx = driver.event_tx.clone();
                    let (agent_events_tx, agent_events_rx) = mpsc::channel(64);
                    tokio::spawn(bridge_agent_events(
                        agent_events_rx,
                        event_tx.clone(),
                        project_root.clone(),
                        prompt.clone(),
                        undo.clone(),
                        undo_dir.clone(),
                    ));
                    let session_for_run = current_session.clone();
                    let sessions_dir_for_run = sessions_dir.clone();
                    let provider_for_run = provider.clone();
                    let model_for_run = model.clone();
                    tokio::spawn(async move {
                        match agent.run(request, agent_events_tx, cancellation).await {
                            Ok(completion) => {
                                let ctx = CompactionContext {
                                    provider: &provider_for_run,
                                    model: &model_for_run,
                                    session: &session_for_run,
                                    sessions_dir: &sessions_dir_for_run,
                                };
                                handle_completed_run(
                                    &ctx,
                                    &prompt,
                                    completion,
                                    auto_compact_enabled,
                                    &event_tx,
                                )
                                .await;
                            }
                            Err(error) => match error {
                                gocode_agent::AgentError::Cancelled => {}
                                gocode_agent::AgentError::Provider(provider_error) => {
                                    let event = match provider_error.severity() {
                                        gocode_core::ErrorSeverity::Blocking => {
                                            gocode_core::AppEvent::BlockingError(
                                                provider_error.to_string(),
                                            )
                                        }
                                        gocode_core::ErrorSeverity::Recoverable => {
                                            gocode_core::AppEvent::ProviderFailed(
                                                provider_error.to_string(),
                                            )
                                        }
                                    };
                                    let _ = event_tx.send(event).await;
                                }
                                other => {
                                    let _ = event_tx
                                        .send(gocode_core::AppEvent::AgentWarning(
                                            other.to_string(),
                                        ))
                                        .await;
                                    let _ =
                                        event_tx.send(gocode_core::AppEvent::AgentStopped).await;
                                }
                            },
                        }
                    });
                }
                AppCommand::SetPreferences(updated) => {
                    gocode_core::save_preferences(&preferences_path, &updated)?;
                }
                AppCommand::SetSessionPersonality(personality) => active_personality = personality,
                AppCommand::CancelProviderRequest => {
                    if let Some(cancellation) = active_cancellation.take() {
                        cancellation.cancel();
                    }
                    if let Some(pending) = permission_pending.lock().await.take() {
                        let _ = pending.send(false);
                    }
                }
                AppCommand::PermissionResponse(approved) => {
                    if let Some(pending) = permission_pending.lock().await.take() {
                        let _ = pending.send(approved);
                    }
                }
                AppCommand::RejectUpdate => {
                    tracing::info!("update declined for this startup");
                }
                AppCommand::SetReasoningEffort(effort) => {
                    reasoning_effort = effort.clone();
                    gocode_core::save_global_config(
                        &config_path,
                        Some("nvidia"),
                        selected_model.as_deref(),
                        reasoning_effort.as_deref(),
                    )?;
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::ReasoningEffortChanged {
                            effort,
                            announce: true,
                        })
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not confirm reasoning-effort selection: {error}"
                            ))
                        })?;
                }
                AppCommand::SetPermissionMode(mode) => {
                    permission_mode = mode;
                }
                AppCommand::SetAutoCompact(enabled) => {
                    auto_compact_enabled = enabled;
                }
                AppCommand::SetSkillEnabled { name, enabled } => {
                    if let Err(error) = gocode_core::set_skill_enabled(
                        &bootstrap.project.gocode_dir,
                        &name,
                        enabled,
                    ) {
                        tracing::warn!("could not persist skill enable state: {error}");
                    }
                }
                AppCommand::CompactContext => {
                    let history_snapshot = current_session.lock().await.history.clone();
                    if history_snapshot.is_empty() {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentWarning(
                                "Nothing to compact yet.".into(),
                            ))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not report an empty compaction request: {error}"
                                ))
                            })?;
                    } else if let (Some(provider), Some(model)) = (&provider, &selected_model) {
                        let provider = provider.clone();
                        let model = model.clone();
                        let session_for_compact = current_session.clone();
                        let sessions_dir_for_compact = sessions_dir.clone();
                        let event_tx = driver.event_tx.clone();
                        tokio::spawn(async move {
                            let ctx = CompactionContext {
                                provider: &provider,
                                model: &model,
                                session: &session_for_compact,
                                sessions_dir: &sessions_dir_for_compact,
                            };
                            compact_and_report(&ctx, history_snapshot, false, &event_tx).await;
                        });
                    } else {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::ContextCompactionFailed(
                                "select a model before compacting".into(),
                            ))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not report a failed compaction: {error}"
                                ))
                            })?;
                    }
                }
                AppCommand::ClearConversation => {
                    let stale_id = {
                        let mut session = current_session.lock().await;
                        let stale_id = session.id.clone();
                        *session = gocode_core::SessionRecord::new();
                        stale_id
                    };
                    let _ = std::fs::remove_file(sessions_dir.join(format!("{stale_id}.json")));
                }
                AppCommand::NewSession => {
                    let id = {
                        let mut session = current_session.lock().await;
                        *session = gocode_core::SessionRecord::new();
                        session.id.clone()
                    };
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::SessionSwitched {
                            id,
                            name: "New session".into(),
                            transition: gocode_core::SessionTransition::New,
                            history: Vec::new(),
                        })
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not confirm the new session: {error}"
                            ))
                        })?;
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::DebugStateUpdated(
                            current_session.lock().await.debug.clone(),
                        ))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not publish new debug state: {error}"
                            ))
                        })?;
                }
                AppCommand::RequestSessionList => {
                    let summaries = gocode_core::list_sessions(&sessions_dir)
                        .unwrap_or_default()
                        .iter()
                        .map(gocode_core::SessionSummary::from)
                        .collect();
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::SessionListAvailable(summaries))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not send the session list: {error}"
                            ))
                        })?;
                }
                AppCommand::ResumeSession(id) => {
                    match gocode_core::load_session(&sessions_dir, &id) {
                        Ok(mut session) => {
                            session.last_used_at_unix = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |duration| {
                                    i64::try_from(duration.as_secs()).unwrap_or(0)
                                });
                            let _ = gocode_core::save_session(&sessions_dir, &session);
                            let id = session.id.clone();
                            let name = session.name.clone();
                            let history = session.history.clone();
                            let debug = session.debug.clone();
                            *current_session.lock().await = session;
                            driver
                                .event_tx
                                .send(gocode_core::AppEvent::SessionSwitched {
                                    id,
                                    name,
                                    transition: gocode_core::SessionTransition::Resumed,
                                    history,
                                })
                                .await
                                .map_err(|error| {
                                    AppError::Initialization(format!(
                                        "could not confirm the resumed session: {error}"
                                    ))
                                })?;
                            driver
                                .event_tx
                                .send(gocode_core::AppEvent::DebugStateUpdated(debug))
                                .await
                                .map_err(|error| {
                                    AppError::Initialization(format!(
                                        "could not publish resumed debug state: {error}"
                                    ))
                                })?;
                        }
                        Err(error) => {
                            driver
                                .event_tx
                                .send(gocode_core::AppEvent::SessionResumeFailed(
                                    error.to_string(),
                                ))
                                .await
                                .map_err(|send_error| {
                                    AppError::Initialization(format!(
                                        "could not report a failed session resume: {send_error}"
                                    ))
                                })?;
                        }
                    }
                }
                AppCommand::ForkSession => {
                    let (id, name, history, debug) = {
                        let mut session = current_session.lock().await;
                        let _ = gocode_core::save_session(&sessions_dir, &session);
                        let fork = session.fork();
                        let _ = gocode_core::save_session(&sessions_dir, &fork);
                        *session = fork;
                        (
                            session.id.clone(),
                            session.name.clone(),
                            session.history.clone(),
                            session.debug.clone(),
                        )
                    };
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::SessionSwitched {
                            id,
                            name,
                            transition: gocode_core::SessionTransition::Forked,
                            history,
                        })
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not confirm the forked session: {error}"
                            ))
                        })?;
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::DebugStateUpdated(debug))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not publish forked debug state: {error}"
                            ))
                        })?;
                }
                AppCommand::McpConnect(name) => {
                    if let Err(error) = mcp_runtime.connect(&name).await {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentWarning(format!(
                                "MCP server '{name}' failed to connect: {error}"
                            )))
                            .await;
                    }
                    tool_registry = Arc::new(build_full_registry(&mcp_runtime, &lsp_manager));
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::McpServersAvailable(
                            mcp_runtime.statuses(),
                        ))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not report MCP server status: {error}"
                            ))
                        })?;
                }
                AppCommand::McpDisconnect(name) => {
                    mcp_runtime.disconnect(&name);
                    tool_registry = Arc::new(build_full_registry(&mcp_runtime, &lsp_manager));
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::McpServersAvailable(
                            mcp_runtime.statuses(),
                        ))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not report MCP server status: {error}"
                            ))
                        })?;
                }
                AppCommand::McpAddServer { entry, api_key } => {
                    if let Some(api_key) = &api_key {
                        let account = gocode_mcp::api_key_account(&entry.name);
                        if let Err(error) = NativeCredentialStore::new()
                            .save_secret(&account, &SecretString::new(api_key.clone()))
                        {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::AgentWarning(format!(
                                    "could not store the API key for '{}': {error:?}",
                                    entry.name
                                )))
                                .await;
                        }
                    }

                    let mcp_path = bootstrap.project.mcp_config_path();
                    let mut project_mcp =
                        gocode_core::load_or_default_mcp_config(&mcp_path).unwrap_or_default();
                    project_mcp.upsert_server(entry.clone());
                    if let Err(error) = gocode_core::save_mcp_config(&mcp_path, &project_mcp) {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentWarning(format!(
                                "could not save MCP server '{}' to {}: {error}",
                                entry.name,
                                mcp_path.display()
                            )))
                            .await;
                    }

                    let name = entry.name.clone();
                    mcp_runtime.add_or_replace_server(entry);
                    if let Err(error) = mcp_runtime.connect(&name).await {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentWarning(format!(
                                "MCP server '{name}' failed to connect: {error}"
                            )))
                            .await;
                    }
                    tool_registry = Arc::new(build_full_registry(&mcp_runtime, &lsp_manager));
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::McpServersAvailable(
                            mcp_runtime.statuses(),
                        ))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not report MCP server status: {error}"
                            ))
                        })?;
                }
                AppCommand::McpAuthorize(name) => {
                    let Some(server) = mcp_runtime
                        .servers
                        .iter()
                        .find(|server| server.name == name)
                        .cloned()
                    else {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentWarning(format!(
                                "no MCP server named '{name}' is configured"
                            )))
                            .await;
                        continue;
                    };

                    match gocode_mcp::oauth::prepare_authorization(&server) {
                        Ok(pending) => {
                            let auth_url = pending.auth_url.clone();
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::McpAuthorizationUrlReady {
                                    server: name.clone(),
                                    url: auth_url.clone(),
                                })
                                .await;
                            let _ = webbrowser::open(&auth_url);

                            match gocode_mcp::oauth::complete_authorization(
                                pending,
                                std::time::Duration::from_secs(180),
                            )
                            .await
                            {
                                Ok(tokens) => {
                                    let account = gocode_mcp::api_key_account(&name);
                                    if let Ok(json) = serde_json::to_string(&tokens)
                                        && let Err(error) = NativeCredentialStore::new()
                                            .save_secret(&account, &SecretString::new(json))
                                    {
                                        let _ = driver
                                            .event_tx
                                            .send(gocode_core::AppEvent::AgentWarning(format!(
                                                "could not store the OAuth token for '{name}': \
                                                 {error:?}"
                                            )))
                                            .await;
                                    }
                                    if let Err(error) = mcp_runtime.connect(&name).await {
                                        let _ = driver
                                            .event_tx
                                            .send(gocode_core::AppEvent::AgentWarning(format!(
                                                "MCP server '{name}' failed to connect after \
                                                 authorizing: {error}"
                                            )))
                                            .await;
                                    }
                                }
                                Err(error) => {
                                    let _ = driver
                                        .event_tx
                                        .send(gocode_core::AppEvent::AgentWarning(format!(
                                            "authorization for '{name}' failed: {error}"
                                        )))
                                        .await;
                                }
                            }
                        }
                        Err(error) => {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::AgentWarning(format!(
                                    "could not start authorization for '{name}': {error}"
                                )))
                                .await;
                        }
                    }

                    tool_registry = Arc::new(build_full_registry(&mcp_runtime, &lsp_manager));
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::McpServersAvailable(
                            mcp_runtime.statuses(),
                        ))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not report MCP server status: {error}"
                            ))
                        })?;
                }
                AppCommand::WorktreeList => {
                    let runner = gocode_tools::process::TokioProcessRunner;
                    match gocode_tools::worktree::list_worktrees(&runner, &project_root).await {
                        Ok(entries) => {
                            let summaries = entries
                                .into_iter()
                                .map(|entry| gocode_core::WorktreeSummary {
                                    path: entry.path.display().to_string(),
                                    branch: entry.branch,
                                    is_main: entry.is_main,
                                })
                                .collect();
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::WorktreeListAvailable(summaries))
                                .await;
                        }
                        Err(error) => {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::WorktreeOperationFailed(
                                    error.to_string(),
                                ))
                                .await;
                        }
                    }
                }
                AppCommand::WorktreeCreate { name, branch } => {
                    let runner = gocode_tools::process::TokioProcessRunner;
                    let source = match branch {
                        gocode_core::WorktreeBranchSource::New => {
                            match gocode_tools::worktree::current_branch(&runner, &project_root)
                                .await
                            {
                                Ok(Some(base)) => {
                                    gocode_tools::worktree::BranchSource::New { base }
                                }
                                Ok(None) => {
                                    let _ = driver
                                        .event_tx
                                        .send(gocode_core::AppEvent::WorktreeOperationFailed(
                                            "the current worktree has no branch checked out \
                                             (detached HEAD); pass an existing branch instead"
                                                .into(),
                                        ))
                                        .await;
                                    continue;
                                }
                                Err(error) => {
                                    let _ = driver
                                        .event_tx
                                        .send(gocode_core::AppEvent::WorktreeOperationFailed(
                                            error.to_string(),
                                        ))
                                        .await;
                                    continue;
                                }
                            }
                        }
                        gocode_core::WorktreeBranchSource::Existing(existing) => {
                            gocode_tools::worktree::BranchSource::Existing(existing)
                        }
                    };
                    match gocode_tools::worktree::create_worktree(
                        &runner,
                        &project_root,
                        &name,
                        &source,
                    )
                    .await
                    {
                        Ok(entry) => {
                            project_root = entry.path.clone();
                            let branch_name = entry.branch.clone().unwrap_or_default();
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::WorktreeCreated {
                                    path: entry.path.display().to_string(),
                                    branch: branch_name,
                                })
                                .await;
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::ProjectContextAvailable {
                                    working_directory: project_root.display().to_string(),
                                })
                                .await;
                        }
                        Err(error) => {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::WorktreeOperationFailed(
                                    error.to_string(),
                                ))
                                .await;
                        }
                    }
                }
                AppCommand::WorktreeSwitch(target) => {
                    let runner = gocode_tools::process::TokioProcessRunner;
                    match gocode_tools::worktree::list_worktrees(&runner, &project_root).await {
                        Ok(entries) => {
                            match gocode_tools::worktree::resolve_target(&entries, &target) {
                                Ok(entry) => {
                                    project_root = entry.path.clone();
                                    let _ = driver
                                        .event_tx
                                        .send(gocode_core::AppEvent::WorktreeSwitched {
                                            path: entry.path.display().to_string(),
                                            branch: entry.branch.clone().unwrap_or_default(),
                                        })
                                        .await;
                                    let _ = driver
                                        .event_tx
                                        .send(gocode_core::AppEvent::ProjectContextAvailable {
                                            working_directory: project_root.display().to_string(),
                                        })
                                        .await;
                                }
                                Err(error) => {
                                    let _ = driver
                                        .event_tx
                                        .send(gocode_core::AppEvent::WorktreeOperationFailed(
                                            error.to_string(),
                                        ))
                                        .await;
                                }
                            }
                        }
                        Err(error) => {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::WorktreeOperationFailed(
                                    error.to_string(),
                                ))
                                .await;
                        }
                    }
                }
                AppCommand::WorktreeRemove(target) => {
                    let runner = gocode_tools::process::TokioProcessRunner;
                    // Resolve against the repository from the main worktree when the session is
                    // currently inside a linked worktree: `git worktree` subcommands work from any
                    // worktree, but resolving from wherever we happen to be keeps this correct even
                    // if the session is inside the worktree being removed.
                    match gocode_tools::worktree::remove_worktree(&runner, &project_root, &target)
                        .await
                    {
                        Ok(removed_path) => {
                            let switched_to = if removed_path == project_root {
                                if let Ok(entries) = gocode_tools::worktree::list_worktrees(
                                    &runner,
                                    &bootstrap.project.root,
                                )
                                .await
                                {
                                    let main_path = entries
                                        .into_iter()
                                        .find(|entry| entry.is_main)
                                        .map_or_else(
                                            || bootstrap.project.root.clone(),
                                            |entry| entry.path,
                                        );
                                    project_root = main_path.clone();
                                    let _ = driver
                                        .event_tx
                                        .send(gocode_core::AppEvent::ProjectContextAvailable {
                                            working_directory: project_root.display().to_string(),
                                        })
                                        .await;
                                    Some(main_path.display().to_string())
                                } else {
                                    project_root = bootstrap.project.root.clone();
                                    Some(project_root.display().to_string())
                                }
                            } else {
                                None
                            };
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::WorktreeRemoved {
                                    path: removed_path.display().to_string(),
                                    switched_to,
                                })
                                .await;
                        }
                        Err(error) => {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::WorktreeOperationFailed(
                                    error.to_string(),
                                ))
                                .await;
                        }
                    }
                }
                AppCommand::Undo(n) => {
                    apply_undo_redo(
                        &driver.event_tx,
                        &undo,
                        &undo_dir,
                        &project_root,
                        n,
                        false,
                        false,
                    )
                    .await;
                }
                AppCommand::UndoForce(n) => {
                    apply_undo_redo(
                        &driver.event_tx,
                        &undo,
                        &undo_dir,
                        &project_root,
                        n,
                        true,
                        false,
                    )
                    .await;
                }
                AppCommand::Redo(n) => {
                    apply_undo_redo(
                        &driver.event_tx,
                        &undo,
                        &undo_dir,
                        &project_root,
                        n,
                        false,
                        true,
                    )
                    .await;
                }
                AppCommand::RedoForce(n) => {
                    apply_undo_redo(
                        &driver.event_tx,
                        &undo,
                        &undo_dir,
                        &project_root,
                        n,
                        true,
                        true,
                    )
                    .await;
                }
                AppCommand::AcceptUpdate => {
                    match prepare_update(&paths.cache_dir, driver.event_tx.clone()).await {
                        Ok(staged) => {
                            staged_update = Some(staged);
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::UpdateReady {
                                    message: "The update is ready. Gocode will restart to \
                                              finish installing it."
                                        .into(),
                                })
                                .await;
                        }
                        Err(error) => {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::UpdateFailed(error))
                                .await;
                        }
                    }
                }
                AppCommand::RestartForUpdate => {
                    let Some(staged) = staged_update.take() else {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::UpdateFailed(
                                "No update is staged to install.".into(),
                            ))
                            .await;
                        continue;
                    };
                    // Installing here (spawning the Windows updater helper, or replacing the
                    // binary in place on Linux) would hand a still-alternate-screen, still-raw
                    // console down to the child process while the TUI task keeps running
                    // concurrently. Instead, only signal the TUI to shut down; the actual
                    // install happens after `tokio::join!` below has observed both tasks finish,
                    // so the terminal is guaranteed restored first (see docs/UPDATER.md §47).
                    //
                    // Keep looping instead of returning here: the TUI still needs to send its
                    // own `AppCommand::Exit` once it tears down (see `run_terminal`), and that
                    // send would fail with a "channel closed" error if this task had already
                    // dropped `driver.command_rx` by returning early.
                    restart_after_exit = Some(staged);
                    let _ = driver
                        .event_tx
                        .send(gocode_core::AppEvent::ExitForUpdate)
                        .await;
                }
                AppCommand::AgentSpawn {
                    task,
                    mode,
                    model,
                    worktree,
                } => {
                    let (Some(provider_ref), Some(session_model)) = (&provider, &selected_model)
                    else {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(
                                "Select a model before spawning a subagent.".into(),
                            ))
                            .await;
                        continue;
                    };
                    let model_id = model.unwrap_or_else(|| session_model.clone());
                    let request = SpawnRequest {
                        parent_session_id: current_session.lock().await.id.clone(),
                        task,
                        mode,
                        model: gocode_core::ModelId::new(model_id),
                        worktree_requested: worktree,
                        parent_permission_mode: permission_mode,
                        provider: Arc::new(provider_ref.clone()),
                        tools: tool_registry.clone(),
                        project_root: project_root.clone(),
                        worktree_runner: Arc::new(TokioProcessRunner),
                        instructions: instructions.clone(),
                        depth: 1,
                        parent_subagent_id: None,
                    };
                    if let Err(error) = subagent_manager.spawn(request).await {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(format!(
                                "Could not spawn subagent: {error}"
                            )))
                            .await;
                    }
                }
                AppCommand::AgentList => {
                    let records = subagent_manager.list().await;
                    let _ = driver
                        .event_tx
                        .send(gocode_core::AppEvent::AgentListAvailable(records))
                        .await;
                }
                AppCommand::AgentStatus(id) | AppCommand::AgentResult(id) => {
                    let record = subagent_manager.find(&id).await.map(Box::new);
                    let _ = driver
                        .event_tx
                        .send(gocode_core::AppEvent::AgentDetailAvailable { id, record })
                        .await;
                }
                AppCommand::AgentMessage { id, text } => {
                    let notice = match subagent_manager.find(&id).await {
                        Some(record) => {
                            match subagent_manager.send_message(&record.id, &text).await {
                                Ok(()) => {
                                    format!("Message queued for subagent {}.", short_id(&record.id))
                                }
                                Err(error) => format!(
                                    "Could not message subagent {}: {error}",
                                    short_id(&record.id)
                                ),
                            }
                        }
                        None => format!("No subagent matches '{id}'."),
                    };
                    let _ = driver
                        .event_tx
                        .send(gocode_core::AppEvent::AgentNotice(notice))
                        .await;
                }
                AppCommand::AgentStop(id) => {
                    let notice = match subagent_manager.find(&id).await {
                        Some(record) => match subagent_manager.stop(&record.id).await {
                            Ok(()) => format!("Stopping subagent {}...", short_id(&record.id)),
                            Err(error) => {
                                format!("Could not stop subagent {}: {error}", short_id(&record.id))
                            }
                        },
                        None => format!("No subagent matches '{id}'."),
                    };
                    let _ = driver
                        .event_tx
                        .send(gocode_core::AppEvent::AgentNotice(notice))
                        .await;
                }
                AppCommand::AgentApplyRequest(id) => match subagent_manager.find(&id).await {
                    Some(record)
                        if record.mode == gocode_core::SubagentMode::Implement
                            && record.branch.is_some() =>
                    {
                        let branch = record.branch.clone().unwrap_or_default();
                        let runner = TokioProcessRunner;
                        let notice = match worktree::current_branch(&runner, &project_root).await {
                            Ok(Some(base)) => {
                                compute_subagent_diff(
                                    &runner,
                                    &project_root,
                                    &record.id,
                                    &base,
                                    &branch,
                                )
                                .await
                            }
                            _ => AgentDiffOutcome::Notice(
                                "Could not determine the current branch.".into(),
                            ),
                        };
                        match notice {
                            AgentDiffOutcome::Diff(diff) => {
                                let _ = driver
                                    .event_tx
                                    .send(gocode_core::AppEvent::AgentDiffReady {
                                        id: record.id,
                                        diff,
                                    })
                                    .await;
                            }
                            AgentDiffOutcome::Notice(message) => {
                                let _ = driver
                                    .event_tx
                                    .send(gocode_core::AppEvent::AgentNotice(message))
                                    .await;
                            }
                        }
                    }
                    Some(_) => {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(
                                "Only implement-mode subagents with a worktree can be \
                                     applied."
                                    .into(),
                            ))
                            .await;
                    }
                    None => {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(format!(
                                "No subagent matches '{id}'."
                            )))
                            .await;
                    }
                },
                AppCommand::AgentApplyConfirm(id) => match subagent_manager.find(&id).await {
                    Some(record)
                        if record.mode == gocode_core::SubagentMode::Implement
                            && record.branch.is_some() =>
                    {
                        let branch = record.branch.clone().unwrap_or_default();
                        let runner = TokioProcessRunner;
                        match attempt_merge(&runner, &project_root, &record.id, &branch).await {
                            MergeAttempt::Applied(notice) => {
                                let _ = driver
                                    .event_tx
                                    .send(gocode_core::AppEvent::AgentNotice(notice))
                                    .await;
                            }
                            MergeAttempt::Conflict(files) => {
                                let _ = driver
                                    .event_tx
                                    .send(gocode_core::AppEvent::AgentMergeConflict {
                                        id: record.id,
                                        files,
                                    })
                                    .await;
                            }
                            MergeAttempt::Error(message) => {
                                let _ = driver
                                    .event_tx
                                    .send(gocode_core::AppEvent::AgentNotice(message))
                                    .await;
                            }
                        }
                    }
                    Some(_) => {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(
                                "Only implement-mode subagents with a worktree can be \
                                     applied."
                                    .into(),
                            ))
                            .await;
                    }
                    None => {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(format!(
                                "No subagent matches '{id}'."
                            )))
                            .await;
                    }
                },
                AppCommand::AgentResolveConflict { id, file, ours } => {
                    let runner = TokioProcessRunner;
                    if resolve_conflict_file(&runner, &project_root, &file, ours).await {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentConflictFileResolved {
                                id,
                                file,
                                ours,
                            })
                            .await;
                    } else {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(format!(
                                "Could not resolve '{file}'; try again or press Esc to abort the \
                                 merge."
                            )))
                            .await;
                    }
                }
                AppCommand::AgentFinishMerge(id) => {
                    let runner = TokioProcessRunner;
                    let (applied, message) = finish_merge(&runner, &project_root, &id).await;
                    let _ = driver
                        .event_tx
                        .send(gocode_core::AppEvent::AgentMergeFinished {
                            id,
                            applied,
                            message,
                        })
                        .await;
                }
                AppCommand::AgentAbortMerge(id) => {
                    let runner = TokioProcessRunner;
                    let message = abort_merge(&runner, &project_root).await;
                    let _ = driver
                        .event_tx
                        .send(gocode_core::AppEvent::AgentMergeFinished {
                            id,
                            applied: false,
                            message,
                        })
                        .await;
                }
                AppCommand::AgentCleanupRequest(id) => match subagent_manager.find(&id).await {
                    Some(record) if !record.status.is_terminal() => {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(format!(
                                "Subagent {} is still {}; stop it first with `/agent stop {}`.",
                                short_id(&record.id),
                                record.status.label(),
                                short_id(&record.id)
                            )))
                            .await;
                    }
                    Some(record) => {
                        let message = cleanup_warning(&record);
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentCleanupWarning {
                                id: record.id,
                                message,
                            })
                            .await;
                    }
                    None => {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(format!(
                                "No subagent matches '{id}'."
                            )))
                            .await;
                    }
                },
                AppCommand::AgentCleanupConfirm(id) => match subagent_manager.find(&id).await {
                    Some(record) if !record.status.is_terminal() => {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(format!(
                                "Subagent {} is still {}; stop it first.",
                                short_id(&record.id),
                                record.status.label()
                            )))
                            .await;
                    }
                    Some(record) => {
                        let notice =
                            cleanup_subagent(&subagent_manager, &project_root, record).await;
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(notice))
                            .await;
                    }
                    None => {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentNotice(format!(
                                "No subagent matches '{id}'."
                            )))
                            .await;
                    }
                },
            }
        }

        Err(AppError::Initialization(
            "interface closed without an exit command".into(),
        ))
    };
    let (tui_result, runtime_result) = tokio::join!(tui, runtime);
    tui_result.map_err(|error| AppError::Io(format!("terminal failed: {error}")))?;
    let staged_for_restart = runtime_result?;
    tracing::info!("application shutdown requested");

    // The TUI task has now fully returned, which means its `TerminalGuard` has already restored
    // the terminal (left the alternate screen, disabled raw mode). Only now is it safe to spawn
    // the updater helper / new binary, so it doesn't inherit a console still mid-render.
    if let Some(staged) = staged_for_restart
        && let Err(error) = install_and_restart(&staged)
    {
        eprintln!("Gocode update failed: {error}");
    }

    Ok(())
}

/// A downloaded, verified, and extracted update sitting in the cache directory, ready to be
/// installed over `installed` once the user confirms on the "Completed" screen.
struct StagedUpdate {
    staged_binary: PathBuf,
    installed: PathBuf,
}

/// Whether this build checks for and can install updates: Windows and Linux release builds
/// only (macOS isn't packaged, and debug builds would otherwise nag during development).
fn updates_supported() -> bool {
    (cfg!(windows) || cfg!(target_os = "linux")) && !cfg!(debug_assertions)
}

fn start_update_check(event_tx: mpsc::Sender<gocode_core::AppEvent>) {
    if !updates_supported() {
        return;
    }
    tokio::spawn(async move {
        let result = async {
            let source = gocode_updater::GitHubReleaseSource::new()?;
            let releases = source.stable_releases().await?;
            let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .map_err(|error| gocode_updater::UpdateError::InvalidRelease(error.to_string()))?;
            let suffix = gocode_updater::current_platform_archive_suffix()?;
            Ok::<_, gocode_updater::UpdateError>((
                current.clone(),
                gocode_updater::available_update(&current, releases, suffix),
            ))
        }
        .await;
        match result {
            Ok((current, Some(update))) => {
                let _ = event_tx
                    .send(gocode_core::AppEvent::UpdateAvailable {
                        current_version: current.to_string(),
                        version: update.version.to_string(),
                        notes: update.notes,
                    })
                    .await;
            }
            Ok((_, None)) => {}
            Err(error) => tracing::info!("update check skipped: {error}"),
        }
    });
}

/// Downloads, verifies, and extracts the newest available update for this platform into the
/// cache directory, without touching the installed executable. Reports download progress via
/// `event_tx` as it goes.
async fn prepare_update(
    cache_dir: &Path,
    event_tx: mpsc::Sender<gocode_core::AppEvent>,
) -> Result<StagedUpdate, String> {
    let archive_suffix =
        gocode_updater::current_platform_archive_suffix().map_err(|e| e.to_string())?;
    let source = gocode_updater::GitHubReleaseSource::new().map_err(|e| e.to_string())?;
    let releases = source.stable_releases().await.map_err(|e| e.to_string())?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    let update = gocode_updater::available_update(&current, releases, archive_suffix)
        .ok_or_else(|| "No newer update is available for this platform.".to_string())?;
    let staging = cache_dir.join("update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let client = reqwest::Client::builder()
        .user_agent("gocode-updater")
        .build()
        .map_err(|e| e.to_string())?;

    let progress_tx = event_tx.clone();
    let mut last_percent = None;
    let archive = gocode_updater::download_to_staging(
        &client,
        &update.archive.download_url,
        &staging,
        move |downloaded, total| {
            let percent = total.map(|total| {
                downloaded
                    .saturating_mul(100)
                    .checked_div(total)
                    .map_or(100, |value| u8::try_from(value.min(100)).unwrap_or(100))
            });
            if percent != last_percent {
                last_percent = percent;
                let _ = progress_tx.try_send(gocode_core::AppEvent::UpdateProgress {
                    percent,
                    message: "Downloading update…".into(),
                });
            }
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    let _ = event_tx
        .send(gocode_core::AppEvent::UpdateProgress {
            percent: Some(100),
            message: "Verifying update…".into(),
        })
        .await;
    let sums_url = gocode_updater::official_download_url(&update.checksums.download_url)
        .map_err(|e| e.to_string())?;
    let sums = client
        .get(sums_url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let expected =
        gocode_updater::checksum_for(&sums, &update.archive.name).map_err(|e| e.to_string())?;
    gocode_updater::verify_sha256(&archive, &expected).map_err(|e| e.to_string())?;

    let _ = event_tx
        .send(gocode_core::AppEvent::UpdateProgress {
            percent: Some(100),
            message: "Extracting update…".into(),
        })
        .await;
    let unpacked = staging.join("unpacked");
    let staged_binary = if cfg!(windows) {
        let (staged_app, _staged_updater) =
            gocode_updater::extract_windows_archive(&archive, &unpacked)
                .map_err(|e| e.to_string())?;
        staged_app
    } else {
        gocode_updater::extract_linux_archive(&archive, &unpacked).map_err(|e| e.to_string())?
    };
    let installed = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(StagedUpdate {
        staged_binary,
        installed,
    })
}

/// Installs a staged update over the running installation and starts the replacement process.
///
/// On Windows the running executable is locked, so this hands off to the separately installed
/// `gocode-updater.exe` helper, which waits for this process to exit before swapping files and
/// restarting. On Linux the executable can be replaced in place (an atomic rename doesn't
/// require the file to be closed), so this does it directly and restarts immediately.
fn install_and_restart(staged: &StagedUpdate) -> Result<(), String> {
    if cfg!(windows) {
        let updater = staged
            .installed
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("gocode-updater.exe");
        if !updater.is_file() {
            return Err(
                "The installed gocode-updater.exe is missing; reinstall Gocode and try again."
                    .into(),
            );
        }
        std::process::Command::new(updater)
            .arg(std::process::id().to_string())
            .arg(&staged.staged_binary)
            .arg(&staged.installed)
            .spawn()
            .map_err(|error| {
                format!("Gocode could not restart automatically ({error}). Please reopen Gocode manually.")
            })?;
        return Ok(());
    }
    gocode_updater::replace_with_rollback(&staged.staged_binary, &staged.installed)
        .map_err(|error| error.to_string())?;
    gocode_updater::restart(&staged.installed, &[]).map_err(|error| {
        format!(
            "Gocode was updated but could not restart automatically ({error}). Please reopen \
             Gocode manually."
        )
    })
}

fn init_logging(
    state_dir: &std::path::Path,
) -> Result<tracing_appender::non_blocking::WorkerGuard, AppError> {
    let logs_dir = state_dir.join("logs");
    std::fs::create_dir_all(&logs_dir).map_err(|error| {
        AppError::Io(format!("could not create {}: {error}", logs_dir.display()))
    })?;
    let appender = tracing_appender::rolling::daily(logs_dir, "gocode.jsonl");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("gocode", tracing::Level::INFO),
            ),
    );
    tracing::subscriber::set_global_default(subscriber).map_err(|error| {
        AppError::Initialization(format!("could not initialize logging: {error}"))
    })?;

    Ok(guard)
}

fn application_paths(
    platform: Platform,
    environment: EnvironmentPaths,
) -> Result<PlatformPaths, gocode_core::AppError> {
    PlatformPaths::from_environment(platform, environment)
}

fn current_platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

fn process_environment() -> EnvironmentPaths {
    EnvironmentPaths {
        home: std::env::var_os("HOME").map(Into::into),
        user_profile: std::env::var_os("USERPROFILE").map(Into::into),
        xdg: gocode_core::XdgDirectories {
            config_home: std::env::var_os("XDG_CONFIG_HOME").map(Into::into),
            state_home: std::env::var_os("XDG_STATE_HOME").map(Into::into),
            cache_home: std::env::var_os("XDG_CACHE_HOME").map(Into::into),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use gocode_core::{EnvironmentPaths, Platform};

    use super::{
        AgentDiffOutcome, MergeAttempt, abort_merge, application_paths, attempt_merge,
        cleanup_subagent, cleanup_warning, compute_subagent_diff, finish_merge,
        parse_conflicting_files, resolve_conflict_file, split_for_compaction,
    };

    #[test]
    fn application_paths_use_the_platform_environment_contract() {
        let paths = application_paths(
            Platform::Linux,
            EnvironmentPaths {
                home: Some("/home/alice".into()),
                ..EnvironmentPaths::default()
            },
        )
        .expect("paths should resolve");

        assert_eq!(paths.config_dir, Path::new("/home/alice/.config/gocode"));
    }

    fn user(text: &str) -> gocode_core::ChatMessage {
        gocode_core::ChatMessage::User(text.into())
    }

    fn assistant(text: &str) -> gocode_core::ChatMessage {
        gocode_core::ChatMessage::assistant_text(text)
    }

    fn tool_result(call_id: &str, content: &str) -> gocode_core::ChatMessage {
        gocode_core::ChatMessage::Tool {
            tool_call_id: call_id.into(),
            content: content.into(),
        }
    }

    #[test]
    fn split_for_compaction_keeps_the_last_n_user_turns_intact() {
        let history = vec![
            user("turn 1"),
            assistant("reply 1"),
            user("turn 2"),
            assistant("reply 2"),
            user("turn 3"),
            assistant("reply 3"),
        ];

        let (older, recent) = split_for_compaction(&history, 2);

        assert_eq!(older, vec![user("turn 1"), assistant("reply 1")]);
        assert_eq!(
            recent,
            vec![
                user("turn 2"),
                assistant("reply 2"),
                user("turn 3"),
                assistant("reply 3"),
            ]
        );
    }

    #[test]
    fn split_for_compaction_never_cuts_a_tool_call_sequence() {
        let history = vec![
            user("turn 1"),
            gocode_core::ChatMessage::Assistant {
                text: None,
                tool_calls: vec![gocode_core::ProviderToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({}),
                }],
            },
            tool_result("call-1", "file contents"),
            assistant("reply 1"),
            user("turn 2"),
            assistant("reply 2"),
        ];

        let (older, recent) = split_for_compaction(&history, 1);

        // The split lands right before "turn 2"; the tool-call/tool-result pair from turn 1
        // stays fully inside `older`, never straddling the boundary.
        assert_eq!(older.len(), 4);
        assert_eq!(recent, vec![user("turn 2"), assistant("reply 2")]);
    }

    #[test]
    fn split_for_compaction_keeps_everything_when_there_are_too_few_turns() {
        let history = vec![user("only turn"), assistant("only reply")];

        let (older, recent) = split_for_compaction(&history, 2);

        assert!(older.is_empty());
        assert_eq!(recent, history);
    }

    #[test]
    fn split_for_compaction_treats_an_empty_history_as_nothing_to_compact() {
        let (older, recent) = split_for_compaction(&[], 2);
        assert!(older.is_empty());
        assert!(recent.is_empty());
    }

    fn sample_record(mode: gocode_core::SubagentMode) -> gocode_core::SubagentRecord {
        gocode_core::SubagentRecord::new(
            "session-1".into(),
            "investigate flaky login test".into(),
            mode,
            "test-model".into(),
            !mode.allows_writes(),
            gocode_core::PermissionMode::Auto,
        )
    }

    #[test]
    fn parses_conflicting_files_from_git_merge_output() {
        let stdout = "Auto-merging src/lib.rs\n\
                       CONFLICT (content): Merge conflict in src/lib.rs\n\
                       CONFLICT (add/add): Merge conflict in docs/README.md\n\
                       Automatic merge failed; fix conflicts and then commit the result.\n";
        assert_eq!(
            parse_conflicting_files(stdout),
            vec!["src/lib.rs".to_string(), "docs/README.md".to_string()]
        );
    }

    #[test]
    fn parsing_conflicting_files_from_unexpected_output_is_an_empty_list_not_an_error() {
        assert!(parse_conflicting_files("some unrelated git output\n").is_empty());
    }

    #[test]
    fn cleanup_warning_mentions_the_worktree_when_there_is_one() {
        let mut record = sample_record(gocode_core::SubagentMode::Implement);
        record.worktree_path = Some(PathBuf::from("/tmp/repo-worktrees/subagent-abc"));
        record.branch = Some("subagent-abc".into());
        let message = cleanup_warning(&record);
        assert!(message.contains("subagent-abc"));
        assert!(message.contains("discarded"));
    }

    #[test]
    fn cleanup_warning_without_a_worktree_only_mentions_metadata() {
        let record = sample_record(gocode_core::SubagentMode::Research);
        let message = cleanup_warning(&record);
        assert!(!message.contains("worktree"));
        assert!(message.contains("metadata"));
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git should be installed for this test");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repo with `main` at one commit and a `feature` branch one commit ahead, isolated per
    /// test under the OS temp dir. Mirrors the fixture pattern in `gocode-tools`' worktree tests.
    fn fixture_repo_with_a_feature_branch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "gocode-main-subagent-tests-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-q", "-b", "main"]);
        run_git(&root, &["config", "user.email", "test@example.com"]);
        run_git(&root, &["config", "user.name", "Test"]);
        run_git(&root, &["config", "core.autocrlf", "false"]);
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "init"]);
        run_git(&root, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(root.join("feature.txt"), "new file\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "add feature file"]);
        run_git(&root, &["checkout", "-q", "main"]);
        root
    }

    #[tokio::test]
    async fn computes_a_diff_between_the_base_and_the_subagent_branch() {
        let root = fixture_repo_with_a_feature_branch("diff");
        let runner = gocode_tools::process::TokioProcessRunner;

        let outcome = compute_subagent_diff(&runner, &root, "sub1", "main", "feature").await;
        let AgentDiffOutcome::Diff(diff) = outcome else {
            panic!("expected a diff, got a notice");
        };
        assert!(diff.contains("feature.txt"));
        assert!(diff.contains("merging into main"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn applying_a_clean_branch_merges_it_into_the_current_branch() {
        let root = fixture_repo_with_a_feature_branch("apply-clean");
        let runner = gocode_tools::process::TokioProcessRunner;

        let outcome = attempt_merge(&runner, &root, "sub1", "feature").await;
        let MergeAttempt::Applied(notice) = outcome else {
            panic!("expected the merge to apply cleanly");
        };
        assert!(notice.contains("Applied"), "unexpected notice: {notice}");
        assert!(root.join("feature.txt").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    /// Diverges `main` so merging `feature` conflicts on `feature.txt`, then attempts the merge.
    /// Returns the repo root and the reported conflicting files.
    async fn conflicted_merge_fixture(
        name: &str,
        runner: &gocode_tools::process::TokioProcessRunner,
    ) -> (PathBuf, Vec<String>) {
        let root = fixture_repo_with_a_feature_branch(name);
        std::fs::write(root.join("feature.txt"), "conflicting content\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "conflicting change on main"]);

        let outcome = attempt_merge(runner, &root, "sub1", "feature").await;
        let MergeAttempt::Conflict(files) = outcome else {
            panic!("expected a conflict");
        };
        (root, files)
    }

    fn git_status_is_clean(root: &Path) -> bool {
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(root)
            .output()
            .expect("git status should run");
        String::from_utf8_lossy(&status.stdout).trim().is_empty()
    }

    #[tokio::test]
    async fn a_conflicting_merge_is_left_in_progress_for_the_guided_resolver() {
        let runner = gocode_tools::process::TokioProcessRunner;
        let (root, files) = conflicted_merge_fixture("apply-conflict", &runner).await;
        assert_eq!(files, vec!["feature.txt".to_string()]);

        // Left in progress (not aborted), unlike the old behavior: the guided resolver needs the
        // conflict markers and index state still present to work through.
        assert!(
            !git_status_is_clean(&root),
            "expected the conflict to still be in progress"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn resolving_every_file_and_finishing_completes_the_merge_keeping_the_chosen_side() {
        let runner = gocode_tools::process::TokioProcessRunner;
        let (root, files) = conflicted_merge_fixture("apply-resolve", &runner).await;

        for file in &files {
            assert!(
                resolve_conflict_file(&runner, &root, file, false).await,
                "keeping theirs should resolve {file}"
            );
        }

        let (applied, message) = finish_merge(&runner, &root, "sub1").await;
        assert!(applied, "unexpected message: {message}");
        assert_eq!(
            std::fs::read_to_string(root.join("feature.txt")).unwrap(),
            "new file\n",
            "keeping theirs should have kept the feature branch's content"
        );
        assert!(git_status_is_clean(&root));

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn aborting_a_conflicted_merge_leaves_the_workspace_clean() {
        let runner = gocode_tools::process::TokioProcessRunner;
        let (root, _files) = conflicted_merge_fixture("apply-abort", &runner).await;

        let message = abort_merge(&runner, &root).await;
        assert!(message.contains("aborted"), "unexpected message: {message}");

        assert!(git_status_is_clean(&root));
        assert_eq!(
            std::fs::read_to_string(root.join("feature.txt")).unwrap(),
            "conflicting content\n"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn cleanup_removes_the_worktree_and_the_record() {
        let root = fixture_repo_with_a_feature_branch("cleanup");
        let runner = gocode_tools::process::TokioProcessRunner;
        let entry = gocode_tools::worktree::create_worktree(
            &runner,
            &root,
            "subagent-cleanup",
            &gocode_tools::worktree::BranchSource::New {
                base: "main".into(),
            },
        )
        .await
        .expect("worktree should be created");

        let state_dir = std::env::temp_dir().join(format!(
            "gocode-main-subagent-cleanup-state-{}",
            std::process::id()
        ));
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let manager = gocode_agent::SubagentManager::new(
            gocode_core::subagents_dir(&state_dir),
            gocode_agent::SubagentLimits::default(),
            event_tx,
        );
        let mut record = sample_record(gocode_core::SubagentMode::Implement);
        record.worktree_path = Some(entry.path.clone());
        record.branch = entry.branch.clone();
        gocode_core::save_subagent(&gocode_core::subagents_dir(&state_dir), &record).unwrap();

        let notice = cleanup_subagent(&manager, &root, record.clone()).await;
        assert!(notice.contains("Removed"), "unexpected notice: {notice}");
        assert!(!entry.path.exists());
        assert!(manager.get(&record.id).await.is_none());

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&state_dir).ok();
    }
}
