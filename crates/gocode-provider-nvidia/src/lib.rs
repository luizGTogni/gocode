use gocode_core::{ChatMessage, ChatRequest, ChatStreamEvent, Model, ModelCapabilities, ModelId};

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
}
