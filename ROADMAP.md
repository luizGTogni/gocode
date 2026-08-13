# Gocode — MVP Roadmap

**Status:** Initial planning draft
**Product:** Gocode
**Target release:** v0.1.0
**Initial platform:** Windows 10/11
**Primary provider:** NVIDIA NIM

---

# 1. Purpose

This roadmap defines the path from the initial project foundation to the first usable public release of Gocode.

It translates the product and architecture specifications into ordered delivery milestones. Each milestone has:

- a concrete objective;
- required deliverables;
- dependencies;
- verifiable exit criteria.

This roadmap covers only the MVP. Post-MVP providers, platforms, plugins, MCP support, and IDE integrations are intentionally excluded.

---

# 2. MVP Outcome

Gocode v0.1.0 is complete when a Windows user can open a terminal inside a project, run:

```powershell
gocode
```

and ask Gocode to perform a coding task such as:

```text
Fix the authentication bug and run the tests.
```

Gocode must then be able to:

1. detect and understand the project context;
2. connect to NVIDIA NIM through secure onboarding;
3. stream model output in the TUI;
4. search and read relevant files;
5. apply targeted code changes;
6. run appropriate validation commands;
7. iterate through multiple model and tool turns;
8. respect workspace, permission, privacy, and cancellation boundaries;
9. clearly summarize what changed and what was validated;
10. detect, verify, and install a newer Gocode release with user approval.

The normal flow must not require manual configuration-file editing.

---

# 3. Planning Principles

## 3.1 Deliver Vertical Slices

Prefer an end-to-end working path over isolated framework construction.

The first meaningful slice is:

```text
CLI starts
↓
TUI opens
↓
credential resolves
↓
NVIDIA connects
↓
model streams text
↓
application shuts down safely
```

The second meaningful slice adds agent behavior:

```text
user prompt
↓
model requests read_file
↓
tool validates and executes
↓
result returns to model
↓
model produces a final response
```

Each later milestone extends these slices without replacing their core contracts.

## 3.2 Keep Milestones Demonstrable

Every milestone must end in behavior that can be demonstrated and tested. A collection of types, traits, or screens without an integrated flow does not complete a milestone.

## 3.3 Preserve Dependency Direction

The roadmap must maintain these architectural boundaries:

- the TUI does not depend directly on NVIDIA;
- the Agent does not depend on Ratatui;
- provider-specific behavior remains inside provider adapters;
- tools enforce filesystem and process boundaries;
- the updater is separate from the main executable replacement process;
- model output never bypasses deterministic validation and permission checks.

## 3.4 Security Is Incremental and Release-Blocking

Security requirements should be implemented alongside each capability, then tested comprehensively during hardening.

Hardening is not permission to postpone foundational controls such as:

- workspace containment;
- credential isolation;
- tool validation;
- command permissions;
- bounded outputs;
- safe cancellation;
- update verification.

## 3.5 Avoid Date Commitments Without Capacity Data

This roadmap uses dependency and release order rather than calendar estimates. Dates may be added only after team capacity, ownership, and implementation velocity are known.

---

# 4. Delivery Sequence

```text
v0.0.1  Foundation
   ↓
v0.0.2  NVIDIA Vertical Slice
   ↓
v0.0.3  Coding Tools
   ↓
v0.0.4  Agent Loop
   ↓
v0.0.5  Product UX
   ↓
v0.0.6  Updater
   ↓
v0.0.7  Security and Platform Hardening
   ↓
v0.0.8  Stability
   ↓
v0.0.9  Release Candidate
   ↓
v0.1.0  MVP
```

The sequence represents release dependencies, not a prohibition on parallel work. Test fixtures, security tests, documentation, and release automation should evolve throughout development.

---

# 5. Cross-Cutting Workstreams

The following workstreams span multiple milestones.

| Workstream | Starts | Release gate |
|---|---:|---:|
| Automated testing | v0.0.1 | v0.0.8 |
| Windows manual testing | v0.0.1 | v0.0.9 |
| Security boundary tests | v0.0.3 | v0.0.8 |
| User-facing documentation | v0.0.2 | v0.0.9 |
| Structured local logging | v0.0.1 | v0.0.7 |
| Error normalization | v0.0.2 | v0.0.8 |
| Performance profiling | v0.0.3 | v0.0.8 |
| Release automation | v0.0.6 | v0.0.9 |

Workstream gates do not mean the work begins at the gate. They identify the milestone by which the work must be complete enough for release progression.

---

# 6. v0.0.1 — Foundation

## Objective

Establish a Windows-compatible Rust application that starts reliably, discovers the active project, loads configuration, renders a basic TUI, and restores the terminal safely.

## Deliverables

### Workspace and Bootstrap

- Rust workspace and initial crates;
- `gocode` binary entry point;
- Tokio runtime initialization;
- dependency construction during bootstrap;
- structured local logging with secret-safe defaults;
- graceful shutdown path.

### TUI Foundation

- Ratatui and Crossterm integration;
- terminal guard for raw mode and alternate screen;
- basic app state;
- `AppCommand` and `AppEvent` channels;
- render and input event loops;
- placeholder boot and chat screens;
- `Ctrl+C` exit behavior;
- terminal restoration after normal failure and panic where practical.

### Project and Configuration

- project-root detection;
- global directory resolution;
- local `.gocode` creation;
- global and project config types;
- deterministic config precedence;
- default config generation;
- atomic config writes;
- schema version field;
- separation between config, state, cache, logs, and sessions.

### Test Foundation

- crate-level unit-test structure;
- temporary filesystem fixtures;
- basic app-state tests;
- Windows CI build or equivalent early validation.

## Dependencies

None. This is the initial milestone.

## Exit Criteria

- [ ] `gocode` builds and starts on Windows without requiring an existing config;
- [ ] the TUI opens and exits without leaving the terminal in raw mode;
- [ ] project root detection has deterministic tests;
- [ ] global and local `.gocode` directories are created in the correct locations;
- [ ] config precedence is covered by tests;
- [ ] invalid config produces an actionable error rather than a panic;
- [ ] startup, resize, and shutdown events flow through the app runtime;
- [ ] logs do not contain secrets or raw environment dumps.

---

# 7. v0.0.2 — NVIDIA Vertical Slice

## Objective

Deliver the first end-to-end inference experience: secure NVIDIA credential onboarding, model discovery and selection, capability-aware request construction, and streamed text in the TUI.

## Deliverables

### Generic Provider Layer

- `Provider` contract;
- provider registry and factory;
- normalized model and model ID types;
- normalized provider stream events;
- normalized provider errors;
- cancellation support;
- reusable HTTP client configuration;
- `FakeProvider` for core tests.

### NVIDIA Provider

- hosted NVIDIA NIM adapter;
- bearer-token authentication;
- credential validation;
- `/v1/models` discovery where supported;
- `/v1/chat/completions` streaming;
- text-delta parsing;
- provider request IDs and safe diagnostic metadata;
- rate-limit, timeout, authentication, server, and malformed-response mapping.

### Credentials

- `NVIDIA_API_KEY` resolution;
- Windows Credential Manager integration;
- secret wrapper with redacted debug behavior;
- masked onboarding input;
- no plaintext fallback when secure persistence is unavailable;
- removal of provider credentials from future subprocess environments by default.

### Models and Capabilities

- model registry;
- capability resolution;
- tool capability metadata;
- thinking/reasoning capability model;
- NVIDIA-specific thinking mapping;
- conservative handling of unknown models;
- cached model metadata with async refresh.

### User Experience

- welcome screen;
- API-key entry and validation states;
- model picker;
- streamed assistant text;
- provider and model status display;
- clear offline, invalid-key, and unavailable-model states;
- disclosure that prompts and selected project context are sent to NVIDIA.

## Dependencies

- v0.0.1 app runtime, config, TUI, and logging foundations.

## Exit Criteria

- [ ] a new user can configure NVIDIA without editing TOML;
- [ ] the API key is never stored in ordinary config or rendered after submission;
- [ ] credential failure, timeout, and provider failure remain distinct;
- [ ] the user can select and persist an available model;
- [ ] chat text streams without freezing the TUI;
- [ ] cancellation stops an active provider request;
- [ ] unknown or removed models do not crash startup;
- [ ] capability settings are validated before a request is sent;
- [ ] provider unit and contract tests use fixtures or fakes without real credentials;
- [ ] NVIDIA-specific request behavior does not leak into the Agent or TUI.

---

# 8. v0.0.3 — Coding Tools

## Objective

Provide a deterministic, independently testable tool layer for project exploration, targeted editing, command execution, and Git inspection.

## Deliverables

### Tool Runtime

- `Tool` contract;
- tool registry;
- JSON schema definitions;
- tool-call and tool-result types;
- argument validation pipeline;
- explicit tool status and event lifecycle;
- bounded and truncatable output;
- timeout and cancellation support.

### Workspace Filesystem

- shared path-validation service;
- workspace containment;
- traversal protection;
- symlink and Windows reparse-point handling;
- relative and absolute path normalization;
- `.gitignore`-aware discovery;
- default exclusion of `.git` and `.gocode` from general discovery;
- binary and encoding detection;
- atomic writes where practical.

### MVP Tools

- `list_files`;
- `read_file`;
- `search`;
- `write_file`;
- `apply_patch`;
- `run_command`;
- `git_status`;
- `git_diff`.

### Process Execution

- program-plus-arguments execution by default;
- explicit shell mode;
- controlled working directory;
- stdout and stderr streaming;
- non-zero exit-code reporting;
- command timeouts;
- process-tree cancellation on Windows where practical;
- provider-credential removal from child environments.

### Permission Foundation

- `Allow`, `Ask`, and `Deny` decisions;
- read-only default policies;
- risk-based command evaluation;
- permission request events;
- clear permission modal integration point;
- no dedicated commit, push, reset, checkout, or generic network tool.

## Dependencies

- v0.0.1 project and runtime services;
- v0.0.2 normalized capability and event contracts for later Agent integration.

## Exit Criteria

- [ ] every tool validates its schema before execution;
- [ ] filesystem tools cannot escape the workspace through traversal, symlinks, junctions, UNC paths, or path casing;
- [ ] text tools reject unsupported binary content safely;
- [ ] edits report partial and failed outcomes accurately;
- [ ] command output is streamed and bounded;
- [ ] commands support timeout and cancellation;
- [ ] provider credentials are absent from child-process environments;
- [ ] permission requests show the actual action and working directory;
- [ ] Git tools are read-only;
- [ ] filesystem, patch, process, Git, and permission tests pass on supported Windows environments.

---

# 9. v0.0.4 — Agent Loop

## Objective

Connect provider inference to validated local tools through a cancellable multi-turn Agent Runtime that preserves user intent and reports completion honestly.

## Deliverables

### Agent Runtime

- `Agent` and isolated `AgentRun` state;
- `AgentRunId` and `ToolCallId` correlation;
- state machine and valid transitions;
- inference → tool → inference loop;
- streamed text and activity events;
- completion detection;
- final-response generation;
- one active task at a time.

### Conversation and Context

- normalized conversation messages;
- tool calls and results in conversation history;
- system instruction construction;
- `.gocode/instructions.md` loading;
- instruction-authority rules;
- context budgeting;
- bounded tool results;
- targeted project exploration instead of full-repository loading.

### Tool Calling

- tool-definition delivery based on model capabilities;
- streamed tool-call assembly;
- complete JSON validation before execution;
- sequential execution for the MVP;
- permission integration;
- denial results returned to the model;
- explicit unknown-tool and invalid-argument handling.

### Safety and Control

- run cancellation token;
- provider, tool, and process cancellation propagation;
- invalidation of pending approvals on cancellation;
- stale-event rejection;
- maximum turns and tool-call limits;
- consecutive-failure and loop detection;
- prompt-injection boundaries;
- preservation of existing user changes;
- no automatic Git commit.

### Validation Behavior

- Agent-directed build, test, lint, or formatting commands;
- command-result interpretation;
- retry decisions without automatic duplication of effects;
- explicit reporting when validation was not run or failed;
- no false completion claims.

## Dependencies

- v0.0.2 provider streaming and capabilities;
- v0.0.3 tools, permissions, and events.

## Exit Criteria

- [ ] a scripted provider can complete a multi-tool coding task end to end;
- [ ] tool calls never execute before complete assembly and validation;
- [ ] unsupported model capabilities are gated before inference;
- [ ] user denial prevents the requested action and is visible to the Agent;
- [ ] cancellation returns the run to a safe terminal state;
- [ ] stale events and approvals cannot affect another run;
- [ ] limits stop repeated or non-progressing loops;
- [ ] project file content cannot override system security policy or current user intent;
- [ ] the final response accurately lists changes and validation results;
- [ ] Agent tests run entirely against fake providers and fake tools.

---

# 10. v0.0.5 — Product UX

## Objective

Turn the working coding Agent into an understandable, keyboard-first terminal product that handles onboarding, configuration, active work, permissions, errors, and recovery coherently.

## Deliverables

### Chat Experience

- final chat layout;
- user and assistant message rendering;
- streaming assistant messages;
- thinking activity without exposing private chain-of-thought;
- concise tool activity;
- expandable command output;
- modified-file summary;
- validation summary;
- scroll and scroll-lock behavior;
- empty and first-prompt states.

### Commands and Settings

- `/model`;
- `/provider`;
- `/config`;
- `/clear`;
- `/help`;
- `/exit`;
- slash-command autocomplete;
- capability-aware model and thinking settings;
- persistent config feedback.

### Interaction

- keyboard navigation;
- multi-line input;
- bracketed paste where available;
- normal terminal copy behavior;
- resize handling;
- narrow and minimum-size layouts;
- modal focus and priority;
- queued input behavior;
- clear `Esc` cancellation and `Ctrl+C` shutdown.

### Error and Recovery UX

- normalized error severity;
- inline recoverable errors;
- blocking error modal;
- provider disconnection and retry states;
- stale permission-modal invalidation;
- panic message after terminal restoration;
- session recovery behavior where basic sessions are enabled.

### Privacy UX

- masked credential rendering;
- remote inference disclosure;
- no raw secrets in tool activity or errors;
- guidance that sessions and logs may contain sensitive project metadata;
- no mandatory telemetry.

## Dependencies

- v0.0.1 TUI architecture;
- v0.0.2 onboarding and provider state;
- v0.0.3 permission and tool events;
- v0.0.4 complete Agent lifecycle.

## Exit Criteria

- [ ] first-time onboarding requires no manual file editing;
- [ ] active thinking, tools, commands, and final completion are distinguishable;
- [ ] permission prompts are concise and unambiguous;
- [ ] resize, scroll, copy, paste, and multi-line input behave correctly in Windows Terminal;
- [ ] all primary flows are keyboard-usable;
- [ ] `Esc` cancels active work and `Ctrl+C` restores the terminal;
- [ ] errors explain the next useful action without exposing secrets;
- [ ] unavailable models and providers do not make the TUI unusable;
- [ ] state, keyboard, modal, resize, and key-screen snapshot tests pass.

---

# 11. v0.0.6 — Updater

## Objective

Deliver a non-blocking, user-approved Windows self-update flow that verifies artifacts, preserves the current installation on failure, and restarts successfully after replacement.

## Deliverables

### Update Discovery

- GitHub Releases source;
- stable-release filtering;
- SemVer parsing and comparison;
- deterministic Windows architecture asset selection;
- non-blocking startup check;
- per-startup prompt behavior after `Not now`;
- cached metadata where useful.

### Update UX

- deferred notification during an active Agent run;
- update modal;
- release version and notes summary;
- `Update now` and `Not now` actions;
- progress events;
- actionable non-destructive failure states.

### Download and Verification

- HTTPS-only official downloads;
- temporary partial-download path;
- final artifact staging;
- SHA-256 checksum parsing and verification;
- archive path-traversal protection;
- expected-file allowlist;
- cancellation before installation begins.

### Windows Installation

- separate `gocode-updater.exe`;
- validated source and target paths;
- wait for main process exit;
- backup of the installed executable;
- replacement;
- rollback on failure;
- updater self-update strategy;
- restart with validated arguments;
- local secret-free diagnostics.

## Dependencies

- v0.0.1 app lifecycle and config;
- v0.0.5 modal, event, and shutdown behavior;
- release asset naming agreed with the future pipeline.

## Exit Criteria

- [ ] update checks never block normal startup;
- [ ] GitHub unavailability leaves the application usable;
- [ ] only a newer stable release triggers the modal;
- [ ] an active Agent run is not interrupted by an update prompt;
- [ ] checksum mismatch and partial download leave the installation unchanged;
- [ ] unsafe or unexpected archive entries are rejected;
- [ ] user approval is required before installation;
- [ ] replacement failure restores the previous executable;
- [ ] the new version restarts successfully after a valid update;
- [ ] checker and installer tests run against fake sources and temporary installations.

---

# 12. v0.0.7 — Security and Platform Hardening

## Objective

Validate the security invariants and Windows-specific behavior of the integrated product under adversarial inputs, failures, and uncommon environments.

## Deliverables

### Security Hardening

- workspace-boundary adversarial suite;
- prompt-injection tests;
- stale approval and event tests;
- credential-redaction audit;
- subprocess-environment audit;
- provider redirect and authorization-scope review;
- bounded stream and tool-output tests;
- config, cache, state, and session corruption tests;
- updater target and archive adversarial tests;
- review against `docs/SECURITY.md` invariants.

### Windows Hardening

- Windows Terminal, PowerShell, and `cmd` testing;
- Unicode project paths and usernames;
- drive-letter, UNC, junction, symlink, and reparse-point tests;
- PowerShell and `cmd` argument-quoting tests;
- process-tree cancellation;
- Credential Manager failure modes;
- secure temporary files;
- terminal panic restoration;
- executable lock and updater rollback tests.

### Scale and Failure Hardening

- large repository discovery;
- large and invalid files;
- long command output;
- slow and interrupted provider streams;
- rate limits and network outages;
- terminal resize during streaming;
- cancellation during tools, permissions, and update checks;
- disk-full and read-only filesystem behavior where practical.

## Dependencies

- integrated functionality from v0.0.1 through v0.0.6.

## Exit Criteria

- [ ] all security invariants have at least one automated or documented manual verification;
- [ ] no known workspace escape remains;
- [ ] provider credentials are absent from config, logs, sessions, UI, and subprocesses;
- [ ] malformed model output cannot bypass tool validation;
- [ ] stale events and approvals cannot trigger actions;
- [ ] cancellation and terminal restoration succeed across critical states;
- [ ] Windows path, quoting, and update edge cases have documented results;
- [ ] high-severity security defects are resolved before stability work begins;
- [ ] remaining limitations are documented accurately rather than hidden.

---

# 13. v0.0.8 — Stability

## Objective

Convert the hardened feature set into a predictable release candidate foundation through integrated testing, recovery validation, performance work, and defect reduction.

## Deliverables

### Automated Coverage

- provider contract suite;
- Agent scripted-flow suite;
- tool contract suite;
- permission suite;
- config and migration suite;
- TUI state-transition suite;
- updater checker and installer suite;
- end-to-end tests for the main vertical slices.

### Recovery

- provider disconnect recovery;
- session write failure handling;
- corrupt cache fallback;
- invalid config behavior;
- interrupted command behavior;
- interrupted update recovery;
- graceful and panic shutdown validation.

### Performance

- startup profiling;
- TUI render responsiveness;
- stream-event coalescing;
- large-output truncation;
- bounded channel and memory behavior;
- large-project search performance;
- model-cache startup behavior.

### Defect Management

- reproducible issue templates;
- release-blocker classification;
- no unresolved crash or data-corruption defects in normal flows;
- documented known limitations.

## Dependencies

- v0.0.7 hardening results and resolved critical findings.

## Exit Criteria

- [ ] the full automated suite passes consistently;
- [ ] the primary coding flow passes end to end on Windows;
- [ ] repeated cancellation, retry, and restart tests do not corrupt state;
- [ ] large projects and outputs remain bounded and responsive;
- [ ] provider and updater unit tests require no live external services;
- [ ] normal network failures are recoverable without restarting the TUI where specified;
- [ ] no known release-blocking crash, secret exposure, workspace escape, data corruption, or update rollback defect remains.

---

# 14. v0.0.9 — Release Candidate

## Objective

Prepare the stable MVP feature set for public distribution, installation, support, and security reporting.

## Deliverables

### Distribution

- Windows release build;
- PowerShell installer;
- user-level PATH setup;
- deterministic asset names;
- `gocode.exe` and `gocode-updater.exe` packaging;
- SHA-256 checksum publication;
- GitHub Release creation;
- install, update, uninstall, and recovery instructions.

### Release Pipeline

- immutable release tag input;
- committed Rust lockfile;
- build and test gates;
- least-privilege CI permissions;
- protected release secrets;
- final artifact checksums;
- source-revision traceability;
- fake update source for non-release tests.

### Documentation

- user installation and onboarding guide;
- provider and privacy explanation;
- configuration reference;
- tool and permission behavior;
- troubleshooting guide;
- security policy and private reporting path;
- release notes;
- known limitations;
- license and contribution guidance.

### Product Decisions

- final open-source license;
- telemetry decision, with no mandatory telemetry assumed;
- final supported Windows versions and architectures;
- final credential-storage crate;
- final session persistence scope;
- final checksum format and release asset contract.

### Manual Release Matrix

- Windows 10 and Windows 11 where available;
- Windows Terminal with PowerShell;
- Windows Terminal with `cmd`;
- standalone PowerShell;
- clean installation without Rust;
- Unicode path;
- onboarding and credential persistence;
- complete coding task;
- cancellation and recovery;
- update, rollback, and relaunch.

## Dependencies

- v0.0.8 stable integrated build;
- release engineering contract from v0.0.6;
- completed security review from v0.0.7.

## Exit Criteria

- [ ] a clean Windows user account can install and run `gocode` in a new terminal;
- [ ] installation does not require Rust or manual PATH editing;
- [ ] the release pipeline produces only the expected assets and checksums;
- [ ] the packaged updater successfully upgrades the packaged application;
- [ ] rollback has been exercised using release-like artifacts;
- [ ] all required documentation is present and consistent;
- [ ] a private vulnerability-reporting channel is enabled;
- [ ] the supported platform matrix has recorded pass/fail results;
- [ ] all release-blocking decisions are resolved;
- [ ] remaining known issues are acceptable, documented, and non-critical.

---

# 15. v0.1.0 — MVP Release

## Objective

Publish the first usable Gocode release that satisfies the product acceptance criteria and supports a complete NVIDIA-backed coding-agent workflow on Windows.

## Release Gates

### Installation and Startup

- [ ] `gocode` is globally available after installation;
- [ ] startup creates required global and project state automatically;
- [ ] the TUI opens quickly and remains responsive;
- [ ] update-check failure does not block startup;
- [ ] terminal state is restored on exit, cancellation, and fatal failure.

### NVIDIA and Models

- [ ] onboarding validates and stores the NVIDIA credential securely;
- [ ] model discovery and selection work;
- [ ] streaming chat works;
- [ ] tools and thinking behavior follow model capabilities;
- [ ] unavailable models and temporary provider failures are recoverable;
- [ ] remote data transmission is disclosed clearly.

### Agent and Tools

- [ ] the Agent can list, search, and read project files;
- [ ] the Agent can apply targeted patches and write files;
- [ ] the Agent can run and interpret validation commands;
- [ ] the Agent receives tool results and completes multiple turns;
- [ ] workspace, schema, permission, output, and cancellation boundaries are enforced;
- [ ] the Agent does not overwrite unrelated user changes;
- [ ] final responses distinguish successful, failed, and unperformed validation.

### UX

- [ ] normal operation requires no manual config editing;
- [ ] onboarding, chat, tools, permissions, errors, and updates are understandable;
- [ ] all primary flows are keyboard accessible;
- [ ] copy, paste, scrolling, and resize work in the supported terminals;
- [ ] secrets remain masked and redacted;
- [ ] cancellation does not leave stale UI or background execution.

### Security

- [ ] the `docs/SECURITY.md` release checklist passes;
- [ ] no known critical or high-severity vulnerability remains unresolved;
- [ ] provider credentials do not reach ordinary files or subprocesses;
- [ ] project content cannot grant itself additional authority;
- [ ] no generic model-controlled network tool is exposed;
- [ ] an unverified update cannot replace the installation.

### Update

- [ ] a newer stable release is detected using SemVer;
- [ ] the user can accept or defer installation;
- [ ] downloads and checksums are verified;
- [ ] update replacement and rollback work on Windows;
- [ ] the updated application restarts on the expected version.

### Quality

- [ ] automated tests pass from a clean checkout;
- [ ] the Windows manual matrix passes;
- [ ] no known normal-flow crash, data-corruption defect, or secret leak remains;
- [ ] installation, usage, privacy, security, and recovery documentation is published;
- [ ] release artifacts can be traced to the tagged source revision.

## Release Definition

Passing individual feature demonstrations is not sufficient. v0.1.0 is ready only when the complete supported flow works from installation through a real coding task and a verified update.

---

# 16. Critical Path

The shortest dependency path to a usable MVP is:

```text
bootstrap and terminal lifecycle
↓
project and config resolution
↓
secure NVIDIA credential
↓
streamed provider response
↓
read_file tool call round trip
↓
search and targeted patch
↓
command execution and validation
↓
multi-turn Agent loop
↓
complete TUI states and permissions
↓
verified Windows updater
↓
hardening and release validation
```

If progress stalls, prioritize restoring this path before expanding secondary features.

---

# 17. Parallelizable Work

After the v0.0.1 contracts stabilize, the following can progress in parallel with coordination:

| Track | Can proceed alongside | Coordination point |
|---|---|---|
| NVIDIA adapter | TUI onboarding | provider events and error model |
| Filesystem tools | Provider work | generic tool schemas and events |
| TUI widgets | Agent runtime | `AppEvent` lifecycle |
| Updater checker | UX polish | modal timing and app shutdown |
| Test fixtures | All milestones | stable contracts and deterministic IDs |
| Documentation | All milestones | verified current behavior |
| Security tests | Tools and Agent | boundary APIs and permission semantics |

Parallel work must not introduce duplicate event, error, credential, or capability models.

---

# 18. Release-Blocking Risks

| Risk | Impact | Required mitigation |
|---|---|---|
| NVIDIA model metadata is incomplete or inconsistent | Incorrect tool/thinking behavior | Centralized capability resolver with conservative defaults and fixtures |
| Windows Credential Manager integration is unreliable | Credentials cannot be stored securely | Select and test the crate early; retain environment-only non-persistent flow |
| Streamed tool-call formats vary by model | Invalid or unsafe tool execution | Provider-specific assembly, strict completion checks, recorded fixtures |
| Windows path edge cases bypass containment | Workspace escape | Shared path service and adversarial Windows tests before v0.0.7 exit |
| Shell quoting differs across PowerShell and `cmd` | Incorrect or dangerous commands | Prefer program/args; explicit shell mode; platform quoting tests |
| Process cancellation leaves child processes running | Continued unintended effects | Windows process-tree strategy and manual cancellation matrix |
| Updater cannot replace locked executables safely | Failed or corrupt self-update | Separate updater, staging, backup, rollback, release-like tests |
| Checksum and asset naming disagree with CI | Updates cannot verify or install | Freeze one release pipeline contract before v0.0.9 |
| TUI event volume causes freezes | Poor streaming and command UX | Bounded channels, coalescing, profiling, output truncation |
| MVP scope expands to post-MVP features | Delayed usable release | Enforce the non-goals in this roadmap and the PRD |

A newly discovered critical security or data-loss issue blocks progression regardless of its milestone label.

---

# 19. Decisions Required Before Release Candidate

These decisions may remain open during early implementation but must be resolved before v0.0.9 exits:

- Windows Credential Manager crate and fallback UX;
- session format, retention, and recovery scope;
- exact `ModelCapabilities` and `ThinkingCapability` schemas;
- NVIDIA capability metadata source and cache versioning;
- `run_command` risk rules and default timeout;
- Agent turn and tool-call limits;
- file-read and tool-output limits;
- checksum file format and release asset naming;
- updater self-replacement behavior;
- supported Windows CPU architectures;
- telemetry policy;
- open-source license;
- private vulnerability-reporting channel.

When a decision affects a public contract or persisted data, document it before implementation becomes difficult to change.

---

# 20. Explicitly Outside the MVP

The following items must not delay v0.1.0:

- Linux and macOS releases;
- OpenAI, Anthropic, Gemini, OpenRouter, or Ollama providers;
- self-hosted NVIDIA NIM configuration;
- MCP support;
- plugins or a plugin marketplace;
- subagents or parallel agent runs;
- IDE integration;
- browser or mobile interfaces;
- cloud sync and team workspaces;
- billing or usage purchasing;
- autonomous background agents;
- full GitHub PR automation;
- automatic Git commit or push;
- vector databases and complex RAG;
- generic internet browsing tools;
- forced or silent updates;
- automatic downgrade;
- mandatory telemetry;
- cryptographic release signing beyond the MVP checksum requirement.

Future-compatible abstractions are acceptable only when they simplify the MVP or prevent a known architectural dead end.

---

# 21. Progress Tracking

Use one status for each milestone:

| Status | Meaning |
|---|---|
| Planned | Scope is defined but implementation has not started |
| In progress | Implementation or verification is active |
| Blocked | A named dependency or decision prevents meaningful progress |
| Release candidate | Deliverables are complete and exit criteria are under validation |
| Complete | All exit criteria pass and the milestone artifact is available |

Initial state:

| Milestone | Status |
|---|---|
| v0.0.1 — Foundation | Planned |
| v0.0.2 — NVIDIA Vertical Slice | Planned |
| v0.0.3 — Coding Tools | Planned |
| v0.0.4 — Agent Loop | Planned |
| v0.0.5 — Product UX | Planned |
| v0.0.6 — Updater | Planned |
| v0.0.7 — Security and Platform Hardening | Planned |
| v0.0.8 — Stability | Planned |
| v0.0.9 — Release Candidate | Planned |
| v0.1.0 — MVP Release | Planned |

Update milestone status only from concrete repository and test evidence. A percentage estimate is not a substitute for passing exit criteria.

---

# 22. Source Documents

This roadmap is derived from and must remain consistent with:

- [`PRD.md`](PRD.md);
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md);
- [`docs/AGENT.md`](docs/AGENT.md);
- [`docs/CONFIG.md`](docs/CONFIG.md);
- [`docs/PROVIDER.md`](docs/PROVIDER.md);
- [`docs/NVIDIA_NIM.md`](docs/NVIDIA_NIM.md);
- [`docs/TOOLS.md`](docs/TOOLS.md);
- [`docs/TUI.md`](docs/TUI.md);
- [`docs/UPDATER.md`](docs/UPDATER.md);
- [`docs/SECURITY.md`](docs/SECURITY.md).

When implementation changes an accepted requirement, update the owning specification and then update this roadmap if milestone scope or exit criteria change.

---

# 23. Final Rule

The roadmap exists to deliver one trustworthy end-to-end product, not ten partially connected subsystems.

> Build the shortest complete coding-agent path first, then strengthen it until it is safe, understandable, recoverable, and ready to distribute.
