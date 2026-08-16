//! Git worktree operations backing the `/worktree` command: create an isolated worktree for a
//! task, list existing ones, and remove them. Every mutation shells out to `git worktree`
//! itself — this module never deletes a directory directly.

use std::path::{Path, PathBuf};

use crate::process::{CommandRequest, ProcessError, ProcessRunner};

use std::time::Duration;

/// A git worktree operation failed in a way the caller should show to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeError {
    /// The given path is not inside a Git repository.
    NotAGitRepository,
    /// A worktree/branch name failed validation (empty, path traversal, disallowed characters).
    InvalidName(String),
    /// `git` could not even be spawned.
    SpawnFailed(String),
    /// `git` ran but exited non-zero; carries its stderr.
    GitFailed(String),
    /// No worktree matched the given name or path.
    NotFound(String),
    /// The target of a `remove` resolved to the repository's main worktree.
    RefusedMainWorktree,
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAGitRepository => write!(f, "this directory is not inside a Git repository"),
            Self::InvalidName(reason) => write!(f, "invalid name: {reason}"),
            Self::SpawnFailed(message) => write!(f, "could not run git: {message}"),
            Self::GitFailed(message) => write!(f, "git failed: {message}"),
            Self::NotFound(target) => write!(f, "no worktree matches '{target}'"),
            Self::RefusedMainWorktree => {
                write!(f, "refusing to remove the repository's main worktree")
            }
        }
    }
}

impl std::error::Error for WorktreeError {}

/// Where a new worktree's branch comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchSource {
    /// Create a new branch named after the worktree, based on the given branch (typically the
    /// one currently checked out in the main worktree).
    New { base: String },
    /// Check out an already-existing branch instead of creating one.
    Existing(String),
}

/// One entry from `git worktree list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// Absolute filesystem path of the worktree.
    pub path: PathBuf,
    /// Checked-out branch name, absent for a detached-HEAD worktree.
    pub branch: Option<String>,
    /// Checked-out commit hash.
    pub head: String,
    /// Whether this is the repository's original (non-linked) worktree.
    pub is_main: bool,
}

/// Returns `true` when `path` is inside a Git repository (has a `.git` file or directory,
/// including linked worktrees where `.git` is a file pointing at the main repo).
#[must_use]
pub fn is_git_repository(path: &Path) -> bool {
    path.join(".git").exists()
}

/// Validates a user-supplied worktree or branch name segment: non-empty, no path separators, no
/// `..`, no leading `-` (which `git` would otherwise parse as a flag), and restricted to a safe
/// character set so it can never be interpreted as a shell token even if later logged or reused.
///
/// # Errors
///
/// Returns [`WorktreeError::InvalidName`] when the name fails any of the above checks.
pub fn validate_name(name: &str) -> Result<(), WorktreeError> {
    if name.is_empty() {
        return Err(WorktreeError::InvalidName("name cannot be empty".into()));
    }
    if name == "." || name == ".." {
        return Err(WorktreeError::InvalidName(
            "name cannot be '.' or '..'".into(),
        ));
    }
    if name.starts_with('-') {
        return Err(WorktreeError::InvalidName(
            "name cannot start with '-'".into(),
        ));
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(WorktreeError::InvalidName(
            "name cannot contain path separators or '..'".into(),
        ));
    }
    let allowed = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !allowed {
        return Err(WorktreeError::InvalidName(
            "name may only contain letters, digits, '-', '_', and '.'".into(),
        ));
    }
    Ok(())
}

/// Derives the predictable, isolated directory new worktrees are created under: a sibling of the
/// repository root named `<repo>-worktrees`, e.g. `/code/myapp` -> `/code/myapp-worktrees`.
///
/// # Errors
///
/// Returns [`WorktreeError::NotAGitRepository`] when `repo_root` has no parent directory or file
/// name to derive the sibling path from.
pub fn worktrees_root(repo_root: &Path) -> Result<PathBuf, WorktreeError> {
    let parent = repo_root.parent().ok_or(WorktreeError::NotAGitRepository)?;
    let repo_name = repo_root
        .file_name()
        .ok_or(WorktreeError::NotAGitRepository)?;
    let mut dir_name = repo_name.to_os_string();
    dir_name.push("-worktrees");
    Ok(parent.join(dir_name))
}

async fn run_git(
    runner: &dyn ProcessRunner,
    repo_root: &Path,
    args: Vec<String>,
) -> Result<crate::process::ProcessResult, WorktreeError> {
    let request = CommandRequest {
        program: "git".into(),
        args,
        cwd: repo_root.to_path_buf(),
        shell: false,
        timeout: Duration::from_secs(60),
    };
    runner
        .run(request, tokio_util::sync::CancellationToken::new(), None)
        .await
        .map_err(|ProcessError::SpawnFailed(message)| WorktreeError::SpawnFailed(message))
}

/// Returns the branch currently checked out in `repo_root`, or `None` for a detached HEAD.
///
/// # Errors
///
/// Returns [`WorktreeError`] when `repo_root` is not a Git repository or `git` fails.
pub async fn current_branch(
    runner: &dyn ProcessRunner,
    repo_root: &Path,
) -> Result<Option<String>, WorktreeError> {
    if !is_git_repository(repo_root) {
        return Err(WorktreeError::NotAGitRepository);
    }
    let process = run_git(
        runner,
        repo_root,
        vec!["rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()],
    )
    .await?;
    if process.exit_code != Some(0) {
        return Err(WorktreeError::GitFailed(process.stderr));
    }
    let branch = process.stdout.trim();
    if branch.is_empty() || branch == "HEAD" {
        Ok(None)
    } else {
        Ok(Some(branch.to_string()))
    }
}

/// Lists every worktree registered against `repo_root`, main worktree first.
///
/// # Errors
///
/// Returns [`WorktreeError::NotAGitRepository`] when `repo_root` is not a Git repository, or
/// [`WorktreeError::GitFailed`]/[`WorktreeError::SpawnFailed`] when `git` cannot be run.
pub async fn list_worktrees(
    runner: &dyn ProcessRunner,
    repo_root: &Path,
) -> Result<Vec<WorktreeEntry>, WorktreeError> {
    if !is_git_repository(repo_root) {
        return Err(WorktreeError::NotAGitRepository);
    }
    let process = run_git(
        runner,
        repo_root,
        vec!["worktree".into(), "list".into(), "--porcelain".into()],
    )
    .await?;
    if process.exit_code != Some(0) {
        return Err(WorktreeError::GitFailed(process.stderr));
    }
    Ok(parse_worktree_list(&process.stdout))
}

fn parse_worktree_list(porcelain: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut head = String::new();
    let mut branch: Option<String> = None;

    let flush = |path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut Option<String>,
                 entries: &mut Vec<WorktreeEntry>| {
        if let Some(path) = path.take() {
            let is_main = entries.is_empty();
            entries.push(WorktreeEntry {
                path,
                branch: branch.take(),
                head: std::mem::take(head),
                is_main,
            });
        }
    };

    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            flush(&mut path, &mut head, &mut branch, &mut entries);
            path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            head = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.strip_prefix("refs/heads/").unwrap_or(rest).to_string());
        }
    }
    flush(&mut path, &mut head, &mut branch, &mut entries);
    entries
}

/// Finds the worktree matching `target`, either by its directory's file name (the worktree
/// "name") or by an exact/canonicalized path match.
///
/// # Errors
///
/// Returns [`WorktreeError::NotFound`] when nothing matches.
pub fn resolve_target<'a>(
    entries: &'a [WorktreeEntry],
    target: &str,
) -> Result<&'a WorktreeEntry, WorktreeError> {
    let target_path = Path::new(target);
    entries
        .iter()
        .find(|entry| {
            entry.path.file_name().and_then(|n| n.to_str()) == Some(target)
                || entry.path == target_path
                || entry
                    .path
                    .canonicalize()
                    .ok()
                    .zip(target_path.canonicalize().ok())
                    .is_some_and(|(a, b)| a == b)
        })
        .ok_or_else(|| WorktreeError::NotFound(target.to_string()))
}

/// Creates a new worktree named `name` under the repository's predictable worktrees directory
/// (see [`worktrees_root`]), checking out either a fresh branch (based on `branch`'s `base`) or
/// an already-existing branch. Never touches the main worktree's branch or files.
///
/// # Errors
///
/// Returns [`WorktreeError`] when `name` is invalid, `repo_root` is not a Git repository, the
/// target directory already exists, the branch already exists (for [`BranchSource::New`]), or
/// `git` otherwise fails.
pub async fn create_worktree(
    runner: &dyn ProcessRunner,
    repo_root: &Path,
    name: &str,
    branch: &BranchSource,
) -> Result<WorktreeEntry, WorktreeError> {
    validate_name(name)?;
    if !is_git_repository(repo_root) {
        return Err(WorktreeError::NotAGitRepository);
    }
    if let BranchSource::Existing(existing) = branch {
        validate_name(existing)?;
    }

    let root = worktrees_root(repo_root)?;
    let target = root.join(name);
    if target.exists() {
        return Err(WorktreeError::InvalidName(format!(
            "'{}' already exists",
            target.display()
        )));
    }

    let mut args = vec!["worktree".into(), "add".into()];
    match branch {
        BranchSource::New { base } => {
            args.push("-b".into());
            args.push(name.to_string());
            args.push(target.display().to_string());
            args.push(base.clone());
        }
        BranchSource::Existing(existing) => {
            args.push(target.display().to_string());
            args.push(existing.clone());
        }
    }

    let process = run_git(runner, repo_root, args).await?;
    if process.exit_code != Some(0) {
        return Err(WorktreeError::GitFailed(process.stderr));
    }

    let branch_name = match branch {
        BranchSource::New { .. } => Some(name.to_string()),
        BranchSource::Existing(existing) => Some(existing.clone()),
    };
    let head = current_head(runner, &target).await.unwrap_or_default();
    Ok(WorktreeEntry {
        path: target,
        branch: branch_name,
        head,
        is_main: false,
    })
}

async fn current_head(runner: &dyn ProcessRunner, worktree_path: &Path) -> Option<String> {
    let process = run_git(
        runner,
        worktree_path,
        vec!["rev-parse".into(), "HEAD".into()],
    )
    .await
    .ok()?;
    (process.exit_code == Some(0)).then(|| process.stdout.trim().to_string())
}

/// Removes an existing worktree, resolved by name or path against `git worktree list`. Refuses
/// to remove the repository's main worktree. Uses `git worktree remove` (never `--force`, never
/// deletes the directory directly), so a worktree with uncommitted changes is refused by `git`
/// itself rather than silently discarded.
///
/// # Errors
///
/// Returns [`WorktreeError::NotAGitRepository`], [`WorktreeError::NotFound`],
/// [`WorktreeError::RefusedMainWorktree`], or [`WorktreeError::GitFailed`].
pub async fn remove_worktree(
    runner: &dyn ProcessRunner,
    repo_root: &Path,
    target: &str,
) -> Result<PathBuf, WorktreeError> {
    let entries = list_worktrees(runner, repo_root).await?;
    let entry = resolve_target(&entries, target)?;
    if entry.is_main {
        return Err(WorktreeError::RefusedMainWorktree);
    }
    let path = entry.path.clone();

    let process = run_git(
        runner,
        repo_root,
        vec![
            "worktree".into(),
            "remove".into(),
            path.display().to_string(),
        ],
    )
    .await?;
    if process.exit_code != Some(0) {
        return Err(WorktreeError::GitFailed(process.stderr));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{
        BranchSource, WorktreeError, create_worktree, current_branch, is_git_repository,
        list_worktrees, remove_worktree, resolve_target, validate_name, worktrees_root,
    };
    use crate::process::TokioProcessRunner;
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
            "gocode-worktree-tests-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(root: &Path, args: &[&str]) {
        let output = Command::new("git")
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

    fn init_repo(root: &Path) {
        run(root, &["init", "-q", "-b", "main"]);
        run(root, &["config", "user.email", "test@example.com"]);
        run(root, &["config", "user.name", "Test"]);
        fs::write(root.join("README.md"), "hello\n").unwrap();
        run(root, &["add", "."]);
        run(root, &["commit", "-q", "-m", "init"]);
    }

    fn cleanup(root: &Path) {
        if let Ok(parent_root) = worktrees_root(root) {
            let _ = fs::remove_dir_all(parent_root);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn names_reject_traversal_and_injection_attempts() {
        assert!(validate_name("feature-x").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("../escape").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("-rf").is_err());
        assert!(validate_name("evil; rm -rf /").is_err());
    }

    #[tokio::test]
    async fn a_non_git_directory_is_reported_clearly() {
        let root = fixture("non-git");
        assert!(!is_git_repository(&root));

        let runner = TokioProcessRunner;
        let error = list_worktrees(&runner, &root).await.unwrap_err();
        assert_eq!(error, WorktreeError::NotAGitRepository);

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn creates_a_worktree_with_a_new_branch_from_the_current_branch() {
        let root = fixture("create-new-branch");
        init_repo(&root);
        let runner = TokioProcessRunner;

        let base = current_branch(&runner, &root).await.unwrap().unwrap();
        assert_eq!(base, "main");

        let entry = create_worktree(
            &runner,
            &root,
            "my-task",
            &BranchSource::New { base: base.clone() },
        )
        .await
        .unwrap();

        assert!(entry.path.exists());
        assert_eq!(entry.branch.as_deref(), Some("my-task"));
        assert_eq!(entry.path, worktrees_root(&root).unwrap().join("my-task"));

        // The main worktree's branch and files are untouched.
        let still_on_main = current_branch(&runner, &root).await.unwrap().unwrap();
        assert_eq!(still_on_main, "main");
        assert!(!root.join("my-task").exists());

        cleanup(&root);
    }

    #[tokio::test]
    async fn creates_a_worktree_from_an_existing_branch_on_request() {
        let root = fixture("create-existing-branch");
        init_repo(&root);
        run(&root, &["branch", "feature-existing"]);
        let runner = TokioProcessRunner;

        let entry = create_worktree(
            &runner,
            &root,
            "reuse",
            &BranchSource::Existing("feature-existing".into()),
        )
        .await
        .unwrap();

        assert_eq!(entry.branch.as_deref(), Some("feature-existing"));

        cleanup(&root);
    }

    #[tokio::test]
    async fn lists_the_main_worktree_and_every_linked_one() {
        let root = fixture("list");
        init_repo(&root);
        let runner = TokioProcessRunner;
        let base = current_branch(&runner, &root).await.unwrap().unwrap();
        create_worktree(&runner, &root, "task-a", &BranchSource::New { base })
            .await
            .unwrap();

        let entries = list_worktrees(&runner, &root).await.unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_main);
        assert_eq!(
            entries[0].path.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
        assert!(!entries[1].is_main);
        assert_eq!(entries[1].branch.as_deref(), Some("task-a"));

        let resolved = resolve_target(&entries, "task-a").unwrap();
        assert_eq!(resolved.path, entries[1].path);

        cleanup(&root);
    }

    #[tokio::test]
    async fn removing_the_main_worktree_is_refused() {
        let root = fixture("remove-main-refused");
        init_repo(&root);
        let runner = TokioProcessRunner;

        let error = remove_worktree(&runner, &root, root.to_str().unwrap())
            .await
            .unwrap_err();

        assert_eq!(error, WorktreeError::RefusedMainWorktree);
        assert!(root.exists());

        cleanup(&root);
    }

    #[tokio::test]
    async fn removing_an_unknown_target_is_reported_clearly() {
        let root = fixture("remove-unknown");
        init_repo(&root);
        let runner = TokioProcessRunner;

        let error = remove_worktree(&runner, &root, "does-not-exist")
            .await
            .unwrap_err();

        assert_eq!(error, WorktreeError::NotFound("does-not-exist".into()));

        cleanup(&root);
    }

    #[tokio::test]
    async fn a_linked_worktree_can_be_removed_by_name() {
        let root = fixture("remove-linked");
        init_repo(&root);
        let runner = TokioProcessRunner;
        let base = current_branch(&runner, &root).await.unwrap().unwrap();
        let created = create_worktree(&runner, &root, "throwaway", &BranchSource::New { base })
            .await
            .unwrap();
        assert!(created.path.exists());

        let removed_path = remove_worktree(&runner, &root, "throwaway").await.unwrap();

        assert_eq!(removed_path, created.path);
        assert!(!created.path.exists());

        cleanup(&root);
    }
}
