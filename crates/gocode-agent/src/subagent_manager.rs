//! Supervisor-side execution engine for subagents: bounded, isolated workers the main agent
//! delegates concrete tasks to. See the subagent architecture plan for the full design; this
//! module owns spawning, concurrency, timeouts, cooperative stop, message delivery, and mapping
//! a finished run onto a [`gocode_core::SubagentResult`].
//!
//! Subagents never run with permissions more permissive than the parent session that spawned
//! them (`spawn` computes the effective mode itself; see [`effective_subagent_mode`]).
//!
//! Nesting is allowed but strictly bounded: a subagent at depth `1` (spawned directly by the
//! main session) gets an `agent_spawn` tool (see [`crate::agent_spawn_tool::AgentSpawnTool`]) it
//! can call to delegate a bounded subtask to a depth-`2` child and wait for its result. A depth-2
//! subagent does **not** get that tool — [`MAX_SUBAGENT_DEPTH`] caps nesting at one level, so
//! there is never an unbounded delegation chain. Every nested spawn still goes through the same
//! `spawn()` validation (permission inheritance, worktree rules, depth check) as a top-level one.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gocode_core::{
    CancellationToken, ChatMessage, ModelId, PermissionMode, Provider, SubagentMessageRole,
    SubagentMode, SubagentRecord, SubagentResult, SubagentStatus,
};
use gocode_tools::{
    FileChangeObserver, ToolRegistry,
    permissions::{
        AlwaysDenyResolver, DefaultPermissionPolicy, PermissionContext, PermissionPolicy,
    },
    process::{ProcessRunner, redact_secrets},
    worktree::{self, BranchSource},
};
use tokio::sync::{Mutex, Semaphore, mpsc};

use crate::{Agent, AgentLimits, AgentRequest};

/// Bounds applied to every subagent run, derived from (and never exceeding) the parent session's
/// own limits.
#[derive(Debug, Clone, Copy)]
pub struct SubagentLimits {
    /// Maximum number of subagents allowed to run at once; further spawns stay `Queued`.
    pub max_concurrent: usize,
    /// Wall-clock budget for one subagent's entire run, across every turn and follow-up message.
    pub per_task_timeout: Duration,
    /// Forwarded as [`AgentLimits::max_turns`] for each of the subagent's underlying runs.
    pub max_turns: usize,
    /// Forwarded as [`AgentLimits::max_total_tool_calls`] for each underlying run.
    pub max_total_tool_calls: usize,
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            per_task_timeout: Duration::from_secs(15 * 60),
            max_turns: 12,
            max_total_tool_calls: 30,
        }
    }
}

/// A fact the supervisor observes about a subagent's lifecycle, meant to be bridged onto an
/// interface's own event type (mirrors how [`crate::AgentEvent`] is bridged today).
#[derive(Debug, Clone)]
pub enum SubagentEvent {
    /// A new subagent was created and persisted.
    Spawned(SubagentRecord),
    /// A subagent's status changed.
    StatusChanged { id: String, status: SubagentStatus },
    /// A short, already-redacted progress note.
    Progress { id: String, line: String },
    /// A subagent finished (successfully or not); carries the final record.
    Finished(SubagentRecord),
}

/// What went wrong trying to spawn or otherwise act on a subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentError {
    /// Plan mode forces every subagent to be read-only; an implement/worktree request was denied.
    ImplementDeniedInPlanMode,
    /// Only [`SubagentMode::Implement`] may use a worktree.
    WorktreeRequiresImplementMode,
    /// The requested worktree name is already claimed by another running subagent.
    WorktreeInUse(String),
    /// Worktree creation failed.
    Worktree(String),
    /// No subagent matches the given id.
    NotFound(String),
    /// The action does not apply to the subagent's current status.
    InvalidState {
        status: &'static str,
        action: &'static str,
    },
    /// The record could not be persisted.
    Persistence(String),
    /// The requested nesting depth exceeds [`MAX_SUBAGENT_DEPTH`].
    MaxDepthExceeded { depth: usize, max: usize },
}

impl std::fmt::Display for SubagentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ImplementDeniedInPlanMode => {
                write!(f, "plan mode only allows read-only subagents")
            }
            Self::WorktreeRequiresImplementMode => {
                write!(f, "only --mode implement subagents may use a worktree")
            }
            Self::WorktreeInUse(name) => write!(f, "worktree '{name}' is already in use"),
            Self::Worktree(message) => write!(f, "could not create worktree: {message}"),
            Self::NotFound(id) => write!(f, "no subagent matches '{id}'"),
            Self::InvalidState { status, action } => {
                write!(f, "cannot {action} a subagent that is {status}")
            }
            Self::Persistence(message) => write!(f, "could not persist subagent: {message}"),
            Self::MaxDepthExceeded { depth, max } => write!(
                f,
                "subagent nesting depth {depth} exceeds the maximum of {max}"
            ),
        }
    }
}

impl std::error::Error for SubagentError {}

/// Maximum subagent nesting depth: `1` is a subagent spawned directly by the main session, `2`
/// is one spawned by that subagent's own `agent_spawn` tool call. A depth-2 subagent never gets
/// the `agent_spawn` tool, so nesting cannot go deeper than this.
pub const MAX_SUBAGENT_DEPTH: usize = 2;

/// A depth-1 subagent's `agent_spawn` call blocks holding its own concurrency slot while it
/// waits for its child, so nesting needs at least 2 slots to make progress — with only 1, the
/// parent would permanently hold the only slot its own child needs. Rather than silently
/// deadlocking (bounded only by `per_task_timeout`) or forcing a floor that would change the
/// behavior of an explicit `max_concurrent: 1` configuration, the `agent_spawn` tool is simply
/// never registered when `SubagentLimits::max_concurrent` is below this — a subagent then has no
/// way to attempt a nested spawn in the first place.
const MIN_CONCURRENCY_FOR_NESTING: usize = 2;

/// Everything needed to spawn one subagent.
pub struct SpawnRequest {
    /// The session that is delegating this task.
    pub parent_session_id: String,
    /// The objective given to the subagent, in the supervisor's own words.
    pub task: String,
    /// Requested work mode.
    pub mode: SubagentMode,
    /// Model id the subagent's runs use.
    pub model: ModelId,
    /// Whether an explicit `--worktree` was requested (only valid alongside `Implement`).
    pub worktree_requested: bool,
    /// The parent session's current permission mode; the subagent's effective permissions never
    /// exceed this.
    pub parent_permission_mode: PermissionMode,
    /// Provider used to drive the subagent's own [`Agent`].
    pub provider: Arc<dyn Provider>,
    /// Tool registry the subagent's [`Agent`] executes against. `spawn` adds an `agent_spawn`
    /// tool on top of this when `depth < MAX_SUBAGENT_DEPTH`; this registry itself should never
    /// already contain one (each spawn — top-level or nested — starts from the same plain base).
    pub tools: Arc<ToolRegistry>,
    /// Repository root the main session is working in.
    pub project_root: PathBuf,
    /// Used to shell out to `git worktree` for implement-mode subagents.
    pub worktree_runner: Arc<dyn ProcessRunner>,
    /// Parsed `AGENTS.md` / project instructions, forwarded to the subagent as-is.
    pub instructions: Option<String>,
    /// Nesting depth this subagent will run at: `1` for a top-level spawn from the main session,
    /// `2` for one spawned by a depth-1 subagent's `agent_spawn` call. Rejected above
    /// [`MAX_SUBAGENT_DEPTH`].
    pub depth: usize,
    /// The subagent that spawned this one via `agent_spawn`, when this is a nested spawn.
    pub parent_subagent_id: Option<String>,
}

/// Supervisor-side execution engine: owns every subagent's record, concurrency slot, and
/// cancellation handle.
///
/// Cheaply [`Clone`]: every field is already `Arc`/`Mutex`-wrapped or `Copy`, so a clone is a
/// shared handle onto the same state, not an independent manager. `spawn` uses this to hand a
/// depth-1 subagent's `agent_spawn` tool a handle back onto itself, so a nested spawn goes
/// through the exact same validation and concurrency pool as a top-level one.
#[derive(Clone)]
pub struct SubagentManager {
    dir: PathBuf,
    records: Arc<Mutex<HashMap<String, SubagentRecord>>>,
    cancel_tokens: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pending_messages: Arc<Mutex<HashMap<String, Vec<String>>>>,
    claimed_worktrees: Arc<Mutex<HashSet<PathBuf>>>,
    concurrency: Arc<Semaphore>,
    limits: SubagentLimits,
    event_tx: mpsc::Sender<SubagentEvent>,
    /// Observer notified after a subagent's tool call writes a file (e.g. an LSP client keeping
    /// open documents in sync). `None` by default; see [`Self::with_file_change_observer`].
    file_change_observer: Option<Arc<dyn FileChangeObserver>>,
}

impl SubagentManager {
    /// Creates a manager persisting under `dir` (typically
    /// `gocode_core::subagents_dir(state_dir)`), applying `limits`, and emitting every lifecycle
    /// fact on `event_tx`.
    #[must_use]
    pub fn new(
        dir: PathBuf,
        limits: SubagentLimits,
        event_tx: mpsc::Sender<SubagentEvent>,
    ) -> Self {
        Self {
            dir,
            records: Arc::new(Mutex::new(HashMap::new())),
            cancel_tokens: Arc::new(Mutex::new(HashMap::new())),
            pending_messages: Arc::new(Mutex::new(HashMap::new())),
            claimed_worktrees: Arc::new(Mutex::new(HashSet::new())),
            concurrency: Arc::new(Semaphore::new(limits.max_concurrent.max(1))),
            limits,
            event_tx,
            file_change_observer: None,
        }
    }

    /// Attaches an observer every subagent's [`Agent`] will notify after a tool call writes a
    /// file, same as [`Agent::with_file_change_observer`] on the top-level agent. Optional: a
    /// subagent run works identically without one.
    #[must_use]
    pub fn with_file_change_observer(mut self, observer: Arc<dyn FileChangeObserver>) -> Self {
        self.file_change_observer = Some(observer);
        self
    }

    /// Validates and starts one subagent, returning its id immediately. The actual run happens on
    /// a spawned task; over-capacity spawns stay `Queued` until a concurrency slot frees up.
    ///
    /// # Errors
    ///
    /// Returns [`SubagentError`] when the request violates Plan-mode restrictions, requests a
    /// worktree outside implement mode, targets an already-claimed worktree, or worktree creation
    /// fails.
    pub async fn spawn(&self, request: SpawnRequest) -> Result<String, SubagentError> {
        if request.depth > MAX_SUBAGENT_DEPTH {
            return Err(SubagentError::MaxDepthExceeded {
                depth: request.depth,
                max: MAX_SUBAGENT_DEPTH,
            });
        }
        if request.worktree_requested && request.mode != SubagentMode::Implement {
            return Err(SubagentError::WorktreeRequiresImplementMode);
        }
        if request.parent_permission_mode == PermissionMode::Plan
            && (request.mode == SubagentMode::Implement || request.worktree_requested)
        {
            return Err(SubagentError::ImplementDeniedInPlanMode);
        }

        let effective_mode = effective_subagent_mode(request.mode, request.parent_permission_mode);
        let read_only = !effective_mode.allows_writes();

        let mut record = SubagentRecord::new(
            request.parent_session_id,
            request.task.clone(),
            effective_mode,
            request.model.as_str().to_string(),
            read_only,
            request.parent_permission_mode,
        );
        record.depth = request.depth;
        record
            .parent_subagent_id
            .clone_from(&request.parent_subagent_id);

        if effective_mode.allows_writes() {
            let name = format!("subagent-{}", short_id(&record.id));
            {
                let mut claimed = self.claimed_worktrees.lock().await;
                let root = worktree::worktrees_root(&request.project_root)
                    .map_err(|error| SubagentError::Worktree(error.to_string()))?;
                let target = root.join(&name);
                if claimed.contains(&target) {
                    return Err(SubagentError::WorktreeInUse(name));
                }
                claimed.insert(target);
            }
            let base =
                worktree::current_branch(request.worktree_runner.as_ref(), &request.project_root)
                    .await
                    .map_err(|error| SubagentError::Worktree(error.to_string()))?
                    .unwrap_or_else(|| "main".to_string());
            let entry = worktree::create_worktree(
                request.worktree_runner.as_ref(),
                &request.project_root,
                &name,
                &BranchSource::New { base },
            )
            .await
            .map_err(|error| SubagentError::Worktree(error.to_string()))?;
            record.worktree_path = Some(entry.path);
            record.branch = entry.branch;
        }

        self.persist(&record).map_err(SubagentError::Persistence)?;
        let _ = self
            .event_tx
            .send(SubagentEvent::Spawned(record.clone()))
            .await;

        let id = record.id.clone();
        let cancellation = CancellationToken::new();
        self.cancel_tokens
            .lock()
            .await
            .insert(id.clone(), cancellation.clone());
        self.records.lock().await.insert(id.clone(), record.clone());

        let worker = SubagentWorker {
            dir: self.dir.clone(),
            records: self.records.clone(),
            pending_messages: self.pending_messages.clone(),
            concurrency: self.concurrency.clone(),
            limits: self.limits,
            event_tx: self.event_tx.clone(),
            project_root: request.project_root,
            instructions: request.instructions,
            manager: Arc::new(self.clone()),
            worktree_runner: request.worktree_runner,
            file_change_observer: self.file_change_observer.clone(),
        };
        tokio::spawn(worker.run(record, request.provider, request.tools, cancellation));

        Ok(id)
    }

    /// Appends a follow-up instruction for a running subagent, delivered once its current turn
    /// finishes (there is no mid-turn injection point, see the architecture plan's open
    /// questions). Recorded in the subagent's message history immediately either way.
    ///
    /// # Errors
    ///
    /// Returns [`SubagentError::NotFound`] when `id` is unknown, or
    /// [`SubagentError::InvalidState`] when the subagent has already reached a terminal status.
    pub async fn send_message(&self, id: &str, text: &str) -> Result<(), SubagentError> {
        let redacted = redact_secrets(text);
        let mut records = self.records.lock().await;
        let record = records
            .get_mut(id)
            .ok_or_else(|| SubagentError::NotFound(id.to_string()))?;
        if record.status.is_terminal() {
            return Err(SubagentError::InvalidState {
                status: record.status.label(),
                action: "message",
            });
        }
        record.push_message(SubagentMessageRole::Supervisor, redacted.clone());
        self.save_locked(record)
            .map_err(SubagentError::Persistence)?;
        drop(records);

        self.pending_messages
            .lock()
            .await
            .entry(id.to_string())
            .or_default()
            .push(redacted);
        Ok(())
    }

    /// Cooperatively stops a running subagent: cancels its token, preserves whatever partial
    /// result exists, and never touches its worktree.
    ///
    /// # Errors
    ///
    /// Returns [`SubagentError::NotFound`] when `id` is unknown, or
    /// [`SubagentError::InvalidState`] when it has already reached a terminal status.
    pub async fn stop(&self, id: &str) -> Result<(), SubagentError> {
        let records = self.records.lock().await;
        let record = records
            .get(id)
            .ok_or_else(|| SubagentError::NotFound(id.to_string()))?;
        if record.status.is_terminal() {
            return Err(SubagentError::InvalidState {
                status: record.status.label(),
                action: "stop",
            });
        }
        drop(records);

        if let Some(token) = self.cancel_tokens.lock().await.get(id) {
            token.cancel();
        }
        Ok(())
    }

    /// Returns the current record for `id`, read from disk so it reflects subagents from earlier
    /// sessions (including ones [`gocode_core::recover_interrupted`] marked at startup), not just
    /// ones spawned by this [`SubagentManager`] instance.
    #[allow(
        clippy::unused_async,
        reason = "kept async: every caller already awaits manager methods, and this read may \
                  move behind spawn_blocking or true async I/O later without changing call sites"
    )]
    pub async fn get(&self, id: &str) -> Option<SubagentRecord> {
        gocode_core::load_subagent(&self.dir, id).ok()
    }

    /// Lists every persisted subagent, most recently updated first. Like [`Self::get`], reads
    /// from disk rather than this instance's own in-memory cache.
    #[allow(
        clippy::unused_async,
        reason = "kept async: every caller already awaits manager methods, and this read may \
                  move behind spawn_blocking or true async I/O later without changing call sites"
    )]
    pub async fn list(&self) -> Vec<SubagentRecord> {
        gocode_core::list_subagents(&self.dir).unwrap_or_default()
    }

    /// Resolves a possibly-abbreviated id (as shown by `/agents`) to the one full record it
    /// matches. Returns `None` when nothing matches or more than one record shares the prefix.
    pub async fn find(&self, id_or_prefix: &str) -> Option<SubagentRecord> {
        if let Some(record) = self.get(id_or_prefix).await {
            return Some(record);
        }
        let mut matches = self
            .list()
            .await
            .into_iter()
            .filter(|record| record.id.starts_with(id_or_prefix));
        let first = matches.next()?;
        if matches.next().is_none() {
            Some(first)
        } else {
            None
        }
    }

    /// Deletes a subagent's persisted record. Callers are responsible for removing its worktree
    /// first, if any (see `gocode_tools::worktree::remove_worktree`), so a failed worktree
    /// removal never leaves the record silently gone with nothing to point at the orphaned files.
    ///
    /// # Errors
    ///
    /// Returns [`SubagentError::Persistence`] when the record file exists but cannot be removed.
    pub async fn delete(&self, id: &str) -> Result<(), SubagentError> {
        self.records.lock().await.remove(id);
        self.cancel_tokens.lock().await.remove(id);
        gocode_core::delete_subagent(&self.dir, id)
            .map_err(|error| SubagentError::Persistence(error.to_string()))
    }

    fn save_locked(&self, record: &SubagentRecord) -> Result<(), String> {
        gocode_core::save_subagent(&self.dir, record).map_err(|error| error.to_string())
    }

    fn persist(&self, record: &SubagentRecord) -> Result<(), String> {
        self.save_locked(record)
    }
}

/// Computes the mode a subagent actually runs under, never more permissive than the parent
/// session's own [`PermissionMode`]: Plan mode forces every subagent to a read-only mode
/// regardless of what was requested.
#[must_use]
pub fn effective_subagent_mode(requested: SubagentMode, parent: PermissionMode) -> SubagentMode {
    if parent == PermissionMode::Plan && requested.allows_writes() {
        SubagentMode::Research
    } else {
        requested
    }
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Owns the state one running subagent task needs, separate from [`SubagentManager`] so the
/// spawned future does not hold the manager's own locks while it runs.
struct SubagentWorker {
    dir: PathBuf,
    records: Arc<Mutex<HashMap<String, SubagentRecord>>>,
    pending_messages: Arc<Mutex<HashMap<String, Vec<String>>>>,
    concurrency: Arc<Semaphore>,
    limits: SubagentLimits,
    event_tx: mpsc::Sender<SubagentEvent>,
    project_root: PathBuf,
    instructions: Option<String>,
    /// Handle back onto the owning manager, given to a depth-eligible subagent's `agent_spawn`
    /// tool so a nested spawn goes through the exact same validation and concurrency pool.
    manager: Arc<SubagentManager>,
    /// Forwarded to a nested spawn's `agent_spawn` tool for its own worktree creation.
    worktree_runner: Arc<dyn ProcessRunner>,
    /// Mirrors [`SubagentManager::file_change_observer`]; applied to this worker's own [`Agent`].
    file_change_observer: Option<Arc<dyn FileChangeObserver>>,
}

impl SubagentWorker {
    async fn run(
        self,
        mut record: SubagentRecord,
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        cancellation: CancellationToken,
    ) {
        let Ok(_permit) = self.concurrency.clone().acquire_owned().await else {
            return;
        };

        if cancellation.is_cancelled() {
            self.finish(record, SubagentStatus::Stopped, None).await;
            return;
        }

        record.set_status(SubagentStatus::Running);
        self.update(&record).await;

        let policy = Self::policy_for(record.mode, record.worktree_path.as_ref());
        let permissions = PermissionContext::new(policy, Arc::new(AlwaysDenyResolver));
        let tools = self.tools_for(&record, &provider, &tools);
        let mut agent = Agent::new(
            provider,
            tools,
            permissions,
            AgentLimits {
                max_turns: self.limits.max_turns,
                max_total_tool_calls: self.limits.max_total_tool_calls,
                ..AgentLimits::default()
            },
        );
        if let Some(observer) = &self.file_change_observer {
            agent = agent.with_file_change_observer(Arc::clone(observer));
        }

        let project_root = record
            .worktree_path
            .clone()
            .unwrap_or_else(|| self.project_root.clone());

        tokio::select! {
            outcome = self.drive_turns(&agent, &record, &project_root, cancellation.clone()) => {
                let (status, result) = outcome;
                self.finish(record, status, Some(result)).await;
            }
            () = tokio::time::sleep(self.limits.per_task_timeout) => {
                cancellation.cancel();
                self.finish(record, SubagentStatus::TimedOut, None).await;
            }
            () = cancellation.cancelled() => {
                self.finish(record, SubagentStatus::Stopped, None).await;
            }
        }
    }

    fn policy_for(
        mode: SubagentMode,
        worktree_path: Option<&PathBuf>,
    ) -> Arc<dyn PermissionPolicy> {
        if mode.allows_writes() && worktree_path.is_some() {
            Arc::new(DefaultPermissionPolicy::editing())
        } else {
            Arc::new(DefaultPermissionPolicy::read_only())
        }
    }

    /// Adds the `agent_spawn` tool on top of `base` when `record` is allowed to nest a child:
    /// below [`MAX_SUBAGENT_DEPTH`] and the concurrency pool has room for a blocked parent plus
    /// at least one child (see [`MIN_CONCURRENCY_FOR_NESTING`]). Otherwise returns `base`
    /// unchanged, so the model never even sees a capability it cannot use.
    fn tools_for(
        &self,
        record: &SubagentRecord,
        provider: &Arc<dyn Provider>,
        base: &Arc<ToolRegistry>,
    ) -> Arc<ToolRegistry> {
        if record.depth >= MAX_SUBAGENT_DEPTH
            || self.limits.max_concurrent < MIN_CONCURRENCY_FOR_NESTING
        {
            return base.clone();
        }
        let mut extended = (**base).clone();
        extended.register(Arc::new(crate::agent_spawn_tool::AgentSpawnTool {
            manager: self.manager.clone(),
            parent_session_id: record.parent_session_id.clone(),
            parent_subagent_id: record.id.clone(),
            depth: record.depth,
            model: record.model.clone(),
            provider: provider.clone(),
            tools: base.clone(),
            project_root: self.project_root.clone(),
            parent_permission_mode: record.permission_mode,
            worktree_runner: self.worktree_runner.clone(),
            instructions: self.instructions.clone(),
        }));
        Arc::new(extended)
    }

    /// Drives the subagent's work as a sequence of bounded [`Agent::run`] calls: the first call's
    /// prompt is the objective, and each subsequent call (if any message arrived while the
    /// previous one was in flight) uses that message as its prompt, seeded with the accumulated
    /// history. This is how `/agent message` reaches a "running" subagent — see the architecture
    /// plan's open questions for why there is no true mid-turn injection point.
    async fn drive_turns(
        &self,
        agent: &Agent,
        record: &SubagentRecord,
        project_root: &Path,
        cancellation: CancellationToken,
    ) -> (SubagentStatus, SubagentResult) {
        let mut prompt = render_objective(record, self.instructions.as_deref());
        let mut history: Vec<ChatMessage> = Vec::new();
        let mut last_text = String::new();

        loop {
            let request = AgentRequest {
                prompt: prompt.clone(),
                model: ModelId::new(record.model.clone()),
                project_root: project_root.to_path_buf(),
                instructions: self.instructions.clone(),
                project_overview: None,
                skills_summary: None,
                tools_enabled: true,
                reasoning_effort: None,
                personality: gocode_core::PersonalityName::Default,
                history: history.clone(),
            };
            let (tx, mut rx) = mpsc::channel(64);
            let id = record.id.clone();
            let event_tx = self.event_tx.clone();
            let relay = tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    if let Some(line) = progress_line(&event) {
                        let _ = event_tx
                            .send(SubagentEvent::Progress {
                                id: id.clone(),
                                line: redact_secrets(&line),
                            })
                            .await;
                    }
                }
            });

            let outcome = agent.run(request, tx, cancellation.clone()).await;
            let _ = relay.await;

            match outcome {
                Ok(completion) => {
                    last_text = completion.final_text;
                    history = completion.history;
                }
                Err(error) => {
                    let mut result = SubagentResult {
                        summary: last_text,
                        ..SubagentResult::default()
                    };
                    result.error = Some(error.to_string());
                    return (SubagentStatus::Failed, result);
                }
            }

            if let Some(question) = detect_waiting_marker(&last_text) {
                let mut working = record.clone();
                working.set_status(SubagentStatus::WaitingInput);
                self.update(&working).await;
                let _ = self
                    .event_tx
                    .send(SubagentEvent::Progress {
                        id: record.id.clone(),
                        line: format!("waiting for input: {}", redact_secrets(&question)),
                    })
                    .await;

                prompt = self.await_message(&record.id).await;
                working.set_status(SubagentStatus::Running);
                self.update(&working).await;
                continue;
            }

            let next = self
                .pending_messages
                .lock()
                .await
                .get_mut(&record.id)
                .map(std::mem::take);
            match next.filter(|queued| !queued.is_empty()) {
                Some(queued) => prompt = queued.join("\n"),
                None => break,
            }
        }

        (SubagentStatus::Completed, parse_result(&last_text))
    }

    /// Blocks until a message is queued for `id` via [`SubagentManager::send_message`], then
    /// returns it. Has no timeout of its own — the caller (`run`) already wraps the whole
    /// [`Self::drive_turns`] call in a `select!` against the subagent's overall timeout and
    /// cancellation, so a subagent stuck waiting for an answer is still bounded from the outside.
    async fn await_message(&self, id: &str) -> String {
        loop {
            {
                let mut pending = self.pending_messages.lock().await;
                if let Some(queue) = pending.get_mut(id)
                    && !queue.is_empty()
                {
                    return std::mem::take(queue).join("\n");
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn finish(
        &self,
        mut record: SubagentRecord,
        status: SubagentStatus,
        result: Option<SubagentResult>,
    ) {
        record.set_status(status);
        if let Some(result) = result {
            record.result = Some(result);
        }
        self.update(&record).await;
        let _ = self.event_tx.send(SubagentEvent::Finished(record)).await;
    }

    async fn update(&self, record: &SubagentRecord) {
        self.records
            .lock()
            .await
            .insert(record.id.clone(), record.clone());
        let _ = gocode_core::save_subagent(&self.dir, record);
        let _ = self
            .event_tx
            .send(SubagentEvent::StatusChanged {
                id: record.id.clone(),
                status: record.status,
            })
            .await;
    }
}

fn render_objective(record: &SubagentRecord, instructions: Option<&str>) -> String {
    let mut prompt = format!(
        "You are a subagent delegated one bounded task by the main Gocode agent.\n\nObjective: {}\n\n",
        record.task_summary
    );
    if let Some(instructions) = instructions.filter(|text| !text.trim().is_empty()) {
        use std::fmt::Write as _;
        let _ = write!(prompt, "Project instructions:\n{instructions}\n\n");
    }
    prompt.push_str(
        "When you are done, end your final message with a fenced ```json block matching this \
         shape: {\"summary\": string, \"findings\": [string], \"files_read\": [string], \
         \"files_changed\": [string], \"commands_run\": [string], \"tests_run\": [string], \
         \"risks\": [string], \"next_steps\": [string]}. If you cannot complete the objective, \
         still emit the block with as much of it filled in as you know.\n\n\
         If you need clarification from the supervisor before you can continue, do not emit \
         that block yet — instead end your message with a single line: `NEEDS_INPUT: <your \
         question>`. You will receive the supervisor's answer as your next message and should \
         continue the task from there.",
    );
    prompt
}

/// Looks for a `NEEDS_INPUT: <question>` line (see [`render_objective`]), scanning from the last
/// line since the subagent is instructed to end its message with it. Returns the question text,
/// or `None` when the subagent's message carries no such line (the common case).
fn detect_waiting_marker(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("NEEDS_INPUT:"))
        .map(str::trim)
        .filter(|question| !question.is_empty())
        .map(str::to_string)
}

/// Parses a subagent's final message into a [`SubagentResult`], looking for a fenced `json` code
/// block matching its shape. Falls back to a plain summary (no error) when no valid block is
/// found — the subagent still produced a completed run, just not in the requested structured
/// format.
#[must_use]
pub fn parse_result(final_text: &str) -> SubagentResult {
    if let Some(json) = extract_json_block(final_text)
        && let Ok(mut result) = serde_json::from_str::<SubagentResult>(&json)
    {
        if result.summary.trim().is_empty() {
            result.summary = final_text.trim().to_string();
        }
        return result;
    }
    SubagentResult {
        summary: final_text.trim().to_string(),
        ..SubagentResult::default()
    }
}

fn extract_json_block(text: &str) -> Option<String> {
    let start = text.find("```json")?;
    let after = &text[start + "```json".len()..];
    let end = after.find("```")?;
    Some(after[..end].trim().to_string())
}

fn progress_line(event: &crate::AgentEvent) -> Option<String> {
    match event {
        crate::AgentEvent::ToolRequested(call) => Some(format!("running {}", call.name.as_str())),
        crate::AgentEvent::ToolFinished(result) => {
            Some(format!("finished {}", result.call_id.as_str()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SUBAGENT_DEPTH, SpawnRequest, SubagentEvent, SubagentLimits, SubagentManager,
        effective_subagent_mode, parse_result,
    };
    use gocode_core::{
        CancellationToken, ChatStreamEvent, FinishReason, ModelId, PermissionMode, ProviderError,
        ProviderFuture, SubagentMode, SubagentStatus, ToolCallDelta, testing::FakeProvider,
    };
    use gocode_tools::{builtin_registry, process::TokioProcessRunner};
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    use tokio::sync::mpsc;

    /// A [`gocode_core::Provider`] that never responds until `release` is notified, used to pin a
    /// subagent in `Running` while the test asserts on a second, queued spawn.
    struct BlockingProvider {
        release: tokio::sync::Notify,
    }

    impl gocode_core::Provider for BlockingProvider {
        fn stream_chat(
            &self,
            _request: gocode_core::ChatRequest,
            _cancellation: CancellationToken,
        ) -> ProviderFuture<'_> {
            Box::pin(async move {
                self.release.notified().await;
                let (sender, receiver) = mpsc::channel(2);
                let _ = sender
                    .send(Ok(ChatStreamEvent::TextDelta("first done".into())))
                    .await;
                let _ = sender
                    .send(Ok(ChatStreamEvent::Finished(FinishReason::Stop)))
                    .await;
                Ok(receiver)
            })
        }
    }

    /// A [`gocode_core::Provider`] that sleeps far longer than any test's timeout/stop budget,
    /// so the manager's own `select!` is what has to cut it off, not the provider returning.
    struct HangingProvider;

    impl gocode_core::Provider for HangingProvider {
        fn stream_chat(
            &self,
            _request: gocode_core::ChatRequest,
            _cancellation: CancellationToken,
        ) -> ProviderFuture<'_> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(60)).await;
                unreachable!("the manager's timeout or stop should have cut this off first")
            })
        }
    }

    fn fixture(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gocode-subagent-manager-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn text_turn(text: &str) -> Vec<Result<ChatStreamEvent, ProviderError>> {
        vec![
            Ok(ChatStreamEvent::TextDelta(text.into())),
            Ok(ChatStreamEvent::Finished(FinishReason::Stop)),
        ]
    }

    fn tool_call_turn(
        id: &str,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Vec<Result<ChatStreamEvent, ProviderError>> {
        vec![
            Ok(ChatStreamEvent::ToolCallDelta(ToolCallDelta {
                index: 0,
                id: Some(id.into()),
                name_delta: Some(name.into()),
                arguments_delta: Some(arguments.to_string()),
            })),
            Ok(ChatStreamEvent::Finished(FinishReason::ToolCalls)),
        ]
    }

    fn base_request(
        dir: &Path,
        task: &str,
        mode: SubagentMode,
        provider: Arc<dyn gocode_core::Provider>,
    ) -> SpawnRequest {
        SpawnRequest {
            parent_session_id: "session-1".into(),
            task: task.into(),
            mode,
            model: ModelId::new("test-model"),
            worktree_requested: false,
            parent_permission_mode: PermissionMode::Auto,
            provider,
            tools: Arc::new(builtin_registry()),
            project_root: dir.to_path_buf(),
            worktree_runner: Arc::new(TokioProcessRunner),
            instructions: None,
            depth: 1,
            parent_subagent_id: None,
        }
    }

    async fn wait_for_terminal(manager: &SubagentManager, id: &str) -> gocode_core::SubagentRecord {
        for _ in 0..200 {
            if let Some(record) = manager.get(id).await
                && record.status.is_terminal()
            {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("subagent {id} did not reach a terminal status in time");
    }

    #[tokio::test]
    async fn a_read_only_subagent_completes_and_records_a_result() {
        let dir = fixture("read-only-lifecycle");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(dir.clone(), SubagentLimits::default(), tx);

        let provider: Arc<dyn gocode_core::Provider> =
            Arc::new(FakeProvider::script(vec![text_turn(
                "Investigated. ```json\n{\"summary\": \"looked at auth\"}\n```",
            )]));
        let id = manager
            .spawn(base_request(
                &dir,
                "investigate auth",
                SubagentMode::Research,
                provider,
            ))
            .await
            .expect("spawn should succeed");

        let record = wait_for_terminal(&manager, &id).await;
        assert_eq!(record.status, SubagentStatus::Completed);
        assert_eq!(
            record.result.expect("result should be set").summary,
            "looked at auth"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn two_research_subagents_run_concurrently() {
        let dir = fixture("parallel-research");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(
            dir.clone(),
            SubagentLimits {
                max_concurrent: 2,
                ..SubagentLimits::default()
            },
            tx,
        );

        let provider_a: Arc<dyn gocode_core::Provider> =
            Arc::new(FakeProvider::script(vec![text_turn("a done")]));
        let provider_b: Arc<dyn gocode_core::Provider> =
            Arc::new(FakeProvider::script(vec![text_turn("b done")]));

        let id_a = manager
            .spawn(base_request(
                &dir,
                "task a",
                SubagentMode::Research,
                provider_a,
            ))
            .await
            .unwrap();
        let id_b = manager
            .spawn(base_request(
                &dir,
                "task b",
                SubagentMode::Research,
                provider_b,
            ))
            .await
            .unwrap();

        let record_a = wait_for_terminal(&manager, &id_a).await;
        let record_b = wait_for_terminal(&manager, &id_b).await;
        assert_eq!(record_a.status, SubagentStatus::Completed);
        assert_eq!(record_b.status, SubagentStatus::Completed);

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn concurrency_cap_queues_the_second_spawn() {
        let dir = fixture("concurrency-cap");
        let (tx, rx) = mpsc::channel(64);
        let manager = SubagentManager::new(
            dir.clone(),
            SubagentLimits {
                max_concurrent: 1,
                ..SubagentLimits::default()
            },
            tx,
        );

        let blocking = Arc::new(BlockingProvider {
            release: tokio::sync::Notify::new(),
        });
        let provider_second: Arc<dyn gocode_core::Provider> =
            Arc::new(FakeProvider::script(vec![text_turn("second done")]));

        let id_first = manager
            .spawn(base_request(
                &dir,
                "first",
                SubagentMode::Research,
                blocking.clone(),
            ))
            .await
            .unwrap();
        let id_second = manager
            .spawn(base_request(
                &dir,
                "second",
                SubagentMode::Research,
                provider_second,
            ))
            .await
            .unwrap();

        // Give the first subagent time to actually start and claim the only slot.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            manager.get(&id_second).await.unwrap().status,
            SubagentStatus::Queued
        );

        blocking.release.notify_one();
        let record_first = wait_for_terminal(&manager, &id_first).await;
        let record_second = wait_for_terminal(&manager, &id_second).await;
        assert_eq!(record_first.status, SubagentStatus::Completed);
        assert_eq!(record_second.status, SubagentStatus::Completed);

        drop(rx);
        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_stalled_subagent_times_out() {
        let dir = fixture("timeout");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(
            dir.clone(),
            SubagentLimits {
                per_task_timeout: Duration::from_millis(50),
                ..SubagentLimits::default()
            },
            tx,
        );

        let provider: Arc<dyn gocode_core::Provider> = Arc::new(HangingProvider);
        let id = manager
            .spawn(base_request(
                &dir,
                "will hang",
                SubagentMode::Research,
                provider,
            ))
            .await
            .unwrap();

        let record = wait_for_terminal(&manager, &id).await;
        assert_eq!(record.status, SubagentStatus::TimedOut);

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stopping_a_running_subagent_preserves_its_partial_record() {
        let dir = fixture("stop");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(dir.clone(), SubagentLimits::default(), tx);

        let provider: Arc<dyn gocode_core::Provider> = Arc::new(HangingProvider);
        let id = manager
            .spawn(base_request(
                &dir,
                "will be stopped",
                SubagentMode::Research,
                provider,
            ))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(30)).await;
        manager.stop(&id).await.expect("stop should succeed");

        let record = wait_for_terminal(&manager, &id).await;
        assert_eq!(record.status, SubagentStatus::Stopped);
        assert_eq!(record.task_summary, "will be stopped");

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn plan_mode_forces_a_read_only_subagent_even_when_implement_was_requested() {
        assert_eq!(
            effective_subagent_mode(SubagentMode::Implement, PermissionMode::Plan),
            SubagentMode::Research
        );
        assert_eq!(
            effective_subagent_mode(SubagentMode::Implement, PermissionMode::Auto),
            SubagentMode::Implement
        );

        let dir = fixture("plan-mode-denied");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(dir.clone(), SubagentLimits::default(), tx);
        let provider: Arc<dyn gocode_core::Provider> =
            Arc::new(FakeProvider::script(vec![text_turn("noop")]));
        let mut request =
            base_request(&dir, "try to write code", SubagentMode::Implement, provider);
        request.parent_permission_mode = PermissionMode::Plan;

        let error = manager.spawn(request).await.unwrap_err();
        assert_eq!(error, super::SubagentError::ImplementDeniedInPlanMode);

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_depth_one_subagent_can_spawn_and_wait_for_a_child_via_agent_spawn() {
        let dir = fixture("nested-spawn");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(dir.clone(), SubagentLimits::default(), tx);

        // The parent and child share one FakeProvider instance (as they would share one real
        // provider in production); this is deterministic because the parent's agent_spawn tool
        // call fully blocks on the child's own run before the parent's next turn happens, so the
        // three scripted turns are consumed in this exact order: parent's tool call, the child's
        // only turn, then the parent's final turn.
        let provider: Arc<dyn gocode_core::Provider> = Arc::new(FakeProvider::script(vec![
            tool_call_turn(
                "call1",
                "agent_spawn",
                &serde_json::json!({"task": "investigate the login flow", "mode": "research"}),
            ),
            text_turn("child investigated the login flow"),
            text_turn(
                "Delegated it out. ```json\n{\"summary\": \"delegated investigation \
                 complete\"}\n```",
            ),
        ]));

        let id = manager
            .spawn(base_request(
                &dir,
                "coordinate an investigation",
                SubagentMode::Research,
                provider,
            ))
            .await
            .unwrap();

        let record = wait_for_terminal(&manager, &id).await;
        assert_eq!(record.status, SubagentStatus::Completed);
        assert_eq!(
            record.result.expect("result should be set").summary,
            "delegated investigation complete"
        );

        let child = manager
            .list()
            .await
            .into_iter()
            .find(|candidate| candidate.parent_subagent_id.as_deref() == Some(id.as_str()))
            .expect("the child subagent should be recorded");
        assert_eq!(child.depth, 2);
        assert_eq!(child.task_summary, "investigate the login flow");
        assert_eq!(child.status, SubagentStatus::Completed);
        assert_eq!(
            child.result.expect("child result should be set").summary,
            "child investigated the login flow"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_depth_two_subagent_has_no_agent_spawn_tool_and_cannot_nest_further() {
        let dir = fixture("nesting-cap");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(dir.clone(), SubagentLimits::default(), tx);
        let mut request = base_request(
            &dir,
            "already a child",
            SubagentMode::Research,
            Arc::new(FakeProvider::script(vec![text_turn(
                "done, no tools offered",
            )])),
        );
        request.depth = MAX_SUBAGENT_DEPTH;

        let id = manager.spawn(request).await.unwrap();
        let record = wait_for_terminal(&manager, &id).await;
        assert_eq!(record.status, SubagentStatus::Completed);

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn spawning_beyond_the_max_depth_is_rejected() {
        let dir = fixture("nesting-rejected");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(dir.clone(), SubagentLimits::default(), tx);
        let mut request = base_request(
            &dir,
            "too deep",
            SubagentMode::Research,
            Arc::new(FakeProvider::script(vec![])),
        );
        request.depth = MAX_SUBAGENT_DEPTH + 1;

        let error = manager.spawn(request).await.unwrap_err();
        assert_eq!(
            error,
            super::SubagentError::MaxDepthExceeded {
                depth: MAX_SUBAGENT_DEPTH + 1,
                max: MAX_SUBAGENT_DEPTH,
            }
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// Builds a minimal [`super::SubagentWorker`] for directly unit-testing `tools_for`, without
    /// going through a full spawn.
    fn test_worker(dir: &Path, limits: SubagentLimits) -> super::SubagentWorker {
        let (event_tx, _rx) = mpsc::channel(64);
        let manager = Arc::new(SubagentManager::new(
            dir.to_path_buf(),
            limits,
            event_tx.clone(),
        ));
        super::SubagentWorker {
            dir: dir.to_path_buf(),
            records: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            pending_messages: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            concurrency: Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent.max(1))),
            limits,
            event_tx,
            project_root: dir.to_path_buf(),
            instructions: None,
            manager,
            worktree_runner: Arc::new(TokioProcessRunner),
            file_change_observer: None,
        }
    }

    fn test_record(mode: SubagentMode, depth: usize) -> gocode_core::SubagentRecord {
        let mut record = gocode_core::SubagentRecord::new(
            "session-1".into(),
            "task".into(),
            mode,
            "test-model".into(),
            !mode.allows_writes(),
            PermissionMode::Auto,
        );
        record.depth = depth;
        record
    }

    #[test]
    fn agent_spawn_tool_is_registered_for_a_depth_one_subagent_with_room_to_nest() {
        let dir = fixture("tools-for-eligible");
        let worker = test_worker(&dir, SubagentLimits::default());
        let record = test_record(SubagentMode::Research, 1);
        let base = Arc::new(builtin_registry());
        let provider: Arc<dyn gocode_core::Provider> = Arc::new(FakeProvider::script(vec![]));

        let tools = worker.tools_for(&record, &provider, &base);

        assert!(tools.contains(&gocode_tools::ToolName::new("agent_spawn")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn agent_spawn_tool_is_absent_at_the_max_depth() {
        let dir = fixture("tools-for-max-depth");
        let worker = test_worker(&dir, SubagentLimits::default());
        let record = test_record(SubagentMode::Research, MAX_SUBAGENT_DEPTH);
        let base = Arc::new(builtin_registry());
        let provider: Arc<dyn gocode_core::Provider> = Arc::new(FakeProvider::script(vec![]));

        let tools = worker.tools_for(&record, &provider, &base);

        assert!(!tools.contains(&gocode_tools::ToolName::new("agent_spawn")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn agent_spawn_tool_is_absent_when_the_concurrency_pool_is_too_small_for_it() {
        let dir = fixture("tools-for-min-concurrency");
        let limits = SubagentLimits {
            max_concurrent: 1,
            ..SubagentLimits::default()
        };
        let worker = test_worker(&dir, limits);
        let record = test_record(SubagentMode::Research, 1);
        let base = Arc::new(builtin_registry());
        let provider: Arc<dyn gocode_core::Provider> = Arc::new(FakeProvider::script(vec![]));

        let tools = worker.tools_for(&record, &provider, &base);

        assert!(!tools.contains(&gocode_tools::ToolName::new("agent_spawn")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_a_fenced_json_result_block() {
        let text = "Done investigating.\n```json\n{\"summary\": \"found the bug\", \"risks\": [\"none\"]}\n```";
        let result = parse_result(text);
        assert_eq!(result.summary, "found the bug");
        assert_eq!(result.risks, vec!["none".to_string()]);
        assert!(result.error.is_none());
    }

    #[test]
    fn falls_back_to_the_raw_text_when_no_json_block_is_present() {
        let result = parse_result("just a plain answer, no structure");
        assert_eq!(result.summary, "just a plain answer, no structure");
        assert!(result.error.is_none());
    }

    #[test]
    fn detects_a_needs_input_line_scanning_from_the_end_of_the_message() {
        assert_eq!(
            super::detect_waiting_marker(
                "Looked at the logs.\nNEEDS_INPUT: which environment should I check?"
            ),
            Some("which environment should I check?".to_string())
        );
        assert_eq!(super::detect_waiting_marker("all done, no questions"), None);
        assert_eq!(super::detect_waiting_marker("NEEDS_INPUT:   "), None);
    }

    #[tokio::test]
    async fn a_subagent_that_asks_for_clarification_pauses_and_resumes_after_a_message() {
        let dir = fixture("waiting-input");
        let (tx, _rx) = mpsc::channel(64);
        let manager = SubagentManager::new(dir.clone(), SubagentLimits::default(), tx);

        let provider: Arc<dyn gocode_core::Provider> = Arc::new(FakeProvider::script(vec![
            text_turn(
                "Before I continue, I need to know something.\nNEEDS_INPUT: which environment \
                 should I check?",
            ),
            text_turn("Done. ```json\n{\"summary\": \"checked staging\"}\n```"),
        ]));
        let id = manager
            .spawn(base_request(
                &dir,
                "investigate the outage",
                SubagentMode::Research,
                provider,
            ))
            .await
            .unwrap();

        let mut reached_waiting = false;
        for _ in 0..200 {
            if manager.get(&id).await.map(|record| record.status)
                == Some(SubagentStatus::WaitingInput)
            {
                reached_waiting = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(reached_waiting, "subagent never reached WaitingInput");

        manager
            .send_message(&id, "check staging")
            .await
            .expect("message should be accepted while waiting for input");

        let record = wait_for_terminal(&manager, &id).await;
        assert_eq!(record.status, SubagentStatus::Completed);
        assert_eq!(
            record.result.expect("result should be set").summary,
            "checked staging"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn events_are_emitted_for_spawn_and_completion() {
        let dir = fixture("events");
        let (tx, mut rx) = mpsc::channel(64);
        let manager = SubagentManager::new(dir.clone(), SubagentLimits::default(), tx);
        let provider: Arc<dyn gocode_core::Provider> =
            Arc::new(FakeProvider::script(vec![text_turn("done")]));
        let id = manager
            .spawn(base_request(
                &dir,
                "observe events",
                SubagentMode::Research,
                provider,
            ))
            .await
            .unwrap();

        let mut saw_spawned = false;
        let mut saw_finished = false;
        for _ in 0..20 {
            match rx.recv().await {
                Some(SubagentEvent::Spawned(record)) if record.id == id => saw_spawned = true,
                Some(SubagentEvent::Finished(record)) if record.id == id => {
                    saw_finished = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_spawned);
        assert!(saw_finished);

        fs::remove_dir_all(&dir).ok();
    }
}
