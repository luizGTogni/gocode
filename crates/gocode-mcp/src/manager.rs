//! Connects every enabled, configured MCP server and bridges its tools into gocode's
//! [`Tool`](gocode_tools::contract::Tool) registry. Best-effort: one server failing to connect
//! does not prevent the others (or the rest of gocode) from starting.

use std::sync::Arc;

use gocode_core::{McpServerEntry, McpTransportConfig};
use gocode_tools::contract::Tool;

use crate::{McpClient, McpError, tool_bridge::McpTool, transport::stdio::StdioTransport};

/// Result of attempting to connect every configured server: the tools successfully discovered,
/// plus one diagnostic per server that failed.
#[derive(Default)]
pub struct McpConnectOutcome {
    /// Every tool discovered across every server that connected successfully.
    pub tools: Vec<Arc<dyn Tool>>,
    /// `(server_name, error)` for each server that failed to connect or initialize.
    pub failures: Vec<(String, McpError)>,
}

/// Connects every `enabled` entry in `servers` and collects their advertised tools.
pub async fn connect_configured_servers(servers: &[McpServerEntry]) -> McpConnectOutcome {
    let mut outcome = McpConnectOutcome::default();
    for server in servers.iter().filter(|server| server.enabled) {
        match connect_one(server).await {
            Ok(tools) => outcome.tools.extend(tools),
            Err(error) => outcome.failures.push((server.name.clone(), error)),
        }
    }
    outcome
}

async fn connect_one(server: &McpServerEntry) -> Result<Vec<Arc<dyn Tool>>, McpError> {
    match &server.transport {
        McpTransportConfig::Stdio { command, args, env } => {
            let env_pairs: Vec<(String, String)> = env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let transport = StdioTransport::spawn(command, args, &env_pairs)?;
            let client = Arc::new(McpClient::connect(transport).await?);
            let infos = client.list_tools().await?;
            Ok(infos
                .into_iter()
                .map(|info| {
                    Arc::new(McpTool::new(Arc::clone(&client), server.name.clone(), info))
                        as Arc<dyn Tool>
                })
                .collect())
        }
        McpTransportConfig::Http { .. } => Err(McpError::Transport(
            "the streamable-HTTP MCP transport is not implemented yet".into(),
        )),
    }
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
        assert_eq!(outcome.tools.len(), 1);
        assert_eq!(
            outcome.tools[0].definition().name.as_str(),
            "mcp__fake__echo"
        );
    }

    #[tokio::test]
    async fn skips_disabled_servers() {
        let mut server = fake_server_entry("fake", FAKE_SERVER);
        server.enabled = false;
        let outcome = connect_configured_servers(&[server]).await;

        assert!(outcome.tools.is_empty());
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
        assert!(outcome.tools.is_empty());
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].0, "broken");
    }

    #[tokio::test]
    async fn records_a_failure_for_an_unimplemented_http_server() {
        let mut server = fake_server_entry("remote", FAKE_SERVER);
        server.transport = McpTransportConfig::Http {
            url: "https://example.com/mcp".into(),
            headers: BTreeMap::new(),
        };

        let outcome = connect_configured_servers(&[server]).await;
        assert!(outcome.tools.is_empty());
        assert_eq!(outcome.failures.len(), 1);
    }
}
