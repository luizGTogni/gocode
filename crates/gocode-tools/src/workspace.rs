//! Shared path validation, discovery, and atomic-write primitives used by every filesystem tool.

use std::{
    io::Write,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::contract::ToolError;

/// Resolves a model-supplied path against `project_root` and proves it stays inside the
/// workspace, following symlinks for every path segment that already exists on disk.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for an empty path, and
/// [`ToolError::OutsideWorkspace`] when the resolved path — after normalization and symlink
/// resolution — falls outside `project_root`.
pub fn resolve_workspace_path(project_root: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    if raw.trim().is_empty() {
        return Err(ToolError::InvalidArguments("path must not be empty".into()));
    }

    let raw_path = Path::new(raw);
    let joined = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        project_root.join(raw_path)
    };
    let normalized = normalize_lexically(&joined);

    let canonical_root = canonicalize_best_effort(project_root);
    let resolved = resolve_through_symlinks(&normalized);

    if resolved.starts_with(&canonical_root) {
        Ok(resolved)
    } else {
        Err(ToolError::OutsideWorkspace(normalized))
    }
}

/// Renders `path` relative to `project_root` using forward slashes, for model-facing output.
#[must_use]
pub fn to_workspace_relative(project_root: &Path, path: &Path) -> String {
    let canonical_root = canonicalize_best_effort(project_root);
    let candidate = resolve_through_symlinks(path);
    let relative = candidate
        .strip_prefix(&canonical_root)
        .unwrap_or(&candidate);
    relative.to_string_lossy().replace('\\', "/")
}

/// Resolves the longest existing ancestor of `path` to its canonical form, then rejoins the
/// remaining (possibly nonexistent) trailing components. This lets the boundary check catch a
/// symlink escape even for a file that does not exist yet.
fn resolve_through_symlinks(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut trailing: Vec<std::ffi::OsString> = Vec::new();

    while !existing.exists() {
        match existing.file_name().map(std::ffi::OsStr::to_os_string) {
            Some(name) => {
                trailing.push(name);
                existing.pop();
            }
            None => break,
        }
    }

    let mut resolved = existing.canonicalize().unwrap_or(existing);
    for component in trailing.into_iter().rev() {
        resolved.push(component);
    }
    resolved
}

fn canonicalize_best_effort(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| normalize_lexically(path))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if matches!(result.components().next_back(), Some(Component::Normal(_))) {
                    result.pop();
                } else {
                    result.push(component.as_os_str());
                }
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// Default names excluded from general project discovery even when not `.gitignore`d.
pub const DEFAULT_EXCLUDED_DIRS: &[&str] = &[".git", ".gocode"];

/// One discovered filesystem entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredEntry {
    /// Project-relative, forward-slash path.
    pub relative_path: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

/// Walks the workspace beneath `start`, honoring `.gitignore` and default exclusions.
///
/// # Errors
///
/// Returns [`ToolError::OutsideWorkspace`] when `start` (relative to `project_root`) escapes the
/// workspace, and [`ToolError::NotFound`] when it does not exist.
pub fn discover(
    project_root: &Path,
    start: &str,
    max_depth: Option<usize>,
    limit: usize,
) -> Result<Vec<DiscoveredEntry>, ToolError> {
    let root = resolve_workspace_path(project_root, start)?;
    if !root.exists() {
        return Err(ToolError::NotFound(root));
    }

    let canonical_project_root = canonicalize_best_effort(project_root);
    let mut builder = ignore::WalkBuilder::new(&root);
    builder.hidden(false).git_ignore(true).git_exclude(true);
    if let Some(depth) = max_depth {
        builder.max_depth(Some(depth));
    }

    let mut entries = Vec::new();
    for result in builder.build() {
        let Ok(entry) = result else { continue };
        let path = entry.path();
        if path == root {
            continue;
        }
        if path
            .components()
            .any(|component| matches!(component, Component::Normal(name) if DEFAULT_EXCLUDED_DIRS.contains(&name.to_string_lossy().as_ref())))
        {
            continue;
        }

        let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        entries.push(DiscoveredEntry {
            relative_path: to_workspace_relative(&canonical_project_root, path),
            is_dir,
        });

        if entries.len() >= limit {
            break;
        }
    }

    entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(entries)
}

/// Number of leading bytes inspected to decide whether content looks binary.
const BINARY_SNIFF_LEN: usize = 8000;

/// Heuristically detects binary content by scanning for a NUL byte, the same signal Git uses.
#[must_use]
pub fn looks_binary(content: &[u8]) -> bool {
    content.iter().take(BINARY_SNIFF_LEN).any(|byte| *byte == 0)
}

/// Atomically replaces (or creates) a UTF-8 text file.
///
/// # Errors
///
/// Returns [`ToolError::Io`] when the temporary file cannot be written, flushed, or renamed into
/// place.
pub fn atomic_write_file(path: &Path, contents: &str) -> Result<(), ToolError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent).map_err(|error| {
            ToolError::Io(format!("could not create {}: {error}", parent.display()))
        })?;
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| ToolError::Io(format!("{} has no file name", path.display())))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp_path = path.with_file_name(format!(
        ".{}.{}-{nonce}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));

    let write_result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp_path)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()
    })();

    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(ToolError::Io(format!(
            "could not write {}: {error}",
            path.display()
        )));
    }

    std::fs::rename(&temp_path, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp_path);
        ToolError::Io(format!("could not replace {}: {error}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::{atomic_write_file, discover, looks_binary, resolve_workspace_path};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_fixture_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after the Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gocode-tools-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("fixture directory should be created");
        dir
    }

    #[test]
    fn relative_path_inside_workspace_resolves() {
        let root = unique_fixture_dir("relative");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let resolved = resolve_workspace_path(&root, "src/main.rs").unwrap();
        assert!(
            resolved.ends_with("src/main.rs")
                || resolved.to_string_lossy().ends_with(r"src\main.rs")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parent_traversal_outside_workspace_is_rejected() {
        let root = unique_fixture_dir("traversal");
        let error = resolve_workspace_path(&root, "../../secret.txt").unwrap_err();
        assert!(matches!(error, super::ToolError::OutsideWorkspace(_)));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn absolute_path_outside_workspace_is_rejected() {
        let root = unique_fixture_dir("absolute-outside");
        let outside = unique_fixture_dir("absolute-outside-target");

        let error = resolve_workspace_path(&root, outside.to_str().unwrap()).unwrap_err();
        assert!(matches!(error, super::ToolError::OutsideWorkspace(_)));

        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn nested_relative_path_normalizes_dot_segments() {
        let root = unique_fixture_dir("dot-segments");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let resolved = resolve_workspace_path(&root, "./src/../src/main.rs").unwrap();
        assert!(resolved.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_the_workspace_is_rejected() {
        let root = unique_fixture_dir("symlink-escape");
        let outside = unique_fixture_dir("symlink-escape-target");
        fs::write(outside.join("secret.txt"), "top secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let error = resolve_workspace_path(&root, "link/secret.txt").unwrap_err();
        assert!(matches!(error, super::ToolError::OutsideWorkspace(_)));

        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn missing_file_still_resolves_for_future_creation() {
        let root = unique_fixture_dir("missing-file");
        let resolved = resolve_workspace_path(&root, "new/nested/file.txt").unwrap();
        assert!(resolved.starts_with(root.canonicalize().unwrap()));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn null_bytes_are_detected_as_binary() {
        assert!(looks_binary(b"hello\0world"));
        assert!(!looks_binary(b"hello world"));
    }

    #[test]
    fn atomic_write_creates_parent_directories_and_replaces_content() {
        let root = unique_fixture_dir("atomic-write");
        let target = root.join("nested/output.txt");

        atomic_write_file(&target, "first").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "first");

        atomic_write_file(&target, "second").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "second");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discover_respects_gitignore_and_default_exclusions() {
        let root = unique_fixture_dir("discover");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(".gocode")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(root.join("target/ignored.txt"), "ignored").unwrap();
        fs::write(root.join(".gocode/instructions.md"), "notes").unwrap();

        let entries = discover(&root, ".", None, 200).unwrap();
        let paths: Vec<_> = entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect();

        assert!(paths.contains(&"Cargo.toml".to_string()));
        assert!(!paths.iter().any(|path| path.starts_with("target")));
        assert!(!paths.iter().any(|path| path.starts_with(".gocode")));
        assert!(
            !paths
                .iter()
                .any(|path| path == ".git" || path.starts_with(".git/"))
        );

        fs::remove_dir_all(root).unwrap();
    }
}
