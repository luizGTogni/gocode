//! Connects every enabled, configured MCP server and bridges its tools into gocode's
//! [`Tool`](gocode_tools::contract::Tool) registry. Best-effort: one server failing to connect
//! does not prevent the others (or the rest of gocode) from starting.

use std::sync::Arc;

use gocode_core::{McpAuthConfig, McpServerEntry, McpTransportConfig};
use gocode_credentials::NativeCredentialStore;
use gocode_tools::contract::Tool;

use crate::{
    McpClient, McpError,
    tool_bridge::McpTool,
    transport::{http::HttpTransport, stdio::StdioTransport},
};

/// The OS keyring account an MCP server's static API key is stored under. Shared between
/// connecting (reads it) and the `/mcp` add-server flow (writes it), so both agree on the name.
#[must_use]
pub fn api_key_account(server_name: &str) -> String {
    format!("mcp/{server_name}")
}

/// One server's tools, discovered after a successful connect.
pub struct McpServerConnection {
    /// Matches the connected [`McpServerEntry::name`].
    pub name: String,
    /// Every tool this server advertised.
    pub tools: Vec<Arc<dyn Tool>>,
}

/// Result of attempting to connect every configured server: one [`McpServerConnection`] per
/// server that connected successfully, plus one diagnostic per server that failed.
#[derive(Default)]
pub struct McpConnectOutcome {
    /// Every server that connected successfully, in `servers` order.
    pub connections: Vec<McpServerConnection>,
    /// `(server_name, error)` for each server that failed to connect or initialize.
    pub failures: Vec<(String, McpError)>,
}

/// Connects every `enabled` entry in `servers` and collects their advertised tools.
pub async fn connect_configured_servers(servers: &[McpServerEntry]) -> McpConnectOutcome {
    let mut outcome = McpConnectOutcome::default();
    for server in servers.iter().filter(|server| server.enabled) {
        match connect_server(server).await {
            Ok(connection) => outcome.connections.push(connection),
            Err(error) => outcome.failures.push((server.name.clone(), error)),
        }
    }
    outcome
}

/// Connects a single server, regardless of its `enabled` flag — used both by
/// [`connect_configured_servers`] and by an explicit `/mcp connect <name>` action.
///
/// # Errors
/// Returns an error if the server cannot be spawned/reached, fails the `initialize` handshake,
/// its auth is not yet implemented (OAuth), or an `ApiKey` server has no key stored.
pub async fn connect_server(server: &McpServerEntry) -> Result<McpServerConnection, McpError> {
    match &server.transport {
        McpTransportConfig::Stdio { command, args, env } => {
            let env_pairs: Vec<(String, String)> = env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let transport = StdioTransport::spawn(command, args, &env_pairs)?;
            connect_and_discover(transport, server).await
        }
        McpTransportConfig::Http { url, headers } => {
            let mut header_pairs: Vec<(String, String)> = headers
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            if let Some(bearer) = resolve_bearer_token(server)? {
                header_pairs.push(("Authorization".to_string(), format!("Bearer {bearer}")));
            }
            let transport = HttpTransport::new(url, &header_pairs)?;
            connect_and_discover(transport, server).await
        }
    }
}

/// Resolves the `Authorization: Bearer` value for a server's configured auth, if any.
fn resolve_bearer_token(server: &McpServerEntry) -> Result<Option<String>, McpError> {
    match &server.auth {
        McpAuthConfig::None => Ok(None),
        McpAuthConfig::ApiKey => {
            let account = api_key_account(&server.name);
            let store = NativeCredentialStore::new();
            let secret = store.get_secret(&account).map_err(|error| {
                McpError::Transport(format!(
                    "could not read the stored API key for '{}': {error:?}",
                    server.name
                ))
            })?;
            let Some(secret) = secret else {
                return Err(McpError::Transport(format!(
                    "no API key is stored for MCP server '{}' — add one via /mcp",
                    server.name
                )));
            };
            Ok(Some(secret.expose().to_string()))
        }
        McpAuthConfig::OAuth { .. } => Err(McpError::Transport(
            "OAuth authentication for MCP servers is not implemented yet".into(),
        )),
    }
}

async fn connect_and_discover<T: crate::McpTransport + 'static>(
    transport: T,
    server: &McpServerEntry,
) -> Result<McpServerConnection, McpError> {
    let client = Arc::new(McpClient::connect(transport).await?);
    let infos = client.list_tools().await?;
    let tools = infos
        .into_iter()
        .map(|info| {
            Arc::new(McpTool::new(Arc::clone(&client), server.name.clone(), info)) as Arc<dyn Tool>
        })
        .collect();
    Ok(McpServerConnection {
        name: server.name.clone(),
        tools,
    })
}

#[cfg(test)]
mod tests {
    use super::connect_configured_servers;
    use gocode_core::{McpAuthConfig, McpServerEntry, McpTransportConfig};
    use std::collections::BTreeMap;

    fn fake_server_entry(name: &str, script: &str) -> McpServerEntry {
        McpServerEntry {
            name: name.to_string(),
            transport: McpTransportConfig::Stdio {
                command: "sh".into(),
                args: vec!["-c".into(), script.into()],
                env: BTreeMap::new(),
            },
            auth: McpAuthConfig::None,
            enabled: true,
        }
    }

    const FAKE_SERVER: &str = r#"
read _init
printf '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"fake"}}}\n'
read _initialized_notification
read _list
printf '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}\n'
"#;

    #[tokio::test]
    async fn connects_an_enabled_stdio_server_and_collects_its_tools() {
        let servers = vec![fake_server_entry("fake", FAKE_SERVER)];
        let outcome = connect_configured_servers(&servers).await;

        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(outcome.connections.len(), 1);
        assert_eq!(outcome.connections[0].name, "fake");
        assert_eq!(outcome.connections[0].tools.len(), 1);
        assert_eq!(
            outcome.connections[0].tools[0].definition().name.as_str(),
            "mcp__fake__echo"
        );
    }

    #[tokio::test]
    async fn skips_disabled_servers() {
        let mut server = fake_server_entry("fake", FAKE_SERVER);
        server.enabled = false;
        let outcome = connect_configured_servers(&[server]).await;

        assert!(outcome.connections.is_empty());
        assert!(outcome.failures.is_empty());
    }

    #[tokio::test]
    async fn records_a_failure_for_a_server_that_will_not_spawn() {
        let servers = vec![fake_server_entry("broken", "")]; // command overridden below
        let mut servers = servers;
        servers[0].transport = McpTransportConfig::Stdio {
            command: "definitely-not-a-real-binary".into(),
            args: vec![],
            env: BTreeMap::new(),
        };

        let outcome = connect_configured_servers(&servers).await;
        assert!(outcome.connections.is_empty());
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].0, "broken");
    }

    #[tokio::test]
    async fn records_a_failure_for_an_unreachable_http_server() {
        let mut server = fake_server_entry("remote", FAKE_SERVER);
        // Port 1 is a privileged, essentially always-unbound port, so this connection fails
        // immediately without any dependency on network access or a real MCP server.
        server.transport = McpTransportConfig::Http {
            url: "http://127.0.0.1:1/mcp".into(),
            headers: BTreeMap::new(),
        };

        let outcome = connect_configured_servers(&[server]).await;
        assert!(outcome.connections.is_empty());
        assert_eq!(outcome.failures.len(), 1);
    }

    #[tokio::test]
    async fn records_a_failure_for_an_invalid_http_url() {
        let mut server = fake_server_entry("remote", FAKE_SERVER);
        server.transport = McpTransportConfig::Http {
            url: "not a url".into(),
            headers: BTreeMap::new(),
        };

        let outcome = connect_configured_servers(&[server]).await;
        assert!(outcome.connections.is_empty());
        assert_eq!(outcome.failures.len(), 1);
    }

    #[tokio::test]
    async fn records_a_failure_for_an_oauth_server_since_oauth_is_not_implemented_yet() {
        let mut server = fake_server_entry("remote", FAKE_SERVER);
        server.transport = McpTransportConfig::Http {
            url: "http://127.0.0.1:1/mcp".into(),
            headers: BTreeMap::new(),
        };
        server.auth = McpAuthConfig::OAuth {
            authorization_url: "https://example.com/authorize".into(),
            token_url: "https://example.com/token".into(),
            client_id: "gocode".into(),
            scopes: vec![],
        };

        let outcome = connect_configured_servers(&[server]).await;
        assert!(outcome.connections.is_empty());
        assert_eq!(outcome.failures.len(), 1);
    }

    #[tokio::test]
    async fn records_a_failure_for_an_api_key_server_with_no_stored_key() {
        // Uses whatever the test environment's keyring backend reports for an account that was
        // never written to — either "no entry" or "unavailable" — both must surface as a
        // connect failure rather than silently proceeding unauthenticated.
        let mut server = fake_server_entry("remote", FAKE_SERVER);
        server.transport = McpTransportConfig::Http {
            url: "http://127.0.0.1:1/mcp".into(),
            headers: BTreeMap::new(),
        };
        server.auth = McpAuthConfig::ApiKey;
        server.name = format!("gocode-mcp-test-no-such-server-{}", uuid_like_suffix());

        let outcome = connect_configured_servers(&[server]).await;
        assert!(outcome.connections.is_empty());
        assert_eq!(outcome.failures.len(), 1);
    }

    /// A cheap process/time-derived suffix, good enough to keep this test's keyring account
    /// name from colliding with a real one, without adding a `uuid` dev-dependency.
    fn uuid_like_suffix() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        format!("{}-{nanos}", std::process::id())
    }
}
