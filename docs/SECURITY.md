# Gocode — Security Specification and Policy

**Status:** Initial technical draft
**Product:** Gocode
**Target version:** v0.1.0
**Scope:** Product security, implementation requirements, and vulnerability disclosure

---

# 1. Purpose

This document defines the security model for Gocode.

Gocode is a local coding agent that can:

- read and modify project files;
- execute development commands;
- send prompts and selected project context to a model provider;
- store local configuration and sessions;
- download and install application updates.

These capabilities create meaningful security and privacy risks. Security must therefore be enforced by the application around the model, not delegated to model behavior or prompt instructions.

This document serves two audiences:

1. implementers, who need concrete security requirements and trust boundaries;
2. users and security researchers, who need an accurate description of expected behavior and a responsible disclosure process.

---

# 2. Security Goals

Gocode should protect:

- the confidentiality of API keys and other secrets;
- the confidentiality of project source code and user prompts;
- the integrity of files inside the active workspace;
- the integrity of the installed Gocode binaries;
- the user's control over consequential actions;
- the availability and recoverability of the current project and installation;
- the isolation of provider, tool, session, and updater responsibilities.

The primary goals for v0.1.0 are:

1. enforce the workspace boundary for file tools;
2. validate every tool call before execution;
3. require risk-based permission decisions for commands;
4. keep provider credentials out of ordinary config, logs, sessions, and subprocesses;
5. treat model output and project file content as untrusted input;
6. disclose when project content may be sent to a remote provider;
7. restrict network access to explicit product components;
8. verify and safely stage updates before replacing binaries;
9. fail safely without corrupting files, sessions, configuration, or the installation.

---

# 3. Non-Goals and Security Limitations

Gocode v0.1.0 is not intended to be:

- an operating-system sandbox;
- an antivirus or malware-analysis environment;
- a perfect classifier for destructive shell commands;
- a secret scanner that can identify every credential format;
- a defense against a fully compromised operating system or user account;
- a multi-user isolation boundary;
- a guarantee that third-party build scripts are safe;
- an end-to-end encrypted remote inference system;
- a substitute for source control, backups, or review of important changes.

Running a build, test, package-manager, or project script may execute arbitrary code from the workspace or its dependencies. A command that looks routine can still be dangerous in an untrusted repository.

The permission engine reduces accidental or unauthorized actions. It does not turn arbitrary process execution into a secure sandbox.

---

# 4. Assets

Security-sensitive assets include:

| Asset | Examples |
|---|---|
| Provider credentials | `NVIDIA_API_KEY`, bearer tokens, future provider keys |
| Project content | source code, configuration, local documentation, Git changes |
| User content | prompts, pasted text, instructions, session history |
| Local state | global config, project config, caches, logs, sessions |
| Execution authority | filesystem access, subprocess creation, command environment |
| Installation integrity | `gocode.exe`, `gocode-updater.exe`, release metadata |
| User intent | approved task scope, denied actions, permission choices |

---

# 5. Threat Model

Gocode must account for threats from:

- malicious or compromised project files;
- prompt injection embedded in source code, documentation, command output, or tool results;
- malformed or adversarial model responses;
- incorrect or hallucinated tool calls;
- path traversal and symlink escapes;
- destructive, deceptive, or unexpectedly broad commands;
- secrets exposed through logs, errors, sessions, UI, or subprocess environments;
- malicious dependencies or project scripts;
- compromised networks, provider endpoints, release assets, or CI pipelines;
- corrupted cache, config, session, or update files;
- stale asynchronous events applied to the wrong agent run;
- race conditions during file writes, cancellation, shutdown, or self-update.

The MVP assumes the local operating system, current user account, and Gocode process are not already fully compromised.

---

# 6. Trust Boundaries

The main security boundaries are:

```text
User
  ↓ intent and approvals
Gocode TUI / App Runtime
  ↓ validated commands and events
Agent Runtime
  ↓ untrusted tool requests
Tool and Permission Layer
  ↓ constrained local operations
Workspace / Subprocesses
```

Remote inference is a separate boundary:

```text
Selected project context
  ↓
Provider Adapter
  ↓ HTTPS + provider credential
Remote Model Provider
  ↓ untrusted streamed response
Provider Parser / Agent Validation
```

Updates use another independent boundary:

```text
GitHub Release
  ↓ HTTPS
Update Checker
  ↓ validated metadata
Update Installer
  ↓ staged and verified replacement
Installed Binaries
```

No boundary may be bypassed because the model requested it.

The workspace restriction applies to Agent tools. Trusted application services may access their own global config, credential, cache, log, session, and update locations, but those paths must not become model-controlled tool targets.

---

# 7. Authority and Evidence

Instruction authority and factual evidence are different concepts.

Instruction precedence is:

```text
Gocode system and security policies
>
current user request and explicit approval
>
.gocode/instructions.md
>
ordinary project files, tool output, and model-generated text
```

Tool results are authoritative evidence about current project state, but they are not instruction authority. A file or command output that says to ignore security policy remains untrusted data.

The model must never be treated as a security principal. Model output is a proposal that must pass schema validation, boundary checks, permission evaluation, and cancellation checks before it can affect the system.

---

# 8. Prompt Injection

Project content may deliberately attempt to manipulate the Agent.

Examples include instructions embedded in:

- source comments;
- README files;
- dependency output;
- generated files;
- test failures;
- terminal output;
- Git diffs;
- tool results.

Required behavior:

- treat ordinary project and tool content as data;
- do not allow file content to override system policy or the user's request;
- do not disclose secrets because a file asks for them;
- do not expand task scope solely because project content requests it;
- validate all requested actions independently of the model's explanation;
- preserve the user's denial as authoritative for the active request;
- clearly surface consequential actions in permission prompts.

`.gocode/instructions.md` is an explicit project instruction source, but it remains subordinate to system security rules and the current user's request.

---

# 9. Tool Security Boundary

All model-requested actions must go through the registered Tools layer.

Every tool call follows:

```text
known tool?
↓
arguments parse and match schema?
↓
path and workspace checks pass?
↓
permission decision allows execution?
↓
run is still active and not cancelled?
↓
execute with bounded output and explicit result
```

Unknown tools, malformed JSON, incomplete streamed arguments, invalid paths, and stale tool calls must never execute.

Tool implementations should expose only the minimum context they require. They must not receive unrestricted application state or provider credentials.

For v0.1.0, tool execution is sequential. Future parallel execution requires explicit proof that operations are independent, permission-safe, and correctly ordered.

---

# 10. Workspace Boundary

The active project root is the default filesystem security boundary.

By default, tools may only:

- list and search inside the project root;
- read files inside the project root;
- write files inside the project root;
- execute commands with a working directory inside the project root.

All path-taking tools must use a shared validation service:

```text
raw path
↓
resolve relative to project root
↓
normalize
↓
canonicalize the target or nearest existing parent
↓
verify workspace containment
↓
evaluate symlinks and reparse points
↓
perform the operation
```

The implementation must reject:

- `..` traversal outside the workspace;
- absolute paths outside the workspace;
- symlinks or Windows reparse points that escape the workspace;
- alternate path forms that bypass containment checks;
- update or application directories presented as project paths.

Containment must be checked using path semantics, not string-prefix comparison. Windows checks must account for drive letters, UNC paths, case-insensitive path behavior, separators, and reparse points.

Outside-workspace access is not part of the normal v0.1.0 flow. A future exception requires an explicit user-facing capability and a separate permission decision.

---

# 11. File Operations

File tools must:

- reject unsupported binary input for text-only operations;
- handle invalid UTF-8 and encoding errors explicitly;
- bound file reads and search results;
- avoid following workspace-escaping links;
- preserve existing user changes unless the active task requires modifying them;
- prefer atomic writes where practical;
- report partial or failed changes accurately;
- never claim a write succeeded without verifying the result.

Recommended write pattern:

```text
validate destination
↓
write a temporary file in the same trusted directory
↓
flush when required
↓
atomically replace destination when supported
↓
report the final state
```

Temporary file names must not allow an attacker to redirect writes to another location.

An `apply_patch` operation must not silently leave an undocumented partial result. The result must identify which changes, if any, were applied.

---

# 12. Command Execution

`run_command` is the highest-risk MVP tool.

Prefer a program-plus-arguments representation:

```rust
CommandRequest {
    program,
    args,
    cwd,
}
```

Shell interpretation must be explicit. Do not treat every command string as shell input.

Every command must be evaluated by the permission engine. Evaluation should consider:

- executable and arguments;
- whether a shell is involved;
- working directory;
- current user intent;
- filesystem and network effects;
- package installation or dependency mutation;
- destructive syntax or broad targets;
- privilege elevation;
- interaction with Git or remote systems.

Obvious destructive, privilege-changing, persistence-creating, credential-reading, or outside-workspace commands must be denied or require explicit confirmation according to policy.

The MVP must not expose dedicated tools for:

- `git commit`;
- `git push`;
- destructive Git reset or checkout;
- arbitrary URL fetching;
- privilege elevation.

Commands must support cancellation and timeouts. On Windows, cancellation should terminate the relevant process tree when safely possible, not only the immediate parent process.

---

# 13. Permission Model

The permission engine returns one of:

```rust
Allow
Ask(PermissionRequest)
Deny(PermissionReason)
```

Default MVP policy:

| Operation | Default |
|---|---|
| List, search, and read inside workspace | Allow |
| Git status and diff | Allow |
| Targeted writes during an explicit editing task | Allow |
| Command execution | Evaluate every call |
| Potentially destructive or broad command | Ask or deny |
| Outside-workspace access | Deny |
| Generic network access | Deny / unavailable |
| Update installation | Ask |

Permission prompts must show the actual action, target, and working directory in language the user can understand. Approval applies only to the displayed action and must not be silently generalized.

A denied action returns a denial result to the Agent. Denial is not an application failure, and the Agent must not repeatedly re-request substantially the same denied action without new user direction.

Permission decisions must be bound to the active `AgentRunId` and `ToolCallId`. A stale approval must never authorize a newer or different action.

---

# 14. Credentials and Secrets

Provider credentials must not be stored in ordinary TOML, JSON, logs, sessions, caches, crash output, or project files.

Credential resolution order is:

```text
explicit supported environment variable
↓
operating-system credential store
↓
onboarding
```

For Windows, the preferred persistent store is Windows Credential Manager through a maintained compatible crate.

Secret values should use a wrapper such as `SecretString` with:

- redacted `Debug` and display behavior;
- no ordinary serialization;
- limited cloning and lifetime;
- zeroization where practical.

API key input must be masked. After submission, the raw value must not be displayed again.

Provider authorization headers and secret-bearing request fields must never be logged.

If secure credential storage is unavailable, Gocode must report the limitation and allow a non-persistent environment-based flow. It must not silently fall back to plaintext credential storage.

---

# 15. Subprocess Environment

Gocode must not inject internally stored provider credentials into child processes.

Known provider secret variables, including `NVIDIA_API_KEY`, must be removed from the subprocess environment by default even when Gocode obtained them from the user's inherited environment. This prevents routine commands such as tests or package scripts from receiving inference credentials accidentally.

If a future feature intentionally passes a secret to a subprocess, it requires:

- an explicit product capability;
- a narrowly scoped secret selection;
- a clear permission prompt;
- no logging of the value;
- documentation of the exposure.

Command logs must not contain a full environment dump. Tool errors should report variable names only when needed, never their values.

---

# 16. Secret Redaction

At minimum, known provider credentials and authorization values must be redacted from:

- TUI output;
- structured logs;
- error messages;
- tool output forwarded to the model;
- session persistence;
- updater diagnostics.

Redaction should cover exact resolved secret values and known header formats.

Redaction is defense in depth, not a guarantee that arbitrary secrets can always be detected. Gocode should avoid collecting or rendering sensitive content in the first place.

Raw prompts, full file contents, and raw provider request bodies must not be logged by default. Debug mode does not disable secret protection.

---

# 17. Source Code and Prompt Privacy

Gocode is a local application, but remote inference is not local processing.

When a remote model provider is selected, Gocode may send the provider:

- user prompts;
- system and project instructions;
- relevant source-code excerpts;
- search and file-reading results;
- command output;
- conversation history;
- tool results required for the agent loop.

The onboarding and public documentation must explain this behavior before a user relies on Gocode for sensitive code.

Gocode must minimize transmitted context to what is reasonably needed for the task. It must not upload the entire repository by default.

Provider data retention, training, geographic processing, and account policy are controlled by the selected provider and its terms. Gocode cannot provide stronger remote privacy guarantees than that provider offers.

No mandatory product telemetry is required for v0.1.0.

---

# 18. Network Security

The v0.1.0 Agent has no generic network tool.

Approved network consumers are limited to:

- provider adapters for inference and model discovery;
- the updater for GitHub release metadata and assets.

Remote clients must use:

- HTTPS;
- certificate validation provided by a maintained TLS stack;
- explicit timeouts;
- bounded, non-infinite retries;
- a product user agent;
- redacted diagnostics.

Redirects must not cause credentials to be forwarded to an unrelated host. Provider authorization must be scoped to the configured trusted provider endpoint.

Custom provider endpoints are outside the initial hosted NVIDIA flow. When added, the UI must make the destination clear because project content and credentials may be sent there.

The absence of a generic network tool does not sandbox subprocess networking. A user-approved command, package manager, build script, or test may access the network through normal operating-system facilities. The permission engine should identify evident network behavior when practical, and the UI must not imply that local command execution is offline.

---

# 19. Provider Response Security

Provider streams are untrusted input.

Adapters must:

- limit response and buffered argument sizes;
- parse streaming data defensively;
- preserve tool-call IDs and ordering;
- reject malformed or incomplete tool argument JSON;
- never execute partially assembled tool calls;
- normalize provider errors without leaking response secrets;
- stop processing promptly after cancellation;
- avoid replaying tool effects after an interrupted stream.

Automatic retry must not duplicate meaningful streamed output or tool actions. If consistency cannot be guaranteed, fail clearly and let the Agent or user decide how to continue.

---

# 20. Configuration, State, Cache, and Sessions

Local files have different trust and sensitivity levels:

| Data | Security requirement |
|---|---|
| Global/project config | No secrets; validate schema and values |
| Credential store | Secrets only through OS-backed interface |
| State | No provider credentials; tolerate corruption |
| Model cache | Untrusted optimization; never authority |
| Sessions | Potentially sensitive prompts, code, and tool results |
| Logs | Metadata by default; redact sensitive values |

Configuration and state writes should be atomic. Migrations should create a backup before changing durable user data.

Config, cache, state, and session content must be parsed as untrusted local input. Invalid content must produce a safe error or be ignored according to the owning subsystem; it must not trigger code execution.

Session persistence must not silently include credentials. Session files should be accessible only to the current user where platform APIs make that practical.

The local `.gocode/sessions/` directory is machine-specific and may contain sensitive project context. Documentation should recommend excluding it from version control.

---

# 21. Logging and Diagnostics

Gocode uses local structured logging through `tracing`.

Useful fields include:

- component;
- provider and model identifiers;
- tool name and status;
- request or run ID;
- duration;
- HTTP status class;
- updater stage;
- error category.

Logs must not include:

- API keys or bearer tokens;
- authorization headers;
- full subprocess environments;
- raw prompts by default;
- full source files by default;
- sensitive cookies;
- raw secret-bearing request bodies.

Log files may still reveal project names, paths, model choices, command names, timings, and error metadata. Public documentation should advise users to review logs before sharing them.

Crash and panic output must restore the terminal first, show only a concise error and local log path, and never print secrets.

---

# 22. Event and Run Isolation

Every active agent run must have its own:

- `AgentRunId`;
- cancellation token;
- provider stream;
- tool-call IDs;
- stats and mutable execution state.

Events that can authorize or mutate state must carry the relevant identifiers. The TUI and runtime must ignore stale events from completed, cancelled, or replaced runs.

Only one task executes at a time in the MVP. Queued user input must not change the authority or permission scope of the currently running task.

Cancellation must invalidate pending permission requests and prevent late provider or tool events from continuing execution.

---

# 23. Safe Failure and Recovery

Security-sensitive failure behavior must be conservative:

- invalid tool input does not execute;
- ambiguous paths are rejected;
- malformed provider tool calls are rejected;
- failed credential validation does not expose or delete a credential automatically;
- a timeout is not treated as proof of an invalid credential;
- checksum mismatch aborts an update;
- failed config or session writes preserve the previous valid file when possible;
- cancellation stops future actions and leaves an explicit final state;
- the Agent never reports unperformed validation as successful.

Graceful shutdown order:

1. invalidate pending approvals;
2. cancel the active agent and provider stream;
3. cancel or terminate active tools and process trees;
4. flush session and log writers safely;
5. restore the terminal;
6. exit.

---

# 24. Update and Supply-Chain Security

The official updater must enforce:

1. an official GitHub Releases source;
2. HTTPS for metadata and downloads;
3. stable SemVer comparison;
4. deterministic OS and architecture asset selection;
5. staged downloads outside the installed binary path;
6. SHA-256 verification before installation;
7. safe archive extraction with traversal prevention;
8. an allowlist of expected archive files;
9. validated replacement paths independent of model input;
10. backup before replacement;
11. rollback on replacement failure;
12. explicit user approval before installation;
13. no forced update and no normal-flow downgrade.

The updater must never accept an arbitrary target path supplied by model output, project content, release archive entries, or unvalidated IPC.

Archive extraction must reject absolute paths, `..` traversal, symlink-based escapes, and unexpected executables.

An SHA-256 checksum obtained from the same compromised release channel detects corruption but does not establish publisher authenticity. Release signing and Windows code signing are important hardening after the MVP and should be completed before broad production distribution when practical.

---

# 25. Build and Release Security

The release pipeline should:

- build from an immutable Git tag;
- use a committed Rust lockfile;
- run unit, integration, and security-boundary tests;
- use least-privilege CI permissions;
- avoid exposing release secrets to untrusted pull-request code;
- pin third-party CI actions to reviewed immutable revisions where practical;
- produce deterministic asset names agreed with the updater;
- generate SHA-256 checksums from final artifacts;
- publish only expected binaries and metadata;
- retain provenance needed to trace an artifact to its source revision.

Dependency review should include known-vulnerability and license checks appropriate to the release process. Security updates to the Rust toolchain and dependencies should be evaluated promptly, especially for HTTP, TLS, archive, credential, terminal, and process-management crates.

---

# 26. Windows-Specific Requirements

The initial platform requires dedicated tests for:

- path normalization across drive letters and UNC paths;
- case-insensitive containment checks;
- symlinks, junctions, and reparse points;
- Windows Credential Manager behavior;
- ACLs on logs, sessions, and temporary files;
- PowerShell and `cmd` quoting;
- process-tree cancellation;
- executable file locks during self-update;
- update rollback after partial replacement;
- Unicode paths and usernames;
- secure temporary file creation;
- terminal restoration after crash or cancellation.

PowerShell command text and argument arrays are not interchangeable. Quoting must be tested using adversarial arguments rather than assumed correct.

---

# 27. Security Testing Strategy

Security behavior should be covered by automated tests wherever practical.

## 27.1 Filesystem Tests

- relative and absolute traversal attempts;
- nonexistent targets with escaping parents;
- symlink and junction escape attempts;
- alternate separators and path casing;
- UNC and cross-drive paths;
- atomic write failure;
- partial patch behavior;
- binary and invalid-encoding input;
- oversized file and search output.

## 27.2 Tool and Permission Tests

- unknown tools and invalid schemas;
- malformed and incomplete JSON arguments;
- risk-classification boundaries;
- stale and mismatched approvals;
- repeated denied requests;
- cancellation before and during execution;
- timeout and process-tree termination;
- removal of provider credentials from subprocess environments.

## 27.3 Agent and Provider Tests

- prompt injection in files and tool output;
- attempts to override instruction authority;
- secret exfiltration requests;
- oversized or malformed stream chunks;
- incomplete tool-call assembly;
- retry after partial streaming;
- stale events from cancelled runs;
- secret-free logs and error mappings.

## 27.4 Updater Tests

- incorrect and malformed checksums;
- wrong platform asset selection;
- archive traversal and unexpected files;
- partial downloads;
- unwritable installation directory;
- replacement failure and rollback;
- malicious replacement targets;
- updater self-replacement on Windows.

Security tests must not depend on real provider credentials or the live GitHub update service. Use fake providers, fake tools, fixtures, and a fake update source.

---

# 28. Security Review Checklist

Before v0.1.0 is released, verify:

- [ ] all file tools enforce the shared workspace boundary;
- [ ] symlink, junction, UNC, and traversal tests pass on Windows;
- [ ] all model-requested actions pass through tool validation and permissions;
- [ ] streamed tool arguments cannot execute before complete validation;
- [ ] provider credentials are absent from config, cache, sessions, logs, and child processes;
- [ ] API key UI input remains masked and is not rendered after submission;
- [ ] raw prompts and source files are not logged by default;
- [ ] remote provider data disclosure is documented in onboarding;
- [ ] no generic network tool is registered;
- [ ] command execution supports timeout, cancellation, and clear permission prompts;
- [ ] stale approvals and events are rejected;
- [ ] config, session, and file writes preserve prior data on failure where practical;
- [ ] update downloads use HTTPS and pass checksum verification;
- [ ] archive extraction rejects unsafe paths and unexpected files;
- [ ] updater backup and rollback are tested on Windows;
- [ ] release CI uses least privilege and produces the expected checksums;
- [ ] logs and crash reports have been reviewed for sensitive data exposure.

---

# 29. Security Defaults

The secure behavior must also be the easy default:

| Area | Default |
|---|---|
| Workspace access | Current project only |
| File discovery | Respect `.gitignore`; skip `.git` and `.gocode` unless explicit |
| Generic network tool | Unavailable |
| Provider credentials | OS credential store or explicit environment variable |
| Credential inheritance by subprocesses | Removed |
| Raw prompt/source logging | Disabled |
| Remote telemetry | Disabled / not implemented |
| Command permissions | Evaluated per call |
| Update check | Non-blocking |
| Update installation | Explicit approval |
| Update verification | Required before replacement |
| Git commit/push tools | Unavailable |

Security controls must not depend on users manually editing TOML or understanding internal risk classifications.

---

# 30. Future Hardening

Potential post-MVP improvements include:

- signed release manifests;
- Sigstore, minisign, or equivalent artifact signatures;
- Windows Authenticode signing;
- stronger dependency and build provenance attestations;
- optional operating-system sandboxing for subprocesses;
- finer-grained persistent permission rules;
- broader secret detection with clear false-positive handling;
- encrypted session storage;
- configurable data-retention controls;
- endpoint allowlists and enterprise policy;
- security event export without prompt or source content;
- formally versioned permission-policy schemas.

These improvements must not weaken or postpone the mandatory MVP controls in this document.

---

# 31. Supported Versions

Before the first stable release, security fixes are applied to the current development version.

After releases begin, the latest stable release is the supported version unless the project publishes a different support schedule. Users reporting a vulnerability should identify the exact Gocode version, operating system, installation method, and whether the issue reproduces on the latest available version.

---

# 32. Reporting a Vulnerability

Do not disclose a suspected vulnerability in a public issue, discussion, pull request, or shared log file before the maintainers have had an opportunity to investigate it.

Use GitHub Private Vulnerability Reporting for the Gocode repository when it is enabled. If private reporting is not available, contact the repository maintainer through a private channel listed on the repository owner's profile and include a request for a secure reporting channel. Do not send working exploits, credentials, private source code, or sensitive user data through a public channel.

A useful report includes:

- affected version or commit;
- operating system and terminal;
- concise vulnerability description;
- security impact;
- reproducible steps or a minimal proof of concept;
- required configuration or permissions;
- whether secrets or third-party data are involved;
- suggested mitigation, if known.

Remove real credentials, proprietary source code, personal data, and unrelated logs from the report. Use synthetic values wherever possible.

Maintainers should acknowledge reports privately, validate impact, coordinate a fix and release, and credit the reporter if requested and appropriate. Public disclosure should be coordinated after a fix or mitigation is available whenever practical.

---

# 33. Vulnerability Scope

Examples of in-scope security issues include:

- workspace-boundary bypass;
- unauthorized file modification or command execution;
- permission-prompt bypass or stale approval reuse;
- provider credential disclosure;
- secrets written to logs, sessions, config, or subprocess environments;
- prompt injection that bypasses enforced tool or permission boundaries;
- unsafe update target selection, archive extraction, verification, or rollback;
- remote endpoint credential leakage;
- cross-run event confusion leading to unintended actions;
- denial of service caused by unbounded provider or tool input when practical to exploit.

Generally out of scope without an additional product flaw:

- a model producing low-quality or incorrect code;
- a user explicitly approving a clearly displayed destructive command;
- malicious behavior in code or dependencies the user intentionally executes outside Gocode;
- provider-side retention or processing that matches the provider's disclosed policy;
- attacks requiring an already fully compromised local user account;
- social engineering with no bypass of a Gocode security control.

Reports are still welcome when the correct classification is uncertain.

---

# 34. Security Invariants

The implementation must preserve these invariants:

1. Model output never directly performs an operating-system action.
2. Every tool call is parsed, validated, authorized, and associated with the active run before execution.
3. File tools do not escape the active workspace in the normal MVP flow.
4. Provider credentials do not enter ordinary config, logs, sessions, or child-process environments.
5. Ordinary project content cannot override system security policy or current user intent.
6. Remote transmission of project context occurs only through the selected provider path and is disclosed to the user.
7. Generic model-controlled network access is unavailable in v0.1.0.
8. A cancelled or obsolete run cannot use stale events or approvals to continue acting.
9. An unverified or partially downloaded update never replaces the installed binary.
10. Security-relevant failures are reported accurately and leave existing data recoverable where practical.

---

# 35. Definition of Done

The Gocode v0.1.0 security model is ready when:

- the invariants in this document are represented in architecture and tests;
- workspace containment is enforced consistently across all tools;
- command execution is permission-aware, cancellable, bounded, and secret-isolated;
- prompt injection cannot bypass application-enforced capabilities;
- credential storage and redaction work on Windows;
- users understand what data remote inference may receive;
- sessions, logs, config, and cache follow their declared sensitivity rules;
- provider and updater network clients follow explicit trust boundaries;
- the updater verifies, stages, backs up, and rolls back safely;
- the release checklist passes on the supported Windows environments;
- a private vulnerability reporting path is available before public release.

---

# 36. Final Rule

The model may recommend an action, but only Gocode can authorize and execute it.

> Security decisions must be enforced in deterministic application code at the boundary where effects occur.
