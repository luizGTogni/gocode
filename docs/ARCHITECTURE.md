# Gocode — Architecture

**Status:** Initial technical draft  
**Product:** Gocode  
**Target version:** v0.1.0  
**Primary language:** Rust  
**Interface:** TUI  
**Initial platform:** Windows

---

# 1. Purpose of This Document

This document describes how Gocode should be structured internally.

`PRD.md` defines what the product needs to do and the experience requirements. This document defines modules, responsibilities, boundaries, data flows, agent architecture, providers, TUI, tools, configuration, sessions, updater, security, errors, and extensibility.

The architecture should allow v0.1.0 to ship quickly without creating rigid dependencies that make future providers or platforms difficult to add.

---

# 2. Architectural Principles

## 2.1 Simple Outside, Modular Inside

The user should run:

```text
gocode
```

and use the product without knowing its architecture.

## 2.2 Provider-Independent Core

The agent should not know whether it is using NVIDIA NIM, OpenAI, Anthropic, Gemini, Ollama, or another future provider.

All provider-specific logic belongs in provider adapters.

## 2.3 TUI Independent from the Agent

The TUI presents state, collects input, renders events, and sends intentions. It does not directly execute HTTP requests, tools, reasoning, or program updates.

## 2.4 Agent Independent from the Interface

The agent should operate through events and commands. This enables future headless mode, tests without a TUI, IDE integration, and a local API.

## 2.5 Capabilities Instead of Assumptions

Never assume every model supports tools, thinking, streaming, vision, reasoning effort, or a specific context window.

Behavior starts from `ModelCapabilities`.

## 2.6 Non-Essential Failures Must Not Block the Product

Examples:

- GitHub being unavailable must not block startup;
- invalid cache must not prevent provider operation;
- session-save failure must not crash the TUI;
- incomplete metadata must not prevent chat when operation is still possible.

## 2.7 Async by Default

Slow operations must not block the UI: model requests, tools, commands, updates, large reads, search, and persistence.

Tokio will be the central runtime.

---

# 3. Overview

```text
┌──────────────────────────────────────────────────────────────┐
│                         Gocode CLI                           │
│                       bootstrap/main                         │
└─────────────────────────────┬────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                         App Runtime                          │
│                                                              │
│  TUI          Agent          Project        Updater           │
│   │             │               │              │              │
│   └─────────────┴────── App Events ────────────┘              │
└─────────────────────────────┬────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                        Gocode Core                           │
│                                                              │
│ Config  Models  Sessions  Tools  Permissions  Conversation   │
└───────────────┬───────────────────────────────┬──────────────┘
                │                               │
                ▼                               ▼
┌──────────────────────────────┐   ┌───────────────────────────┐
│       Provider Layer         │   │       System Layer        │
│                              │   │                           │
│ Provider trait               │   │ Filesystem                │
│ Model registry               │   │ Process execution         │
│ NVIDIA adapter               │   │ Credentials               │
└──────────────┬───────────────┘   │ Git                       │
               │                   └───────────────────────────┘
               ▼
┌──────────────────────────────┐
│ External Model APIs          │
│ NVIDIA NIM                   │
└──────────────────────────────┘
```

---

# 4. Rust Workspace

Recommended initial structure:

```text
gocode/
├── Cargo.toml
├── README.md
├── PRD.md
├── SECURITY.md
├── CONTRIBUTING.md
│
├── docs/
│   ├── ARCHITECTURE.md
│   ├── AGENT.md
│   ├── NVIDIA_NIM.md
│   ├── TOOLS.md
│   ├── TUI.md
│   ├── CONFIG.md
│   ├── UPDATER.md
│   └── DECISIONS.md
│
└── crates/
    ├── gocode/
    ├── gocode-core/
    ├── gocode-tui/
    ├── gocode-provider/
    ├── gocode-provider-nvidia/
    └── gocode-updater/
```

---

# 5. Crate Dependencies

```text
gocode
├── gocode-core
├── gocode-tui
├── gocode-provider
├── gocode-provider-nvidia
└── gocode-updater

gocode-tui
└── gocode-core

gocode-provider-nvidia
└── gocode-provider

gocode-core
└── gocode-provider
```

Important rule:

```text
specific provider → generic abstraction
```

Avoid:

```text
gocode-core → concrete NVIDIA implementation
```

---

# 6. `gocode` Crate

Responsible for bootstrap.

Responsibilities:

- initial CLI parsing;
- directory resolution;
- Tokio runtime creation;
- logging initialization;
- dependency wiring;
- provider registry creation;
- TUI initialization;
- shutdown.

Conceptual example:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bootstrap().await
}
```

Avoid domain logic in `main.rs`.

---

# 7. `gocode-core` Crate

Contains most logic that is independent from UI and concrete providers.

```text
gocode-core/src/
├── agent/
├── config/
├── conversation/
├── project/
├── tools/
├── permissions/
├── sessions/
├── models/
├── events/
├── errors/
└── lib.rs
```

---

# 8. `gocode-tui` Crate

Responsible for the terminal experience.

```text
gocode-tui/src/
├── app.rs
├── event_loop.rs
├── state/
├── screens/
├── widgets/
├── input/
├── modals/
└── lib.rs
```

Responsibilities:

- rendering;
- layout;
- input;
- keyboard shortcuts;
- modals;
- selection state;
- scrolling;
- streaming visualization;
- tool activity;
- onboarding;
- model picker;
- provider picker;
- update prompt;
- permission prompt.

---

# 9. `gocode-provider` Crate

Defines shared contracts between providers.

```text
gocode-provider/src/
├── provider.rs
├── model.rs
├── request.rs
├── response.rs
├── stream.rs
├── capabilities.rs
├── errors.rs
└── lib.rs
```

No NVIDIA-specific detail should exist here.

---

# 10. `gocode-provider-nvidia` Crate

Responsible for NVIDIA NIM.

```text
gocode-provider-nvidia/src/
├── client.rs
├── auth.rs
├── models.rs
├── chat.rs
├── streaming.rs
├── capabilities.rs
├── thinking.rs
├── tools.rs
├── error_mapping.rs
└── lib.rs
```

---

# 11. `gocode-updater` Crate

Responsible for update discovery and application.

```text
gocode-updater/src/
├── github.rs
├── version.rs
├── download.rs
├── checksum.rs
├── windows.rs
└── lib.rs
```

It may produce the second binary:

```text
gocode-updater.exe
```

---

# 12. Main Runtime

The runtime coordinates subsystems through commands and events.

```text
TUI input
   ↓
AppCommand
   ↓
Runtime / subsystem
   ↓
AppEvent
   ↓
TUI state
   ↓
render
```

---

# 13. `AppCommand`

Represents intentions sent by the UI.

```rust
enum AppCommand {
    SubmitPrompt(String),
    CancelAgent,

    SelectProvider(ProviderId),
    SelectModel(ModelId),

    ConfirmTool(ToolCallId),
    RejectTool(ToolCallId),

    AcceptUpdate,
    RejectUpdate,

    ClearConversation,
    Exit,
}
```

---

# 14. `AppEvent`

Represents changes that occurred in the system.

```rust
enum AppEvent {
    BootStarted,
    BootCompleted,

    ProviderConnected(ProviderId),
    ProviderError(AppError),

    AgentStarted,
    AgentTextDelta(String),
    AgentThinkingState(ThinkingUiState),
    AgentFinished,

    ToolRequested(ToolCall),
    ToolStarted(ToolCallId),
    ToolOutput {
        id: ToolCallId,
        chunk: String,
    },
    ToolFinished(ToolResult),

    ModelChanged(ModelId),

    UpdateAvailable(UpdateInfo),
    UpdateProgress(UpdateProgress),

    Error(AppError),
}
```

---

# 15. Channels

Initial suggestion:

```rust
tokio::sync::mpsc
```

```text
TUI
 │
 ├── command_tx ───────────► Runtime
 │
 ◄──────────── event_rx ─────
```

Avoid creating multiple event buses before there is a real need.

---

# 16. TUI State

```rust
struct AppState {
    screen: Screen,
    conversation: ConversationView,
    composer: ComposerState,
    agent: AgentViewState,
    provider: ProviderViewState,
    model: ModelViewState,
    modal: Option<ModalState>,
    notifications: Vec<Notification>,
}
```

The TUI should not be the sole source of truth for critical domain data.

---

# 17. Screens and Modals

```rust
enum Screen {
    Boot,
    Onboarding,
    Chat,
    ModelPicker,
    ProviderPicker,
    Config,
}
```

```rust
enum ModalState {
    Update(UpdateInfo),
    ToolPermission(PermissionRequest),
    Error(ErrorView),
    ConfirmExit,
}
```

---

# 18. Provider Trait

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    async fn validate_credentials(
        &self,
    ) -> Result<CredentialStatus, ProviderError>;

    async fn list_models(
        &self,
    ) -> Result<Vec<Model>, ProviderError>;

    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Result<ChatStream, ProviderError>;
}
```

---

# 19. Provider Registry

```rust
struct ProviderRegistry {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
}
```

The MVP will only include NVIDIA, but the core must not assume that.

---

# 20. Model

```rust
struct Model {
    id: ModelId,
    provider: ProviderId,
    display_name: String,
    capabilities: ModelCapabilities,
}
```

---

# 21. Model Capabilities

```rust
struct ModelCapabilities {
    streaming: bool,
    tools: ToolCapability,
    thinking: ThinkingCapability,
    vision: VisionCapability,
    context: ContextCapability,
    sampling: SamplingCapability,
}
```

---

# 22. Tool Capability

```rust
enum ToolCapability {
    Unsupported,
    Supported {
        parallel_calls: Option<bool>,
    },
}
```

---

# 23. Thinking Capability

Thinking/reasoning should be modeled as its own capability.

```rust
enum ThinkingCapability {
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

Never globally hardcode only `Low`, `Medium`, and `High`.

---

# 24. Thinking Settings

```rust
struct ThinkingSettings {
    mode: ThinkingMode,
    display: ThinkingDisplay,
}
```

```rust
enum ThinkingMode {
    Auto,
    Off,
    On,
    Effort(String),
    Budget(u32),
}
```

`Auto` should be the preferred default.

---

# 25. Thinking Display

```rust
enum ThinkingDisplay {
    Hidden,
    Summary,
}
```

Using thinking does not imply exposing literal internal reasoning.

The UI may show summarized activity:

```text
● Thinking
● Reading src/auth.rs
● Checking token validation
● Editing src/auth.rs
```

---

# 26. Model Registry

Responsibilities:

- store normalized models;
- associate capabilities;
- update cache;
- select defaults;
- resolve a saved model;
- detect unavailable models.

```text
Provider metadata
↓
Provider mapper
↓
Model
↓
ModelRegistry
↓
Agent/TUI
```

---

# 27. Capability Resolution

Not every capability must come from an endpoint.

```text
API metadata
+
provider-specific known metadata
+
local cache
=
resolved capabilities
```

Model-ID-specific checks must stay centralized in the adapter and never be scattered throughout the core.

---

# 28. Agent

The agent is a state machine responsible for carrying out a task.

It receives:

- conversation;
- project instructions;
- model;
- capabilities;
- available tools;
- permissions;
- user prompt.

---

# 29. Agent State

```rust
enum AgentState {
    Idle,
    Preparing,
    Inference,
    WaitingForTool,
    ExecutingTools,
    Finalizing,
    Cancelled,
    Failed,
}
```

---

# 30. Agent Loop

```text
User prompt
↓
Build context
↓
Inference
↓
Final text?
 ├─ yes → Finalize
 └─ no
      ↓
   Tool calls
      ↓
   Permission check
      ↓
   Execute tools
      ↓
   Add tool results
      ↓
   Inference
      ↓
     ...
```

---

# 31. Agent Limits

```rust
struct AgentLimits {
    max_turns: usize,
    max_tool_calls_per_turn: usize,
    max_total_tool_calls: usize,
}
```

These limits prevent unintended loops.

---

# 32. Cancellation

Each execution must have a cancellation token.

Suggested:

```rust
tokio_util::sync::CancellationToken
```

Flow:

```text
Esc
↓
CancelAgent
↓
CancellationToken.cancel()
↓
provider/tool/process attempts to stop
↓
AgentState::Cancelled
```

---

# 33. Conversation

Provider-independent representation:

```rust
struct Conversation {
    messages: Vec<Message>,
}
```

```rust
enum Message {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolMessage),
}
```

Adapters convert this into each API's format.

---

# 34. Context Builder

Input:

```text
system instructions
project instructions
conversation
tool definitions
model settings
```

Output:

```text
ChatRequest
```

Do not automatically load every project file.

---

# 35. Tool System

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> Result<ToolResult, ToolError>;
}
```

---

# 36. Tool Registry

```rust
struct ToolRegistry {
    tools: HashMap<ToolName, Arc<dyn Tool>>,
}
```

The agent uses the registry to generate schemas, validate calls, and execute tools.

---

# 37. MVP Tools

```text
list_files
read_file
search
write_file
apply_patch
run_command
git_status
```

Possible simple addition:

```text
git_diff
```

---

# 38. Tool Context

```rust
struct ToolContext {
    project_root: PathBuf,
    cancellation: CancellationToken,
    permissions: PermissionContext,
}
```

Do not provide unrestricted operating-system access.

---

# 39. Filesystem Boundary

By default, operations are restricted to the project root.

```text
tool path
↓
normalize
↓
canonicalize when possible
↓
verify inside workspace
↓
execute
```

Protect against `../../` and symlink escapes where relevant.

---

# 40. Search Engine

Characteristics:

- respects `.gitignore`;
- ignores `.git`;
- avoids `.gocode` when not relevant;
- ignores binaries;
- limits results;
- does not index everything at startup.

---

# 41. File Reading

```rust
struct ReadLimits {
    max_bytes: usize,
    max_lines: usize,
}
```

Large files should support ranged reads and explicit truncation reporting.

---

# 42. File Editing

Prefer:

```text
read
↓
apply_patch
↓
validate
```

`write_file` is appropriate for creation or simple replacement.

---

# 43. Change Tracking

```rust
struct FileChange {
    path: PathBuf,
    kind: ChangeKind,
    before_hash: Option<String>,
    after_hash: Option<String>,
}
```

This enables changed-file display, session persistence, and future diff/rollback support.

---

# 44. Process Execution

```rust
struct CommandRequest {
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
}
```

Prefer program + arguments over shell strings when possible.

---

# 45. Command Streaming

```text
process
├── stdout → ToolOutput event
└── stderr → ToolOutput event
```

The UI should show progress in real time.

---

# 46. Command Cancellation

When cancelling:

1. signal the process;
2. attempt graceful termination;
3. force termination if necessary;
4. collect status;
5. mark the tool as cancelled.

Windows requires specific implementation and testing.

---

# 47. Permissions

```text
Tool call
↓
Permission engine
↓
Allow / Ask / Deny
```

```rust
enum PermissionDecision {
    Allow,
    Ask(PermissionRequest),
    Deny(PermissionReason),
}
```

---

# 48. Initial Policy

```text
read_file       → allow
list_files      → allow
search          → allow
git_status      → allow
git_diff        → allow
apply_patch     → allow within the task
write_file      → allow within the task
run_command     → evaluate risk
```

Potentially destructive commands require confirmation.

---

# 49. Command Risk

```rust
enum CommandRisk {
    Low,
    Medium,
    High,
}
```

v0.1.0 may use simple heuristics. Do not try to build a perfect sandbox in the MVP.

---

# 50. Project Service

Responsible for:

- detecting project root;
- creating `.gocode`;
- reading instructions;
- detecting Git;
- exposing workspace paths and metadata.

---

# 51. Project Root Detection

Suggested priority:

```text
.git
Cargo.toml
package.json
pyproject.toml
go.mod
other manifests
cwd
```

---

# 52. Directories

Global on Windows:

```text
%USERPROFILE%\.gocode\
```

Local:

```text
<project-root>/.gocode/
```

All resolution should go through centralized functions to make future Linux/macOS support easier.

---

# 53. Config Architecture

Separate:

```text
GlobalConfig
ProjectConfig
ResolvedConfig
```

Precedence:

```text
CLI
↓
Project
↓
Global
↓
Provider defaults
↓
Built-in defaults
```

---

# 54. Resolved Config

```rust
struct ResolvedConfig {
    provider: ProviderId,
    model: Option<ModelId>,
    thinking: ThinkingSettings,
    ui: UiConfig,
    updates: UpdateConfig,
    agent: AgentConfig,
}
```

---

# 55. Secrets

Secrets should not enter config as ordinary `String` values.

Use a type such as:

```rust
SecretString
```

and a separate credential service.

---

# 56. Credential Store

```rust
trait CredentialStore {
    async fn get(&self, key: CredentialKey) -> Result<Option<SecretString>>;
    async fn set(&self, key: CredentialKey, value: SecretString) -> Result<()>;
    async fn delete(&self, key: CredentialKey) -> Result<()>;
}
```

MVP:

```text
Windows Credential Manager
```

Resolution:

```text
environment variable
↓
OS credential store
↓
onboarding
```

---

# 57. Sessions

Minimal persistence:

```text
.gocode/sessions/<uuid>.jsonl
```

May store:

- messages;
- tool calls;
- tool results;
- file changes;
- timestamps;
- provider;
- model;
- relevant thinking config.

JSONL is suitable for the MVP because it is append-only and tolerant of partial failures.

---

# 58. Updater Architecture

Separate:

```text
UpdateChecker
UpdateInstaller
```

`UpdateChecker` runs without blocking the TUI.

---

# 59. Update Flow

```text
Startup
↓
TUI available
↓
check GitHub Releases
↓
new version?
├── no → end
└── yes
     ↓
UpdateAvailable
     ↓
modal
```

If the user chooses `Not now`, do not persist `ignore_version`.

---

# 60. Windows Self Update

```text
gocode.exe
↓
download temp
↓
verify checksum
↓
launch gocode-updater.exe
↓
gocode.exe exits
↓
updater replaces binary
↓
updater starts gocode.exe
↓
updater exits
```

Preserve the previous binary until the update completes.

---

# 61. Error Architecture

Main types:

```text
ProviderError
ToolError
ConfigError
ProjectError
CredentialError
UpdateError
SessionError
AppError
```

Mapping:

```text
reqwest::Error
↓
ProviderError::Network
↓
AppError::ProviderUnavailable
↓
"Could not reach NVIDIA."
```

Technical details remain in logs.

---

# 62. Logging

Use `tracing`.

Directory:

```text
~/.gocode/logs/
```

Never log:

- API keys;
- Authorization headers;
- secrets;
- sensitive cookies.

---

# 63. Network Layer

The HTTP client should configure:

- TLS;
- timeout;
- connection pooling;
- user agent;
- limited retries when safe.

Do not blindly retry operations that may duplicate effects.

---

# 64. Normalized Streaming

```rust
enum ChatStreamEvent {
    TextDelta(String),
    ToolCallDelta(ToolCallDelta),
    ThinkingState(ThinkingState),
    Usage(Usage),
    Finished(FinishReason),
}
```

The provider adapter converts the external protocol into these events.

---

# 65. Tool Call Assembly

If the API streams tool calls in chunks:

```text
chunks
↓
provider parser
↓
complete ToolCall
↓
agent
```

The core should not know NVIDIA streaming details.

---

# 66. Usage

```rust
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
}
```

Fields are optional because providers differ.

---

# 67. NVIDIA Adapter

```text
ChatRequest
↓
NvidiaRequestMapper
↓
NVIDIA HTTP request
↓
stream
↓
NvidiaStreamParser
↓
ChatStreamEvent
```

Thinking settings are translated exclusively here.

---

# 68. Startup Sequence

```text
1. parse CLI
2. resolve global paths
3. init logging
4. detect project root
5. ensure global .gocode
6. ensure local .gocode
7. load configs
8. resolve credential availability
9. construct providers
10. construct registries/services
11. enter terminal mode
12. render TUI
13. start update check async
14. initialize provider/models async
```

The UI should appear early.

---

# 69. Onboarding Sequence

```text
TUI
↓
Onboarding
↓
Provider selection
↓
API key
↓
validate
↓
save credential
↓
fetch models
↓
select model
↓
save config
↓
Chat
```

---

# 70. Cache

```text
~/.gocode/cache/
```

May store:

- model metadata;
- release metadata;
- provider discovery.

Cache must not be the sole source of truth.

---

# 71. Graceful Shutdown

On exit:

1. cancel agent;
2. cancel tools;
3. flush session writer;
4. flush logs;
5. restore terminal;
6. exit.

---

# 72. Terminal Lifecycle

Encapsulate:

```text
enable raw mode
enter alternate screen
hide cursor
...
show cursor
leave alternate screen
disable raw mode
```

Prefer an RAII guard.

Also install a panic hook to attempt terminal restoration.

---

# 73. Windows Considerations

Test specifically:

- Unicode paths;
- path separators;
- PowerShell;
- cmd;
- Windows Terminal;
- resize;
- process termination;
- Credential Manager;
- updater;
- PATH;
- CRLF.

---

# 74. Test Architecture

Categories:

```text
unit
integration
provider contract
tool
filesystem
updater
TUI state
```

---

# 75. Fake Provider

Create a deterministic provider for tests:

```rust
struct FakeProvider;
```

It should be able to simulate:

- streaming;
- tool calls;
- errors;
- thinking states;
- cancellation;
- final responses.

This allows Agent testing without depending on NVIDIA.

---

# 76. Provider Contract Tests

Every future provider should pass a common suite:

- credential validation;
- list models;
- text streaming;
- tool calls;
- completion;
- cancellation;
- malformed responses;
- errors.

---

# 77. Dependency Direction

```text
UI → Core abstractions
Core → Provider abstractions
Provider implementation → Provider abstractions
System adapters → Core abstractions
```

Avoid circular dependencies.

---

# 78. Mutable State

Prefer:

```text
clear ownership
Arc for shared services
channels for events
locks only when needed
```

Avoid using something like:

```text
Arc<Mutex<AppEverything>>
```

as the central architecture.

---

# 79. Agent Concurrency

By default:

```text
1 session = 1 active agent task
```

Do not implement subagents in the MVP.

Parallel tool calls may be added later when provider/model/tools allow it.

---

# 80. Backpressure

Model streaming and command output may generate many events.

Channels should have appropriate bounds, and the TUI may aggregate small deltas before rendering.

Avoid thousands of renders per second.

---

# 81. Performance

Avoid:

- indexing the entire project at startup;
- loading all files into memory;
- cloning the entire conversation repeatedly;
- rendering history outside the viewport;
- listing models on every prompt;
- heavy filesystem work on the main async task.

---

# 82. Large Projects

For large projects:

- respect ignore rules;
- limit results;
- use ranged reads;
- use lazy filesystem traversal;
- no mandatory full indexing in the MVP.

---

# 83. Workspace Security

The workspace is a boundary.

Default:

```text
read/write/run cwd inside project root
```

Future exceptions must be explicit.

---

# 84. Network Permissions

In the MVP, agent tools do not receive a generic internet tool.

Network is used by:

- NVIDIA provider;
- GitHub updater.

This reduces attack surface.

---

# 85. Shell Environment

`run_command` may inherit the necessary user environment, but Gocode must not inject the NVIDIA API key into subprocesses by default.

Secrets remain isolated inside the Gocode process.

---

# 86. Configuration Migration

Prepare configs for schema versioning:

```toml
schema_version = 1
```

Future migrations should be automatic and idempotent.

---

# 87. Versioning

The product uses SemVer:

```text
0.1.0
0.2.0
1.0.0
```

Internal schemas may have separate versioning.

---

# 88. Build and Release

Windows artifacts:

```text
gocode.exe
gocode-updater.exe
```

Pipeline:

```text
Git tag
↓
GitHub Actions
↓
build Windows
↓
tests
↓
checksums
↓
GitHub Release
↓
assets
```

---

# 89. Observability

MVP:

- local logs;
- tracing;
- debug mode.

No mandatory remote telemetry should be assumed by the architecture.

---

# 90. Abstractions That Should Not Be Prematurely Added

Do not create now:

- full plugin engine;
- complex MCP framework (v0.4.0 added a minimal MCP client — `gocode-mcp`: JSON-RPC framing,
  stdio/streamable-HTTP transports, OAuth — reusing the existing `Tool` trait rather than a
  generic plugin engine; keep it that way);
- distributed agents;
- database abstraction;
- full event sourcing;
- multi-workspace orchestration;
- heavy dependency injection framework;
- custom async runtime.

Add them only when there is a real need.

---

# 91. Core Contracts

The main architectural contracts are:

```text
Provider
Tool
CredentialStore
SessionStore
UpdateSource
ProcessRunner
```

Not everything needs to become a trait immediately. Create traits when there is a clear external boundary, multiple implementations, or meaningful testability benefit.

---

# 92. Initial Vertical Slice

First end-to-end flow:

```text
gocode
↓
TUI opens
↓
NVIDIA credential
↓
select model
↓
streaming text
↓
read_file tool
↓
tool result
↓
model finalizes
```

Then:

```text
search
↓
apply_patch
↓
run_command
↓
full agent loop
```

---

# 93. Recommended Implementation Order

## Phase 1 — Foundation

```text
Rust workspace
CLI
terminal guard
basic TUI
event loop
```

## Phase 2 — Project/Config

```text
global .gocode
local .gocode
project root
config precedence
```

## Phase 3 — Provider

```text
Provider trait
NVIDIA client
credential store
models
streaming
```

## Phase 4 — Basic Agent

```text
conversation
context builder
read_file
search
tool loop
```

## Phase 5 — Coding Agent

```text
apply_patch
run_command
permissions
cancellation
```

## Phase 6 — Capabilities

```text
model registry
thinking
model picker
capability resolution
```

## Phase 7 — Product

```text
sessions
errors
updater
hardening
```

---

# 94. Target v0.1.0 Architecture

```text
                         ┌────────────────┐
                         │    gocode      │
                         │   bootstrap    │
                         └───────┬────────┘
                                 │
             ┌───────────────────┼───────────────────┐
             │                   │                   │
             ▼                   ▼                   ▼
      ┌─────────────┐     ┌──────────────┐    ┌──────────────┐
      │ gocode-tui  │     │ gocode-core  │    │   updater    │
      └──────┬──────┘     └──────┬───────┘    └──────────────┘
             │                   │
             │             ┌─────┼───────────────┐
             │             │     │               │
             │             ▼     ▼               ▼
             │          Agent   Tools          Project
             │             │     │               │
             │             │     ▼               ▼
             │             │ Filesystem         Config
             │             │ Processes          Sessions
             │             │ Git
             │             │
             │             ▼
             │      gocode-provider
             │             │
             │             ▼
             │   gocode-provider-nvidia
             │             │
             │             ▼
             │         NVIDIA NIM
             │
             └──────────── AppEvent/AppCommand
```

---

# 95. Architectural Definition of Done

The v0.1.0 architecture is sufficiently mature when:

- the TUI does not directly depend on NVIDIA;
- the agent does not depend on Ratatui;
- tools are independently testable;
- model capabilities are normalized;
- thinking is encapsulated by the provider;
- global/local config is separated;
- credentials are not stored in plaintext;
- update checking does not block startup;
- agent/processes can be cancelled;
- workspace boundary is enforced;
- a fake provider can test the agent;
- `gocode.exe` can be updated by the separate updater.

---

# 96. Final Rule

The architecture should serve the product, not the other way around.

If there is a conflict between theoretically perfect architecture and a simple experience with sufficiently modular code, prefer the latter.

> The user should think about the code they want to build, not about how the coding agent works internally.
