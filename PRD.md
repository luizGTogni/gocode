# Gocode — Product Requirements Document (PRD)

**Status:** Draft for MVP kickoff  
**Product:** Gocode  
**Target version:** v0.1.0  
**Primary language:** Rust  
**Interface:** TUI  
**Initial platform:** Windows  
**Global command:** `gocode`

---

# 1. Product Vision

Gocode is a terminal-based coding agent inspired by tools such as OpenCode, Claude Code, and Codex.

The core idea is to let a user open a terminal inside a project, run:

```powershell
gocode
```

and immediately gain access to an agent capable of:

- understanding the project;
- reading files;
- searching code;
- editing files;
- applying patches;
- running commands;
- analyzing results;
- iterating when needed;
- using AI models with tool and thinking/reasoning support;
- keeping both global and project-specific configuration;
- updating itself when a new version is published.

The product must prioritize ease of use above almost every other UX consideration.

> Anything Gocode can discover, configure, or resolve automatically should not be required from the user.

---

# 2. MVP Goal

Version v0.1.0 must allow a Windows user to:

1. install Gocode globally;
2. run `gocode` from any terminal;
3. configure NVIDIA NIM through a visual onboarding flow;
4. enter an API key without manually editing files;
5. select a model;
6. chat with the model using streaming;
7. allow the agent to read, search, edit, and execute code;
8. use tool calling;
9. use thinking/reasoning capabilities when available;
10. keep global and project-specific settings;
11. receive a notification when a new version is available;
12. accept an automatic Gocode update.

The short definition of the MVP is:

> Open a terminal inside a project, run `gocode`, say what you want to change, and Gocode understands the code, edits the files, and runs the necessary commands using NVIDIA NIM.

---

# 3. Product Principles

## 3.1 Ease of Use Above Everything

Ease of use is a non-negotiable requirement.

The user should not need to:

- edit TOML files to get started;
- understand endpoints;
- configure HTTP headers;
- know how tool calling works;
- know what `reasoning_effort` means;
- know what `thinking_budget` means;
- configure paths manually;
- create `.gocode` manually;
- understand the internal provider architecture.

Advanced configuration may exist, but it must never be required for the primary workflow.

---

## 3.2 Smart Defaults

Gocode should choose good defaults automatically.

| Situation | Expected behavior |
|---|---|
| Local `.gocode` does not exist | Create it automatically |
| Global config does not exist | Create it automatically |
| Provider is not configured | Open onboarding |
| API key is invalid | Explain clearly and allow correction |
| Project is a Git repository | Detect automatically |
| `.gitignore` exists | Respect it |
| Terminal size changes | Adapt |
| Streaming is available | Use it |
| Tools are available | Enable them |
| Thinking is available | Configure it automatically |
| Configured model no longer exists | Ask the user to choose another |
| GitHub is unavailable | Continue without blocking startup |

---

# 4. Platform

## 4.1 MVP

Initial supported platform:

- Windows 10/11

Priority terminals:

- Windows Terminal
- PowerShell
- cmd

---

## 4.2 Future

After stabilization:

- Linux
- macOS

The architecture must not block future cross-platform support.

---

# 5. Technical Stack

| Responsibility | Technology |
|---|---|
| Language | Rust |
| Async runtime | Tokio |
| TUI | Ratatui |
| Terminal backend | Crossterm |
| HTTP | Reqwest |
| Serialization | Serde / serde_json |
| Config | TOML |
| SemVer | semver |
| Async streams | futures |
| Errors | thiserror / anyhow |
| Logging | tracing |
| File scanning | ignore / walkdir |
| Git ignore | ignore |
| Diff | similar or equivalent |
| Update source | GitHub Releases |
| Secret storage | Windows Credential Manager / compatible credential crate |

---

# 6. Installation

## 6.1 Requirement

After installation, this command must work globally:

```powershell
gocode
```

---

## 6.2 Global Directory

Initially:

```text
%USERPROFILE%\.gocode\
```

Structure:

```text
.gocode\
├── config.toml
├── state.json
├── models.json
├── logs\
├── cache\
└── bin\
    ├── gocode.exe
    └── gocode-updater.exe
```

The installer must add:

```text
%USERPROFILE%\.gocode\bin
```

to the user's PATH.

---

## 6.3 Initial Installer

The MVP may use a simple PowerShell installer.

Future options:

```text
winget install Gocode
scoop install gocode
cargo install gocode
```

These methods are not required for v0.1.0.

---

# 7. Local Project Structure

When `gocode` runs inside a project, Gocode should determine the project root and create:

```text
<project-root>\.gocode\
```

Initial structure:

```text
.gocode\
├── project.toml
├── instructions.md
└── sessions\
```

---

# 8. Project Root Detection

Suggested priority:

1. `.git`
2. `Cargo.toml`
3. `package.json`
4. `pyproject.toml`
5. `go.mod`
6. other supported manifests
7. current working directory

The user should not need to manually specify the project root in normal cases.

---

# 9. Global Configuration

File:

```text
~/.gocode/config.toml
```

Conceptual example:

```toml
default_provider = "nvidia"
default_model = "..."

[ui]
theme = "system"
show_thinking_summary = true

[updates]
check_on_startup = true
```

---

# 10. Local Configuration

File:

```text
<project>/.gocode/project.toml
```

Example:

```toml
[project]
name = "my-project"

[agent]
instructions = "instructions.md"
```

Local configuration should override global configuration where applicable.

---

# 11. Project Instructions

File:

```text
.gocode/instructions.md
```

Example:

```md
# Project instructions

- Use TypeScript.
- Don't modify generated files.
- Run `pnpm test` after changes.
- Follow the existing repository architecture.
```

This content should automatically become part of the agent context.

---

# 12. Providers

The architecture must be provider-agnostic.

The Gocode core must not know NVIDIA-specific details.

Conceptual interface:

```rust
trait Provider {
    async fn models(&self) -> Result<Vec<Model>>;

    async fn stream_chat(
        &self,
        request: ChatRequest
    ) -> Result<ChatStream>;

    async fn validate_credentials(&self) -> Result<()>;
}
```

Future providers:

```text
Provider
├── NvidiaProvider
├── OpenAIProvider
├── AnthropicProvider
├── GeminiProvider
├── OpenRouterProvider
└── OllamaProvider
```

Only NVIDIA is required for v0.1.0.

---

# 13. NVIDIA NIM

NVIDIA NIM will be the main provider for the MVP and initial development.

Goals:

- API key authentication;
- chat completions;
- streaming;
- tool calling;
- model discovery/listing;
- model capabilities;
- thinking/reasoning;
- consistent error handling.

The provider implementation must completely hide NVIDIA-specific API details from the rest of Gocode.

---

# 14. API Keys

The API key must not be stored as plain text inside `config.toml`.

Suggested resolution priority:

1. environment variable;
2. operating system credential store;
3. onboarding.

Example variable:

```text
NVIDIA_API_KEY
```

For a normal user, the ideal flow is to paste the key once during onboarding.

---

# 15. Onboarding

First launch:

```text
Welcome to Gocode

Connect an AI provider

> NVIDIA NIM
```

Then:

```text
NVIDIA API Key

Paste your API key:

> nvapi-••••••••••••••••

Checking...

✓ Connected
```

Then:

```text
Select model

> Model A
  Model B
  Model C
```

Then:

```text
✓ Ready

What do you want to build?
>
```

Onboarding should avoid unnecessary screens and decisions.

---

# 16. Model Registry

Gocode must not maintain only a static list of model names.

It should have a `ModelRegistry` that normalizes capabilities.

Flow:

```text
Provider API
    ↓
Provider adapter
    ↓
ModelRegistry
    ↓
ModelCapabilities
    ↓
Agent / TUI
```

---

# 17. Model Capabilities

Conceptual structure:

```rust
struct ModelCapabilities {
    tools: ToolCapabilities,
    thinking: ThinkingCapability,
    vision: VisionCapability,
    streaming: bool,
    context_window: Option<u64>,
}
```

Capabilities must be discovered or defined per model.

Never assume all models from the same provider support the same features.

---

# 18. Thinking / Reasoning

Thinking/reasoning is a first-class feature in Gocode.

The system must not reduce this to:

```rust
reasoning: bool
```

Different models may expose:

- thinking enabled/disabled;
- reasoning effort;
- thinking effort;
- reasoning budget;
- thinking budget;
- different effort levels;
- model-specific parameters.

Conceptual structure:

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
    },
}
```

Important:

Do not globally hardcode:

```rust
enum ReasoningEffort {
    Low,
    Medium,
    High,
}
```

because supported levels may vary between models.

---

# 19. Thinking UX

The UI should abstract technical differences whenever possible.

Example:

```text
Thinking mode

> Auto
  Off
  Low
  Medium
  High
```

`Auto` should be the preferred default.

The provider adapter translates:

```text
Gocode setting
    ↓
Model capabilities
    ↓
Provider-specific parameters
```

---

# 20. Thinking vs Display

Using thinking does not mean exposing a model's internal reasoning.

Separate:

```rust
struct ThinkingSettings {
    mode: ThinkingMode,
    display: ThinkingDisplay,
}
```

Example:

```rust
enum ThinkingDisplay {
    Hidden,
    Summary,
}
```

The UI may show high-level activity:

```text
● Thinking
● Reading src/auth.rs
● Checking token validation
● Editing src/auth.rs
```

The goal is to make the agent's activity understandable without polluting the TUI.

---

# 21. Inference Capabilities

Instead of one `ReasoningConfig`, use a broader structure:

```text
InferenceCapabilities
├── ThinkingCapability
├── ToolCapability
├── SamplingCapability
├── ContextCapability
└── OutputCapability
```

This makes future providers easier to support.

---

# 22. Main UI

The MVP UI should be minimal.

Conceptual layout:

```text
┌───────────────────────────────────────────────────────────────┐
│ Gocode                                      NVIDIA • Model X  │
├───────────────────────────────────────────────────────────────┤
│                                                               │
│ You                                                           │
│ Fix the authentication bug                                    │
│                                                               │
│ Gocode                                                        │
│ I'll inspect the authentication flow.                         │
│                                                               │
│ ● Reading src/auth.rs                                         │
│ ● Searching "validate_token"                                  │
│ ● Editing src/auth.rs                                         │
│ ● Running cargo test                                          │
│                                                               │
│ ✓ 18 tests passed                                             │
│                                                               │
│ The issue was...                                              │
│                                                               │
├───────────────────────────────────────────────────────────────┤
│ > Ask Gocode...                                               │
├───────────────────────────────────────────────────────────────┤
│ Enter send • Esc stop • / commands                           │
└───────────────────────────────────────────────────────────────┘
```

Principle:

> Chat + agent actions + input.

Avoid unnecessary extra panels.

---

# 23. Slash Commands

MVP:

```text
/model
/provider
/config
/clear
/help
/exit
```

Possible future commands:

```text
/review
/compact
/context
```

`/init` is probably unnecessary because project initialization is automatic.

---

# 24. TUI Event Architecture

The TUI must not block while the model responds or tools execute.

Flow:

```text
Keyboard events ─────┐
LLM stream ──────────┤
Tool events ─────────┼──> AppEvent ──> State ──> Render
Updater events ──────┤
System events ───────┘
```

Conceptual structure:

```rust
enum AppEvent {
    Key(KeyEvent),

    AgentStarted,
    AgentToken(String),
    AgentFinished,

    ToolStarted(ToolCall),
    ToolFinished(ToolResult),

    UpdateAvailable(Version),

    Error(AppError),
}
```

---

# 25. Agent Loop

Conceptual state machine:

```text
Idle
 ↓
UserMessage
 ↓
Inference
 ↓
Text response? ──────→ Finished
 ↓
Tool calls
 ↓
Execute
 ↓
Tool results
 ↓
Inference
 ↓
...
```

There must be loop protection.

Example:

```text
max_agent_turns
```

The user must also be able to cancel with `Esc`.

---

# 26. MVP Tools

Initial set:

1. `list_files`
2. `read_file`
3. `search`
4. `write_file`
5. `apply_patch`
6. `run_command`
7. `git_status`

`git_diff` may also be included in the MVP if implementation is straightforward.

---

# 27. File Reading

Requirements:

- respect `.gitignore`;
- avoid binary files;
- enforce size limits;
- normalize paths;
- prevent reads outside the workspace by default;
- handle encoding cleanly.

---

# 28. Search

Search should work across the entire project.

Ideally support:

- text;
- filename;
- path;
- extension.

The agent should prefer searching before loading large amounts of files.

---

# 29. Editing

Prefer structured patch-based editing.

Ideal flow:

```text
read
↓
reason
↓
apply_patch
↓
validate
```

Changes should be recorded in the session so the TUI can tell the user which files changed.

---

# 30. Command Execution

The `run_command` tool executes processes inside the project.

Requirements:

- controlled cwd;
- stdout/stderr streaming;
- cancellation;
- configurable timeout;
- confirmation for dangerous actions;
- preservation of relevant logs.

---

# 31. Permissions

Avoid excessive permission prompts.

Suggested categories:

| Operation | Policy |
|---|---|
| read files | automatic |
| list files | automatic |
| search | automatic |
| git status/diff | automatic |
| edit files | allowed during the task |
| normal command | allowed according to policy |
| potentially destructive command | mandatory confirmation |

Example:

```text
Gocode wants to run:

  npm install

Working directory:
  C:\dev\my-app

[ Run ]     [ Cancel ]
```

The message must be simple and understandable.

---

# 32. Context Management

Avoid complex RAG in the MVP.

Primary context:

```text
system prompt
+
project instructions
+
conversation
+
tool results
```

The model should use tools to explore the project as needed.

Preferred flow:

```text
search
read
reason
edit
test
```

not:

```text
load the entire project into the prompt
```

---

# 33. Sessions

`.gocode/sessions/` should store minimal session data.

Possible initial scope:

- messages;
- tool calls;
- modified files;
- timestamps;
- provider/model;
- relevant thinking settings.

The format can start simple, for example JSON or JSONL.

Advanced persistence is not a priority for v0.1.0.

---

# 34. Normal Startup

When running:

```text
gocode
```

internal flow:

1. start TUI;
2. detect project root;
3. ensure global `.gocode`;
4. ensure local `.gocode`;
5. load global config;
6. load local config;
7. resolve credentials;
8. start provider;
9. start update check in the background;
10. load model registry;
11. enter chat.

The update check must never block TUI startup.

---

# 35. Auto Update

Gocode should check GitHub Releases on startup.

Example:

```text
installed version = 0.1.0
latest release = 0.2.0
```

Show:

```text
Gocode 0.2.0 is available

You're using 0.1.0

[ Update now ]    [ Not now ]
```

---

# 36. Update Decline Policy

If the user selects:

```text
Not now
```

Gocode should ask again on the next launch as long as that version is still the latest available version.

Do not automatically create an `ignore_version`.

---

# 37. Update Source of Truth

Use GitHub Releases associated with repository tags.

Version format:

```text
v0.1.0
v0.2.0
v0.3.0
```

Use SemVer for comparison.

Prereleases and drafts should not be presented as stable updates by default.

---

# 38. Windows Self Update

Because a running executable should not directly replace itself, use:

```text
gocode.exe
gocode-updater.exe
```

Flow:

```text
gocode.exe
   ↓
detect update
   ↓
download new binary
   ↓
verify integrity
   ↓
launch gocode-updater.exe
   ↓
exit

gocode-updater.exe
   ↓
replace old binary
   ↓
start gocode.exe
```

UX:

```text
Updating Gocode...

✓ Updated 0.1.0 → 0.2.0

Starting Gocode...
```

---

# 39. Update Security

Before replacing binaries:

- HTTPS is mandatory;
- validate checksum;
- validate version;
- write to a temporary file;
- replace only after download completes;
- perform a simple rollback if replacement fails.

Artifact signing may be added later.

---

# 40. Rust Architecture

Suggested workspace:

```text
gocode/
├── Cargo.toml
└── crates/
    ├── gocode/
    ├── gocode-core/
    ├── gocode-tui/
    ├── gocode-provider/
    ├── gocode-provider-nvidia/
    └── gocode-updater/
```

---

# 41. Crate Responsibilities

## `gocode`

Entry point.

Responsibilities:

- CLI;
- bootstrap;
- dependency wiring;
- TUI initialization.

## `gocode-core`

Responsibilities:

- agent;
- conversation;
- tools;
- project;
- config;
- sessions;
- model abstractions;
- permissions.

## `gocode-tui`

Responsibilities:

- state;
- screens;
- widgets;
- rendering;
- keyboard;
- modals;
- event handling.

## `gocode-provider`

Responsibilities:

- traits;
- shared types;
- provider registry;
- model capability abstractions.

## `gocode-provider-nvidia`

Responsibilities:

- NVIDIA NIM API;
- auth;
- models;
- inference;
- streaming;
- tool calls;
- thinking/reasoning mappings;
- errors.

## `gocode-updater`

Responsibilities:

- current version;
- GitHub Releases;
- download;
- checksum;
- Windows replacement;
- restart.

---

# 42. Config Precedence

Suggested order:

```text
CLI flags
↓
project config
↓
global config
↓
provider defaults
↓
built-in defaults
```

Credentials should follow a separate resolution flow.

---

# 43. Logging

Global logs:

```text
~/.gocode/logs/
```

Requirements:

- never log API keys;
- never log secrets;
- redact headers;
- configurable levels;
- simple future log rotation.

---

# 44. Error UX

Errors should be translated into clear language.

Bad:

```text
HTTP 401
```

Better:

```text
Your NVIDIA API key was rejected.

Check the key or connect another one.

[ Update key ]
```

Bad:

```text
ReqwestError::TimedOut
```

Better:

```text
NVIDIA took too long to respond.

[ Retry ]
```

Technical details can remain available in logs or debug mode.

---

# 45. Offline Behavior

If GitHub is offline:

- ignore update check failure;
- continue working.

If NVIDIA is offline:

- show a connection error;
- keep the TUI functional;
- allow retry;
- do not destroy the session.

---

# 46. Model Picker

Example:

```text
Select model

NVIDIA NIM

> Model A
  Tools       ✓
  Thinking    ✓
  Streaming   ✓

  Model B
  Tools       ✓
  Thinking    -
  Streaming   ✓
```

Information should be useful, not excessive.

---

# 47. Provider Picker

MVP:

```text
Provider

> NVIDIA NIM
```

Even with only one provider, keep the architecture ready for multiple providers.

---

# 48. Model Unavailable

If a previously saved model is no longer available:

```text
Your previous model is no longer available.

Choose another model:

> ...
```

Do not fail startup.

---

# 49. TUI States

Suggested main states:

```text
Boot
Onboarding
Chat
ModelPicker
ProviderPicker
Config
UpdatePrompt
PermissionPrompt
ErrorModal
Exit
```

---

# 50. Keyboard

MVP:

| Key | Action |
|---|---|
| Enter | send |
| Esc | cancel current action / agent |
| Ctrl+C | safe exit |
| ↑/↓ | contextual selection/history |
| Tab | navigation when needed |

Avoid requiring complex shortcuts.

---

# 51. Cancellation

When pressing `Esc` during execution:

- cancel the model request when possible;
- interrupt the active tool when possible;
- return the agent to a safe state;
- preserve context already produced.

---

# 52. Git

The MVP must at least expose:

```text
git_status
```

Ideally also:

```text
git_diff
```

Gocode must not create commits automatically in v0.1.0.

---

# 53. MVP v0.1.0

| Feature | Status |
|---|---|
| Windows | required |
| global `gocode` command | required |
| TUI | required |
| global `.gocode` | required |
| local `.gocode` | required |
| NVIDIA NIM | required |
| API key onboarding | required |
| secure credential storage | required |
| model selection | required |
| model capabilities | required |
| streaming | required |
| thinking/reasoning | required |
| tool calling | required |
| read files | required |
| search | required |
| edit/apply patch | required |
| run commands | required |
| git status | required |
| agent loop | required |
| project instructions | required |
| basic sessions | desirable |
| GitHub update check | required |
| self update | required |
| Windows updater | required |
| OpenAI | outside MVP |
| Anthropic | outside MVP |
| Gemini | outside MVP |
| MCP | outside MVP |
| subagents | outside MVP |
| plugins | outside MVP |
| IDE integration | outside MVP |
| Linux/macOS | outside initial MVP |

---

# 54. Non-Goals for v0.1.0

Do not try to solve:

- full IDE;
- built-in code editor;
- plugin marketplace;
- full MCP support;
- subagents;
- cloud sync;
- multi-user;
- team workspaces;
- billing;
- browser UI;
- mobile;
- model training;
- vector database;
- complex RAG;
- full GitHub PR automation;
- autonomous background agents.

---

# 55. Roadmap

## v0.0.1 — Foundation

- Rust workspace;
- `gocode` CLI;
- Ratatui TUI;
- event loop;
- global config;
- local config;
- automatic `.gocode` creation;
- project root detection.

## v0.0.2 — NVIDIA

- onboarding;
- API key;
- credential storage;
- `NvidiaProvider`;
- chat completions;
- streaming;
- model picker;
- model registry;
- capabilities;
- thinking mappings.

## v0.0.3 — Coding Tools

- list files;
- read file;
- search;
- write/apply patch;
- run command;
- git status;
- tool UI.

## v0.0.4 — Agent

- inference → tool → inference loop;
- cancellation;
- max turns;
- project instructions;
- permissions;
- tool result handling.

## v0.0.5 — UX

- `/model`;
- `/provider`;
- `/config`;
- `/clear`;
- `/help`;
- error UX;
- terminal resize;
- onboarding polish;
- empty states;
- copy/paste behavior.

## v0.0.6 — Updater

- GitHub Releases;
- SemVer;
- update modal;
- download;
- checksum;
- updater executable;
- replace;
- relaunch.

## v0.0.7 — Hardening

- Windows Terminal testing;
- PowerShell testing;
- cmd testing;
- Unicode;
- network failures;
- terminal crashes;
- Ctrl+C;
- large projects;
- large files;
- long-running commands;
- model errors.

## v0.0.8 — Stability

- integration tests;
- provider tests;
- updater tests;
- filesystem sandbox tests;
- permission tests;
- session recovery.

## v0.0.9 — Release Candidate

- installer;
- release pipeline;
- docs;
- telemetry decision;
- legal/license;
- binary optimization.

## v0.1.0 — MVP

First usable public release.

---

# 56. v0.1.0 Acceptance Criteria

## Installation

- [ ] user can install on Windows;
- [ ] `gocode` works in a new terminal;
- [ ] installation does not require Rust;
- [ ] PATH is configured correctly.

## Startup

- [ ] TUI opens quickly;
- [ ] global `.gocode` is created automatically;
- [ ] local `.gocode` is created automatically;
- [ ] project root is detected.

## NVIDIA

- [ ] user can paste an API key;
- [ ] API key is validated;
- [ ] API key is not stored in plaintext TOML;
- [ ] user can select a model;
- [ ] chat works;
- [ ] streaming works.

## Models

- [ ] model registry exists;
- [ ] capabilities exist;
- [ ] tools can be detected/configured per model;
- [ ] thinking can be detected/configured per model;
- [ ] unavailable model does not break startup.

## Agent

- [ ] model can list files;
- [ ] model can read files;
- [ ] model can search;
- [ ] model can apply patches;
- [ ] model can execute commands;
- [ ] model receives tool results;
- [ ] agent can execute multiple steps;
- [ ] user can cancel.

## UX

- [ ] normal flow requires no manual config editing;
- [ ] error messages are understandable;
- [ ] agent actions appear in the TUI;
- [ ] dangerous permissions require confirmation;
- [ ] UI responds to resize.

## Update

- [ ] Gocode checks latest release;
- [ ] version is compared with SemVer;
- [ ] modal appears when a newer version exists;
- [ ] `Not now` makes the warning appear again next startup;
- [ ] update downloads the new binary;
- [ ] checksum is validated;
- [ ] updater replaces the executable;
- [ ] Gocode restarts on the new version.

---

# 57. MVP Quality Metrics

Even without telemetry, the project should aim for:

- first launch to working chat in very few steps;
- zero manual config editing in the normal flow;
- startup not blocked by update check;
- interface fully usable with keyboard only;
- no secrets in logs;
- no accidental access outside the workspace;
- predictable tool loop;
- recoverable errors without restarting the application.

---

# 58. Important Architectural Decisions

1. Rust is the primary language.
2. Ratatui + Crossterm power the TUI.
3. Tokio manages async execution.
4. NVIDIA NIM is the first provider.
5. Provider-specific logic is isolated.
6. Model capabilities are first-class.
7. Thinking is first-class.
8. Global and local config are separate.
9. Secrets do not live in TOML.
10. Gocode creates `.gocode` automatically.
11. GitHub Releases are the source of truth for updates.
12. Update checking does not block startup.
13. Windows uses a separate updater.
14. UX favors defaults and automation.
15. The agent explores the project with tools instead of loading everything into the prompt.

---

# 59. Open Decisions

Items that must be decided during implementation without blocking the start:

- exact crate for Windows Credential Manager;
- final session format;
- final checksum/release strategy;
- exact `ModelRegistry` structure;
- final `ThinkingCapability` schema;
- exact `run_command` policy;
- default agent turn limit;
- file read size limit;
- policy for files outside the workspace;
- how NVIDIA capabilities are discovered/cached;
- whether model metadata is remote, embedded, or hybrid;
- `.gocode` migration strategy;
- telemetry: none, opt-in, or another model;
- open-source license.

---

# 60. Recommended Implementation Order

Start with the shortest vertical path:

```text
gocode
↓
TUI opens
↓
config works
↓
NVIDIA connects
↓
model streams responses
↓
read_file
↓
tool calling
↓
apply_patch
↓
run_command
↓
full agent loop
↓
updater
↓
polish
```

Avoid overbuilding abstractions before the first end-to-end path works.

---

# 61. Definition of Success

Gocode v0.1.0 will be successful when a person can run:

```text
cd my-project
gocode
```

and then type:

```text
Fix the authentication bug and run the tests.
```

and Gocode can, intuitively:

1. understand the request;
2. explore the project;
3. find the relevant files;
4. use thinking when supported by the model;
5. edit the code;
6. run the tests;
7. analyze the result;
8. fix again if necessary;
9. briefly explain what changed.

Without requiring that person to manually configure the agent's internal infrastructure.

---

# 62. North Star

> Gocode should feel simple on the outside and sophisticated on the inside.

If a capability can be detected automatically, it should be detected.

If a setting can have a good default, it should have a good default.

If a provider difference can be abstracted, it should be abstracted.

The user should think about the code they want to build — not about how to configure the coding agent.
