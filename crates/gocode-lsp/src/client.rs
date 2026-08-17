//! A client for one running language server: the `initialize` handshake, the read-only queries
//! the [`crate::tool::LspTool`] exposes to the model, and a background task that keeps a
//! per-file diagnostics cache fresh from the server's `textDocument/publishDiagnostics` pushes.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    LspError,
    jsonrpc::{JsonRpcNotification, JsonRpcRequest, RequestId},
    transport::{LspTransport, stdio::StdioTransport},
};

/// One diagnostic (error/warning/hint) a language server has reported for a file. Deliberately
/// minimal — enough for the model to locate and understand the problem.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub line: u32,
    pub character: u32,
    pub severity: Option<String>,
    pub message: String,
    pub source: Option<String>,
}

type DiagnosticsCache = Arc<std::sync::Mutex<HashMap<PathBuf, Vec<Diagnostic>>>>;

/// A live connection to one language server process, plus the open-document state LSP requires
/// (`didOpen`/`didChange` with monotonically increasing versions).
pub struct LspClient {
    transport: Arc<dyn LspTransport>,
    next_id: AtomicI64,
    open_versions: Mutex<HashMap<PathBuf, i64>>,
    diagnostics: DiagnosticsCache,
    root_uri: String,
}

fn path_to_uri(path: &Path) -> String {
    let display = path.to_string_lossy().replace('\\', "/");
    if display.starts_with('/') {
        format!("file://{display}")
    } else {
        format!("file:///{display}")
    }
}

impl LspClient {
    /// Spawns `command` at `project_root`, performs the `initialize`/`initialized` handshake,
    /// and starts a background task that folds `textDocument/publishDiagnostics` notifications
    /// into the diagnostics cache.
    ///
    /// # Errors
    /// Returns an error if the process cannot be spawned or the handshake fails.
    pub async fn start(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        project_root: &Path,
    ) -> Result<Self, LspError> {
        let transport: Arc<dyn LspTransport> = Arc::new(StdioTransport::spawn(command, args, env)?);
        let root_uri = path_to_uri(project_root);

        let client = Self {
            transport,
            next_id: AtomicI64::new(1),
            open_versions: Mutex::new(HashMap::new()),
            diagnostics: Arc::new(std::sync::Mutex::new(HashMap::new())),
            root_uri: root_uri.clone(),
        };

        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "hover": {},
                    "definition": {},
                    "references": {},
                    "documentSymbol": {},
                    "publishDiagnostics": {},
                }
            },
        });
        client.request("initialize", init_params).await?;
        client
            .transport
            .send_notification(JsonRpcNotification::new("initialized", Some(json!({}))))
            .await?;

        client.spawn_diagnostics_listener();
        Ok(client)
    }

    fn spawn_diagnostics_listener(&self) {
        let transport = Arc::clone(&self.transport);
        let diagnostics = Arc::clone(&self.diagnostics);
        tokio::spawn(async move {
            loop {
                match transport.recv_notification().await {
                    Ok(Some(notification))
                        if notification.method == "textDocument/publishDiagnostics" =>
                    {
                        if let Some(params) = notification.params {
                            apply_published_diagnostics(&diagnostics, &params);
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        });
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        let id = RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let request = JsonRpcRequest::new(id, method, Some(params));
        let response = self.transport.send_request(request).await?;
        response.into_result(method)
    }

    /// Notifies the server a document was opened (or reopened after an edit) with its current
    /// content, starting version tracking at `1`.
    ///
    /// # Errors
    /// Returns an error if the notification cannot be sent.
    pub async fn did_open(
        &self,
        path: &Path,
        text: &str,
        language_id: &str,
    ) -> Result<(), LspError> {
        let mut versions = self.open_versions.lock().await;
        let version = 1;
        versions.insert(path.to_path_buf(), version);
        drop(versions);

        self.transport
            .send_notification(JsonRpcNotification::new(
                "textDocument/didOpen",
                Some(json!({
                    "textDocument": {
                        "uri": path_to_uri(path),
                        "languageId": language_id,
                        "version": version,
                        "text": text,
                    }
                })),
            ))
            .await
    }

    /// Notifies the server a document changed, sending the full new content and incrementing
    /// its version. Opens the document first if it was not already tracked.
    ///
    /// # Errors
    /// Returns an error if the notification cannot be sent.
    pub async fn did_change(
        &self,
        path: &Path,
        text: &str,
        language_id: &str,
    ) -> Result<(), LspError> {
        let mut versions = self.open_versions.lock().await;
        let Some(previous) = versions.get(path).copied() else {
            drop(versions);
            return self.did_open(path, text, language_id).await;
        };
        let version = previous + 1;
        versions.insert(path.to_path_buf(), version);
        drop(versions);

        self.transport
            .send_notification(JsonRpcNotification::new(
                "textDocument/didChange",
                Some(json!({
                    "textDocument": { "uri": path_to_uri(path), "version": version },
                    "contentChanges": [{ "text": text }],
                })),
            ))
            .await
    }

    /// Queries hover information (type signature, docs) at a position.
    ///
    /// # Errors
    /// Returns an error if the request fails.
    pub async fn hover(&self, path: &Path, line: u32, character: u32) -> Result<Value, LspError> {
        self.request(
            "textDocument/hover",
            text_document_position(path, line, character),
        )
        .await
    }

    /// Queries the definition site(s) of the symbol at a position.
    ///
    /// # Errors
    /// Returns an error if the request fails.
    pub async fn definition(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value, LspError> {
        self.request(
            "textDocument/definition",
            text_document_position(path, line, character),
        )
        .await
    }

    /// Queries every reference to the symbol at a position.
    ///
    /// # Errors
    /// Returns an error if the request fails.
    pub async fn references(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value, LspError> {
        let mut params = text_document_position(path, line, character);
        params["context"] = json!({ "includeDeclaration": true });
        self.request("textDocument/references", params).await
    }

    /// Lists every symbol (function, type, etc.) declared in a document.
    ///
    /// # Errors
    /// Returns an error if the request fails.
    pub async fn document_symbol(&self, path: &Path) -> Result<Value, LspError> {
        self.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": path_to_uri(path) } }),
        )
        .await
    }

    /// Returns the most recently cached diagnostics for `path`. Best-effort: reflects whatever
    /// the server has pushed so far, which may be briefly stale right after an edit.
    ///
    /// # Panics
    /// Panics only if the internal diagnostics mutex is poisoned by an earlier panic.
    #[must_use]
    pub fn cached_diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.diagnostics
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// The `rootUri` this client was initialized with.
    #[must_use]
    pub fn root_uri(&self) -> &str {
        &self.root_uri
    }
}

fn text_document_position(path: &Path, line: u32, character: u32) -> Value {
    json!({
        "textDocument": { "uri": path_to_uri(path) },
        "position": { "line": line, "character": character },
    })
}

fn apply_published_diagnostics(diagnostics: &DiagnosticsCache, params: &Value) {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return;
    };
    let Some(path) = uri_to_path(uri) else {
        return;
    };
    let entries = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_diagnostic).collect())
        .unwrap_or_default();
    diagnostics.lock().unwrap().insert(path, entries);
}

fn parse_diagnostic(value: &Value) -> Option<Diagnostic> {
    let range = value.get("range")?;
    let start = range.get("start")?;
    let line = u32::try_from(start.get("line")?.as_u64()?).ok()?;
    let character = u32::try_from(start.get("character")?.as_u64()?).ok()?;
    let message = value.get("message")?.as_str()?.to_string();
    let severity = value
        .get("severity")
        .and_then(Value::as_u64)
        .map(|code| match code {
            1 => "error",
            2 => "warning",
            3 => "information",
            _ => "hint",
        })
        .map(str::to_string);
    let source = value
        .get("source")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(Diagnostic {
        line,
        character,
        severity,
        message,
        source,
    })
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let stripped = uri.strip_prefix("file://")?;
    #[cfg(windows)]
    let stripped = stripped.strip_prefix('/').unwrap_or(stripped);
    Some(PathBuf::from(stripped))
}

#[cfg(test)]
mod tests {
    use super::{apply_published_diagnostics, path_to_uri, uri_to_path};
    use serde_json::json;
    use std::{collections::HashMap, path::PathBuf, sync::Mutex};

    #[test]
    fn round_trips_a_path_through_a_uri() {
        #[cfg(windows)]
        let path = PathBuf::from(r"C:\Users\user\project\src\lib.rs");
        #[cfg(not(windows))]
        let path = PathBuf::from("/home/user/project/src/lib.rs");

        let uri = path_to_uri(&path);
        let round_tripped = uri_to_path(&uri).unwrap();
        assert_eq!(
            round_tripped.to_string_lossy().replace('\\', "/"),
            path.to_string_lossy().replace('\\', "/")
        );
    }

    #[test]
    fn applies_published_diagnostics_to_the_cache() {
        #[cfg(windows)]
        let path = PathBuf::from(r"C:\proj\src\main.rs");
        #[cfg(not(windows))]
        let path = PathBuf::from("/proj/src/main.rs");

        let cache = std::sync::Arc::new(Mutex::new(HashMap::new()));
        let params = json!({
            "uri": path_to_uri(&path),
            "diagnostics": [{
                "range": {"start": {"line": 4, "character": 2}, "end": {"line": 4, "character": 10}},
                "severity": 1,
                "message": "mismatched types",
                "source": "rustc",
            }],
        });
        apply_published_diagnostics(&cache, &params);
        let stored = cache.lock().unwrap();
        let looked_up_path = uri_to_path(&path_to_uri(&path)).unwrap();
        let entries = stored.get(&looked_up_path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "mismatched types");
        assert_eq!(entries[0].severity.as_deref(), Some("error"));
    }
}
