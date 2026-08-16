//! Loading for project-local Markdown documents that extend the interface at runtime: custom
//! slash commands (`.gocode/commands/*.md`) and skills (`.agents/skills/` or the project's
//! `.gocode/skills/` fallback). Both share the same optional YAML-like frontmatter block.

use std::{collections::HashMap, path::Path, path::PathBuf};

/// Splits a Markdown document into its optional leading `---`-delimited frontmatter and body.
///
/// The frontmatter block, when present, is a sequence of `key: value` lines; malformed lines
/// (missing a colon) are ignored. Returns an empty map and the whole text as the body when no
/// frontmatter block is present.
#[must_use]
pub fn parse_frontmatter(text: &str) -> (HashMap<String, String>, &str) {
    let Some(after_marker) = text.strip_prefix("---\n") else {
        return (HashMap::new(), text);
    };
    let Some(end) = after_marker.find("\n---") else {
        return (HashMap::new(), text);
    };

    let frontmatter = &after_marker[..end];
    let rest = &after_marker[end + "\n---".len()..];
    let body = rest.strip_prefix('\n').unwrap_or(rest);

    let fields = frontmatter
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect();

    (fields, body)
}

/// A project-local slash command defined as a Markdown file under `.gocode/commands/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommand {
    /// Invoked as `/{name}`, taken from the filename stem.
    pub name: String,
    /// Short description shown in autocomplete and `/help`, from the `description:` frontmatter
    /// field, if present.
    pub description: String,
    /// The command's prompt template, with `$ARGUMENTS` replaced by the text typed after the
    /// command name.
    pub body: String,
}

/// Loads every `*.md` file directly under `directory` as a [`CustomCommand`].
///
/// Files that cannot be read are skipped. Not recursive: subdirectories are ignored.
#[must_use]
pub fn load_custom_commands(directory: &Path) -> Vec<CustomCommand> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut commands: Vec<CustomCommand> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_stem()?.to_str()?.to_string();
            let text = std::fs::read_to_string(&path).ok()?;
            let (fields, body) = parse_frontmatter(&text);
            Some(CustomCommand {
                name,
                description: fields.get("description").cloned().unwrap_or_default(),
                body: body.trim().to_string(),
            })
        })
        .collect();

    commands.sort_by(|a, b| a.name.cmp(&b.name));
    commands
}

/// Where a discovered skill came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// Found under the user's global `~/.agents/skills/` directory.
    Global,
    /// Found under the project's `.agents/skills/` (or `.gocode/skills/` fallback) directory.
    Project,
}

/// A discovered skill: a short capability description the model can read the full file for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    /// The skill's name, from the `name:` frontmatter field, or the file/directory stem.
    pub name: String,
    /// Short description of when and how to use the skill, from `description:` frontmatter.
    pub description: String,
    /// Whether this skill came from the global or the project skills directory.
    pub source: SkillSource,
    /// Path to the skill's Markdown file, for the model to read on demand.
    pub path: PathBuf,
}

/// Loads skills from `global_dir` (if given and present) and from `project_dir`.
///
/// A skill is either `<project_dir>/<name>/SKILL.md` or a bare `<project_dir>/<name>.md`.
/// Directories that don't exist are silently skipped.
#[must_use]
pub fn load_skills(global_dir: Option<&Path>, project_dir: &Path) -> Vec<SkillSummary> {
    let mut skills = Vec::new();
    if let Some(global_dir) = global_dir {
        skills.extend(load_skills_from(global_dir, SkillSource::Global));
    }
    skills.extend(load_skills_from(project_dir, SkillSource::Project));
    skills
}

fn load_skills_from(directory: &Path, source: SkillSource) -> Vec<SkillSummary> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut skills: Vec<SkillSummary> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            let (skill_path, stem) = if file_type.is_dir() {
                let skill_md = path.join("SKILL.md");
                if !skill_md.is_file() {
                    return None;
                }
                (skill_md, path.file_name()?.to_str()?.to_string())
            } else if path.extension().is_some_and(|ext| ext == "md") {
                (path.clone(), path.file_stem()?.to_str()?.to_string())
            } else {
                return None;
            };

            let text = std::fs::read_to_string(&skill_path).ok()?;
            let (fields, _body) = parse_frontmatter(&text);
            Some(SkillSummary {
                name: fields.get("name").cloned().unwrap_or(stem),
                description: fields.get("description").cloned().unwrap_or_default(),
                source,
                path: skill_path,
            })
        })
        .collect();

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_extracts_fields_and_body() {
        let text = "---\ndescription: Do the thing\nname: thing\n---\nBody text here.\n";
        let (fields, body) = parse_frontmatter(text);
        assert_eq!(fields.get("description"), Some(&"Do the thing".to_string()));
        assert_eq!(fields.get("name"), Some(&"thing".to_string()));
        assert_eq!(body, "Body text here.\n");
    }

    #[test]
    fn parse_frontmatter_missing_block_returns_whole_text_as_body() {
        let text = "Just a plain command body.\n";
        let (fields, body) = parse_frontmatter(text);
        assert!(fields.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn parse_frontmatter_unterminated_block_returns_whole_text_as_body() {
        let text = "---\ndescription: oops\nno closing marker\n";
        let (fields, body) = parse_frontmatter(text);
        assert!(fields.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn load_custom_commands_reads_md_files_and_sorts_by_name() {
        let dir = std::env::temp_dir().join(format!("gocode-test-cmds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("zeta.md"),
            "---\ndescription: Zeta command\n---\nRun zeta on $ARGUMENTS.",
        )
        .unwrap();
        std::fs::write(dir.join("alpha.md"), "Plain alpha body.").unwrap();
        std::fs::write(dir.join("ignore.txt"), "not markdown").unwrap();

        let commands = load_custom_commands(&dir);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].name, "alpha");
        assert_eq!(commands[0].description, "");
        assert_eq!(commands[0].body, "Plain alpha body.");
        assert_eq!(commands[1].name, "zeta");
        assert_eq!(commands[1].description, "Zeta command");
        assert_eq!(commands[1].body, "Run zeta on $ARGUMENTS.");
    }

    #[test]
    fn load_skills_reads_bare_files_and_skill_md_directories() {
        let dir = std::env::temp_dir().join(format!("gocode-test-skills-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("bundled")).unwrap();
        std::fs::write(
            dir.join("bundled").join("SKILL.md"),
            "---\nname: bundled-skill\ndescription: A bundled skill\n---\nBody.",
        )
        .unwrap();
        std::fs::write(dir.join("flat.md"), "---\ndescription: A flat skill\n---\n").unwrap();

        let skills = load_skills(None, &dir);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(skills.len(), 2);
        assert!(
            skills
                .iter()
                .any(|skill| skill.name == "bundled-skill" && skill.source == SkillSource::Project)
        );
        assert!(skills.iter().any(|skill| skill.name == "flat"));
    }

    #[test]
    fn load_skills_missing_directories_return_empty() {
        let missing = std::env::temp_dir().join(format!("gocode-missing-{}", uuid::Uuid::new_v4()));
        assert!(load_skills(Some(&missing), &missing).is_empty());
    }
}
