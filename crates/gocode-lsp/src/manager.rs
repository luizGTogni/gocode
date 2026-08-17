//! Owns one [`LspClient`] per language actually touched during a session, spawned lazily on
//! first use and kept alive for the rest of the session, plus a declarative table describing
//! which command to spawn for which file extension.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use gocode_tools::contract::{ChangeKind, FileChange, FileChangeObserver};
use tokio::sync::Mutex;

use crate::{LspClient, LspError};

/// Declarative description of one language server: which extensions it covers and how to spawn
/// it. Mirrors the shape of `gocode-core`'s `LspServerEntry` config so callers can build this
/// straight from a loaded, merged `lsp.toml`.
#[derive(Debug, Clone)]
pub struct LspServerSpec {
    pub name: String,
    pub extensions: Vec<String>,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Re-exported so callers of this crate don't need a direct `gocode-tools` dependency just to
/// name the file-change kind.
pub use gocode_tools::contract::ChangeKind as FileChangeKind;

/// Manages the set of live language server connections for one project.
pub struct LspManager {
    project_root: PathBuf,
    specs_by_extension: HashMap<String, LspServerSpec>,
    clients: Mutex<HashMap<String, Arc<LspClient>>>,
}

/// Whether `command` resolves to an executable on `PATH`. Used to decide whether a built-in
/// default server spec is actually usable, so an unconfigured language fails with a clear
/// "not supported" message instead of a spawn error for a binary the user never installed.
#[must_use]
pub fn is_command_available(command: &str) -> bool {
    which::which(command).is_ok()
}

impl LspManager {
    /// Builds a manager for `project_root` from a flat list of server specs. Extensions are
    /// matched case-insensitively; a later spec wins over an earlier one for the same extension.
    #[must_use]
    pub fn new(project_root: PathBuf, specs: Vec<LspServerSpec>) -> Self {
        let mut specs_by_extension = HashMap::new();
        for spec in specs {
            for extension in &spec.extensions {
                specs_by_extension.insert(extension.to_ascii_lowercase(), spec.clone());
            }
        }
        Self {
            project_root,
            specs_by_extension,
            clients: Mutex::new(HashMap::new()),
        }
    }

    fn extension_of(path: &Path) -> Option<String> {
        path.extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
    }

    fn language_id_of(extension: &str) -> &'static str {
        match extension {
            "rs" => "rust",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            "py" => "python",
            "go" => "go",
            _ => "plaintext",
        }
    }

    /// Resolves (spawning on first use) the client responsible for `path`'s language.
    ///
    /// # Errors
    /// Returns [`LspError::Unsupported`] if no server is configured for the file's extension, or
    /// a transport/protocol error if spawning or the `initialize` handshake fails.
    pub async fn client_for(&self, path: &Path) -> Result<Arc<LspClient>, LspError> {
        let extension = Self::extension_of(path).ok_or_else(|| {
            LspError::Unsupported(format!("{} has no file extension", path.display()))
        })?;
        let spec = self.specs_by_extension.get(&extension).ok_or_else(|| {
            LspError::Unsupported(format!(
                "no language server configured for .{extension} files"
            ))
        })?;

        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(&spec.name) {
            return Ok(Arc::clone(client));
        }

        let client = Arc::new(
            LspClient::start(&spec.command, &spec.args, &spec.env, &self.project_root).await?,
        );
        clients.insert(spec.name.clone(), Arc::clone(&client));
        Ok(client)
    }

    /// Notifies the responsible language server that `path` changed, opening or updating the
    /// document so future queries and diagnostics reflect the new content. Silently does nothing
    /// if no server is configured for the file's language, if the file was deleted, or if
    /// reading it fails — this is best-effort background housekeeping, not a user-facing action.
    pub async fn notify_file_changed(&self, path: &Path, kind: ChangeKind) {
        if kind == ChangeKind::Deleted {
            return;
        }
        let Ok(client) = self.client_for(path).await else {
            return;
        };
        let Ok(text) = tokio::fs::read_to_string(self.project_root.join(path)).await else {
            return;
        };
        let extension = Self::extension_of(path).unwrap_or_default();
        let language_id = Self::language_id_of(&extension);
        let _ = client.did_change(path, &text, language_id).await;
    }
}

/// Adapter that implements [`FileChangeObserver`] for an `Arc<LspManager>`, since the trait's
/// `&self` receiver can't spawn a `'static` task holding a borrowed manager directly.
pub struct ArcLspManagerObserver(pub Arc<LspManager>);

impl FileChangeObserver for ArcLspManagerObserver {
    fn notify(&self, _project_root: &Path, change: &FileChange) {
        let manager = Arc::clone(&self.0);
        let path = change.path.clone();
        let kind = change.kind;
        tokio::spawn(async move {
            manager.notify_file_changed(&path, kind).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{LspManager, LspServerSpec};
    use std::path::{Path, PathBuf};

    fn spec(name: &str, extensions: &[&str]) -> LspServerSpec {
        LspServerSpec {
            name: name.to_string(),
            extensions: extensions.iter().map(ToString::to_string).collect(),
            command: "does-not-matter".into(),
            args: vec![],
            env: vec![],
        }
    }

    #[tokio::test]
    async fn reports_unsupported_for_an_extension_with_no_configured_server() {
        let manager = LspManager::new(PathBuf::from("."), vec![spec("rust-analyzer", &["rs"])]);
        let result = manager.client_for(Path::new("main.py")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn reports_unsupported_for_a_file_with_no_extension() {
        let manager = LspManager::new(PathBuf::from("."), vec![spec("rust-analyzer", &["rs"])]);
        let result = manager.client_for(Path::new("Makefile")).await;
        assert!(result.is_err());
    }
}
