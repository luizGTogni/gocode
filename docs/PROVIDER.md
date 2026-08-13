# Gocode — Provider Architecture

**Status:** Initial technical draft  
**Product:** Gocode  
**Target version:** v0.1.0  
**Scope:** Model provider abstraction and provider integration contract

---

# 1. Purpose

This document defines the provider layer used by Gocode.

A provider is an adapter between the Gocode core and an external or local model inference service.

Examples of future providers include:

- NVIDIA NIM;
- OpenAI;
- Anthropic;
- Google Gemini;
- OpenRouter;
- Ollama;
- other OpenAI-compatible endpoints.

For v0.1.0, NVIDIA NIM is the only required implementation.

The provider layer must prevent provider-specific API details from leaking into:

- the Agent Runtime;
- the TUI;
- the tool system;
- project configuration;
- session logic.

---

# 2. Core Principle

Gocode should reason in terms of normalized capabilities and normalized inference events.

It should not reason in terms of:

```text
NVIDIA reasoning_effort
Anthropic thinking budget
OpenAI reasoning parameters
provider-specific streaming chunks
provider-specific tool call formats
```

Conceptually:

```text
Gocode Core
    ↓
Provider Contract
    ↓
Provider Adapter
    ↓
External API
```

---

# 3. Goals

The provider architecture must support:

- credential validation;
- model discovery;
- model metadata;
- capability resolution;
- streaming chat;
- tool calling;
- thinking/reasoning controls;
- usage reporting;
- provider-specific request mapping;
- provider-specific response parsing;
- cancellation;
- errors;
- future multiple providers.

---

# 4. Non-Goals for v0.1.0

The first provider layer does not need to support:

- provider marketplaces;
- user-defined JavaScript adapters;
- plugins;
- arbitrary protocol extensions;
- remote MCP provider discovery;
- provider load balancing;
- automatic multi-provider fallback;
- cost-based model routing.

---

# 5. Dependency Direction

The dependency direction must remain:

```text
gocode-core
    ↓
gocode-provider
    ↑
gocode-provider-nvidia
```

The generic provider crate must never depend on NVIDIA.

---

# 6. Provider Crate

Recommended structure:

```text
gocode-provider/
└── src/
    ├── provider.rs
    ├── registry.rs
    ├── model.rs
    ├── capabilities.rs
    ├── request.rs
    ├── response.rs
    ├── stream.rs
    ├── credentials.rs
    ├── errors.rs
    └── lib.rs
```

---

# 7. Provider Trait

Conceptual contract:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    fn display_name(&self) -> &'static str;

    async fn validate_credentials(
        &self,
    ) -> Result<CredentialStatus, ProviderError>;

    async fn list_models(
        &self,
    ) -> Result<Vec<Model>, ProviderError>;

    async fn stream_chat(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> Result<ChatStream, ProviderError>;
}
```

The exact signature may change, but these responsibilities should remain separated.

---

# 8. Provider ID

Provider IDs are stable machine-readable identifiers.

```rust
pub struct ProviderId(String);
```

Examples:

```text
nvidia
openai
anthropic
gemini
ollama
```

Do not use display names as identifiers.

---

# 9. Provider Metadata

Conceptual type:

```rust
pub struct ProviderMetadata {
    pub id: ProviderId,
    pub display_name: String,
    pub supports_model_discovery: bool,
}
```

Future fields may include:

- documentation hint;
- credential type;
- local/remote classification;
- configurable base URL support.

---

# 10. Provider Registry

The runtime owns a registry:

```rust
pub struct ProviderRegistry {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
}
```

Responsibilities:

- register available providers;
- resolve a provider by ID;
- return provider metadata;
- expose configured providers;
- support fake providers in tests.

---

# 11. Provider Construction

Providers should be constructed during application bootstrap.

Conceptually:

```text
Config
+
CredentialStore
+
HTTP client
↓
Provider factory
↓
ProviderRegistry
```

The TUI should never instantiate providers directly.

---

# 12. Provider Factory

A small factory can map provider configuration to implementations.

Conceptual example:

```rust
pub fn create_provider(
    config: &ProviderConfig,
    credentials: &dyn CredentialStore,
    http: reqwest::Client,
) -> Result<Arc<dyn Provider>>;
```

Avoid a heavy dependency injection framework.

---

# 13. Credentials

Credentials belong outside provider config files.

Provider adapters receive secret values through a credential resolver.

Conceptual key:

```rust
pub struct CredentialKey {
    pub provider: ProviderId,
    pub profile: String,
}
```

Example:

```text
provider = nvidia
profile = default
```

---

# 14. Credential Status

```rust
pub enum CredentialStatus {
    Valid,
    Missing,
    Invalid,
}
```

Provider-specific diagnostic information may be attached separately.

---

# 15. Credential Resolution

Recommended order:

```text
environment variable
↓
OS credential store
↓
onboarding
```

The provider should receive a resolved secret and should not need to know where it came from.

---

# 16. Secret Types

Use a secret wrapper rather than plain `String` where practical.

Conceptually:

```rust
SecretString
```

Requirements:

- redact `Debug`;
- do not log;
- zeroize when practical;
- never serialize into ordinary config.

---

# 17. Base URL

A provider implementation should encapsulate its default base URL.

Future provider config may support custom endpoints.

Conceptual:

```rust
pub struct ProviderEndpoint {
    pub base_url: Url,
}
```

For v0.1.0, NVIDIA may use its hosted API endpoint by default.

---

# 18. Model

Normalized representation:

```rust
pub struct Model {
    pub id: ModelId,
    pub provider: ProviderId,
    pub display_name: String,
    pub capabilities: ModelCapabilities,
    pub metadata: ModelMetadata,
}
```

---

# 19. Model ID

```rust
pub struct ModelId(String);
```

The provider's canonical model identifier should normally be preserved.

Example:

```text
nvidia/nemotron-3-super-120b-a12b
```

---

# 20. Model Metadata

Optional normalized metadata:

```rust
pub struct ModelMetadata {
    pub description: Option<String>,
    pub context_window: Option<u64>,
    pub input_modalities: Vec<Modality>,
    pub output_modalities: Vec<Modality>,
}
```

Do not require fields providers cannot reliably supply.

---

# 21. Model Discovery

Providers may discover models dynamically.

Conceptually:

```text
Provider API
↓
raw provider models
↓
provider model mapper
↓
normalized Model
↓
ModelRegistry
```

---

# 22. Static vs Dynamic Metadata

A model list endpoint may not expose every capability needed by Gocode.

Therefore capability resolution may combine:

```text
dynamic API metadata
+
provider-maintained metadata
+
known model overrides
+
cache
```

This is expected.

---

# 23. Capability Resolution

Capabilities are normalized before reaching the Agent Runtime.

```rust
pub struct ModelCapabilities {
    pub streaming: bool,
    pub tools: ToolCapability,
    pub thinking: ThinkingCapability,
    pub vision: VisionCapability,
    pub context: ContextCapability,
    pub sampling: SamplingCapability,
}
```

---

# 24. Capability Source

Useful internal representation:

```rust
pub enum CapabilitySource {
    Api,
    ProviderMetadata,
    BuiltInOverride,
    Cache,
}
```

A resolved capability may optionally retain provenance for debugging.

---

# 25. Tool Capability

Conceptual:

```rust
pub enum ToolCapability {
    Unsupported,
    Supported {
        parallel_calls: Option<bool>,
    },
}
```

Future fields may include:

- tool choice modes;
- strict schemas;
- parallel execution;
- streaming tool arguments.

---

# 26. Thinking Capability

Thinking/reasoning varies significantly between models and providers.

The generic layer must not assume one universal enum.

Conceptual:

```rust
pub enum ThinkingCapability {
    Unsupported,

    Toggle {
        default: bool,
    },

    Effort {
        levels: Vec<String>,
        default: Option<String>,
    },

    Budget {
        min_tokens: Option<u32>,
        max_tokens: Option<u32>,
        default_tokens: Option<u32>,
    },

    ToggleAndBudget {
        min_tokens: Option<u32>,
        max_tokens: Option<u32>,
        default_tokens: Option<u32>,
    },
}
```

---

# 27. Thinking Settings

Request-level normalized settings:

```rust
pub struct ThinkingSettings {
    pub mode: ThinkingMode,
    pub display: ThinkingDisplay,
}
```

---

# 28. Thinking Mode

```rust
pub enum ThinkingMode {
    Auto,
    Off,
    On,
    Effort(String),
    Budget(u32),
}
```

The provider adapter converts this to provider/model-specific parameters.

---

# 29. Why Strings for Effort Levels

Do not globally define:

```rust
enum Effort {
    Low,
    Medium,
    High,
}
```

because model APIs can expose different sets.

Examples can include:

```text
low / medium / high
none / high / max
```

The provider capability object defines the valid levels for each model.

---

# 30. Vision Capability

Conceptual:

```rust
pub enum VisionCapability {
    Unsupported,
    Supported,
}
```

Vision is not required by the initial Gocode MVP, but it should not require redesigning `ModelCapabilities`.

---

# 31. Context Capability

```rust
pub struct ContextCapability {
    pub max_tokens: Option<u64>,
}
```

Do not fabricate a context size when metadata is unknown.

---

# 32. Sampling Capability

Conceptual:

```rust
pub struct SamplingCapability {
    pub temperature: bool,
    pub top_p: bool,
    pub max_output_tokens: bool,
}
```

Provider adapters decide the exact request field mapping.

---

# 33. Chat Request

Normalized request:

```rust
pub struct ChatRequest {
    pub model: ModelId,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub thinking: ThinkingSettings,
    pub sampling: SamplingSettings,
}
```

Optional fields can evolve.

---

# 34. Message Model

Generic roles:

```rust
pub enum Message {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolMessage),
}
```

Provider adapters convert these into external API wire formats.

---

# 35. Provider Request Mapping

Flow:

```text
ChatRequest
↓
ProviderRequestMapper
↓
wire request
```

Provider-specific concepts belong exclusively in this mapping layer.

---

# 36. Provider Response Mapping

Flow:

```text
wire response
↓
ProviderStreamParser
↓
ChatStreamEvent
↓
Agent
```

---

# 37. Streaming

Generic event model:

```rust
pub enum ChatStreamEvent {
    TextDelta(String),
    ToolCallDelta(ToolCallDelta),
    ThinkingState(ThinkingState),
    Usage(Usage),
    Finished(FinishReason),
}
```

Not every provider needs to produce every event.

---

# 38. Text Deltas

Text content should stream as normalized UTF-8 strings.

The TUI receives agent events rather than raw provider chunks.

---

# 39. Tool Call Deltas

Some APIs stream tool calls incrementally.

Conceptual representation:

```rust
pub struct ToolCallDelta {
    pub index: Option<usize>,
    pub id: Option<String>,
    pub name_delta: Option<String>,
    pub arguments_delta: Option<String>,
}
```

A provider-specific assembler may convert these into complete tool calls before the Agent executes them.

---

# 40. Finish Reason

```rust
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Cancelled,
    ContentFilter,
    Other(String),
}
```

Provider-specific finish reasons should map into normalized variants where possible.

---

# 41. Usage

```rust
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}
```

Fields remain optional.

---

# 42. Cancellation

Provider requests must respect the AgentRun cancellation token.

The provider should stop:

- HTTP streaming;
- response parsing;
- request processing;

as soon as safely practical.

---

# 43. Error Model

Generic provider errors:

```rust
pub enum ProviderError {
    MissingCredential,
    InvalidCredential,

    Network(String),
    Timeout,
    RateLimited {
        retry_after: Option<Duration>,
    },

    ModelNotFound(ModelId),
    UnsupportedCapability(String),

    InvalidRequest(String),
    InvalidResponse(String),

    Server {
        status: Option<u16>,
        message: String,
    },

    Cancelled,
}
```

---

# 44. Provider-Specific Error Mapping

Provider adapters map wire/API errors into generic errors.

Example:

```text
HTTP 401
↓
ProviderError::InvalidCredential
```

```text
HTTP 429
↓
ProviderError::RateLimited
```

---

# 45. User-Facing Errors

The provider layer should not own final TUI wording.

Flow:

```text
ProviderError
↓
AppError
↓
ErrorView
```

This keeps UX language centralized.

---

# 46. Retries

Provider retries must be limited and explicit.

Possible policy:

```text
connection failure → limited retry
timeout → limited retry
429 → obey retry hints when reasonable
5xx → limited retry
401/403 → no blind retry
```

Never retry indefinitely.

---

# 47. Streaming Retry

Do not automatically restart a response after meaningful streamed content has already been consumed unless the architecture can guarantee consistency.

For the MVP, fail clearly instead of risking duplicated model output.

---

# 48. HTTP Client

Remote providers should reuse an HTTP client.

Recommended:

```text
reqwest::Client
```

Configure:

- TLS;
- connection pooling;
- timeout;
- user agent;
- proxy behavior inherited appropriately;
- redacted logging.

---

# 49. User Agent

Recommended format:

```text
gocode/<version>
```

This can help provider diagnostics.

---

# 50. Provider Cache

Provider metadata may be cached in:

```text
~/.gocode/cache/
```

Possible cached information:

- model list;
- capability metadata;
- last successful refresh.

---

# 51. Cache Is Not Authority

Cache is an optimization.

A stale cached model must not permanently prevent refreshing provider state.

---

# 52. Model Registry

The application-level `ModelRegistry` can aggregate models from all providers.

Conceptually:

```rust
pub struct ModelRegistry {
    models: HashMap<(ProviderId, ModelId), Model>,
}
```

---

# 53. Model Refresh

Possible flow:

```text
load cache
↓
show TUI quickly
↓
refresh provider models async
↓
merge registry
↓
emit ModelRegistryUpdated
```

This supports fast startup.

---

# 54. Missing Saved Model

If a configured model disappears:

```text
saved model
↓
not found in refreshed registry
↓
mark unavailable
↓
prompt user to select another model
```

Do not crash startup.

---

# 55. Capability Changes

Capabilities may change between provider versions or model deployments.

The refresh process must be able to replace cached capability data.

---

# 56. Provider Configuration

Conceptual config:

```toml
[providers.nvidia]
enabled = true
```

Future:

```toml
[providers.some-provider]
base_url = "..."
```

Credentials remain separate.

---

# 57. Default Provider

Global config may store:

```toml
default_provider = "nvidia"
```

Project config may override it later.

---

# 58. Default Model

Config may store:

```toml
default_model = "nvidia/nemotron-..."
```

On startup, it must be validated against available models.

---

# 59. Provider Onboarding

Provider onboarding flow:

```text
select provider
↓
enter credential
↓
validate credential
↓
list models
↓
select model
↓
save non-secret config
```

---

# 60. Provider Validation

Credential validation should use the cheapest reliable provider action available.

Possible approaches:

- model listing;
- lightweight authenticated endpoint;
- minimal inference request only if necessary.

Provider implementation decides.

---

# 61. Provider Health

A temporary network failure is different from invalid credentials.

Do not incorrectly delete credentials after a timeout.

---

# 62. Local Providers

The generic provider architecture should allow local inference later.

Examples:

```text
Ollama
self-hosted NIM
OpenAI-compatible local endpoint
```

Local providers may not require credentials.

---

# 63. Hosted vs Self-Hosted

Provider identity and endpoint configuration are separate concepts.

For example:

```text
Provider implementation = NVIDIA NIM
Endpoint = NVIDIA hosted API
```

Future:

```text
Provider implementation = NVIDIA NIM
Endpoint = self-hosted NIM
```

The adapter should be reusable when protocol behavior is compatible.

---

# 64. OpenAI-Compatible Does Not Mean Identical

Even when multiple services use an OpenAI-compatible API, Gocode must not assume identical:

- model capabilities;
- reasoning fields;
- tool parser behavior;
- response metadata;
- context limits;
- authentication;
- supported endpoints.

Compatibility is a wire-protocol advantage, not a reason to remove provider adapters.

---

# 65. Provider Capability Overrides

Provider implementations may maintain a small model capability table.

Conceptually:

```rust
match model.id.as_str() {
    "..." => ...
}
```

However, model-specific checks must remain centralized in capability resolution.

Never scatter model-name conditionals across the Agent or TUI.

---

# 66. Unknown Models

If a provider returns a model unknown to built-in metadata:

Use conservative defaults.

Example:

```text
streaming = provider-level known support
tools = unknown/unsupported until verified
thinking = unsupported/unknown
context = unknown
```

Do not claim capabilities without evidence.

---

# 67. Capability Verification

Future provider contract tests may probe model behavior.

For the MVP, use documented metadata and known mappings.

---

# 68. Provider Tests

Every provider should test:

- credential success;
- credential failure;
- model listing;
- model mapping;
- streaming text;
- tool calls;
- malformed chunks;
- reasoning settings;
- cancellation;
- 429;
- 5xx;
- timeout.

---

# 69. Fake Provider

The core test suite must include:

```rust
FakeProvider
```

It should support scripted:

- text deltas;
- tool calls;
- failures;
- delays;
- cancellation;
- usage.

Agent tests should not depend on NVIDIA.

---

# 70. Provider Contract Tests

Common test behavior should be reusable across implementations.

Conceptual suite:

```text
provider_contract::models()
provider_contract::stream_text()
provider_contract::tool_call()
provider_contract::cancel()
provider_contract::errors()
```

---

# 71. Observability

Provider calls may emit structured tracing fields:

```text
provider
model
request_id
duration
status
```

Never include:

- API key;
- authorization headers;
- raw secret values.

---

# 72. Request Logging

Raw prompts and file contents should not be logged by default.

Debug logging must still avoid secrets.

---

# 73. Provider Metrics

Local optional metrics may include:

- first-token latency;
- total duration;
- input tokens;
- output tokens;
- reasoning tokens;
- tool-call count.

No remote telemetry is required by the MVP.

---

# 74. Cost Metadata

Cost tracking is not required in v0.1.0.

The architecture should not bake cost assumptions into `Usage`.

A future layer can combine usage with pricing metadata.

---

# 75. Provider Switching

When switching providers:

```text
/provider
↓
select provider
↓
ensure credentials
↓
refresh models
↓
select model
↓
update session state
```

Existing conversation compatibility must be handled conservatively.

---

# 76. Model Switching

When switching models within a provider:

- update active model;
- update capabilities;
- update thinking options;
- update tool availability;
- preserve conversation when safe.

---

# 77. Capability-Driven UI

The TUI queries normalized capabilities.

Examples:

```text
tools unsupported
→ show chat-only warning
```

```text
thinking unsupported
→ hide thinking settings
```

```text
thinking effort
→ show valid levels
```

The TUI does not inspect provider names.

---

# 78. Capability-Driven Agent

Before starting a coding run:

```text
ModelCapabilities.tools
```

must determine whether tool definitions can be sent.

The Agent does not contain:

```rust
if provider == "nvidia"
```

---

# 79. Provider Boundary Summary

Provider layer owns:

- authentication wire behavior;
- endpoints;
- model API parsing;
- request JSON;
- streaming protocol;
- reasoning parameter translation;
- tool-call protocol translation;
- provider errors.

Core owns:

- conversation semantics;
- tools;
- permissions;
- agent loop;
- project;
- user intent.

---

# 80. MVP Provider Definition of Done

The provider architecture is ready for v0.1.0 when:

- NVIDIA can implement the generic `Provider` contract;
- Agent code contains no NVIDIA-specific request logic;
- TUI contains no NVIDIA-specific capability logic;
- models are normalized;
- tool calls are normalized;
- streaming is normalized;
- thinking settings are capability-driven;
- credentials are external to config;
- provider errors map into stable generic errors;
- FakeProvider can run all Agent tests.

---

# 81. Final Rule

Provider APIs will change.

Model capabilities will vary.

Gocode should absorb that complexity inside provider adapters.

> The Gocode core should understand what a model can do, not how a specific vendor exposes that capability over HTTP.
