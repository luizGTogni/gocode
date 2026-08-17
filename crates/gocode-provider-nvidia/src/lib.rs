use futures_util::StreamExt;
use gocode_core::{
    CancellationToken, ChatMessage, ChatRequest, ChatStream, ChatStreamEvent, FinishReason, Model,
    ModelCapabilities, ModelId, Provider, ProviderError, ProviderFuture, ToolCallDelta, Usage,
};
use gocode_credentials::SecretString;

const HOSTED_BASE_URL: &str = "https://integrate.api.nvidia.com/";
/// Maximum buffered bytes without an SSE newline. A provider response is untrusted input, so a
/// malformed never-ending event must not consume an unbounded amount of memory.
const MAX_PENDING_SSE_BYTES: usize = 1024 * 1024;

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
    ) -> Result<ChatStream, ProviderError> {
        let response = self
            .client
            .post(self.chat_url())
            .bearer_auth(self.credential.expose())
            .json(&build_chat_body(&request))
            .send()
            .await
            .map_err(|error| ProviderError::Network(error.to_string()))?;

        if !response.status().is_success() {
            return Err(map_status_error(response.status().as_u16()).into());
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
                            if pending.len() > MAX_PENDING_SSE_BYTES {
                                let _ = sender
                                    .send(Err(ProviderError::InvalidResponse(
                                        "stream event exceeded the maximum permitted size".into(),
                                    )))
                                    .await;
                                return;
                            }
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
                                        let _ = sender
                                            .send(Err(ProviderError::InvalidResponse(error.to_string())))
                                            .await;
                                        return;
                                    }
                                }
                            }
                        }
                        Some(Err(error)) => {
                            let _ = sender.send(Err(ProviderError::Network(error.to_string()))).await;
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

impl Provider for NvidiaProvider {
    fn stream_chat(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_> {
        Box::pin(self.stream_chat(request, cancellation))
    }
}

/// Safe error surface produced by the NVIDIA client outside the streamed event channel.
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

impl From<NvidiaClientError> for ProviderError {
    fn from(error: NvidiaClientError) -> Self {
        match error {
            NvidiaClientError::Network(message) => Self::Network(message),
            NvidiaClientError::Http(kind) => kind.into(),
            NvidiaClientError::Protocol(error) => Self::InvalidResponse(error.to_string()),
        }
    }
}

impl From<NvidiaErrorKind> for ProviderError {
    fn from(kind: NvidiaErrorKind) -> Self {
        match kind {
            NvidiaErrorKind::InvalidCredential => Self::InvalidCredential,
            NvidiaErrorKind::RateLimited => Self::RateLimited,
            NvidiaErrorKind::Server => Self::Server {
                status: None,
                message: "NVIDIA reported a server-side failure".into(),
            },
            NvidiaErrorKind::Request => Self::InvalidRequest("NVIDIA rejected the request".into()),
        }
    }
}

/// Builds NVIDIA's OpenAI-compatible streaming chat body from normalized input.
#[must_use]
pub fn build_chat_body(request: &ChatRequest) -> serde_json::Value {
    let messages = request
        .messages
        .iter()
        .map(build_message)
        .collect::<Vec<_>>();

    let mut body = serde_json::json!({
        "model": request.model.as_str(),
        "messages": messages,
        "stream": true,
    });

    if !request.tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                        }
                    })
                })
                .collect(),
        );
        // Some OpenAI-compatible backends (NIM's vLLM-hosted models in particular) only engage
        // their structured tool-call decoder when `tool_choice` is explicit; without it, a model
        // that sees `tools` in the prompt may still attempt a call using its own chat-template
        // syntax, which the server then passes through as plain assistant text instead of
        // parsing it into `tool_calls`.
        body["tool_choice"] = serde_json::Value::String("auto".into());
    }

    if let Some(effort) = &request.reasoning_effort {
        body["reasoning_effort"] = serde_json::Value::String(effort.clone());
    }

    body
}

fn build_message(message: &ChatMessage) -> serde_json::Value {
    match message {
        ChatMessage::System(content) => serde_json::json!({ "role": "system", "content": content }),
        ChatMessage::User(content) => serde_json::json!({ "role": "user", "content": content }),
        ChatMessage::Assistant { text, tool_calls } => {
            let mut object = serde_json::json!({
                "role": "assistant",
                "content": text.clone().unwrap_or_default(),
            });
            if !tool_calls.is_empty() {
                object["tool_calls"] = serde_json::Value::Array(
                    tool_calls
                        .iter()
                        .map(|call| {
                            serde_json::json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments.to_string(),
                                }
                            })
                        })
                        .collect(),
                );
            }
            object
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content,
        }),
    }
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

    if let Some(choices) = object.get("choices").and_then(serde_json::Value::as_array) {
        for choice in choices {
            if let Some(content) = choice
                .get("delta")
                .and_then(|delta| delta.get("content"))
                .and_then(serde_json::Value::as_str)
                .filter(|content| !content.is_empty())
            {
                events.push(ChatStreamEvent::TextDelta(content.into()));
            }

            // Some reasoning-capable models (served via vLLM/NIM) stream their chain-of-thought
            // in a separate `reasoning_content` field rather than `content`; without this, a
            // turn that finishes without producing final-answer text looks completely empty.
            if let Some(reasoning) = choice
                .get("delta")
                .and_then(|delta| delta.get("reasoning_content"))
                .and_then(serde_json::Value::as_str)
                .filter(|reasoning| !reasoning.is_empty())
            {
                events.push(ChatStreamEvent::ReasoningDelta(reasoning.into()));
            }

            if let Some(tool_calls) = choice
                .get("delta")
                .and_then(|delta| delta.get("tool_calls"))
                .and_then(serde_json::Value::as_array)
            {
                for tool_call in tool_calls {
                    events.push(ChatStreamEvent::ToolCallDelta(map_tool_call_delta(
                        tool_call,
                    )));
                }
            }

            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(serde_json::Value::as_str)
            {
                events.push(ChatStreamEvent::Finished(map_finish_reason(reason)));
            }
        }
    }

    if let Some(usage) = object.get("usage") {
        events.push(ChatStreamEvent::Usage(Usage {
            input_tokens: usage
                .get("prompt_tokens")
                .and_then(serde_json::Value::as_u64),
            output_tokens: usage
                .get("completion_tokens")
                .and_then(serde_json::Value::as_u64),
            reasoning_tokens: usage
                .get("completion_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(serde_json::Value::as_u64),
        }));
    }

    Ok(events)
}

fn map_tool_call_delta(tool_call: &serde_json::Value) -> ToolCallDelta {
    let index = tool_call
        .get("index")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let function = tool_call.get("function");

    ToolCallDelta {
        index: usize::try_from(index).unwrap_or(usize::MAX),
        id: tool_call
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(String::from),
        name_delta: function
            .and_then(|function| function.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(String::from),
        arguments_delta: function
            .and_then(|function| function.get("arguments"))
            .and_then(serde_json::Value::as_str)
            .map(String::from),
    }
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "tool_calls" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.into()),
    }
}

/// Maps one SSE `data:` value from NVIDIA into generic events.
///
/// The `[DONE]` sentinel carries no information beyond what the preceding chunk's
/// `finish_reason` already reported, so it maps to no events.
///
/// # Errors
///
/// Returns an error if a non-terminal data value is malformed JSON.
pub fn map_sse_data(data: &str) -> Result<Vec<ChatStreamEvent>, NvidiaProtocolError> {
    if data == "[DONE]" {
        return Ok(Vec::new());
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
    use gocode_core::{ChatStreamEvent, FinishReason, ToolCallDelta, Usage};

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
    fn maps_reasoning_content_delta_to_a_generic_event() {
        let event = super::map_stream_chunk(
            r#"{"id":"req-123","choices":[{"delta":{"reasoning_content":"pensando..."}}]}"#,
        )
        .expect("valid NVIDIA chunk should map");

        assert_eq!(
            event,
            vec![
                ChatStreamEvent::RequestId("req-123".into()),
                ChatStreamEvent::ReasoningDelta("pensando...".into())
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
    fn maps_streamed_tool_call_fragments_and_finish_reason() {
        let events = super::map_stream_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read_file","arguments":"{\"path\""}}]},"finish_reason":null}]}"#,
        )
        .expect("tool-call fragment should map");

        assert_eq!(
            events,
            vec![ChatStreamEvent::ToolCallDelta(ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name_delta: Some("read_file".into()),
                arguments_delta: Some("{\"path\"".into()),
            })]
        );
    }

    #[test]
    fn maps_finish_reason_and_usage() {
        let events = super::map_stream_chunk(
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .expect("finish reason and usage should map");

        assert_eq!(
            events,
            vec![
                ChatStreamEvent::Finished(FinishReason::ToolCalls),
                ChatStreamEvent::Usage(Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    reasoning_tokens: None,
                }),
            ]
        );
    }

    #[test]
    fn builds_an_openai_compatible_streaming_request() {
        let request = gocode_core::ChatRequest::single_user("nvidia/model", "Olá");

        let body = super::build_chat_body(&request);

        assert_eq!(body["model"], "nvidia/model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Olá");
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn builds_tool_definitions_and_tool_result_messages() {
        let mut request = gocode_core::ChatRequest::single_user("nvidia/model", "Olá");
        request.tools.push(gocode_core::ToolDefinition {
            name: "read_file".into(),
            description: "Reads a file.".into(),
            input_schema: serde_json::json!({"type": "object"}),
        });
        request.messages.push(gocode_core::ChatMessage::Assistant {
            text: None,
            tool_calls: vec![gocode_core::ProviderToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "a.rs"}),
            }],
        });
        request.messages.push(gocode_core::ChatMessage::Tool {
            tool_call_id: "call-1".into(),
            content: "fn main() {}".into(),
        });

        let body = super::build_chat_body(&request);

        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call-1");
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
    fn done_sse_marker_maps_to_no_additional_events() {
        let events = super::map_sse_data("[DONE]").expect("done marker should map");

        assert!(events.is_empty());
    }
}
