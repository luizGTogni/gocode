//! Persisted subagent records: metadata, messages, and results for tasks the main agent delegates
//! to an isolated worker. See `docs/AGENT.md` and the subagent architecture plan for the full
//! model — this module only owns the data shape and its on-disk persistence, not execution.

use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{AppError, PermissionMode, atomic_write};

/// The schema version written by this build. Bump when a breaking change to [`SubagentRecord`] is
/// made; [`load_subagent`]/[`list_subagents`] tolerate older values via `#[serde(default)]`
/// fields, so this is informational rather than a hard compatibility gate.
pub const SUBAGENT_SCHEMA_VERSION: u32 = 1;

/// What kind of work a subagent was asked to do. Only [`SubagentMode::Implement`] is allowed to
/// write files, and then only inside a worktree created for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SubagentMode {
    /// Investigate and report back; never writes files.
    #[default]
    Research,
    /// Draft a plan or notes; never writes files.
    Plan,
    /// Make code changes, isolated to its own worktree.
    Implement,
    /// Review existing code or a diff and report findings; never writes files.
    Review,
}

impl SubagentMode {
    /// Whether this mode is allowed to write files at all (only inside its own worktree).
    #[must_use]
    pub const fn allows_writes(self) -> bool {
        matches!(self, Self::Implement)
    }

    /// Short, lowercase, user-facing name for this mode, also the `--mode` flag value.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::Plan => "plan",
            Self::Implement => "implement",
            Self::Review => "review",
        }
    }

    /// Parses a `--mode` flag value.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "research" => Some(Self::Research),
            "plan" => Some(Self::Plan),
            "implement" => Some(Self::Implement),
            "review" => Some(Self::Review),
            _ => None,
        }
    }
}

/// A subagent's position in its lifecycle. See the subagent architecture plan for the full state
/// diagram; the only automatic transition performed outside the execution engine is
/// [`recover_interrupted`], which moves a non-terminal status found on disk at startup to
/// [`SubagentStatus::Interrupted`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SubagentStatus {
    /// Created, waiting for a concurrency slot.
    #[default]
    Queued,
    /// Actively running.
    Running,
    /// Waiting on a `/agent message` reply before continuing.
    WaitingInput,
    /// Finished successfully; `result` is set.
    Completed,
    /// Finished with an error; `result.error` is set.
    Failed,
    /// Stopped by `/agent stop`; partial results are preserved.
    Stopped,
    /// Exceeded its configured time budget.
    TimedOut,
    /// Found `Queued`/`Running`/`WaitingInput` on disk when the application restarted.
    Interrupted,
}

impl SubagentStatus {
    /// Whether this status is final — no further progress will be made without a new run.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running | Self::WaitingInput)
    }

    /// Short, lowercase, user-facing name for this status.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingInput => "waiting_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Who wrote one [`SubagentMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentMessageRole {
    /// The main agent / user, via `/agent message`.
    Supervisor,
    /// The subagent itself, as a progress note or its final report.
    Subagent,
}

/// One entry in a subagent's message history: a follow-up instruction or a progress note.
/// Free-text `text` is always passed through [`gocode_tools::process::redact_secrets`] by the
/// execution engine before being recorded here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentMessage {
    /// When this message was recorded, as Unix seconds.
    pub at_unix: i64,
    /// Who wrote it.
    pub role: SubagentMessageRole,
    /// Already-redacted text.
    pub text: String,
}

/// The structured outcome of one subagent run, returned via `/agent result`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubagentResult {
    /// Short human-readable summary of what happened.
    pub summary: String,
    /// Concrete findings, one per entry.
    #[serde(default)]
    pub findings: Vec<String>,
    /// Paths read during the run.
    #[serde(default)]
    pub files_read: Vec<String>,
    /// Paths changed during the run (only possible for [`SubagentMode::Implement`]).
    #[serde(default)]
    pub files_changed: Vec<String>,
    /// Commands executed during the run.
    #[serde(default)]
    pub commands_run: Vec<String>,
    /// Tests executed during the run, if any.
    #[serde(default)]
    pub tests_run: Vec<String>,
    /// Risks or caveats worth the supervisor's attention.
    #[serde(default)]
    pub risks: Vec<String>,
    /// Suggested follow-up actions.
    #[serde(default)]
    pub next_steps: Vec<String>,
    /// The worktree this result's changes live in, when the subagent wrote files.
    pub worktree_path: Option<PathBuf>,
    /// Set when the run failed; `summary`/other fields may still hold partial information.
    pub error: Option<String>,
}

/// A persisted subagent: metadata, message history, and result. Mirrors
/// [`crate::session::SessionRecord`]'s persistence shape and schema-tolerance conventions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentRecord {
    /// Stable identifier, also the filename stem on disk.
    pub id: String,
    /// The session that spawned this subagent, for display and cleanup scoping.
    pub parent_session_id: String,
    /// Short description of the delegated task.
    pub task_summary: String,
    /// What kind of work this subagent was asked to do.
    pub mode: SubagentMode,
    /// Current lifecycle state.
    pub status: SubagentStatus,
    /// Model id used for this subagent's runs.
    pub model: String,
    /// When this subagent was created, as Unix seconds.
    pub created_at_unix: i64,
    /// When this record was last updated, as Unix seconds.
    pub updated_at_unix: i64,
    /// The worktree this subagent is isolated to, when it is (or was) allowed to write files.
    pub worktree_path: Option<PathBuf>,
    /// The branch checked out in `worktree_path`, when there is one.
    pub branch: Option<String>,
    /// Whether this subagent was constrained to read-only tools.
    pub read_only: bool,
    /// The effective permission mode this subagent ran under (never more permissive than its
    /// parent's).
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// Follow-ups and progress notes, oldest first.
    #[serde(default)]
    pub messages: Vec<SubagentMessage>,
    /// The final structured result, once the run reaches a terminal status.
    #[serde(default)]
    pub result: Option<SubagentResult>,
    /// Schema version this record was written with.
    #[serde(default)]
    pub schema_version: u32,
}

impl SubagentRecord {
    /// Creates a new, freshly queued subagent record.
    #[must_use]
    pub fn new(
        parent_session_id: String,
        task_summary: String,
        mode: SubagentMode,
        model: String,
        read_only: bool,
        permission_mode: PermissionMode,
    ) -> Self {
        let now = unix_now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            parent_session_id,
            task_summary,
            mode,
            status: SubagentStatus::Queued,
            model,
            created_at_unix: now,
            updated_at_unix: now,
            worktree_path: None,
            branch: None,
            read_only,
            permission_mode,
            messages: Vec::new(),
            result: None,
            schema_version: SUBAGENT_SCHEMA_VERSION,
        }
    }

    /// Appends a message and bumps `updated_at_unix`.
    pub fn push_message(&mut self, role: SubagentMessageRole, text: String) {
        self.updated_at_unix = unix_now();
        self.messages.push(SubagentMessage {
            at_unix: self.updated_at_unix,
            role,
            text,
        });
    }

    /// Transitions to a new status and bumps `updated_at_unix`.
    pub fn set_status(&mut self, status: SubagentStatus) {
        self.status = status;
        self.updated_at_unix = unix_now();
    }

    /// Elapsed seconds since this subagent was created.
    #[must_use]
    pub fn elapsed_seconds(&self) -> i64 {
        (unix_now() - self.created_at_unix).max(0)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

/// The directory subagent records are persisted under, given the application's state directory.
#[must_use]
pub fn subagents_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("subagents")
}

/// Persists one subagent record, creating the subagents directory if needed.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the directory or file cannot be written, or
/// [`AppError::Configuration`] when the record cannot be serialized.
pub fn save_subagent(dir: &Path, record: &SubagentRecord) -> Result<(), AppError> {
    std::fs::create_dir_all(dir)
        .map_err(|error| AppError::Io(format!("could not create {}: {error}", dir.display())))?;
    let contents = serde_json::to_string_pretty(record).map_err(|error| {
        AppError::Configuration(format!("could not serialize subagent: {error}"))
    })?;
    atomic_write(&dir.join(format!("{}.json", record.id)), &contents)
}

/// Loads one subagent record by id.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the file cannot be read, or [`AppError::Configuration`] when its
/// contents are not a valid subagent record.
pub fn load_subagent(dir: &Path, id: &str) -> Result<SubagentRecord, AppError> {
    let path = dir.join(format!("{id}.json"));
    let contents = std::fs::read_to_string(&path)
        .map_err(|error| AppError::Io(format!("could not read {}: {error}", path.display())))?;
    serde_json::from_str(&contents)
        .map_err(|error| AppError::Configuration(format!("could not parse subagent: {error}")))
}

/// Deletes one persisted subagent record. Never touches its worktree — callers that need the
/// worktree gone too must remove it separately (see `gocode_tools::worktree::remove_worktree`)
/// before calling this, so a failed worktree removal never leaves the record silently orphaned.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the file exists but cannot be removed. Deleting an already-absent
/// record is not an error.
pub fn delete_subagent(dir: &Path, id: &str) -> Result<(), AppError> {
    let path = dir.join(format!("{id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::Io(format!(
            "could not remove {}: {error}",
            path.display()
        ))),
    }
}

/// Lists every saved subagent record, most recently updated first. Returns an empty list when the
/// subagents directory does not exist yet; individually unreadable files are skipped rather than
/// failing the whole listing.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the directory exists but cannot be enumerated.
pub fn list_subagents(dir: &Path) -> Result<Vec<SubagentRecord>, AppError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|error| AppError::Io(format!("could not read {}: {error}", dir.display())))?;

    let mut records = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(entry.path())
            && let Ok(record) = serde_json::from_str::<SubagentRecord>(&contents)
        {
            records.push(record);
        }
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at_unix));
    Ok(records)
}

/// Recovers from an ungraceful restart: any record still `Queued`, `Running`, or `WaitingInput`
/// on disk is moved to [`SubagentStatus::Interrupted`] and re-saved. Never resumes a run — the
/// caller is expected to surface the returned ids so the user can decide what to do with them via
/// `/agents` and `/agent cleanup`.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the directory exists but cannot be enumerated, propagated from
/// [`list_subagents`]. Individual save failures during recovery are skipped rather than aborting
/// the whole pass, so one corrupt record cannot hide the rest.
pub fn recover_interrupted(dir: &Path) -> Result<Vec<String>, AppError> {
    let mut recovered = Vec::new();
    for mut record in list_subagents(dir)? {
        if record.status.is_terminal() {
            continue;
        }
        record.set_status(SubagentStatus::Interrupted);
        if save_subagent(dir, &record).is_ok() {
            recovered.push(record.id);
        }
    }
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::{
        SubagentMessageRole, SubagentMode, SubagentRecord, SubagentStatus, list_subagents,
        load_subagent, recover_interrupted, save_subagent, subagents_dir,
    };
    use crate::PermissionMode;
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_fixture_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("gocode-subagent-tests-{label}-{nonce}"))
    }

    fn sample(task: &str) -> SubagentRecord {
        SubagentRecord::new(
            "parent-session".into(),
            task.into(),
            SubagentMode::Research,
            "test-model".into(),
            true,
            PermissionMode::Auto,
        )
    }

    #[test]
    fn a_new_subagent_is_queued_and_read_only_by_default_mode() {
        let record = sample("investigate flaky test");
        assert_eq!(record.status, SubagentStatus::Queued);
        assert!(!record.mode.allows_writes());
        assert!(record.result.is_none());
    }

    #[test]
    fn only_implement_mode_allows_writes() {
        assert!(!SubagentMode::Research.allows_writes());
        assert!(!SubagentMode::Plan.allows_writes());
        assert!(!SubagentMode::Review.allows_writes());
        assert!(SubagentMode::Implement.allows_writes());
    }

    #[test]
    fn messages_and_status_changes_update_the_timestamp() {
        let mut record = sample("review the auth module");
        let created = record.updated_at_unix;
        record.push_message(SubagentMessageRole::Supervisor, "focus on login.rs".into());
        assert_eq!(record.messages.len(), 1);
        record.set_status(SubagentStatus::Running);
        assert_eq!(record.status, SubagentStatus::Running);
        assert!(record.updated_at_unix >= created);
    }

    #[test]
    fn saved_subagents_round_trip_and_list_newest_first() {
        let fixture = unique_fixture_dir("round-trip");
        let dir = subagents_dir(&fixture);

        let mut older = sample("older task");
        older.updated_at_unix = 1000;
        let mut newer = sample("newer task");
        newer.updated_at_unix = 2000;

        save_subagent(&dir, &older).expect("older subagent should save");
        save_subagent(&dir, &newer).expect("newer subagent should save");

        let listed = list_subagents(&dir).expect("subagents should list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, newer.id);
        assert_eq!(listed[1].id, older.id);

        let reloaded = load_subagent(&dir, &older.id).expect("older subagent should reload");
        assert_eq!(reloaded, older);

        std::fs::remove_dir_all(&fixture).expect("fixture should be removed");
    }

    #[test]
    fn listing_an_absent_subagents_directory_is_an_empty_list_not_an_error() {
        let fixture = unique_fixture_dir("absent");
        let dir = subagents_dir(&fixture);

        assert_eq!(
            list_subagents(&dir).expect("missing dir should list empty"),
            Vec::new()
        );
    }

    #[test]
    fn restart_recovery_marks_non_terminal_records_interrupted_and_leaves_terminal_ones_alone() {
        let fixture = unique_fixture_dir("recovery");
        let dir = subagents_dir(&fixture);

        let mut running = sample("still running");
        running.status = SubagentStatus::Running;
        let mut completed = sample("already done");
        completed.status = SubagentStatus::Completed;

        save_subagent(&dir, &running).expect("running subagent should save");
        save_subagent(&dir, &completed).expect("completed subagent should save");

        let recovered = recover_interrupted(&dir).expect("recovery should succeed");
        assert_eq!(recovered, vec![running.id.clone()]);

        let reloaded_running = load_subagent(&dir, &running.id).expect("should reload");
        assert_eq!(reloaded_running.status, SubagentStatus::Interrupted);
        let reloaded_completed = load_subagent(&dir, &completed.id).expect("should reload");
        assert_eq!(reloaded_completed.status, SubagentStatus::Completed);

        // Recovery never resumes automatically: it only ever rewrites terminal-adjacent statuses.
        let recovered_again = recover_interrupted(&dir).expect("second pass should succeed");
        assert!(recovered_again.is_empty());

        std::fs::remove_dir_all(&fixture).expect("fixture should be removed");
    }
}
