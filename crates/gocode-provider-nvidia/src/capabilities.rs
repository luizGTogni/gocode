use gocode_core::{ModelCapabilities, ThinkingCapability, ToolCapability};

/// Centralizes NVIDIA/NIM model-specific capability knowledge so no other module needs to
/// special-case a model name (`docs/NVIDIA_NIM.md` §21-22, "No Scattered Model Name Checks").
///
/// NIM's `/v1/models` endpoint does not report tool-calling or reasoning support, so this is the
/// only place that infers it. Everything not recognized here falls back to
/// [`ModelCapabilities::unknown`], the conservative default.
pub struct NvidiaCapabilityResolver;

impl NvidiaCapabilityResolver {
    /// Resolves capabilities for one model ID reported by NVIDIA's model catalog.
    #[must_use]
    pub fn resolve(model_id: &str) -> ModelCapabilities {
        // Nemotron's NIM-hosted chat models document OpenAI-compatible tool calling and an
        // `enable_thinking` + `reasoning_budget` reasoning control (see `build_chat_body`), which
        // covers every model in the Nemotron family we've observed in the hosted catalog.
        if model_id.to_ascii_lowercase().contains("nemotron") {
            return ModelCapabilities {
                streaming: true,
                tools: ToolCapability::Supported,
                thinking: ThinkingCapability::Effort {
                    levels: vec!["low".into(), "high".into()],
                    default: None,
                },
            };
        }

        ModelCapabilities::unknown()
    }
}

#[cfg(test)]
mod tests {
    use super::NvidiaCapabilityResolver;
    use gocode_core::{ThinkingCapability, ToolCapability};

    #[test]
    fn nemotron_models_are_known_to_support_tools_and_reasoning_effort() {
        let capabilities =
            NvidiaCapabilityResolver::resolve("nvidia/llama-3.1-nemotron-70b-instruct");

        assert_eq!(capabilities.tools, ToolCapability::Supported);
        assert!(matches!(
            capabilities.thinking,
            ThinkingCapability::Effort { .. }
        ));
    }

    #[test]
    fn nemotron_match_is_case_insensitive() {
        let capabilities = NvidiaCapabilityResolver::resolve("NVIDIA/Nemotron-3-Ultra");

        assert_eq!(capabilities.tools, ToolCapability::Supported);
    }

    #[test]
    fn unrecognized_models_fall_back_to_the_conservative_unknown_default() {
        let capabilities = NvidiaCapabilityResolver::resolve("nvidia/some-other-model");

        assert_eq!(capabilities, gocode_core::ModelCapabilities::unknown());
    }
}
