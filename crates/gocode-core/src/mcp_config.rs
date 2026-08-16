//! Configured MCP (Model Context Protocol) servers: how to reach and, if needed, authenticate
//! to each one. Persisted separately from `config.toml`/`project.toml` (in `mcp.toml`, at the
//! same global and project layers) since servers are a growing list of structured records
//! rather than a handful of scalar settings.
//!
//! Secrets never live in this file: an [`McpAuthConfig::ApiKey`] or
//! [`McpAuthConfig::OAuth`] entry only records that a server needs a credential, not the
//! credential itself — that is stored in the OS keyring, one entry per server.

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::{AppError, atomic_write};

/// How gocode reaches an MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpTransportConfig {
    /// A local server launched as a subprocess, speaking JSON-RPC over its stdin/stdout.
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    /// A remote server reachable over MCP's streamable-HTTP transport.
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

impl McpTransportConfig {
    /// Short, user-facing transport label, as shown in the `/mcp` server list.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Stdio { .. } => "stdio",
            Self::Http { .. } => "http",
        }
    }
}

/// How gocode authenticates to an MCP server, if at all.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpAuthConfig {
    /// No authentication required.
    #[default]
    None,
    /// A static bearer token/API key, entered once and stored in the OS keyring.
    ApiKey,
    /// OAuth 2.1 authorization code + PKCE. `client_id` and endpoints are public, non-secret
    /// values; the exchanged access/refresh tokens are stored in the OS keyring.
    OAuth {
        authorization_url: String,
        token_url: String,
        client_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        scopes: Vec<String>,
    },
}

/// One configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerEntry {
    /// Stable, user-chosen name; also the keyring account suffix and the tool-name namespace
    /// prefix (`mcp__<name>__<tool>`).
    pub name: String,
    #[serde(flatten)]
    pub transport: McpTransportConfig,
    #[serde(default, skip_serializing_if = "is_no_auth")]
    pub auth: McpAuthConfig,
    /// Connected servers can be temporarily disabled without deleting their configuration.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn is_no_auth(auth: &McpAuthConfig) -> bool {
    matches!(auth, McpAuthConfig::None)
}

fn default_enabled() -> bool {
    true
}

/// One configured MCP server's live status, for the `/mcp` server list. Distinct from
/// [`McpServerEntry`]: this is runtime state (is it actually connected right now?), not
/// persisted configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerStatus {
    /// Matches the corresponding [`McpServerEntry::name`].
    pub name: String,
    /// Short transport label (`"stdio"` or `"http"`), from [`McpTransportConfig::label`].
    pub transport: &'static str,
    /// Whether this server currently has a live connection.
    pub connected: bool,
    /// Number of tools discovered from this server, `0` when not connected.
    pub tool_count: usize,
    /// Model-facing names of the tools discovered from this server (`mcp__<server>__<tool>`),
    /// for the `/mcp` server detail view. Empty when not connected.
    pub tool_names: Vec<String>,
    /// The most recent connection error, if any. Cleared on a successful connect.
    pub error: Option<String>,
    /// Whether this server is configured for OAuth, so the `/mcp` server list can offer an
    /// explicit "authorize" action for it.
    pub needs_authorization: bool,
}

/// MCP server configuration for one precedence layer (global or project).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpConfig {
    /// Version of the persisted configuration schema.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Configured servers, in file order.
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
}

fn default_schema_version() -> u32 {
    1
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            schema_version: default_schema_version(),
            servers: Vec::new(),
        }
    }
}

impl McpConfig {
    /// Parses and validates an `mcp.toml` document.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Configuration`] when the TOML is invalid, the schema is unsupported,
    /// or a server name is duplicated within this document.
    pub fn parse(contents: &str) -> Result<Self, AppError> {
        let config: Self = toml::from_str(contents).map_err(|error| {
            AppError::Configuration(format!("could not parse mcp.toml: {error}"))
        })?;

        if config.schema_version != 1 {
            return Err(AppError::Configuration(format!(
                "mcp.toml schema_version {} is unsupported; expected 1",
                config.schema_version
            )));
        }

        let mut seen = std::collections::HashSet::new();
        for server in &config.servers {
            if !seen.insert(server.name.as_str()) {
                return Err(AppError::Configuration(format!(
                    "mcp.toml declares server '{}' more than once",
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
    /// Returns [`AppError::Configuration`] if the value cannot be represented as TOML (only
    /// possible for internally inconsistent data, since every field here is TOML-representable).
    pub fn to_toml(&self) -> Result<String, AppError> {
        toml::to_string_pretty(self).map_err(|error| {
            AppError::Configuration(format!("could not serialize mcp.toml: {error}"))
        })
    }

    /// Adds or replaces (by name) a server entry.
    pub fn upsert_server(&mut self, entry: McpServerEntry) {
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

/// Loads `path` as an MCP server configuration, or returns empty schema-v1 defaults when the
/// file does not exist yet (servers are opt-in; nothing is created on disk until the first
/// `/mcp add`).
///
/// # Errors
///
/// Returns [`AppError::Io`] when the file exists but cannot be read, and
/// [`AppError::Configuration`] when it is invalid or unsupported.
pub fn load_or_default_mcp_config(path: &Path) -> Result<McpConfig, AppError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => McpConfig::parse(&contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(McpConfig::default()),
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
pub fn save_mcp_config(path: &Path, config: &McpConfig) -> Result<(), AppError> {
    let contents = config.to_toml()?;
    atomic_write(path, &contents)
}

/// Merges a global and a project-layer server list into the effective set: every global server,
/// plus every project server, with a project entry replacing a global one of the same name.
#[must_use]
pub fn merge_mcp_servers(global: &McpConfig, project: &McpConfig) -> Vec<McpServerEntry> {
    let mut merged: Vec<McpServerEntry> = global.servers.clone();
    for project_server in &project.servers {
        if let Some(existing) = merged.iter_mut().find(|s| s.name == project_server.name) {
            *existing = project_server.clone();
        } else {
            merged.push(project_server.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::{
        McpAuthConfig, McpConfig, McpServerEntry, McpTransportConfig, load_or_default_mcp_config,
        merge_mcp_servers, save_mcp_config,
    };

    fn stdio_entry(name: &str) -> McpServerEntry {
        McpServerEntry {
            name: name.to_string(),
            transport: McpTransportConfig::Stdio {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                ],
                env: std::collections::BTreeMap::new(),
            },
            auth: McpAuthConfig::None,
            enabled: true,
        }
    }

    #[test]
    fn round_trips_a_stdio_server_through_toml() {
        let mut config = McpConfig::default();
        config.upsert_server(stdio_entry("filesystem"));

        let toml = config.to_toml().expect("serialize");
        let parsed = McpConfig::parse(&toml).expect("parse");

        assert_eq!(parsed, config);
    }

    #[test]
    fn round_trips_an_http_server_with_oauth() {
        let mut config = McpConfig::default();
        config.upsert_server(McpServerEntry {
            name: "linear".into(),
            transport: McpTransportConfig::Http {
                url: "https://mcp.example.com".into(),
                headers: std::collections::BTreeMap::new(),
            },
            auth: McpAuthConfig::OAuth {
                authorization_url: "https://example.com/authorize".into(),
                token_url: "https://example.com/token".into(),
                client_id: "gocode".into(),
                scopes: vec!["read".into()],
            },
            enabled: true,
        });

        let toml = config.to_toml().expect("serialize");
        let parsed = McpConfig::parse(&toml).expect("parse");

        assert_eq!(parsed, config);
    }

    #[test]
    fn rejects_a_duplicate_server_name() {
        let toml = r#"
schema_version = 1

[[servers]]
name = "dup"
transport = "stdio"
command = "echo"

[[servers]]
name = "dup"
transport = "stdio"
command = "echo"
"#;
        assert!(McpConfig::parse(toml).is_err());
    }

    #[test]
    fn rejects_an_unsupported_schema_version() {
        let toml = "schema_version = 2\n";
        assert!(McpConfig::parse(toml).is_err());
    }

    #[test]
    fn upsert_replaces_an_existing_server_by_name() {
        let mut config = McpConfig::default();
        config.upsert_server(stdio_entry("filesystem"));
        config.upsert_server(McpServerEntry {
            enabled: false,
            ..stdio_entry("filesystem")
        });

        assert_eq!(config.servers.len(), 1);
        assert!(!config.servers[0].enabled);
    }

    #[test]
    fn remove_server_reports_whether_one_was_present() {
        let mut config = McpConfig::default();
        config.upsert_server(stdio_entry("filesystem"));

        assert!(config.remove_server("filesystem"));
        assert!(!config.remove_server("filesystem"));
    }

    #[test]
    fn missing_file_loads_as_empty_defaults() {
        let path =
            std::env::temp_dir().join(format!("gocode-mcp-test-{}.toml", uuid::Uuid::new_v4()));
        let config = load_or_default_mcp_config(&path).expect("load default");
        assert_eq!(config, McpConfig::default());
    }

    #[test]
    fn save_then_load_round_trips_to_disk() {
        let path =
            std::env::temp_dir().join(format!("gocode-mcp-test-{}.toml", uuid::Uuid::new_v4()));
        let mut config = McpConfig::default();
        config.upsert_server(stdio_entry("filesystem"));

        save_mcp_config(&path, &config).expect("save");
        let loaded = load_or_default_mcp_config(&path).expect("load");
        std::fs::remove_file(&path).ok();

        assert_eq!(loaded, config);
    }

    #[test]
    fn project_server_overrides_global_server_of_the_same_name() {
        let mut global = McpConfig::default();
        global.upsert_server(stdio_entry("filesystem"));
        global.upsert_server(stdio_entry("shared"));

        let mut project = McpConfig::default();
        project.upsert_server(McpServerEntry {
            enabled: false,
            ..stdio_entry("shared")
        });

        let merged = merge_mcp_servers(&global, &project);
        assert_eq!(merged.len(), 2);
        let shared = merged.iter().find(|s| s.name == "shared").unwrap();
        assert!(!shared.enabled);
    }
}
