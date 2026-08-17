//! Deterministic, minimal-diff patch parsing and application for `apply_patch`.
//!
//! Recognized format:
//!
//! ```text
//! *** Begin Patch
//! *** Update File: src/auth.rs
//!  context line
//! -removed line
//! +added line
//! *** End Patch
//! ```
//!
//! A patch may contain several `*** Update File:`, `*** Add File:`, and `*** Delete File:`
//! sections. `Update File` bodies may split their edits into multiple hunks with `@@` markers;
//! a body with no `@@` marker is treated as a single hunk.

use std::path::{Path, PathBuf};

use crate::{
    contract::{ChangeKind, FileChange, ToolError},
    workspace::{atomic_write_file, resolve_workspace_path},
};

const BEGIN_MARKER: &str = "*** Begin Patch";
const END_MARKER: &str = "*** End Patch";
const UPDATE_PREFIX: &str = "*** Update File: ";
const ADD_PREFIX: &str = "*** Add File: ";
const DELETE_PREFIX: &str = "*** Delete File: ";
const HUNK_MARKER: &str = "@@";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileOperation {
    Update { path: String, hunks: Vec<Hunk> },
    Add { path: String, content: Vec<String> },
    Delete { path: String },
}

/// Parses `patch` into an ordered list of file operations.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when the patch is missing its begin/end markers, has
/// no recognized file sections, or contains a malformed line inside a hunk.
fn parse_patch(patch: &str) -> Result<Vec<FileOperation>, ToolError> {
    let mut lines = patch.lines();
    let first = lines
        .next()
        .map(str::trim)
        .ok_or_else(|| ToolError::InvalidArguments("patch is empty".into()))?;
    if first != BEGIN_MARKER {
        return Err(ToolError::InvalidArguments(format!(
            "patch must start with `{BEGIN_MARKER}`"
        )));
    }

    let body: Vec<&str> = lines.collect();
    let end_index = body
        .iter()
        .position(|line| line.trim() == END_MARKER)
        .ok_or_else(|| {
            ToolError::InvalidArguments(format!("patch must end with `{END_MARKER}`"))
        })?;
    let body = &body[..end_index];

    let mut operations = Vec::new();
    let mut index = 0;
    while index < body.len() {
        let line = body[index];
        if let Some(path) = line.strip_prefix(UPDATE_PREFIX) {
            let (hunks, next) = parse_hunks(body, index + 1)?;
            operations.push(FileOperation::Update {
                path: path.trim().to_string(),
                hunks,
            });
            index = next;
        } else if let Some(path) = line.strip_prefix(ADD_PREFIX) {
            let (content, next) = parse_add_body(body, index + 1);
            operations.push(FileOperation::Add {
                path: path.trim().to_string(),
                content,
            });
            index = next;
        } else if let Some(path) = line.strip_prefix(DELETE_PREFIX) {
            operations.push(FileOperation::Delete {
                path: path.trim().to_string(),
            });
            index += 1;
        } else if line.trim().is_empty() {
            index += 1;
        } else {
            return Err(ToolError::InvalidArguments(format!(
                "unrecognized patch line: `{line}`"
            )));
        }
    }

    if operations.is_empty() {
        return Err(ToolError::InvalidArguments(
            "patch contains no file operations".into(),
        ));
    }

    Ok(operations)
}

/// Scans a `*** Begin Patch` document for the workspace-relative paths its `Update File`, `Add
/// File`, and `Delete File` sections name, without parsing or validating their hunk bodies.
///
/// Used to snapshot a file's pre-edit content before the patch is applied; returns an empty list
/// for a patch that fails [`parse_patch`] later, since callers treat a missing snapshot as "no
/// prior content to restore" rather than an error.
#[must_use]
pub fn touched_paths(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|line| {
            line.strip_prefix(UPDATE_PREFIX)
                .or_else(|| line.strip_prefix(ADD_PREFIX))
                .or_else(|| line.strip_prefix(DELETE_PREFIX))
                .map(|path| path.trim().to_string())
        })
        .collect()
}

fn is_section_marker(line: &str) -> bool {
    line.starts_with(UPDATE_PREFIX)
        || line.starts_with(ADD_PREFIX)
        || line.starts_with(DELETE_PREFIX)
}

fn parse_hunks(body: &[&str], start: usize) -> Result<(Vec<Hunk>, usize), ToolError> {
    let mut index = start;
    let mut hunks = Vec::new();
    let mut current = Hunk {
        old_lines: Vec::new(),
        new_lines: Vec::new(),
    };
    let mut has_content = false;

    while index < body.len() && !is_section_marker(body[index]) {
        let line = body[index];
        if line.starts_with(HUNK_MARKER) {
            if has_content {
                hunks.push(std::mem::replace(
                    &mut current,
                    Hunk {
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                    },
                ));
                has_content = false;
            }
            index += 1;
            continue;
        }

        let mut chars = line.chars();
        match chars.next() {
            Some(' ') => {
                let content = &line[1..];
                current.old_lines.push(content.to_string());
                current.new_lines.push(content.to_string());
                has_content = true;
            }
            Some('-') => {
                current.old_lines.push(line[1..].to_string());
                has_content = true;
            }
            Some('+') => {
                current.new_lines.push(line[1..].to_string());
                has_content = true;
            }
            _ if line.is_empty() => {
                current.old_lines.push(String::new());
                current.new_lines.push(String::new());
                has_content = true;
            }
            _ => {
                return Err(ToolError::InvalidArguments(format!(
                    "patch hunk line must start with ' ', '-', or '+': `{line}`"
                )));
            }
        }
        index += 1;
    }

    if has_content {
        hunks.push(current);
    }
    if hunks.is_empty() {
        return Err(ToolError::InvalidArguments(
            "update section has no hunk content".into(),
        ));
    }
    if hunks.iter().any(|hunk| hunk.old_lines.is_empty()) {
        return Err(ToolError::InvalidArguments(
            "each hunk needs at least one context or removed line to locate its position".into(),
        ));
    }

    Ok((hunks, index))
}

fn parse_add_body(body: &[&str], start: usize) -> (Vec<String>, usize) {
    let mut index = start;
    let mut content = Vec::new();
    while index < body.len() && !is_section_marker(body[index]) {
        let line = body[index];
        content.push(line.strip_prefix('+').unwrap_or(line).to_string());
        index += 1;
    }
    (content, index)
}

struct SplitContent {
    lines: Vec<String>,
    ending: &'static str,
    trailing_newline: bool,
}

fn split_content(content: &str) -> SplitContent {
    let ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let normalized = content.replace("\r\n", "\n");
    let trailing_newline = normalized.ends_with('\n');
    let trimmed = normalized.strip_suffix('\n').unwrap_or(&normalized);
    let lines = if trimmed.is_empty() && !trailing_newline {
        Vec::new()
    } else {
        trimmed.split('\n').map(str::to_string).collect()
    };
    SplitContent {
        lines,
        ending,
        trailing_newline,
    }
}

fn join_content(lines: &[String], ending: &str, trailing_newline: bool) -> String {
    let mut joined = lines.join(ending);
    if trailing_newline && !lines.is_empty() {
        joined.push_str(ending);
    }
    joined
}

/// Every position in `haystack[from..]` where the exact `needle` subsequence occurs. Collecting
/// every match (not just the first) is what lets [`locate_hunk`] tell a genuine ambiguity (the
/// same context appears twice) apart from a clean single match.
fn find_all_subsequences(haystack: &[String], needle: &[String], from: usize) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    (from..=haystack.len() - needle.len())
        .filter(|&start| haystack[start..start + needle.len()] == *needle)
        .collect()
}

/// Same as [`find_all_subsequences`], but ignoring trailing whitespace on each line. Catches the
/// most common near-miss a model produces (context copied with different trailing spaces/tabs
/// than the file actually has) without loosening matching enough to risk landing on the wrong
/// block.
fn find_all_subsequences_fuzzy(haystack: &[String], needle: &[String], from: usize) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    (from..=haystack.len() - needle.len())
        .filter(|&start| {
            (0..needle.len())
                .all(|i| haystack[start + i].trim_end() == needle[i].trim_end())
        })
        .collect()
}

/// Whether a hunk's location was found by an exact match or the trailing-whitespace-insensitive
/// fallback, so the caller can tell the model when a patch was accepted despite not matching
/// byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchKind {
    Exact,
    FuzzyTrailingWhitespace,
}

/// Locates where `hunk.old_lines` starts in `lines` at or after `cursor`.
///
/// Tries an exact match first; a single exact match wins outright. Zero exact matches fall back
/// to a trailing-whitespace-insensitive search. More than one match at either stage is reported
/// as ambiguous rather than silently applied to the first occurrence — applying to the wrong one
/// of several identical blocks is exactly the kind of "quiet corruption" this function exists to
/// prevent.
fn locate_hunk(
    lines: &[String],
    hunk: &Hunk,
    cursor: usize,
    display_path: &str,
) -> Result<(usize, MatchKind), ToolError> {
    let exact = find_all_subsequences(lines, &hunk.old_lines, cursor);
    match exact.len() {
        1 => return Ok((exact[0], MatchKind::Exact)),
        n if n > 1 => {
            return Err(ToolError::InvalidArguments(format!(
                "patch context in `{display_path}` matches {n} locations in the file; add more \
                 surrounding context lines to the hunk so it identifies a unique location"
            )));
        }
        _ => {}
    }

    let fuzzy = find_all_subsequences_fuzzy(lines, &hunk.old_lines, cursor);
    match fuzzy.len() {
        1 => Ok((fuzzy[0], MatchKind::FuzzyTrailingWhitespace)),
        n if n > 1 => Err(ToolError::InvalidArguments(format!(
            "patch context in `{display_path}` matches {n} locations after ignoring trailing \
             whitespace; add more surrounding context lines to the hunk so it identifies a \
             unique location"
        ))),
        _ => Err(ToolError::InvalidArguments(format!(
            "patch could not be applied because the expected context in `{display_path}` no \
             longer matches the file; read the file again and generate a new patch"
        ))),
    }
}

/// UTF-8 byte order mark some editors and Windows tools prepend to text files. Neither Rust's
/// `str` matching nor the patch's `+`/`-`/` ` lines account for it, so it must be stripped before
/// matching and restored after, or the first hunk of a BOM-prefixed file would never match.
const BOM: char = '\u{feff}';

fn apply_hunks(
    original: &str,
    hunks: &[Hunk],
    display_path: &str,
) -> Result<(String, usize), ToolError> {
    let (had_bom, body) = match original.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, original),
    };

    let split = split_content(body);
    let mut lines = split.lines;
    let mut cursor = 0;
    let mut fuzzy_matches = 0;

    for hunk in hunks {
        let (position, kind) = locate_hunk(&lines, hunk, cursor, display_path)?;
        if kind == MatchKind::FuzzyTrailingWhitespace {
            fuzzy_matches += 1;
        }
        lines.splice(
            position..position + hunk.old_lines.len(),
            hunk.new_lines.clone(),
        );
        cursor = position + hunk.new_lines.len();
    }

    let mut result = join_content(&lines, split.ending, split.trailing_newline);
    if had_bom {
        result.insert(0, BOM);
    }
    Ok((result, fuzzy_matches))
}

/// A [`FileChange`] plus how many of its hunks (if any) only matched after ignoring trailing
/// whitespace, so callers can tell the model its patch was accepted but wasn't byte-exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedChange {
    pub change: FileChange,
    pub fuzzy_hunks: usize,
}

/// Applies a `*** Begin Patch` document to the workspace rooted at `project_root`.
///
/// Every file touched by the patch must resolve inside the workspace. Update sections fail
/// atomically per-file when their context no longer matches; files already written by earlier
/// operations in the same patch are not rolled back, matching the documented "prefer atomic
/// behavior, report explicitly on partial application" guidance. A pre-existing UTF-8 BOM is
/// preserved; hunks that only match once trailing whitespace is ignored are applied and reported
/// back via [`AppliedChange::fuzzy_hunks`] rather than rejected outright.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for a malformed patch, an ambiguous or missing hunk
/// context, [`ToolError::OutsideWorkspace`] for a path escaping the project root, and
/// [`ToolError::NotFound`] when updating or deleting a file that does not exist.
pub fn apply_patch(project_root: &Path, patch: &str) -> Result<Vec<AppliedChange>, ToolError> {
    let operations = parse_patch(patch)?;
    let mut changes = Vec::new();

    for operation in operations {
        match operation {
            FileOperation::Update { path, hunks } => {
                let resolved = resolve_workspace_path(project_root, &path)?;
                if !resolved.is_file() {
                    return Err(ToolError::NotFound(resolved));
                }
                let original = std::fs::read_to_string(&resolved).map_err(|error| {
                    ToolError::Io(format!("could not read {}: {error}", resolved.display()))
                })?;
                let (updated, fuzzy_hunks) = apply_hunks(&original, &hunks, &path)?;
                atomic_write_file(&resolved, &updated)?;
                changes.push(AppliedChange {
                    change: FileChange {
                        path: PathBuf::from(path),
                        kind: ChangeKind::Modified,
                    },
                    fuzzy_hunks,
                });
            }
            FileOperation::Add { path, content } => {
                let resolved = resolve_workspace_path(project_root, &path)?;
                if resolved.exists() {
                    return Err(ToolError::InvalidArguments(format!(
                        "`{path}` already exists; use an Update File section instead"
                    )));
                }
                let joined = if content.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", content.join("\n"))
                };
                atomic_write_file(&resolved, &joined)?;
                changes.push(AppliedChange {
                    change: FileChange {
                        path: PathBuf::from(path),
                        kind: ChangeKind::Created,
                    },
                    fuzzy_hunks: 0,
                });
            }
            FileOperation::Delete { path } => {
                let resolved = resolve_workspace_path(project_root, &path)?;
                if !resolved.is_file() {
                    return Err(ToolError::NotFound(resolved));
                }
                std::fs::remove_file(&resolved).map_err(|error| {
                    ToolError::Io(format!("could not delete {}: {error}", resolved.display()))
                })?;
                changes.push(AppliedChange {
                    change: FileChange {
                        path: PathBuf::from(path),
                        kind: ChangeKind::Deleted,
                    },
                    fuzzy_hunks: 0,
                });
            }
        }
    }

    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::apply_patch;
    use crate::contract::{ChangeKind, ToolError};
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
        let dir = std::env::temp_dir().join(format!(
            "gocode-patch-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn single_file_update_replaces_the_matched_lines() {
        let root = fixture("single-update");
        fs::write(
            root.join("main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();

        let patch = "*** Begin Patch\n*** Update File: main.rs\n fn main() {\n-    println!(\"hi\");\n+    println!(\"hello\");\n }\n*** End Patch";
        let changes = apply_patch(&root, patch).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change.kind, ChangeKind::Modified);
        assert_eq!(changes[0].fuzzy_hunks, 0);
        assert_eq!(
            fs::read_to_string(root.join("main.rs")).unwrap(),
            "fn main() {\n    println!(\"hello\");\n}\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multiple_hunks_in_one_file_are_applied_in_order() {
        let root = fixture("multi-hunk");
        fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();

        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n one\n-two\n+TWO\n@@\n four\n-five\n+FIVE\n*** End Patch";
        apply_patch(&root, patch).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\nTWO\nthree\nfour\nFIVE\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_creation_writes_new_content() {
        let root = fixture("add-file");
        let patch =
            "*** Begin Patch\n*** Add File: new_module.rs\n+pub fn hello() {}\n*** End Patch";
        let changes = apply_patch(&root, patch).unwrap();

        assert_eq!(changes[0].change.kind, ChangeKind::Created);
        assert_eq!(
            fs::read_to_string(root.join("new_module.rs")).unwrap(),
            "pub fn hello() {}\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn context_mismatch_fails_without_modifying_the_file() {
        let root = fixture("mismatch");
        fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let patch =
            "*** Begin Patch\n*** Update File: a.txt\n one\n-DOES_NOT_MATCH\n+two\n*** End Patch";
        let error = apply_patch(&root, patch).unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_patch_without_markers_is_rejected() {
        let root = fixture("invalid");
        let error = apply_patch(&root, "not a patch").unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_escape_is_rejected() {
        let root = fixture("escape");
        let patch =
            "*** Begin Patch\n*** Update File: ../outside.txt\n context\n-old\n+new\n*** End Patch";
        let error = apply_patch(&root, patch).unwrap_err();
        assert!(matches!(error, ToolError::OutsideWorkspace(_)));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crlf_line_endings_are_preserved() {
        let root = fixture("crlf");
        fs::write(root.join("a.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();

        let patch = "*** Begin Patch\n*** Update File: a.txt\n one\n-two\n+TWO\n*** End Patch";
        apply_patch(&root, patch).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\r\nTWO\r\nthree\r\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unicode_content_round_trips() {
        let root = fixture("unicode");
        fs::write(root.join("a.txt"), "café\nnaïve\n").unwrap();

        let patch = "*** Begin Patch\n*** Update File: a.txt\n café\n-naïve\n+naive\n*** End Patch";
        apply_patch(&root, patch).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "café\nnaive\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_a_missing_file_is_reported_as_not_found() {
        let root = fixture("delete-missing");
        let patch = "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch";
        let error = apply_patch(&root, patch).unwrap_err();
        assert!(matches!(error, ToolError::NotFound(_)));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_leading_bom_is_preserved_across_an_update() {
        let root = fixture("bom");
        fs::write(root.join("a.txt"), "\u{feff}one\ntwo\nthree\n").unwrap();

        let patch = "*** Begin Patch\n*** Update File: a.txt\n one\n-two\n+TWO\n*** End Patch";
        apply_patch(&root, patch).unwrap();

        let written = fs::read_to_string(root.join("a.txt")).unwrap();
        assert!(written.starts_with('\u{feff}'), "BOM should be preserved");
        assert_eq!(written, "\u{feff}one\nTWO\nthree\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn context_with_mismatched_trailing_whitespace_still_applies_and_is_reported_as_fuzzy() {
        let root = fixture("fuzzy-trailing-ws");
        // The file has trailing whitespace the patch's context/removed lines don't reproduce;
        // an exact match fails but the trailing-whitespace-insensitive fallback finds the
        // (unique) location. The spliced-in lines come from the patch's own text, so the
        // context line's trailing whitespace is normalized away as a side effect of applying
        // at that position.
        fs::write(root.join("a.txt"), "one  \ntwo\t\nthree\n").unwrap();

        let patch = "*** Begin Patch\n*** Update File: a.txt\n one\n-two\n+TWO\n*** End Patch";
        let changes = apply_patch(&root, patch).unwrap();

        assert_eq!(changes[0].fuzzy_hunks, 1);
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ambiguous_context_is_rejected_instead_of_guessing() {
        let root = fixture("ambiguous");
        // "start\nend" appears twice; the hunk gives no way to tell which one is meant.
        fs::write(root.join("a.txt"), "start\nend\nmiddle\nstart\nend\n").unwrap();

        let patch = "*** Begin Patch\n*** Update File: a.txt\n start\n-end\n+END\n*** End Patch";
        let error = apply_patch(&root, patch).unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(message) if message.contains("2 locations")));
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "start\nend\nmiddle\nstart\nend\n"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
