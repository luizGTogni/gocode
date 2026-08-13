# Gocode — NVIDIA NIM Provider

**Status:** Initial technical draft  
**Product:** Gocode  
**Target version:** v0.1.0  
**Provider ID:** `nvidia`  
**Scope:** NVIDIA NIM integration

---

# 1. Purpose

This document defines the NVIDIA NIM provider implementation for Gocode.

NVIDIA NIM is the primary provider used to develop and validate the Gocode v0.1.0 MVP.

The implementation must support:

- NVIDIA API key authentication;
- hosted NVIDIA inference;
- model discovery;
- OpenAI-compatible chat completions;
- streaming;
- tool calling;
- thinking/reasoning controls;
- per-model capability resolution;
- normalized errors;
- cancellation.

All NVIDIA-specific behavior must remain inside:

```text
gocode-provider-nvidia
```

---

# 2. Current NVIDIA API Model

NVIDIA NIM for LLMs exposes OpenAI-compatible inference APIs.

The relevant API family includes:

```text
POST /v1/chat/completions
GET  /v1/models
```

Recent NIM LLM versions also expose additional APIs such as Responses-compatible and Anthropic-compatible endpoints.

For Gocode v0.1.0, the preferred integration surface is:

```text
/v1/chat/completions
```

because it supports the features required by the coding agent:

- multi-turn messages;
- streaming;
- tool calling.

---

# 3. Hosted API Base URL

For NVIDIA's hosted API catalog, the default base URL is:

```text
https://integrate.api.nvidia.com
```

The chat endpoint is:

```text
POST /v1/chat/completions
```

Therefore:

```text
https://integrate.api.nvidia.com/v1/chat/completions
```

The base URL must remain configurable internally to preserve future support for self-hosted NIM deployments.

---

# 4. Authentication

Hosted NVIDIA API requests use bearer-token authentication.

Conceptually:

```http
Authorization: Bearer <NVIDIA_API_KEY>
Content-Type: application/json
```

The user-facing environment variable supported by Gocode should be:

```text
NVIDIA_API_KEY
```

---

# 5. Credential Storage

Gocode must not write the NVIDIA API key to:

```text
~/.gocode/config.toml
```

Recommended resolution order:

```text
NVIDIA_API_KEY
↓
Windows Credential Manager
↓
onboarding
```

---

# 6. Provider Configuration

Non-secret configuration may look like:

```toml
[providers.nvidia]
enabled = true
```

Optional future configuration:

```toml
[providers.nvidia]
base_url = "https://integrate.api.nvidia.com"
```

The default hosted URL should not require user configuration.

---

# 7. NVIDIA Provider Structure

Recommended module layout:

```text
gocode-provider-nvidia/
└── src/
    ├── client.rs
    ├── auth.rs
    ├── models.rs
    ├── capabilities.rs
    ├── chat.rs
    ├── request.rs
    ├── streaming.rs
    ├── tools.rs
    ├── thinking.rs
    ├── errors.rs
    └── lib.rs
```

---

# 8. Provider Type

Conceptual:

```rust
pub struct NvidiaProvider {
    client: reqwest::Client,
    base_url: Url,
    credential: SecretString,
    capabilities: NvidiaCapabilityResolver,
}
```

Avoid storing unnecessary mutable state inside the provider.

---

# 9. Credential Validation

Validation should distinguish:

```text
missing credential
invalid credential
network failure
server failure
```

Do not treat a timeout as an invalid API key.

A lightweight authenticated model-list or equivalent request is preferred when reliable.

---

# 10. Chat Completions

Generic Gocode request:

```text
ChatRequest
```

maps to NVIDIA's OpenAI-compatible chat completion request.

Core fields include:

```json
{
  "model": "...",
  "messages": [...],
  "stream": true
}
```

When tools are supported:

```json
{
  "tools": [...]
}
```

Provider-specific reasoning fields are added according to the selected model.

---

# 11. Streaming

Gocode should use streaming by default when the model supports it.

Conceptual request:

```json
{
  "stream": true
}
```

NVIDIA chunks must be parsed and converted into:

```rust
ChatStreamEvent
```

The Agent must never parse NVIDIA wire chunks directly.

---

# 12. Streaming Pipeline

```text
NVIDIA HTTP stream
↓
SSE / streaming parser
↓
NVIDIA chunk representation
↓
NvidiaStreamMapper
↓
ChatStreamEvent
↓
Agent Runtime
```

---

# 13. Text Streaming

Text deltas map to:

```rust
ChatStreamEvent::TextDelta(String)
```

The adapter should tolerate empty chunks and metadata-only chunks.

---

# 14. Tool Calling

NIM LLM supports OpenAI-compatible tool calling for supported models/deployments.

Gocode sends tool definitions using the normalized tool registry.

Conceptually:

```json
{
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "read_file",
        "description": "...",
        "parameters": {}
      }
    }
  ]
}
```

---

# 15. Tool Choice

Gocode should initially allow the model to choose when to call tools.

Provider request mapping may use the default tool-choice behavior supported by the model/API.

Do not force a tool call unless the Agent architecture explicitly requires it.

---

# 16. Tool Call Parsing

NVIDIA/OpenAI-compatible responses may represent tool calls incrementally while streaming.

The NVIDIA adapter must:

1. identify tool call index/ID;
2. assemble function name;
3. assemble argument JSON;
4. validate completed JSON shape;
5. emit normalized tool call data.

---

# 17. Tool Calling Is Model-Dependent

NIM supporting tool calling at the API level does not mean every model is suitable for tool use.

Therefore:

```text
NIM API supports tools
≠
every NVIDIA catalog model supports tools
```

Tool support must be represented per model in `ModelCapabilities`.

---

# 18. Model Discovery

NIM deployments expose model listing through:

```text
GET /v1/models
```

However, the hosted NVIDIA model catalog and individual model documentation may expose richer metadata than a simple model-list response.

Gocode should separate:

```text
model discovery
```

from:

```text
capability discovery
```

---

# 19. Model Discovery Pipeline

```text
NVIDIA model endpoint/catalog
↓
raw model IDs
↓
NvidiaModelMapper
↓
Model
↓
ModelRegistry
```

---

# 20. Capability Resolution

The model-list response alone may not be sufficient to determine:

- tools;
- reasoning;
- valid effort values;
- reasoning budget;
- vision;
- context size.

Therefore:

```text
API model metadata
+
documented NVIDIA model metadata
+
built-in capability mappings
+
cache
=
ModelCapabilities
```

---

# 21. Capability Resolver

Conceptual:

```rust
pub struct NvidiaCapabilityResolver {
    known_models: HashMap<ModelId, NvidiaModelCapabilities>,
}
```

All special cases must stay centralized here.

---

# 22. No Scattered Model Name Checks

Avoid:

```rust
if model.contains("nemotron") {
    ...
}
```

inside:

- Agent;
- TUI;
- request builder;
- tools.

Use:

```text
NvidiaCapabilityResolver
```

instead.

---

# 23. Thinking and Reasoning

NVIDIA's hosted catalog includes models with different reasoning controls.

There is no single universal NVIDIA reasoning mode.

Examples in the current API documentation include models with:

```text
reasoning_effort = low | medium | high
```

others with:

```text
reasoning_effort = none | high | max
```

and Nemotron models that can additionally expose a reasoning token budget.

Therefore Gocode must use capability-driven mapping.

---

# 24. Example: Low / Medium / High

Some NVIDIA-hosted model APIs expose:

```text
reasoning_effort
```

with:

```text
low
medium
high
```

Generic mapping:

```text
ThinkingMode::Effort("low")
ThinkingMode::Effort("medium")
ThinkingMode::Effort("high")
```

---

# 25. Example: None / High / Max

Other model APIs expose:

```text
reasoning_effort
```

with:

```text
none
high
max
```

This must be modeled exactly as the model capability declares.

Do not translate `max` into a generic `high`.

---

# 26. Example: Reasoning Budget

Some Nemotron model APIs expose a separate reasoning budget.

A current documented pattern includes a token budget with an upper bound in the tens of thousands and a special value that disables budget enforcement.

The provider adapter should model this generically as:

```rust
ThinkingCapability::ToggleAndBudget { ... }
```

or another normalized capability selected by the final schema.

---

# 27. Provider-Specific Thinking Mapping

Conceptual:

```rust
fn map_thinking(
    model: &Model,
    settings: &ThinkingSettings,
    body: &mut serde_json::Map<String, Value>,
) -> Result<(), ProviderError>
```

This function is the only place that should know fields such as:

```text
reasoning_effort
reasoning_budget
chat_template_kwargs
```

---

# 28. Thinking Auto Mode

Gocode default:

```text
ThinkingMode::Auto
```

For NVIDIA, Auto should use the model/provider recommended default.

The first MVP implementation may simply omit explicit reasoning parameters when Auto is selected if that preserves the model's documented default.

---

# 29. Thinking Off

If a model exposes a documented off value, map:

```text
ThinkingMode::Off
```

to that value.

Example pattern:

```text
reasoning_effort = "none"
```

only when that model explicitly supports it.

Do not send `"none"` universally.

---

# 30. Unsupported Thinking

If:

```text
ThinkingCapability::Unsupported
```

then Gocode must not send reasoning-specific fields.

---

# 31. Invalid Effort

If the user/session requests:

```text
medium
```

but the selected model only supports:

```text
none
high
max
```

the provider should reject the request before HTTP execution.

Return:

```text
ProviderError::UnsupportedCapability
```

The TUI should normally prevent this state through capability-driven controls.

---

# 32. Reasoning Output

Some reasoning models can produce separate reasoning/thinking information depending on deployment/parser configuration.

Gocode must not depend on displaying raw reasoning traces.

The normalized interface should favor:

```text
ThinkingState
```

and final answer/tool behavior.

---

# 33. Thinking UI

The TUI may show:

```text
● Thinking
```

It should not require exposing internal reasoning text.

This keeps model differences isolated and the UI stable.

---

# 34. Nemotron

Nemotron models are important candidates for Gocode because NVIDIA documents reasoning-capable and coding/agentic models in the family.

However, capabilities differ across Nemotron generations.

Never use:

```text
Nemotron → always same reasoning settings
```

Resolve by exact model ID.

---

# 35. Coding Model Selection

The MVP should prioritize models that satisfy:

```text
chat
+
streaming
+
tool calling
+
strong coding behavior
```

Thinking/reasoning is highly desirable but may be optional if the model is otherwise useful.

The model picker should clearly communicate capability differences.

---

# 36. Model Picker Representation

Example:

```text
NVIDIA NIM

> Model A
  Tools       ✓
  Thinking    ✓
  Streaming   ✓

  Model B
  Tools       -
  Thinking    ✓
  Streaming   ✓
```

The actual names should come from current provider metadata.

---

# 37. Unknown NVIDIA Models

If an NVIDIA catalog model is not in Gocode's known capability table:

Use conservative behavior.

Suggested:

```text
streaming = known provider default when safe
tools = unknown/disabled
thinking = unsupported/unknown
vision = metadata-dependent
```

The user may still use it for chat if inference succeeds.

---

# 38. Capability Cache

Resolved NVIDIA model metadata may be cached:

```text
~/.gocode/cache/nvidia-models.json
```

Conceptual format:

```json
{
  "schema_version": 1,
  "updated_at": "...",
  "models": []
}
```

Exact filename is not part of the public contract.

---

# 39. Cache Refresh

Recommended:

```text
load cached metadata
↓
start TUI
↓
refresh NVIDIA metadata asynchronously
↓
merge
↓
notify UI
```

Do not block startup unnecessarily.

---

# 40. Context Windows

Context size is model-specific.

Store:

```rust
Option<u64>
```

Do not assume one global NVIDIA context size.

The model picker may display it when known.

---

# 41. Vision

Some NVIDIA-hosted models are multimodal.

Vision is outside the initial Gocode coding workflow but should be reflected in:

```text
ModelCapabilities
```

when confidently known.

---

# 42. Request Builder

Recommended separation:

```text
chat.rs
request.rs
thinking.rs
tools.rs
```

Conceptually:

```rust
let mut body = map_messages(request.messages);

map_tools(...);
map_thinking(...);
map_sampling(...);
```

---

# 43. Sampling

Generic Gocode sampling settings may map to fields such as:

```text
temperature
top_p
max_tokens / max completion equivalent
```

Do not send unsupported parameters simply because another model accepts them.

---

# 44. Max Output Tokens

The field and limits may vary by model/API.

The NVIDIA adapter should validate any documented limits available through capability metadata.

---

# 45. Error Mapping

Important mappings:

```text
missing local key
→ MissingCredential
```

```text
401/403 authentication failure
→ InvalidCredential
```

```text
404 model/endpoint
→ ModelNotFound or InvalidRequest
```

```text
429
→ RateLimited
```

```text
5xx
→ Server
```

```text
network timeout
→ Timeout
```

---

# 46. Rate Limits

Do not assume fixed NVIDIA rate limits.

When available:

- inspect response headers;
- obey retry hints;
- surface a friendly message.

Never implement infinite retries.

---

# 47. Request IDs

If NVIDIA returns a request/correlation ID, include it in structured tracing metadata.

This can make support/debugging easier.

Do not show it in normal UI unless needed.

---

# 48. HTTP Client

Use a shared:

```rust
reqwest::Client
```

Recommended headers:

```text
Authorization: Bearer ...
Content-Type: application/json
User-Agent: gocode/<version>
```

Never log the authorization header.

---

# 49. Timeout Policy

Separate:

```text
connect timeout
request timeout
stream inactivity considerations
```

Streaming requests may need different timeout behavior from ordinary metadata calls.

---

# 50. Cancellation

If the Agent is cancelled:

```text
CancellationToken
↓
stop reading NVIDIA stream
↓
drop/cancel HTTP response
↓
AgentState::Cancelled
```

If using an API endpoint with explicit remote cancellation in the future, that can be integrated separately.

---

# 51. Tool Definition Mapping

Gocode's generic:

```rust
ToolDefinition
```

maps to OpenAI-compatible function tools.

Conceptual:

```json
{
  "type": "function",
  "function": {
    "name": "...",
    "description": "...",
    "parameters": {}
  }
}
```

---

# 52. Tool Result Mapping

After local tool execution:

```text
ToolResult
↓
generic ToolMessage
↓
NVIDIA/OpenAI-compatible tool role message
↓
next chat completion request
```

Tool call IDs must be preserved.

---

# 53. Tool Call Integrity

The adapter must preserve:

```text
tool_call_id
function name
arguments
ordering
```

across streaming and subsequent tool result messages.

---

# 54. Malformed Tool Arguments

If streamed tool argument JSON is malformed at completion:

Return a provider/stream parsing error or normalized invalid tool call result according to the final stream architecture.

Never execute partially parsed JSON.

---

# 55. Multiple Tool Calls

The API may return multiple tool calls.

The provider should preserve all calls.

The Agent MVP may execute them sequentially.

---

# 56. Parallel Tool Calling

Do not infer that parallel tool calls are safe simply because a model/API can emit multiple calls.

Execution policy belongs to the Agent/Tools layer.

---

# 57. Self-Hosted NIM Future

NIM can also be deployed by users on their own infrastructure.

The adapter should preserve the possibility of:

```text
custom base URL
```

Future config:

```toml
[providers.nvidia]
base_url = "http://localhost:8000"
```

Authentication behavior may differ for local deployments.

---

# 58. Hosted vs Local Authentication

Do not hardcode hosted bearer-auth assumptions into the generic provider trait.

Keep them inside NVIDIA endpoint configuration.

A future self-hosted NIM profile may use:

- no auth;
- custom bearer auth;
- gateway auth.

---

# 59. Endpoint Profile

Possible future internal type:

```rust
pub enum NvidiaEndpointProfile {
    HostedCatalog,
    Custom {
        base_url: Url,
        auth: NvidiaAuthMode,
    },
}
```

Not necessary to expose in v0.1.0 UI.

---

# 60. Hosted MVP Priority

For v0.1.0, optimize for:

```text
NVIDIA hosted API
+
NVIDIA_API_KEY
```

Do not let self-hosting complexity delay the MVP.

---

# 61. NVIDIA-Specific Config Boundary

NVIDIA config may know:

```text
base_url
credential profile
metadata cache
```

It should not control:

```text
Agent max turns
workspace permissions
TUI layout
tool policies
```

---

# 62. Model Metadata Updates

The NVIDIA model catalog changes over time.

Therefore Gocode must be able to update model metadata independently of core Agent behavior.

Possible future approaches:

- metadata shipped with Gocode releases;
- provider endpoint discovery;
- remote metadata source;
- hybrid.

For MVP, a hybrid built-in mapping + API model list is acceptable.

---

# 63. Capability Metadata Versioning

If built-in model mappings are stored separately:

```json
{
  "schema_version": 1,
  "models": {}
}
```

Keep schema version independent from Gocode's product version.

---

# 64. Capability Mapping Example

Conceptual only:

```rust
NvidiaModelCapabilities {
    tools: Supported,
    thinking: Effort {
        levels: vec!["low", "medium", "high"],
        default: Some("medium"),
    },
}
```

Another model:

```rust
NvidiaModelCapabilities {
    tools: Supported,
    thinking: Effort {
        levels: vec!["none", "high", "max"],
        default: Some("high"),
    },
}
```

Another:

```rust
NvidiaModelCapabilities {
    tools: Supported,
    thinking: ToggleAndBudget {
        min_tokens: Some(...),
        max_tokens: Some(...),
        default_tokens: Some(...),
    },
}
```

---

# 65. Capability Fidelity

Preserve vendor/model semantics instead of forcing every model into the same vocabulary.

This is especially important for reasoning.

---

# 66. NVIDIA Provider Tests

Unit tests should cover:

- auth header creation without secret logging;
- model mapping;
- capability lookup;
- request serialization;
- thinking mapping;
- tool serialization;
- stream parsing;
- tool-call assembly;
- finish reasons;
- error mapping;
- cancellation.

---

# 67. Recorded Fixtures

Provider parsing tests may use sanitized response fixtures.

Never commit:

- live API keys;
- private prompts;
- sensitive repository contents.

---

# 68. Integration Tests

Optional live NVIDIA tests can be gated by:

```text
NVIDIA_API_KEY
```

They should not run by default in every developer test suite.

Example category:

```text
#[ignore]
```

or feature/environment gated.

---

# 69. Fake NVIDIA Responses

Most tests should not call NVIDIA.

Use local fixtures to test:

```text
text streaming
tool calls
reasoning metadata
429
401
5xx
malformed chunks
```

---

# 70. Logging

Useful structured fields:

```text
provider = "nvidia"
model
endpoint class
status
duration
request id
```

Never log:

```text
NVIDIA_API_KEY
Authorization
full secret-bearing request headers
```

---

# 71. Privacy

Tool results containing source code may be included in requests to NVIDIA when the Agent needs that code as context.

The public Gocode documentation must make this behavior understandable to users.

---

# 72. No Generic Network Tool

The NVIDIA provider has network access because inference requires it.

This does not grant the model itself arbitrary network access.

Agent network capabilities remain controlled by the Tools layer.

---

# 73. Failure During Streaming

If the connection fails after output has begun:

- stop the active Agent inference step;
- preserve already received UI text as incomplete;
- surface an error;
- do not blindly replay the request if that could duplicate actions/tool calls.

---

# 74. Tool Calls During Failed Streams

Never execute a tool call until Gocode considers its structured call complete.

This prevents partially streamed arguments from creating local side effects.

---

# 75. Thinking During Tool Use

A model may reason before or between tool calls.

Gocode should treat:

```text
thinking
tool requests
final text
```

as provider outputs that ultimately drive the same normalized Agent loop.

---

# 76. Default MVP Model Strategy

Do not permanently hardcode one model as "the NVIDIA model."

The onboarding should:

1. obtain current available models;
2. identify suitable coding-agent candidates using capabilities;
3. present a simple model picker;
4. remember the selected model.

A recommended default can be added when capability data is reliable.

---

# 77. Model Suitability

Possible internal ranking factors:

```text
tool calling required
streaming required
coding suitability preferred
reasoning preferred
context size useful
```

Avoid introducing a complex ranking engine in v0.1.0.

---

# 78. Chat-Only Models

A model without tool support can still be used for chat.

The TUI must say:

```text
This model cannot use Gocode project tools.
```

Do not attempt to simulate tool calling through fragile text parsing in the MVP.

---

# 79. Provider Health UX

Examples:

Invalid key:

```text
Your NVIDIA API key was rejected.

[ Update key ]
```

Network failure:

```text
Could not reach NVIDIA.

[ Retry ]
```

Rate limit:

```text
NVIDIA is temporarily rate limiting requests.

Try again shortly.
```

Exact wording belongs to the UI layer.

---

# 80. Documentation Sources

Implementation should be regularly checked against current official NVIDIA documentation, especially:

- NVIDIA NIM for LLMs API Reference;
- NVIDIA NIM Tool Calling and MCP Integration;
- NVIDIA API Catalog model-specific inference references;
- model-specific Nemotron documentation;
- NVIDIA NIM support matrices.

Model-specific API pages are important because reasoning and tool capabilities vary between models.

---

# 81. Known Architectural Facts to Preserve

As of the current documentation reviewed for this draft:

1. NIM LLM exposes an OpenAI-compatible inference interface.
2. `/v1/chat/completions` supports multi-turn chat, streaming, and tool calling at the API level.
3. NIM deployments expose `/v1/models`.
4. Tool calling is model/deployment dependent.
5. NVIDIA-hosted models do not share one universal reasoning configuration.
6. Documented reasoning effort value sets differ between models.
7. Some Nemotron models expose a separate reasoning budget.
8. The hosted NVIDIA API catalog uses `integrate.api.nvidia.com`.
9. Hosted inference uses bearer-token authentication.

These facts justify the capability-driven adapter design.

---

# 82. Definition of Done

The NVIDIA provider is ready for Gocode v0.1.0 when:

- onboarding validates an NVIDIA API key;
- the key is stored securely;
- available models can be loaded;
- a model can be selected and persisted;
- chat text streams into the Agent/TUI;
- tool definitions are sent correctly;
- streamed tool calls are assembled correctly;
- local tool results can be returned to the model;
- thinking settings map according to the selected model;
- unsupported thinking settings are rejected locally;
- API errors are normalized;
- cancellation works;
- no NVIDIA-specific logic leaks into Agent or TUI code.

---

# 83. Reference Flow

```text
User starts Gocode
↓
NVIDIA credential resolved
↓
NvidiaProvider initialized
↓
models loaded
↓
ModelCapabilities resolved
↓
user submits coding task
↓
ChatRequest
↓
NVIDIA request mapper
↓
POST /v1/chat/completions
↓
streaming response
↓
tool call
↓
Gocode executes local tool
↓
tool result appended
↓
next NVIDIA chat completion
↓
final answer
```

---

# 84. Final Rule

NVIDIA NIM is the first provider, not the architecture.

The NVIDIA adapter should take advantage of NIM's OpenAI-compatible API while preserving exact per-model differences in tools and reasoning.

> Gocode should expose a simple, stable model experience even when the NVIDIA model catalog contains many different capability shapes.
