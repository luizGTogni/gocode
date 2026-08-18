//! Built-in MVP tool implementations.

pub mod apply_patch;
pub mod ask_user;
pub mod git;
pub mod list_files;
pub mod read_file;
pub mod run_command;
pub mod search;
pub mod write_file;

use serde::de::DeserializeOwned;

use crate::contract::{ToolContext, ToolError};

/// Parses and validates model-supplied JSON arguments against `T`'s shape.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when `input` does not deserialize into `T`.
pub(crate) fn parse_args<T: DeserializeOwned>(input: serde_json::Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|error| ToolError::InvalidArguments(error.to_string()))
}

/// Returns [`ToolError::Cancelled`] if the run's cancellation token has already fired.
pub(crate) fn check_cancelled(ctx: &ToolContext) -> Result<(), ToolError> {
    if ctx.cancellation.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

/// Caps `content` at `max_chars`, splitting at a `char` boundary, and reports truncation.
pub(crate) fn cap_chars(content: String, max_chars: usize) -> (String, bool) {
    if content.len() <= max_chars {
        return (content, false);
    }
    let mut split = max_chars;
    while split > 0 && !content.is_char_boundary(split) {
        split -= 1;
    }
    (content[..split].to_string(), true)
}

/// Matches `text` against a simple glob `pattern` supporting `*` (any run of characters) and `?`
/// (exactly one character). Sufficient for tool-facing filters like `*.rs`.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut memo = vec![vec![None; text.len() + 1]; pattern.len() + 1];
    glob_match_inner(&pattern, &text, 0, 0, &mut memo)
}

fn glob_match_inner(
    pattern: &[char],
    text: &[char],
    p: usize,
    t: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(cached) = memo[p][t] {
        return cached;
    }
    let result = if p == pattern.len() {
        t == text.len()
    } else if pattern[p] == '*' {
        (t..=text.len()).any(|next_t| glob_match_inner(pattern, text, p + 1, next_t, memo))
    } else if t < text.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
        glob_match_inner(pattern, text, p + 1, t + 1, memo)
    } else {
        false
    };
    memo[p][t] = Some(result);
    result
}

#[cfg(test)]
mod tests {
    use super::{cap_chars, glob_match};

    #[test]
    fn cap_chars_leaves_short_content_untouched() {
        assert_eq!(cap_chars("hello".into(), 10), ("hello".into(), false));
    }

    #[test]
    fn cap_chars_truncates_long_content_at_a_char_boundary() {
        let (content, truncated) = cap_chars("héllo".into(), 2);
        assert!(truncated);
        assert!(content.len() <= 2);
    }

    #[test]
    fn glob_match_supports_extension_wildcards() {
        assert!(glob_match("*.rs", "src/main.rs"));
        assert!(!glob_match("*.rs", "src/main.py"));
    }

    #[test]
    fn glob_match_supports_single_character_wildcards() {
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
    }
}
