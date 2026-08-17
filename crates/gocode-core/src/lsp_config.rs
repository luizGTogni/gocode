//! Configured language servers: which command to spawn for which file extensions. Persisted
//! separately from `config.toml`/`project.toml` (in `lsp.toml`, at the same global and project
//! layers), mirroring [`crate::mcp_config`] — servers are a growing list of structured records,
//! not a handful of scalar settings.
//!
//! Unlike MCP servers, language servers need no authentication: they run locally and talk only
//! to the project's own files, so there is no keyring entry to manage.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{AppError, atomic_write};

/// One configured language server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerEntry {
    /// Stable, user-chosen name (e.g. `"rust-analyzer"`); also the key used when a project entry
    /// overrides a global one.
    pub name: String,
    /// File extensions (without the leading dot, e.g. `"rs"`) this server handles.
    pub extensions: Vec<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// Configured servers can be temporarily disabled without deleting their configuration.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Language server configuration for one precedence layer (global or project).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub servers: Vec<LspServerEntry>,
}

fn default_schema_version() -> u32 {
    1
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            schema_version: default_schema_version(),
            servers: Vec::new(),
        }
    }
}

impl LspConfig {
    /// Parses and validates an `lsp.toml` document.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Configuration`] when the TOML is invalid, the schema is unsupported,
    /// or a server name is duplicated within this document.
    pub fn parse(contents: &str) -> Result<Self, AppError> {
        let config: Self = toml::from_str(contents).map_err(|error| {
            AppError::Configuration(format!("could not parse lsp.toml: {error}"))
        })?;

        if config.schema_version != 1 {
            return Err(AppError::Configuration(format!(
                "lsp.toml schema_version {} is unsupported; expected 1",
                config.schema_version
            )));
        }

        let mut seen = std::collections::HashSet::new();
        for server in &config.servers {
            if !seen.insert(server.name.as_str()) {
                return Err(AppError::Configuration(format!(
                    "lsp.toml declares server '{}' more than once",
                    server.name
                )));
            }
        }

        Ok(config)
    }

    /// Serializes this configuration back to TOML, in the same shape [`Self::parse`] accepts.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Configuration`] if the value cannot be represented as TOML.
    pub fn to_toml(&self) -> Result<String, AppError> {
        toml::to_string_pretty(self).map_err(|error| {
            AppError::Configuration(format!("could not serialize lsp.toml: {error}"))
        })
    }

    /// Adds or replaces (by name) a server entry.
    pub fn upsert_server(&mut self, entry: LspServerEntry) {
        if let Some(existing) = self.servers.iter_mut().find(|s| s.name == entry.name) {
            *existing = entry;
        } else {
            self.servers.push(entry);
        }
    }

    /// Removes the server named `name`, reporting whether one was present.
    pub fn remove_server(&mut self, name: &str) -> bool {
        let before = self.servers.len();
        self.servers.retain(|server| server.name != name);
        self.servers.len() != before
    }
}

/// Loads `path` as a language server configuration, or returns empty schema-v1 defaults when the
/// file does not exist yet.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the file exists but cannot be read, and
/// [`AppError::Configuration`] when it is invalid or unsupported.
pub fn load_or_default_lsp_config(path: &Path) -> Result<LspConfig, AppError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => LspConfig::parse(&contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LspConfig::default()),
        Err(error) => Err(AppError::Io(format!(
            "could not read {}: {error}",
            path.display()
        ))),
    }
}

/// Persists `config` to `path` atomically.
///
/// # Errors
///
/// Returns [`AppError::Configuration`] when the value cannot be serialized, or an error from
/// [`atomic_write`] when the replacement cannot be completed.
pub fn save_lsp_config(path: &Path, config: &LspConfig) -> Result<(), AppError> {
    let contents = config.to_toml()?;
    atomic_write(path, &contents)
}

/// Merges a global and a project-layer server list into the effective set: every global server,
/// plus every project server, with a project entry replacing a global one of the same name.
#[must_use]
pub fn merge_lsp_servers(global: &LspConfig, project: &LspConfig) -> Vec<LspServerEntry> {
    let mut merged: Vec<LspServerEntry> = global.servers.clone();
    for project_server in &project.servers {
        if let Some(existing) = merged.iter_mut().find(|s| s.name == project_server.name) {
            *existing = project_server.clone();
        } else {
            merged.push(project_server.clone());
        }
    }
    merged
}

/// Built-in server entries for common languages, used when the user hasn't configured (or
/// disabled) a server for that extension. Callers are expected to filter these to servers whose
/// `command` is actually found on `PATH` before spawning — an unconfigured language should fail
/// with a clear "not supported" message, not a spawn error for a binary the user never installed.
#[must_use]
pub fn builtin_lsp_defaults() -> Vec<LspServerEntry> {
    vec![
        LspServerEntry {
            name: "rust-analyzer".into(),
            extensions: vec!["rs".into()],
            command: "rust-analyzer".into(),
            args: vec![],
            env: std::collections::BTreeMap::new(),
            enabled: true,
        },
        LspServerEntry {
            name: "typescript-language-server".into(),
            extensions: vec!["ts".into(), "tsx".into(), "js".into(), "jsx".into()],
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
            env: std::collections::BTreeMap::new(),
            enabled: true,
        },
        LspServerEntry {
            name: "pyright".into(),
            extensions: vec!["py".into()],
            command: "pyright-langserver".into(),
            args: vec!["--stdio".into()],
            env: std::collections::BTreeMap::new(),
            enabled: true,
        },
        LspServerEntry {
            name: "gopls".into(),
            extensions: vec!["go".into()],
            command: "gopls".into(),
            args: vec![],
            env: std::collections::BTreeMap::new(),
            enabled: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        LspConfig, LspServerEntry, builtin_lsp_defaults, load_or_default_lsp_config,
        merge_lsp_servers, save_lsp_config,
    };

    fn entry(name: &str) -> LspServerEntry {
        LspServerEntry {
            name: name.to_string(),
            extensions: vec!["rs".into()],
            command: "rust-analyzer".into(),
            args: vec![],
            env: std::collections::BTreeMap::new(),
            enabled: true,
        }
    }

    #[test]
    fn round_trips_a_server_through_toml() {
        let mut config = LspConfig::default();
        config.upsert_server(entry("rust-analyzer"));

        let toml = config.to_toml().expect("serialize");
        let parsed = LspConfig::parse(&toml).expect("parse");

        assert_eq!(parsed, config);
    }

    #[test]
    fn rejects_a_duplicate_server_name() {
        let toml = r#"
schema_version = 1

[[servers]]
name = "dup"
extensions = ["rs"]
command = "rust-analyzer"

[[servers]]
name = "dup"
extensions = ["rs"]
command = "rust-analyzer"
"#;
        assert!(LspConfig::parse(toml).is_err());
    }

    #[test]
    fn rejects_an_unsupported_schema_version() {
        let toml = "schema_version = 2\n";
        assert!(LspConfig::parse(toml).is_err());
    }

    #[test]
    fn upsert_replaces_an_existing_server_by_name() {
        let mut config = LspConfig::default();
        config.upsert_server(entry("rust-analyzer"));
        config.upsert_server(LspServerEntry {
            enabled: false,
            ..entry("rust-analyzer")
        });

        assert_eq!(config.servers.len(), 1);
        assert!(!config.servers[0].enabled);
    }

    #[test]
    fn remove_server_reports_whether_one_was_present() {
        let mut config = LspConfig::default();
        config.upsert_server(entry("rust-analyzer"));

        assert!(config.remove_server("rust-analyzer"));
        assert!(!config.remove_server("rust-analyzer"));
    }

    #[test]
    fn missing_file_loads_as_empty_defaults() {
        let path =
            std::env::temp_dir().join(format!("gocode-lsp-test-{}.toml", uuid::Uuid::new_v4()));
        let config = load_or_default_lsp_config(&path).expect("load default");
        assert_eq!(config, LspConfig::default());
    }

    #[test]
    fn save_then_load_round_trips_to_disk() {
        let path =
            std::env::temp_dir().join(format!("gocode-lsp-test-{}.toml", uuid::Uuid::new_v4()));
        let mut config = LspConfig::default();
        config.upsert_server(entry("rust-analyzer"));
        save_lsp_config(&path, &config).expect("save");

        let loaded = load_or_default_lsp_config(&path).expect("load");
        assert_eq!(loaded, config);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn project_server_overrides_global_by_name() {
        let mut global = LspConfig::default();
        global.upsert_server(entry("rust-analyzer"));

        let mut project = LspConfig::default();
        project.upsert_server(LspServerEntry {
            enabled: false,
            ..entry("rust-analyzer")
        });

        let merged = merge_lsp_servers(&global, &project);
        assert_eq!(merged.len(), 1);
        assert!(!merged[0].enabled);
    }

    #[test]
    fn builtin_defaults_cover_the_mvp_languages() {
        let names: Vec<_> = builtin_lsp_defaults()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        for expected in [
            "rust-analyzer",
            "typescript-language-server",
            "pyright",
            "gopls",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
    }
}
