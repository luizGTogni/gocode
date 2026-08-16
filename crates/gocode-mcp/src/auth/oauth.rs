//! OAuth 2.1 authorization-code + PKCE flow, per MCP's authorization spec: build the
//! authorization URL, catch the redirect on a loopback listener, and exchange (or refresh) the
//! code for tokens. Opening the system browser is left to the caller (the `gocode` binary),
//! keeping this crate free of a browser-launching dependency.

use std::{collections::HashMap, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gocode_core::{McpAuthConfig, McpServerEntry};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::McpError;

/// A stored OAuth token set, serialized as-is into the OS keyring (never written to `mcp.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthTokenSet {
    pub access_token: String,
    pub token_type: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds when `access_token` expires, if the server reported an `expires_in`.
    #[serde(default)]
    pub expires_at_unix: Option<i64>,
}

impl OAuthTokenSet {
    /// Whether this access token is expired (or expiring within a 30-second margin). A token
    /// with no reported expiry is treated as never expiring.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expires_at_unix
            .is_some_and(|expires_at| now_unix() >= expires_at - 30)
    }
}

/// A loopback listener bound and an authorization URL built, waiting for the user to complete
/// login in their browser and be redirected back.
pub struct PendingAuthorization {
    /// Open this in the user's browser.
    pub auth_url: String,
    listener: TcpListener,
    state: String,
    verifier: String,
    redirect_uri: String,
    token_url: String,
    client_id: String,
}

/// Binds a loopback redirect listener and builds the authorization URL for `server`.
///
/// # Errors
/// Returns an error if `server` is not configured for OAuth, its authorization URL is invalid,
/// or a loopback port could not be bound.
pub fn prepare_authorization(server: &McpServerEntry) -> Result<PendingAuthorization, McpError> {
    let McpAuthConfig::OAuth {
        authorization_url,
        token_url,
        client_id,
        scopes,
    } = &server.auth
    else {
        return Err(McpError::Transport(format!(
            "MCP server '{}' is not configured for OAuth",
            server.name
        )));
    };

    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| {
        McpError::Transport(format!(
            "could not open a loopback port for the OAuth redirect: {error}"
        ))
    })?;
    std_listener.set_nonblocking(true).map_err(|error| {
        McpError::Transport(format!("could not prepare the loopback listener: {error}"))
    })?;
    let listener = TcpListener::from_std(std_listener).map_err(|error| {
        McpError::Transport(format!("could not prepare the loopback listener: {error}"))
    })?;
    let port = listener
        .local_addr()
        .map_err(|error| McpError::Transport(format!("could not read the loopback port: {error}")))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let pkce = PkcePair::generate();
    let state = random_token(16);
    let auth_url = build_authorization_url(
        authorization_url,
        client_id,
        &redirect_uri,
        scopes,
        &state,
        &pkce.challenge,
    )?;

    Ok(PendingAuthorization {
        auth_url,
        listener,
        state,
        verifier: pkce.verifier,
        redirect_uri,
        token_url: token_url.clone(),
        client_id: client_id.clone(),
    })
}

/// Waits (up to `timeout`) for the browser redirect, then exchanges the authorization code for
/// tokens.
///
/// # Errors
/// Returns an error if the wait times out, the redirect is malformed or its `state` does not
/// match, the server denied authorization, or the token exchange fails.
pub async fn complete_authorization(
    pending: PendingAuthorization,
    timeout: Duration,
) -> Result<OAuthTokenSet, McpError> {
    let code = tokio::time::timeout(timeout, await_redirect(pending.listener, &pending.state))
        .await
        .map_err(|_| {
            McpError::Transport("authorization timed out waiting for the browser redirect".into())
        })??;

    exchange_code_for_tokens(
        &pending.token_url,
        &pending.client_id,
        &code,
        &pending.redirect_uri,
        &pending.verifier,
    )
    .await
}

/// Exchanges a refresh token for a new access token.
///
/// # Errors
/// Returns an error if the request fails or the token endpoint rejects the refresh token.
pub async fn refresh_tokens(
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthTokenSet, McpError> {
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    post_token_request(token_url, &params).await
}

async fn exchange_code_for_tokens(
    token_url: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthTokenSet, McpError> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];
    post_token_request(token_url, &params).await
}

async fn post_token_request(
    token_url: &str,
    params: &[(&str, &str)],
) -> Result<OAuthTokenSet, McpError> {
    let client = reqwest::Client::new();
    let response = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(params)
        .send()
        .await
        .map_err(|error| {
            McpError::Transport(format!("token request to '{token_url}' failed: {error}"))
        })?;

    let status = response.status();
    let bytes = response.bytes().await.map_err(|error| {
        McpError::Transport(format!("failed to read the token response: {error}"))
    })?;
    if !status.is_success() {
        return Err(McpError::Transport(format!(
            "token endpoint returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        )));
    }
    parse_token_response(&bytes)
}

fn parse_token_response(bytes: &[u8]) -> Result<OAuthTokenSet, McpError> {
    #[derive(Deserialize)]
    struct RawTokenResponse {
        access_token: String,
        #[serde(default = "default_token_type")]
        token_type: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: Option<i64>,
    }
    fn default_token_type() -> String {
        "Bearer".to_string()
    }

    let raw: RawTokenResponse = serde_json::from_slice(bytes)
        .map_err(|error| McpError::Protocol(format!("invalid token response: {error}")))?;
    Ok(OAuthTokenSet {
        access_token: raw.access_token,
        token_type: raw.token_type,
        refresh_token: raw.refresh_token,
        expires_at_unix: raw.expires_in.map(|seconds| now_unix() + seconds),
    })
}

/// Accepts exactly one connection, reads its request line, verifies `state`, and responds with
/// a small confirmation page. Returns the authorization `code` on success.
async fn await_redirect(listener: TcpListener, expected_state: &str) -> Result<String, McpError> {
    let (mut socket, _) = listener.accept().await.map_err(|error| {
        McpError::Transport(format!("failed to accept the OAuth redirect: {error}"))
    })?;

    let mut buffer = [0_u8; 8192];
    let read = socket.read(&mut buffer).await.map_err(|error| {
        McpError::Transport(format!("failed to read the OAuth redirect: {error}"))
    })?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| McpError::Protocol("malformed OAuth redirect request".into()))?;
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{path}"))
        .map_err(|error| McpError::Protocol(format!("malformed OAuth redirect URL: {error}")))?;
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();

    if params.get("state").map(String::as_str) != Some(expected_state) {
        respond(
            &mut socket,
            400,
            "Authorization failed: state did not match.",
        )
        .await;
        return Err(McpError::Protocol(
            "OAuth redirect state did not match".into(),
        ));
    }

    let Some(code) = params.get("code").cloned() else {
        let reason = params
            .get("error_description")
            .or_else(|| params.get("error"))
            .cloned()
            .unwrap_or_else(|| "no authorization code was returned".to_string());
        respond(&mut socket, 400, &format!("Authorization failed: {reason}")).await;
        return Err(McpError::Transport(format!(
            "authorization was not granted: {reason}"
        )));
    };

    respond(
        &mut socket,
        200,
        "Authorization complete. You can close this tab and return to gocode.",
    )
    .await;
    Ok(code)
}

async fn respond(socket: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

fn build_authorization_url(
    authorization_url: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
    challenge: &str,
) -> Result<String, McpError> {
    let mut url = reqwest::Url::parse(authorization_url).map_err(|error| {
        McpError::Transport(format!(
            "invalid authorization URL '{authorization_url}': {error}"
        ))
    })?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        if !scopes.is_empty() {
            pairs.append_pair("scope", &scopes.join(" "));
        }
    }
    Ok(url.into())
}

/// A PKCE (RFC 7636) verifier/challenge pair.
struct PkcePair {
    verifier: String,
    challenge: String,
}

impl PkcePair {
    fn generate() -> Self {
        let verifier = random_token(32);
        let challenge = code_challenge_for(&verifier);
        Self {
            verifier,
            challenge,
        }
    }
}

/// Derives the S256 PKCE code challenge for a given verifier.
fn code_challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A random, URL-safe token of roughly `byte_len * 4 / 3` characters.
fn random_token(byte_len: usize) -> String {
    let mut bytes = vec![0_u8; byte_len];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        OAuthTokenSet, build_authorization_url, code_challenge_for, complete_authorization,
        now_unix, parse_token_response, prepare_authorization,
    };
    use gocode_core::{McpAuthConfig, McpServerEntry, McpTransportConfig};
    use std::{collections::BTreeMap, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    /// Verifies the S256 challenge derivation (base64url(sha256(verifier))) against an
    /// independently computed value for a fixed verifier.
    #[test]
    fn pkce_challenge_is_the_base64url_sha256_of_the_verifier() {
        let verifier = "dbjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge_for(verifier),
            "eINzvQ3Z8aARYw9pLv0ISwvsVZ3cecpv476AyyP_wEo"
        );
    }

    #[test]
    fn authorization_url_carries_every_required_parameter() {
        let url = build_authorization_url(
            "https://example.com/authorize",
            "gocode",
            "http://127.0.0.1:12345/callback",
            &["read".to_string(), "write".to_string()],
            "the-state",
            "the-challenge",
        )
        .expect("build url");

        assert!(url.starts_with("https://example.com/authorize?"));
        for expected in [
            "response_type=code",
            "client_id=gocode",
            "state=the-state",
            "code_challenge=the-challenge",
            "code_challenge_method=S256",
            "scope=read+write",
        ] {
            assert!(url.contains(expected), "missing '{expected}' in {url}");
        }
    }

    #[test]
    fn parses_a_token_response_with_an_expiry() {
        let body = br#"{"access_token":"abc","token_type":"bearer","refresh_token":"r","expires_in":3600}"#;
        let tokens = parse_token_response(body).expect("parse");
        assert_eq!(tokens.access_token, "abc");
        assert_eq!(tokens.refresh_token.as_deref(), Some("r"));
        assert!(tokens.expires_at_unix.unwrap() > now_unix());
    }

    #[test]
    fn a_token_with_no_expiry_never_expires() {
        let tokens = OAuthTokenSet {
            access_token: "abc".into(),
            token_type: "Bearer".into(),
            refresh_token: None,
            expires_at_unix: None,
        };
        assert!(!tokens.is_expired());
    }

    #[test]
    fn a_token_past_its_expiry_is_expired() {
        let tokens = OAuthTokenSet {
            access_token: "abc".into(),
            token_type: "Bearer".into(),
            refresh_token: None,
            expires_at_unix: Some(now_unix() - 60),
        };
        assert!(tokens.is_expired());
    }

    fn oauth_server_entry(authorization_url: &str, token_url: &str) -> McpServerEntry {
        McpServerEntry {
            name: "oauth-server".into(),
            transport: McpTransportConfig::Http {
                url: "http://127.0.0.1:1/mcp".into(),
                headers: BTreeMap::new(),
            },
            auth: McpAuthConfig::OAuth {
                authorization_url: authorization_url.into(),
                token_url: token_url.into(),
                client_id: "gocode".into(),
                scopes: vec!["read".into()],
            },
            enabled: true,
        }
    }

    /// Spawns a fake token endpoint that always answers with one canned token response.
    async fn spawn_token_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0_u8; 4096];
            let _ = socket.read(&mut buf).await;
            let body = r#"{"access_token":"issued-token","token_type":"Bearer","expires_in":3600}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}/token")
    }

    #[tokio::test]
    async fn full_loopback_round_trip_yields_the_issued_token() {
        let token_url = spawn_token_server().await;
        let server = oauth_server_entry("https://example.com/authorize", &token_url);
        let pending = prepare_authorization(&server).expect("prepare");

        // Extract the port and state gocode itself generated, exactly as a browser redirect
        // would carry them back.
        let auth_url = reqwest::Url::parse(&pending.auth_url).expect("parse auth url");
        let state = auth_url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .expect("state present");
        let redirect_uri = auth_url
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .map(|(_, value)| value.into_owned())
            .expect("redirect_uri present");
        let redirect_port = reqwest::Url::parse(&redirect_uri)
            .expect("parse redirect uri")
            .port()
            .expect("port present");

        let completion = tokio::spawn(complete_authorization(pending, Duration::from_secs(5)));

        // Simulate the browser hitting the loopback redirect with a code and matching state.
        let mut client = TcpStream::connect(("127.0.0.1", redirect_port))
            .await
            .expect("connect to loopback");
        let request =
            format!("GET /callback?code=the-code&state={state} HTTP/1.1\r\nHost: x\r\n\r\n");
        client
            .write_all(request.as_bytes())
            .await
            .expect("write redirect request");

        let tokens = completion
            .await
            .expect("join")
            .expect("authorization completes");
        assert_eq!(tokens.access_token, "issued-token");
    }
}
