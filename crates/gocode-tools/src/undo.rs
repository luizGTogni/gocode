//! Undo/redo history for the agent's own file edits (`write_file`, `apply_patch`), persisted per
//! worktree so it survives a restart.
//!
//! Never uses `git reset`/`checkout`/`restore`: every revert is a direct read/compare/write
//! against the file the agent touched, using the same [`workspace::atomic_write_file`] the write
//! tools themselves use. A transaction only applies when every file it covers still holds the
//! content the agent left it in; a file changed since by anything else (a human edit, another
//! tool) is reported as a conflict instead of being silently overwritten.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    contract::ToolError,
    workspace::{atomic_write_file, resolve_workspace_path},
};

/// Default cap on retained undo transactions; oldest entries are dropped once exceeded.
pub const DEFAULT_MAX_ENTRIES: usize = 50;

/// One file's content immediately before and after a single agent edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshot {
    /// Workspace-relative path.
    pub path: PathBuf,
    /// Content before the edit, or `None` if the file did not exist yet.
    pub before: Option<String>,
    /// Content after the edit, or `None` if the edit deleted the file.
    pub after: Option<String>,
}

impl FileSnapshot {
    /// Informational content hash, distinct from the full-content equality check that actually
    /// guards every undo/redo apply.
    #[must_use]
    pub fn after_hash(&self) -> Option<u64> {
        self.after.as_ref().map(|content| hash_content(content))
    }
}

fn hash_content(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// One agent turn's worth of file edits, undone or redone as a single unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoTransaction {
    /// Stable identifier for this transaction.
    pub id: String,
    /// When this transaction was recorded, as Unix seconds.
    pub created_at_unix: i64,
    /// Short description of the agent turn that produced this transaction (e.g. the prompt).
    pub description: String,
    /// Every file this transaction touched.
    pub files: Vec<FileSnapshot>,
}

/// What happened to one file during an undo or redo apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// The file's prior content was restored.
    Restored,
    /// The file was removed (undoing its creation, or re-deleting on redo).
    Removed,
    /// The file was recreated (undoing its deletion, or redoing its creation).
    Recreated,
}

/// One file's outcome from an applied undo/redo transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedFile {
    /// Workspace-relative path.
    pub path: PathBuf,
    /// What was done to it.
    pub action: FileAction,
}

/// One file that blocked a transaction from applying because its current content no longer
/// matches what the transaction expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoConflictFile {
    /// Workspace-relative path.
    pub path: PathBuf,
    /// Content the transaction expected to find (`None` means it expected the file to be
    /// absent).
    pub expected: Option<String>,
    /// Content actually found on disk (`None` means the file is currently absent).
    pub actual: Option<String>,
}

/// Result of an `undo`/`redo` request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UndoOutcome {
    /// Transactions that applied successfully, oldest first.
    pub applied: Vec<AppliedTransaction>,
    /// The transaction that stopped the request, when one did.
    pub conflict: Option<UndoConflict>,
}

/// One transaction's worth of applied file changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedTransaction {
    /// The transaction that applied.
    pub id: String,
    /// Its description, carried through for the confirmation message.
    pub description: String,
    /// Files it touched and what was done to each.
    pub files: Vec<AppliedFile>,
}

/// A transaction that could not apply because one or more of its files no longer match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoConflict {
    /// The transaction that conflicted.
    pub id: String,
    /// Its description.
    pub description: String,
    /// Every file that conflicted (files that matched are not repeated here).
    pub files: Vec<UndoConflictFile>,
}

/// Direction of an undo/redo request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Undo,
    Redo,
}

/// Per-worktree undo/redo history, persisted to disk (see [`load_undo_store`]/
/// [`save_undo_store`]) so it survives a restart.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UndoStore {
    undo_stack: Vec<UndoTransaction>,
    redo_stack: Vec<UndoTransaction>,
    max_entries: usize,
}

impl UndoStore {
    /// Creates an empty store retaining at most `max_entries` undo transactions.
    #[must_use]
    pub fn new(max_entries: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_entries,
        }
    }

    /// Number of transactions currently available to `/undo`.
    #[must_use]
    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }

    /// Number of transactions currently available to `/redo`.
    #[must_use]
    pub fn redo_count(&self) -> usize {
        self.redo_stack.len()
    }

    /// Records a newly completed agent transaction. Clears the redo stack — once new edits
    /// happen, previously-undone transactions can no longer be safely redone against the file
    /// state they were computed from.
    pub fn commit(&mut self, transaction: UndoTransaction) {
        self.redo_stack.clear();
        self.undo_stack.push(transaction);
        if self.undo_stack.len() > self.max_entries {
            let overflow = self.undo_stack.len() - self.max_entries;
            self.undo_stack.drain(0..overflow);
        }
    }

    /// Undoes up to `n` transactions, most recent first. Stops (without reverting that
    /// transaction) at the first one whose files no longer match, unless `force` is set.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] only for a filesystem failure while applying an already-verified
    /// transaction; conflicts are reported in the returned [`UndoOutcome`], not as an error.
    pub fn undo(
        &mut self,
        n: usize,
        project_root: &Path,
        force: bool,
    ) -> Result<UndoOutcome, ToolError> {
        self.apply(n, project_root, force, Direction::Undo)
    }

    /// Redoes up to `n` previously undone transactions, most recently undone first. Stops at the
    /// first mismatch unless `force` is set.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] only for a filesystem failure while applying an already-verified
    /// transaction; conflicts are reported in the returned [`UndoOutcome`], not as an error.
    pub fn redo(
        &mut self,
        n: usize,
        project_root: &Path,
        force: bool,
    ) -> Result<UndoOutcome, ToolError> {
        self.apply(n, project_root, force, Direction::Redo)
    }

    fn apply(
        &mut self,
        n: usize,
        project_root: &Path,
        force: bool,
        direction: Direction,
    ) -> Result<UndoOutcome, ToolError> {
        let mut outcome = UndoOutcome::default();
        for _ in 0..n {
            let source = match direction {
                Direction::Undo => &mut self.undo_stack,
                Direction::Redo => &mut self.redo_stack,
            };
            let Some(transaction) = source.pop() else {
                break;
            };

            let conflicts = detect_conflicts(&transaction, project_root, direction);
            if !conflicts.is_empty() && !force {
                outcome.conflict = Some(UndoConflict {
                    id: transaction.id.clone(),
                    description: transaction.description.clone(),
                    files: conflicts,
                });
                // Put it back — nothing was written, so the stack is unchanged.
                let source = match direction {
                    Direction::Undo => &mut self.undo_stack,
                    Direction::Redo => &mut self.redo_stack,
                };
                source.push(transaction);
                break;
            }

            let files = apply_transaction(&transaction, project_root, direction)?;
            outcome.applied.push(AppliedTransaction {
                id: transaction.id.clone(),
                description: transaction.description.clone(),
                files,
            });

            let destination = match direction {
                Direction::Undo => &mut self.redo_stack,
                Direction::Redo => &mut self.undo_stack,
            };
            destination.push(transaction);
        }
        Ok(outcome)
    }
}

/// The directory undo/redo history files are persisted under, given the application's state
/// directory.
#[must_use]
pub fn undo_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("undo")
}

/// Stable filename for one worktree's undo/redo history, derived from its canonicalized path so
/// unrelated worktrees never collide and the same worktree always resolves to the same file
/// across restarts.
fn undo_store_filename(project_root: &Path) -> String {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}.json", hasher.finish())
}

/// Loads one worktree's persisted undo/redo history. Falls back to a fresh, empty store — capped
/// at `max_entries` — when nothing has been persisted yet or the file cannot be read; a missing
/// or corrupt history file is never treated as an error, since it just means "nothing to undo
/// yet".
#[must_use]
pub fn load_undo_store(dir: &Path, project_root: &Path, max_entries: usize) -> UndoStore {
    let path = dir.join(undo_store_filename(project_root));
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str::<UndoStore>(&contents).ok())
        .map_or_else(
            || UndoStore::new(max_entries),
            |mut store| {
                store.max_entries = max_entries;
                store
            },
        )
}

/// Persists one worktree's undo/redo history, creating the undo directory if needed.
///
/// # Errors
///
/// Returns [`ToolError::Io`] when the directory or file cannot be written, or
/// [`ToolError::Internal`] when the store cannot be serialized (should not happen in practice —
/// every field is plain data).
pub fn save_undo_store(
    dir: &Path,
    project_root: &Path,
    store: &UndoStore,
) -> Result<(), ToolError> {
    std::fs::create_dir_all(dir)
        .map_err(|error| ToolError::Io(format!("could not create {}: {error}", dir.display())))?;
    let contents = serde_json::to_string_pretty(store).map_err(|error| {
        ToolError::Internal(format!("could not serialize undo history: {error}"))
    })?;
    atomic_write_file(&dir.join(undo_store_filename(project_root)), &contents)
}

/// Reads a workspace-relative path's current content, or `None` if it does not exist / is not
/// readable as UTF-8 text.
fn read_current(project_root: &Path, path: &Path) -> Option<String> {
    let resolved = resolve_workspace_path(project_root, &path.to_string_lossy()).ok()?;
    std::fs::read_to_string(resolved).ok()
}

/// The state a transaction's files must currently hold for it to apply cleanly in `direction`.
fn expected_before_apply(snapshot: &FileSnapshot, direction: Direction) -> Option<&String> {
    match direction {
        Direction::Undo => snapshot.after.as_ref(),
        Direction::Redo => snapshot.before.as_ref(),
    }
}

/// The state a transaction's files are left in after it applies in `direction`.
fn expected_after_apply(snapshot: &FileSnapshot, direction: Direction) -> Option<&String> {
    match direction {
        Direction::Undo => snapshot.before.as_ref(),
        Direction::Redo => snapshot.after.as_ref(),
    }
}

fn detect_conflicts(
    transaction: &UndoTransaction,
    project_root: &Path,
    direction: Direction,
) -> Vec<UndoConflictFile> {
    let mut conflicts = Vec::new();
    for snapshot in &transaction.files {
        let expected = expected_before_apply(snapshot, direction);
        let actual = read_current(project_root, &snapshot.path);
        if actual.as_ref() != expected {
            conflicts.push(UndoConflictFile {
                path: snapshot.path.clone(),
                expected: expected.cloned(),
                actual,
            });
        }
    }
    conflicts
}

fn apply_transaction(
    transaction: &UndoTransaction,
    project_root: &Path,
    direction: Direction,
) -> Result<Vec<AppliedFile>, ToolError> {
    let mut applied = Vec::with_capacity(transaction.files.len());
    for snapshot in &transaction.files {
        let before = expected_before_apply(snapshot, direction);
        let target = expected_after_apply(snapshot, direction);
        let resolved = resolve_workspace_path(project_root, &snapshot.path.to_string_lossy())?;

        let action = if let Some(content) = target {
            atomic_write_file(&resolved, content)?;
            if before.is_none() {
                FileAction::Recreated
            } else {
                FileAction::Restored
            }
        } else {
            if resolved.exists() {
                std::fs::remove_file(&resolved).map_err(|error| {
                    ToolError::Io(format!("could not remove {}: {error}", resolved.display()))
                })?;
            }
            FileAction::Removed
        };

        applied.push(AppliedFile {
            path: snapshot.path.clone(),
            action,
        });
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::{
        AppliedFile, DEFAULT_MAX_ENTRIES, FileAction, FileSnapshot, UndoStore, UndoTransaction,
        load_undo_store, save_undo_store, undo_dir,
    };
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn fixture(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("gocode-undo-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn txn(id: &str, files: Vec<FileSnapshot>) -> UndoTransaction {
        UndoTransaction {
            id: id.into(),
            created_at_unix: 0,
            description: format!("txn {id}"),
            files,
        }
    }

    fn snapshot(path: &str, before: Option<&str>, after: Option<&str>) -> FileSnapshot {
        FileSnapshot {
            path: PathBuf::from(path),
            before: before.map(str::to_string),
            after: after.map(str::to_string),
        }
    }

    #[test]
    fn undoes_and_redoes_a_simple_content_change() {
        let root = fixture("simple");
        fs::write(root.join("a.txt"), "new").unwrap();

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn("1", vec![snapshot("a.txt", Some("old"), Some("new"))]));

        let outcome = store.undo(1, &root, false).unwrap();
        assert!(outcome.conflict.is_none());
        assert_eq!(outcome.applied.len(), 1);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "old");
        assert_eq!(store.undo_count(), 0);
        assert_eq!(store.redo_count(), 1);

        let outcome = store.redo(1, &root, false).unwrap();
        assert!(outcome.conflict.is_none());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "new");
        assert_eq!(store.undo_count(), 1);
        assert_eq!(store.redo_count(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_new_commit_clears_the_redo_stack() {
        let root = fixture("clears-redo");
        fs::write(root.join("a.txt"), "new").unwrap();

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn("1", vec![snapshot("a.txt", Some("old"), Some("new"))]));
        store.undo(1, &root, false).unwrap();
        assert_eq!(store.redo_count(), 1);

        store.commit(txn(
            "2",
            vec![snapshot("a.txt", Some("old"), Some("newer"))],
        ));
        assert_eq!(store.redo_count(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undo_of_a_file_creation_removes_it() {
        let root = fixture("create");
        fs::write(root.join("new.txt"), "hello").unwrap();

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn("1", vec![snapshot("new.txt", None, Some("hello"))]));

        let outcome = store.undo(1, &root, false).unwrap();
        assert_eq!(
            outcome.applied[0].files,
            vec![AppliedFile {
                path: PathBuf::from("new.txt"),
                action: FileAction::Removed,
            }]
        );
        assert!(!root.join("new.txt").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undo_of_a_file_deletion_restores_it() {
        let root = fixture("delete");

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn("1", vec![snapshot("gone.txt", Some("content"), None)]));

        let outcome = store.undo(1, &root, false).unwrap();
        assert_eq!(
            outcome.applied[0].files,
            vec![AppliedFile {
                path: PathBuf::from("gone.txt"),
                action: FileAction::Recreated,
            }]
        );
        assert_eq!(
            fs::read_to_string(root.join("gone.txt")).unwrap(),
            "content"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_multi_file_transaction_is_all_or_nothing_on_conflict() {
        let root = fixture("multi-file");
        fs::write(root.join("a.txt"), "a-new").unwrap();
        fs::write(root.join("b.txt"), "tampered").unwrap();

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn(
            "1",
            vec![
                snapshot("a.txt", Some("a-old"), Some("a-new")),
                snapshot("b.txt", Some("b-old"), Some("b-new")),
            ],
        ));

        let outcome = store.undo(1, &root, false).unwrap();
        assert!(outcome.applied.is_empty());
        let conflict = outcome.conflict.unwrap();
        assert_eq!(conflict.files.len(), 1);
        assert_eq!(conflict.files[0].path, PathBuf::from("b.txt"));
        // Neither file was touched.
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a-new");
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "tampered");
        assert_eq!(store.undo_count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_manually_edited_file_is_reported_as_a_conflict_not_overwritten() {
        let root = fixture("manual-edit");
        fs::write(root.join("a.txt"), "human edit").unwrap();

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn(
            "1",
            vec![snapshot("a.txt", Some("old"), Some("agent edit"))],
        ));

        let outcome = store.undo(1, &root, false).unwrap();
        assert!(outcome.applied.is_empty());
        assert!(outcome.conflict.is_some());
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "human edit"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn force_bypasses_a_detected_conflict() {
        let root = fixture("force");
        fs::write(root.join("a.txt"), "human edit").unwrap();

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn(
            "1",
            vec![snapshot("a.txt", Some("old"), Some("agent edit"))],
        ));

        let outcome = store.undo(1, &root, true).unwrap();
        assert!(outcome.conflict.is_none());
        assert_eq!(outcome.applied.len(), 1);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "old");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_stacks_apply_nothing() {
        let root = fixture("empty");
        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);

        let outcome = store.undo(1, &root, false).unwrap();
        assert!(outcome.applied.is_empty());
        assert!(outcome.conflict.is_none());

        let outcome = store.redo(1, &root, false).unwrap();
        assert!(outcome.applied.is_empty());
        assert!(outcome.conflict.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oldest_transactions_are_trimmed_past_the_cap() {
        let root = fixture("cap");
        let mut store = UndoStore::new(2);

        store.commit(txn("1", vec![snapshot("a.txt", None, Some("1"))]));
        store.commit(txn("2", vec![snapshot("a.txt", None, Some("2"))]));
        store.commit(txn("3", vec![snapshot("a.txt", None, Some("3"))]));

        assert_eq!(store.undo_count(), 2);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undo_n_stops_at_the_first_conflicting_transaction() {
        let root = fixture("undo-n");
        fs::write(root.join("a.txt"), "v2").unwrap();

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn(
            "1",
            vec![snapshot("a.txt", Some("v0"), Some("v1-tampered"))],
        ));
        store.commit(txn("2", vec![snapshot("a.txt", Some("v1"), Some("v2"))]));

        let outcome = store.undo(2, &root, false).unwrap();
        assert_eq!(outcome.applied.len(), 1);
        assert_eq!(outcome.applied[0].id, "2");
        assert!(outcome.conflict.is_some());
        assert_eq!(outcome.conflict.unwrap().id, "1");
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "v1");
        assert_eq!(store.undo_count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_saved_store_round_trips_and_survives_a_restart() {
        let root = fixture("persist-round-trip");
        let state_dir = fixture("persist-round-trip-state");
        let dir = undo_dir(&state_dir);

        let mut store = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store.commit(txn("1", vec![snapshot("a.txt", Some("old"), Some("new"))]));
        save_undo_store(&dir, &root, &store).unwrap();

        // Simulate a restart: a freshly loaded store for the same worktree sees the same
        // history, not an empty one.
        let mut reloaded = load_undo_store(&dir, &root, DEFAULT_MAX_ENTRIES);
        assert_eq!(reloaded.undo_count(), 1);
        assert_eq!(reloaded.redo_count(), 0);

        fs::write(root.join("a.txt"), "new").unwrap();
        let outcome = reloaded.undo(1, &root, false).unwrap();
        assert!(outcome.conflict.is_none());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "old");

        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&state_dir).unwrap();
    }

    #[test]
    fn loading_a_worktree_with_no_saved_history_is_a_fresh_empty_store() {
        let root = fixture("persist-missing");
        let state_dir = fixture("persist-missing-state");
        let dir = undo_dir(&state_dir);

        let store = load_undo_store(&dir, &root, DEFAULT_MAX_ENTRIES);
        assert_eq!(store.undo_count(), 0);
        assert_eq!(store.redo_count(), 0);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn different_worktrees_persist_to_different_files() {
        let root_a = fixture("persist-worktree-a");
        let root_b = fixture("persist-worktree-b");
        let state_dir = fixture("persist-worktree-state");
        let dir = undo_dir(&state_dir);

        let mut store_a = UndoStore::new(DEFAULT_MAX_ENTRIES);
        store_a.commit(txn("a", vec![snapshot("a.txt", None, Some("a"))]));
        save_undo_store(&dir, &root_a, &store_a).unwrap();

        // Worktree b never had anything saved; it must not see worktree a's history.
        let store_b = load_undo_store(&dir, &root_b, DEFAULT_MAX_ENTRIES);
        assert_eq!(store_b.undo_count(), 0);

        fs::remove_dir_all(&root_a).unwrap();
        fs::remove_dir_all(&root_b).unwrap();
        fs::remove_dir_all(&state_dir).unwrap();
    }
}
