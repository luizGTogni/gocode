use futures_util::StreamExt;
use gocode_core::{
    CancellationToken, ChatMessage, ChatRequest, ChatStreamEvent, Model, ModelCapabilities, ModelId,
};
use gocode_credentials::SecretString;

const HOSTED_BASE_URL: &str = "https://integrate.api.nvidia.com/";

/// HTTP client for NVIDIA NIM's hosted OpenAI-compatible API.
#[derive(Clone)]
pub struct NvidiaProvider {
    client: reqwest::Client,
    base_url: reqwest::Url,
    credential: SecretString,
}

impl NvidiaProvider {
    /// Creates a provider for NVIDIA's hosted NIM catalog.
    ///
    /// # Panics
    ///
    /// Panics only if the compile-time hosted NVIDIA URL becomes invalid.
    #[must_use]
    pub fn hosted(credential: SecretString) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: reqwest::Url::parse(HOSTED_BASE_URL)
                .expect("the built-in NVIDIA hosted URL must be valid"),
            credential,
        }
    }

    /// Returns the authenticated model-discovery endpoint.
    ///
    /// # Panics
    ///
    /// Panics only if the compile-time models path becomes invalid.
    #[must_use]
    pub fn models_url(&self) -> reqwest::Url {
        self.base_url
            .join("v1/models")
            .expect("the built-in NVIDIA models path must be valid")
    }

    /// Returns the streamed chat-completions endpoint.
    ///
    /// # Panics
    ///
    /// Panics only if the compile-time chat path becomes invalid.
    #[must_use]
    pub fn chat_url(&self) -> reqwest::Url {
        self.base_url
            .join("v1/chat/completions")
            .expect("the built-in NVIDIA chat path must be valid")
    }

    /// Builds the authorization value used only on outbound NVIDIA requests.
    #[must_use]
    pub fn authorization_value(&self) -> String {
        format!("Bearer {}", self.credential.expose())
    }

    /// Discovers models exposed by the authenticated NVIDIA endpoint.
    ///
    /// # Errors
    ///
    /// Returns normalized network, HTTP, or protocol failures without preserving response bodies.
    pub async fn list_models(&self) -> Result<Vec<Model>, NvidiaClientError> {
        let response = self
            .client
            .get(self.models_url())
            .bearer_auth(self.credential.expose())
            .send()
            .await
            .map_err(|error| NvidiaClientError::Network(error.to_string()))?;

        if !response.status().is_success() {
            return Err(NvidiaClientError::Http(map_status_error(
                response.status().as_u16(),
            )));
        }

        let payload = response
            .text()
            .await
            .map_err(|error| NvidiaClientError::Network(error.to_string()))?;
        map_models(&payload).map_err(NvidiaClientError::Protocol)
    }

    /// Starts an NVIDIA streamed chat request and returns its normalized event channel.
    ///
    /// # Errors
    ///
    /// Returns an error when NVIDIA rejects or cannot start the HTTP request. Failures that occur
    /// after streaming starts are delivered through the returned channel.
    pub async fn stream_chat(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> Result<
        tokio::sync::mpsc::Receiver<Result<ChatStreamEvent, NvidiaClientError>>,
        NvidiaClientError,
    > {
        let response = self
            .client
            .post(self.chat_url())
            .bearer_auth(self.credential.expose())
            .json(&build_chat_body(&request))
            .send()
            .await
            .map_err(|error| NvidiaClientError::Network(error.to_string()))?;

        if !response.status().is_success() {
            return Err(NvidiaClientError::Http(map_status_error(
                response.status().as_u16(),
            )));
        }

        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        let mut body = response.bytes_stream();
        tokio::spawn(async move {
            let mut pending = String::new();
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    chunk = body.next() => match chunk {
                        Some(Ok(bytes)) => {
                            pending.push_str(&String::from_utf8_lossy(&bytes));
                            while let Some(newline) = pending.find('\n') {
                                let line = pending[..newline].trim_end_matches('\r').to_owned();
                                pending.drain(..=newline);
                                let Some(data) = line.strip_prefix("data:") else {
                                    continue;
                                };
                                match map_sse_data(data.trim()) {
                                    Ok(events) => {
                                        for event in events {
                                            if sender.send(Ok(event)).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        let _ = sender.send(Err(NvidiaClientError::Protocol(error))).await;
                                        return;
                                    }
                                }
                            }
                        }
                        Some(Err(error)) => {
                            let _ = sender.send(Err(NvidiaClientError::Network(error.to_string()))).await;
                            return;
                        }
                        None => return,
                    }
                }
            }
        });
        Ok(receiver)
    }
}

/// Safe error surface produced by the NVIDIA client.
#[derive(Debug)]
pub enum NvidiaClientError {
    /// The request could not reach NVIDIA or finish before the HTTP client's timeout.
    Network(String),
    /// NVIDIA returned a non-success HTTP status.
    Http(NvidiaErrorKind),
    /// NVIDIA returned an unexpected JSON or SSE body.
    Protocol(NvidiaProtocolError),
}

impl std::fmt::Display for NvidiaClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(_) => formatter.write_str("could not reach NVIDIA"),
            Self::Http(kind) => write!(formatter, "NVIDIA request failed: {kind:?}"),
            Self::Protocol(_) => formatter.write_str("NVIDIA returned an invalid response"),
        }
    }
}

impl std::error::Error for NvidiaClientError {}

/// Builds NVIDIA's OpenAI-compatible streaming chat body from normalized input.
#[must_use]
pub fn build_chat_body(request: &ChatRequest) -> serde_json::Value {
    let messages = request
        .messages
        .iter()
        .map(|message| {
            let (role, content) = match message {
                ChatMessage::System(content) => ("system", content),
                ChatMessage::User(content) => ("user", content),
                ChatMessage::Assistant(content) => ("assistant", content),
            };
            serde_json::json!({ "role": role, "content": content })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "model": request.model.as_str(),
        "messages": messages,
        "stream": true,
    })
}

/// Converts the NVIDIA `/v1/models` body into provider-neutral catalog entries.
///
/// # Errors
///
/// Returns [`NvidiaProtocolError`] when the response cannot be parsed or has no model list.
pub fn map_models(payload: &str) -> Result<Vec<Model>, NvidiaProtocolError> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| NvidiaProtocolError(format!("invalid model-list JSON: {error}")))?;
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| NvidiaProtocolError("model-list JSON has no data array".into()))?;

    Ok(data
        .iter()
        .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
        .map(|id| Model {
            id: ModelId::new(id),
            display_name: id.into(),
            capabilities: ModelCapabilities::unknown(),
        })
        .collect())
}

/// Maps one OpenAI-compatible NVIDIA streaming JSON payload into generic stream events.
///
/// # Errors
///
/// Returns [`NvidiaProtocolError`] when the payload is not a JSON object with the expected
/// top-level structure.
pub fn map_stream_chunk(payload: &str) -> Result<Vec<ChatStreamEvent>, NvidiaProtocolError> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| NvidiaProtocolError(format!("invalid streaming JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| NvidiaProtocolError("streaming JSON must be an object".into()))?;
    let mut events = Vec::new();

    if let Some(request_id) = object.get("id").and_then(serde_json::Value::as_str) {
        events.push(ChatStreamEvent::RequestId(request_id.into()));
    }

    let Some(choices) = object.get("choices").and_then(serde_json::Value::as_array) else {
        return Ok(events);
    };

    for choice in choices {
        let Some(content) = choice
            .get("delta")
            .and_then(|delta| delta.get("content"))
            .and_then(serde_json::Value::as_str)
            .filter(|content| !content.is_empty())
        else {
            continue;
        };
        events.push(ChatStreamEvent::TextDelta(content.into()));
    }

    Ok(events)
}

/// Maps one SSE `data:` value from NVIDIA into generic events.
///
/// # Errors
///
/// Returns an error if a non-terminal data value is malformed JSON.
pub fn map_sse_data(data: &str) -> Result<Vec<ChatStreamEvent>, NvidiaProtocolError> {
    if data == "[DONE]" {
        return Ok(vec![ChatStreamEvent::Completed]);
    }

    map_stream_chunk(data)
}

/// A malformed NVIDIA wire payload that cannot be normalized safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaProtocolError(String);

impl std::fmt::Display for NvidiaProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for NvidiaProtocolError {}

/// Stable categories for NVIDIA HTTP failures safe to show in the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvidiaErrorKind {
    /// Authentication was rejected by the provider.
    InvalidCredential,
    /// The provider asked the client to slow down.
    RateLimited,
    /// A temporary provider-side failure occurred.
    Server,
    /// Any other non-success response.
    Request,
}

/// Maps HTTP status codes without preserving potentially sensitive response bodies.
#[must_use]
pub const fn map_status_error(status: u16) -> NvidiaErrorKind {
    match status {
        401 | 403 => NvidiaErrorKind::InvalidCredential,
        429 => NvidiaErrorKind::RateLimited,
        500..=599 => NvidiaErrorKind::Server,
        _ => NvidiaErrorKind::Request,
    }
}

#[cfg(test)]
mod tests {
    use gocode_core::ChatStreamEvent;

    #[test]
    fn maps_openai_compatible_text_delta_to_generic_event() {
        let event =
            super::map_stream_chunk(r#"{"id":"req-123","choices":[{"delta":{"content":"Olá"}}]}"#)
                .expect("valid NVIDIA chunk should map");

        assert_eq!(
            event,
            vec![
                ChatStreamEvent::RequestId("req-123".into()),
                ChatStreamEvent::TextDelta("Olá".into())
            ]
        );
    }

    #[test]
    fn ignores_metadata_only_chunks() {
        let event = super::map_stream_chunk(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#)
            .expect("metadata-only chunk should map");

        assert!(event.is_empty());
    }

    #[test]
    fn builds_an_openai_compatible_streaming_request() {
        let request = gocode_core::ChatRequest::single_user("nvidia/model", "Olá");

        let body = super::build_chat_body(&request);

        assert_eq!(body["model"], "nvidia/model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Olá");
    }

    #[test]
    fn maps_discovered_models_with_conservative_capabilities() {
        let models = super::map_models(r#"{"data":[{"id":"nvidia/model-a"}]}"#)
            .expect("model list should map");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.as_str(), "nvidia/model-a");
        assert!(models[0].capabilities.streaming);
        assert_eq!(
            models[0].capabilities.tools,
            gocode_core::ToolCapability::Unsupported
        );
    }

    #[test]
    fn maps_auth_rate_limit_and_server_statuses_without_leaking_body() {
        assert_eq!(
            super::map_status_error(401),
            super::NvidiaErrorKind::InvalidCredential
        );
        assert_eq!(
            super::map_status_error(429),
            super::NvidiaErrorKind::RateLimited
        );
        assert_eq!(super::map_status_error(503), super::NvidiaErrorKind::Server);
    }

    #[test]
    fn hosted_client_uses_the_nvidia_nim_endpoints() {
        let provider = super::NvidiaProvider::hosted(gocode_credentials::SecretString::new("test"));

        assert_eq!(
            provider.models_url().as_str(),
            "https://integrate.api.nvidia.com/v1/models"
        );
        assert_eq!(
            provider.chat_url().as_str(),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
    }

    #[test]
    fn provider_attaches_a_bearer_credential_without_exposing_it_elsewhere() {
        let provider = super::NvidiaProvider::hosted(gocode_credentials::SecretString::new("test"));

        assert_eq!(provider.authorization_value(), "Bearer test");
    }

    #[test]
    fn done_sse_marker_becomes_a_generic_completion_event() {
        let events = super::map_sse_data("[DONE]").expect("done marker should map");

        assert_eq!(events, vec![ChatStreamEvent::Completed]);
    }
}
