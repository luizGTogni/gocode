# MCP servers

Gocode can connect to [MCP](https://modelcontextprotocol.io) (Model Context Protocol) servers
and use their tools alongside its built-in ones. Added in v0.4.0.

## Quick start

Run `/mcp` in the TUI, choose **Add server**, and follow the wizard:

1. **Name** — a short, unique identifier (also used as the tool-name prefix and the keyring
   account for any stored credential).
2. **Transport** — `stdio` (a local command) or `http` (a remote streamable-HTTP endpoint).
3. **Command/URL** — for stdio, the command and its arguments, space-separated (e.g.
   `npx -y @modelcontextprotocol/server-filesystem /home/you/project`); for http, the server's
   endpoint URL.
4. **Authentication** — `None` or `API key`. If you choose API key, the value you type is saved
   to your OS keyring, never written to a config file.

The wizard saves the server to the current project's `.gocode/mcp.toml` and connects it
immediately. Once connected, its tools become available to the model like any built-in tool,
and you'll see them listed if you drill into the server from `/mcp`'s server list (`→`).

## The `/mcp` popup

- **Servers** — every configured server, its transport, and live connection status.
  - `Enter` connects a disconnected server, or disconnects a connected one.
  - `o` starts OAuth authorization for a server that needs it (see below).
  - `→` shows the server's discovered tools; `Esc` goes back.
- **Add server** — the wizard described above.

A server that fails to connect (bad command, unreachable URL, missing credential) doesn't block
Gocode from starting or block other servers — the failure is shown as a warning, and you can
retry from `/mcp`.

## Configuration file

Servers are also hand-editable. Gocode reads two layers and merges them (a project entry
overrides a global one of the same name):

- global: `~/.config/gocode/mcp.toml` (Linux) or `%USERPROFILE%\.gocode\mcp.toml` (Windows);
- project: `<project root>/.gocode/mcp.toml`.

```toml
schema_version = 1

[[servers]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/you/project"]
enabled = true

[[servers]]
name = "linear"
transport = "http"
url = "https://mcp.linear.app/mcp"
enabled = true

[servers.auth]
type = "oauth"
authorization_url = "https://mcp.linear.app/oauth/authorize"
token_url = "https://mcp.linear.app/oauth/token"
client_id = "your-registered-client-id"
scopes = ["read"]
```

Fields:

| Field | Meaning |
|---|---|
| `name` | Unique identifier; also the tool-name prefix (`mcp__<name>__<tool>`) and keyring account suffix. |
| `transport` | `"stdio"` (`command`, `args`, `env`) or `"http"` (`url`, `headers`). |
| `auth.type` | `"none"` (default, may be omitted), `"apikey"`, or `"oauth"`. |
| `enabled` | Set to `false` to keep a server configured but not auto-connected at startup. |

Secrets are never written here. An `apikey` or `oauth` entry only records that a credential is
needed; the credential itself lives in your OS keyring (Windows Credential Manager / Linux
Secret Service), under account `mcp/<name>`.

## OAuth

For a server whose `auth.type` is `"oauth"`, select it in `/mcp`'s server list and press `o`.
Gocode opens your system browser to the server's authorization page (an authorization-code +
PKCE flow) and catches the redirect on a local loopback port. If the browser doesn't open
automatically, the URL is also printed to the transcript so you can open it yourself. Access
tokens are refreshed automatically when they expire, as long as the server issued a refresh
token; otherwise you'll be asked to authorize again.

Gocode does not perform dynamic client registration — `client_id` (and, for confidential
clients, anything the server requires beyond PKCE) must already be registered with the server
and entered in `mcp.toml`.

## Limitations

- The streamable-HTTP transport speaks single-response and SSE-response JSON-RPC; a server's
  independent, unsolicited SSE push stream (opened via a bare `GET` outside a request) is not
  supported.
- MCP `sampling` requests (a server asking the client's model to generate something) are not
  answered.
- Tool call progress notifications are not yet streamed into the TUI's live output — you'll see
  the final result once a call completes.

See [docs/TUI.md](TUI.md#142-mcp) for the `/mcp` popup's exact keybindings.
