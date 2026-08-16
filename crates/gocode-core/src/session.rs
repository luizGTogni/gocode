//! Persisted conversation sessions: `/new` starts one, `/resume` picks a saved one, and each
//! session's history is completely isolated from every other — nothing is shared between them.

use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{AppError, ChatMessage, atomic_write};

/// Characters kept from the first user prompt when naming a session, or from the latest activity
/// when summarizing one.
const DISPLAY_TEXT_LIMIT: usize = 60;

/// A saved conversation: its own isolated history plus the metadata `/resume` displays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Stable identifier, also the filename stem on disk.
    pub id: String,
    /// Display name, derived from the first message once there is one.
    pub name: String,
    /// When this session was first created, as Unix seconds.
    pub created_at_unix: i64,
    /// When this session was last active, as Unix seconds.
    pub last_used_at_unix: i64,
    /// Short description of the most recent activity, for the `/resume` list.
    pub summary: String,
    /// This session's own conversation turns, oldest first. Never shared with another session.
    pub history: Vec<ChatMessage>,
}

impl SessionRecord {
    /// Creates a brand-new, empty session with a fresh random id.
    #[must_use]
    pub fn new() -> Self {
        let now = unix_now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "New session".into(),
            created_at_unix: now,
            last_used_at_unix: now,
            summary: String::new(),
            history: Vec::new(),
        }
    }

    /// Updates `last_used_at`, and derives `name` (once, from the first prompt) and `summary`
    /// (every time, from the latest activity) from the given turn.
    pub fn record_turn(&mut self, prompt: &str, final_text: &str) {
        self.last_used_at_unix = unix_now();
        if self.name == "New session" {
            self.name = truncate_for_display(prompt);
        }
        let latest = if final_text.trim().is_empty() {
            prompt
        } else {
            final_text
        };
        self.summary = truncate_for_display(latest);
    }

    /// Branches this session for `/fork`: an independent copy under a fresh id, carrying the
    /// same history and summary forward, while this session's own saved file is untouched.
    #[must_use]
    pub fn fork(&self) -> Self {
        let now = unix_now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: format!("{} (fork)", self.name),
            created_at_unix: now,
            last_used_at_unix: now,
            summary: self.summary.clone(),
            history: self.history.clone(),
        }
    }
}

impl Default for SessionRecord {
    fn default() -> Self {
        Self::new()
    }
}

/// How the interface arrived at the current session, carried on [`crate::AppEvent::SessionSwitched`]
/// so the transcript notice can name the right action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTransition {
    /// `/new`: a brand-new, empty session.
    New,
    /// `/resume`: a previously saved session, reopened.
    Resumed,
    /// `/fork`: an independent copy of the session just left, carrying its history forward.
    Forked,
}

/// Lightweight session metadata for the `/resume` picker, without the full history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Matches [`SessionRecord::id`].
    pub id: String,
    /// Matches [`SessionRecord::name`].
    pub name: String,
    /// Matches [`SessionRecord::summary`].
    pub summary: String,
    /// Matches [`SessionRecord::last_used_at_unix`].
    pub last_used_at_unix: i64,
}

impl From<&SessionRecord> for SessionSummary {
    fn from(record: &SessionRecord) -> Self {
        Self {
            id: record.id.clone(),
            name: record.name.clone(),
            summary: record.summary.clone(),
            last_used_at_unix: record.last_used_at_unix,
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn truncate_for_display(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let truncated: String = first_line.chars().take(DISPLAY_TEXT_LIMIT).collect();
    if first_line.chars().count() > DISPLAY_TEXT_LIMIT {
        format!("{truncated}…")
    } else if truncated.is_empty() {
        "(empty)".into()
    } else {
        truncated
    }
}

/// The directory sessions are persisted under, given the application's state directory.
#[must_use]
pub fn sessions_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("sessions")
}

/// Persists one session, creating the sessions directory if needed.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the directory or file cannot be written, or
/// [`AppError::Configuration`] when the record cannot be serialized.
pub fn save_session(dir: &Path, session: &SessionRecord) -> Result<(), AppError> {
    std::fs::create_dir_all(dir)
        .map_err(|error| AppError::Io(format!("could not create {}: {error}", dir.display())))?;
    let contents = serde_json::to_string_pretty(session).map_err(|error| {
        AppError::Configuration(format!("could not serialize session: {error}"))
    })?;
    atomic_write(&dir.join(format!("{}.json", session.id)), &contents)
}

/// Loads one session by id.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the file cannot be read, or [`AppError::Configuration`] when its
/// contents are not a valid session record.
pub fn load_session(dir: &Path, id: &str) -> Result<SessionRecord, AppError> {
    let path = dir.join(format!("{id}.json"));
    let contents = std::fs::read_to_string(&path)
        .map_err(|error| AppError::Io(format!("could not read {}: {error}", path.display())))?;
    serde_json::from_str(&contents)
        .map_err(|error| AppError::Configuration(format!("could not parse session: {error}")))
}

/// Lists every saved session, most recently used first. Returns an empty list when the sessions
/// directory does not exist yet; individually unreadable files are skipped rather than failing
/// the whole listing.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the directory exists but cannot be enumerated.
pub fn list_sessions(dir: &Path) -> Result<Vec<SessionRecord>, AppError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|error| AppError::Io(format!("could not read {}: {error}", dir.display())))?;

    let mut sessions = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(entry.path())
            && let Ok(session) = serde_json::from_str::<SessionRecord>(&contents)
        {
            sessions.push(session);
        }
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_used_at_unix));
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::{SessionRecord, list_sessions, load_session, save_session, sessions_dir};
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_fixture_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("gocode-session-tests-{label}-{nonce}"))
    }

    #[test]
    fn a_new_session_is_named_and_summarized_from_its_first_turn() {
        let mut session = SessionRecord::new();
        assert_eq!(session.name, "New session");

        session.record_turn("fix the flaky login test", "Done, it was a race condition.");

        assert_eq!(session.name, "fix the flaky login test");
        assert_eq!(session.summary, "Done, it was a race condition.");
    }

    #[test]
    fn the_session_name_is_set_once_but_the_summary_keeps_updating() {
        let mut session = SessionRecord::new();
        session.record_turn("first prompt", "first reply");
        session.record_turn("second prompt", "second reply");

        assert_eq!(session.name, "first prompt");
        assert_eq!(session.summary, "second reply");
    }

    #[test]
    fn forking_a_session_copies_its_history_under_a_new_id_and_leaves_the_original_alone() {
        let mut original = SessionRecord::new();
        original.record_turn("implement JWT auth", "Done, added the middleware.");

        let fork = original.fork();

        assert_ne!(fork.id, original.id);
        assert_eq!(fork.name, "implement JWT auth (fork)");
        assert_eq!(fork.summary, original.summary);
        assert_eq!(fork.history, original.history);
        assert_eq!(original.name, "implement JWT auth");
    }

    #[test]
    fn saved_sessions_round_trip_and_list_newest_first() {
        let fixture = unique_fixture_dir("round-trip");
        let dir = sessions_dir(&fixture);

        let mut older = SessionRecord::new();
        older.record_turn("older session", "older reply");
        older.last_used_at_unix = 1000;
        let mut newer = SessionRecord::new();
        newer.record_turn("newer session", "newer reply");
        newer.last_used_at_unix = 2000;

        save_session(&dir, &older).expect("older session should save");
        save_session(&dir, &newer).expect("newer session should save");

        let listed = list_sessions(&dir).expect("sessions should list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, newer.id);
        assert_eq!(listed[1].id, older.id);

        let reloaded = load_session(&dir, &older.id).expect("older session should reload");
        assert_eq!(reloaded, older);

        std::fs::remove_dir_all(&fixture).expect("fixture should be removed");
    }

    #[test]
    fn listing_an_absent_sessions_directory_is_an_empty_list_not_an_error() {
        let fixture = unique_fixture_dir("absent");
        let dir = sessions_dir(&fixture);

        assert_eq!(
            list_sessions(&dir).expect("missing dir should list empty"),
            Vec::new()
        );
    }
}
