# Gocode — Tools Specification

**Status:** Initial technical draft  
**Product:** Gocode  
**Target version:** v0.1.0  
**Scope:** Built-in coding agent tools

---

# 1. Purpose

This document defines the built-in tool system used by the Gocode coding agent.

Tools are the only supported mechanism through which the agent interacts with the local project and operating system.

The model must not directly access:

- the filesystem;
- processes;
- Git;
- credentials;
- network resources;
- the shell;
- external system state.

Instead, it requests structured tool calls and Gocode validates, authorizes, executes, records, and returns their results.

This document defines:

- the tool contract;
- the tool registry;
- tool call validation;
- workspace boundaries;
- permission handling;
- result schemas;
- streaming;
- cancellation;
- truncation;
- error behavior;
- built-in MVP tools;
- security rules;
- testing requirements.

---

# 2. Core Principle

Tools are the capability boundary of the agent.

The model may request an action, but Gocode decides whether that action:

1. exists;
2. has valid arguments;
3. is allowed in the current workspace;
4. requires user confirmation;
5. can be executed safely.

Conceptually:

```text
Model
  ↓
ToolCall
  ↓
Validation
  ↓
Workspace checks
  ↓
Permission engine
  ↓
Execution
  ↓
ToolResult
  ↓
Model
```

---

# 3. MVP Tool Set

The v0.1.0 built-in tool set should include:

```text
list_files
read_file
search
write_file
apply_patch
run_command
git_status
git_diff
```

`git_diff` is strongly recommended for the MVP because it gives the agent a safe way to inspect existing and newly created changes.

---

# 4. Tool Trait

Conceptual Rust interface:

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

The exact Rust types may evolve, but the boundary should remain equivalent.

---

# 5. Tool Definition

Each tool exposes metadata to the model.

```rust
pub struct ToolDefinition {
    pub name: ToolName,
    pub description: String,
    pub input_schema: serde_json::Value,
}
```

Requirements:

- names must be stable;
- descriptions must be concise and explicit;
- schemas must reject ambiguous arguments;
- schemas should not expose provider-specific concepts.

---

# 6. Tool Registry

The runtime stores tools in a registry.

```rust
pub struct ToolRegistry {
    tools: HashMap<ToolName, Arc<dyn Tool>>,
}
```

Responsibilities:

- register built-in tools;
- resolve a tool by name;
- export tool definitions to the provider;
- reject unavailable tools;
- support fake tools in tests.

---

# 7. Tool Call

Normalized representation:

```rust
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: ToolName,
    pub arguments: serde_json::Value,
}
```

The provider adapter is responsible for converting provider-specific tool call formats into this normalized representation.

---

# 8. Tool Result

Normalized result:

```rust
pub struct ToolResult {
    pub call_id: ToolCallId,
    pub status: ToolStatus,
    pub output: ToolOutput,
    pub metadata: ToolMetadata,
}
```

---

# 9. Tool Status

```rust
pub enum ToolStatus {
    Success,
    Failed,
    Cancelled,
    Denied,
}
```

A non-zero process exit code does not necessarily mean the tool runtime itself failed.

For example:

```text
cargo test
```

may execute successfully while tests fail.

That should generally be represented as:

```text
ToolStatus::Success
```

with process metadata containing:

```text
exit_code != 0
```

This allows the model to reason about test failures instead of treating them as infrastructure failures.

---

# 10. Tool Output

Conceptual structure:

```rust
pub struct ToolOutput {
    pub content: String,
    pub truncated: bool,
}
```

Specific tools may attach structured metadata in addition to human-readable content.

---

# 11. Tool Metadata

Metadata may include:

```rust
pub struct ToolMetadata {
    pub duration_ms: Option<u64>,
    pub affected_files: Vec<PathBuf>,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}
```

Keep the common metadata small.

Tool-specific metadata can use an enum or structured extension later if needed.

---

# 12. Tool Context

Every tool receives a controlled runtime context.

```rust
pub struct ToolContext {
    pub project_root: PathBuf,
    pub cancellation: CancellationToken,
    pub permissions: PermissionContext,
}
```

Potential future fields:

```text
session_id
agent_run_id
logger
process_runner
filesystem service
```

Do not expose unrestricted application state.

---

# 13. Workspace Boundary

The project root is the default security boundary for file operations.

By default, tools may only:

```text
read inside project root
write inside project root
search inside project root
execute with cwd inside project root
```

Outside-workspace access is not part of the v0.1.0 normal flow.

---

# 14. Path Validation

All path-taking tools must pass through the same validation layer.

Conceptual flow:

```text
raw path
↓
join with project root if relative
↓
normalize
↓
canonicalize where possible
↓
check workspace containment
↓
check symlink behavior
↓
execute
```

The implementation must prevent path traversal such as:

```text
../../secret.txt
```

---

# 15. Relative Paths

Tool schemas should prefer project-relative paths.

Good:

```text
src/auth.rs
```

Avoid returning absolute paths to the model unless necessary.

This improves:

- portability;
- readability;
- privacy;
- session reproducibility.

---

# 16. Symlinks

Symlinks require special care.

A path may appear to be inside the workspace while resolving outside it.

The file boundary layer should verify the resolved destination when practical.

Default policy:

> A symlink must not be used to escape the project workspace.

---

# 17. `.gitignore`

Filesystem discovery tools should respect `.gitignore` by default.

Applicable tools:

```text
list_files
search
```

Common ignored paths should also be avoided when appropriate:

```text
.git
node_modules
target
dist
build
vendor
```

However, project ignore rules remain the primary source of truth.

---

# 18. `.gocode`

The local `.gocode` directory should normally be excluded from general project search and listing.

Exceptions may exist for explicit requests.

Default:

```text
search project code → skip .gocode
read explicit .gocode/instructions.md → allowed
```

---

# 19. Binary Files

Tools should detect or avoid binary files.

`read_file` should reject binary content with a clear result.

Example:

```text
Cannot read `assets/logo.png` as text because it appears to be a binary file.
```

---

# 20. Encoding

UTF-8 is the preferred encoding.

For text files with invalid UTF-8:

- do not silently corrupt;
- return a clear error;
- future versions may add encoding detection.

The MVP may limit text editing to UTF-8-compatible files.

---

# 21. Tool Validation Pipeline

Every call should follow:

```text
Tool exists?
↓
Arguments parse?
↓
Schema valid?
↓
Path/workspace valid?
↓
Permission decision?
↓
Cancellation check?
↓
Execute
```

---

# 22. Invalid Tool Name

If the model calls an unavailable tool:

```text
ToolStatus::Failed
```

Suggested output:

```text
Tool `foo` is not available in this session.
```

This result should return to the model so it can recover.

---

# 23. Invalid Arguments

Invalid arguments must never be coerced into risky behavior.

Example:

```text
read_file(path = null)
```

should return a structured validation failure.

The model may then retry with corrected arguments.

---

# 24. Permission Engine

Every tool call is evaluated before execution.

Conceptual decision:

```rust
pub enum PermissionDecision {
    Allow,
    Ask(PermissionRequest),
    Deny(PermissionReason),
}
```

---

# 25. Default Permission Policy

Recommended MVP behavior:

| Tool | Default policy |
|---|---|
| `list_files` | Allow |
| `read_file` | Allow |
| `search` | Allow |
| `git_status` | Allow |
| `git_diff` | Allow |
| `write_file` | Allow within active editing task |
| `apply_patch` | Allow within active editing task |
| `run_command` | Risk-based evaluation |

The final implementation should also consider the user's current intent.

---

# 26. Read-Only Intent

If the user asks:

```text
explain
review
analyze
find
inspect
```

the agent should avoid write tools unless explicitly needed and authorized by user intent.

This is an agent-level rule, not only a permission-engine rule.

---

# 27. Editing Intent

If the user asks:

```text
fix
implement
change
refactor
add
remove
```

file editing tools may be enabled for the task.

---

# 28. Permission Prompts

Prompts must be easy to understand.

Example:

```text
Gocode wants to run:

  npm install

Working directory:
  C:\dev\my-project

[ Run ]   [ Cancel ]
```

Avoid exposing internal permission classifications unless useful.

---

# 29. Denied Tool Calls

If the user denies an operation, return:

```text
ToolStatus::Denied
```

Suggested tool result:

```text
The user denied this action.
```

The model can then:

- choose another approach;
- continue without the action;
- explain the limitation.

---

# 30. Cancellation

All long-running tools should support cancellation.

Use:

```rust
tokio_util::sync::CancellationToken
```

where practical.

Cancellation should be checked:

- before execution;
- during long loops;
- during process execution;
- during large file operations.

---

# 31. Tool Events

Long-running tools may emit progress.

Conceptual events:

```rust
pub enum ToolEvent {
    Started(ToolCallId),
    OutputChunk {
        id: ToolCallId,
        chunk: String,
    },
    Progress {
        id: ToolCallId,
        progress: ToolProgress,
    },
    Finished(ToolResult),
}
```

---

# 32. Output Limits

Tool outputs must be bounded.

Reasons:

- model context limits;
- TUI performance;
- memory usage;
- log size.

Each tool should define a sensible maximum.

---

# 33. Truncation

When output is truncated:

```text
truncated = true
```

and human-readable output should mention it.

Example:

```text
[Output truncated. Showing the last 200 lines.]
```

Never silently drop content.

---

# 34. Tool Timeouts

Long-running tools may define timeouts.

Examples:

```text
search → short timeout
run_command → configurable timeout
```

A timeout should return a clear result rather than panic.

---

# 35. Error Model

Conceptual error enum:

```rust
pub enum ToolError {
    InvalidArguments(String),
    OutsideWorkspace(PathBuf),
    NotFound(PathBuf),
    PermissionDenied(String),
    UnsupportedFileType(String),
    Io(std::io::Error),
    Cancelled,
    Timeout,
    Internal(String),
}
```

Provider-specific errors do not belong here.

---

# 36. Recoverable Tool Errors

Most tool failures should return to the model as a result.

Examples:

- file not found;
- search returned nothing;
- patch failed to apply;
- process exited with errors;
- invalid range.

These usually should not terminate the entire AgentRun.

---

# 37. Fatal Tool Errors

Tool runtime failures should only terminate the run when the agent cannot safely continue.

Examples:

- corrupted internal tool state;
- critical workspace initialization failure.

These should be rare.

---

# 38. `list_files`

Purpose:

> Discover project files and directories without loading their contents.

---

# 39. `list_files` Input

Suggested schema:

```json
{
  "path": ".",
  "depth": 2,
  "limit": 200
}
```

Fields:

```text
path   optional, default "."
depth  optional
limit  optional
```

---

# 40. `list_files` Behavior

Requirements:

- only list inside workspace;
- respect ignore rules;
- skip binary inspection;
- use project-relative paths;
- support depth limits;
- support result limits;
- return directories and files clearly.

Example output:

```text
src/
src/main.rs
src/auth.rs
Cargo.toml
README.md
```

---

# 41. `list_files` Limits

Default recommendations:

```text
depth = 2
limit = 200
```

The model can request a narrower path if more detail is needed.

---

# 42. `list_files` Permissions

Default:

```text
Allow
```

It is read-only.

---

# 43. `read_file`

Purpose:

> Read text content from a specific file.

---

# 44. `read_file` Input

Suggested schema:

```json
{
  "path": "src/auth.rs",
  "start_line": 1,
  "end_line": 200
}
```

Fields:

```text
path        required
start_line  optional
end_line    optional
```

Line numbers should be 1-based.

---

# 45. `read_file` Behavior

Requirements:

- validate workspace path;
- reject directories;
- reject binary files;
- support line ranges;
- add line numbers to output when useful;
- detect truncation;
- avoid loading huge files entirely.

---

# 46. `read_file` Output

Example:

```text
1 | use crate::token::Token;
2 |
3 | pub fn validate_token(token: &str) -> bool {
4 |     ...
5 | }
```

Line numbers help the model generate precise patches.

---

# 47. `read_file` Large Files

If no range is provided and the file is too large:

- return an initial bounded range;
- mark output as truncated;
- tell the model how to request another range.

Example:

```text
Showing lines 1-300 of 2,418.
Request another range to continue.
```

---

# 48. `read_file` Permissions

Default:

```text
Allow
```

---

# 49. `search`

Purpose:

> Search project text efficiently before reading entire files.

---

# 50. `search` Input

Suggested schema:

```json
{
  "query": "validate_token",
  "path": ".",
  "glob": "*.rs",
  "max_results": 50
}
```

Fields:

```text
query        required
path         optional
glob         optional
max_results  optional
```

---

# 51. `search` Behavior

Requirements:

- search only inside workspace;
- respect `.gitignore`;
- skip binary files;
- return compact matches;
- include file path and line number;
- cap results.

---

# 52. `search` Output

Example:

```text
src/auth.rs:42: pub fn validate_token(...)
src/middleware.rs:17: if !validate_token(token) {
tests/auth_test.rs:88: assert!(validate_token(...))
```

Avoid returning whole files.

---

# 53. `search` No Results

A valid search with no matches is not an execution error.

Return:

```text
No matches found for `validate_token`.
```

with:

```text
ToolStatus::Success
```

---

# 54. `search` Permissions

Default:

```text
Allow
```

---

# 55. `write_file`

Purpose:

> Create a new text file or fully replace a file when full replacement is explicitly appropriate.

---

# 56. `write_file` Input

Suggested schema:

```json
{
  "path": "src/new_module.rs",
  "content": "..."
}
```

Optional future field:

```text
create_only
```

---

# 57. `write_file` Behavior

Requirements:

- workspace-only;
- UTF-8 text;
- create parent directories only when allowed;
- track before/after state;
- avoid accidental replacement when a patch is more appropriate;
- emit `FileChange`.

---

# 58. `write_file` Existing Files

If the file already exists, full replacement is a high-impact operation compared with a patch.

The Agent should prefer `apply_patch`.

The tool may still allow replacement when explicitly called and policy permits.

---

# 59. `write_file` Permissions

Default:

```text
Allow within a user-authorized editing task.
```

For read-only tasks, deny or avoid exposing the tool to the model depending on architecture.

---

# 60. `write_file` Result

Example:

```text
Created `src/new_module.rs` (84 lines).
```

or:

```text
Replaced `src/config.rs` (112 lines).
```

Metadata should include the affected path.

---

# 61. `apply_patch`

Purpose:

> Apply minimal, targeted edits to existing text files.

This is the preferred editing tool.

---

# 62. `apply_patch` Input

The exact patch format should be standardized.

Recommended MVP direction:

```json
{
  "patch": "*** Begin Patch\n*** Update File: src/auth.rs\n...\n*** End Patch"
}
```

Alternative formats may be used if implementation quality is better.

The important requirement is deterministic, reviewable edits.

---

# 63. `apply_patch` Requirements

The patch implementation must:

- operate only inside workspace;
- support file updates;
- support file creation if desired;
- support file deletion only if explicitly designed and permission-checked;
- fail safely if context no longer matches;
- avoid partially applying ambiguous edits;
- report changed files.

---

# 64. Patch Failure

If patch context does not match:

```text
ToolStatus::Failed
```

Example:

```text
Patch could not be applied because the expected context in `src/auth.rs` no longer matches the file.
Read the file again and generate a new patch.
```

This encourages the model to re-read instead of guessing.

---

# 65. Partial Patch Application

Prefer atomic behavior.

If a multi-file patch cannot be safely applied in full, the MVP should avoid leaving unclear partial state where possible.

If partial application is unavoidable, the result must explicitly list what changed.

---

# 66. `apply_patch` Permissions

Default:

```text
Allow within an active user-authorized editing task.
```

---

# 67. Change Tracking

All write operations should generate:

```rust
pub struct FileChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
}
```

Possible kinds:

```rust
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
}
```

---

# 68. Existing User Changes

Editing tools must not assume the working tree is clean.

A file may contain changes made by the user before Gocode started.

Gocode must not automatically revert or overwrite unrelated user changes.

---

# 69. Pre-Edit Awareness

Before complex edits, the agent should generally:

```text
read file
```

and may use:

```text
git_diff
```

when the working tree already has modifications.

---

# 70. `run_command`

Purpose:

> Execute development commands needed to inspect, build, test, lint, format, or validate the project.

This is the highest-risk MVP tool.

---

# 71. Command Representation

Prefer structured command execution.

Suggested input:

```json
{
  "program": "cargo",
  "args": ["test"],
  "cwd": ".",
  "timeout_seconds": 300
}
```

This is safer and clearer than a single shell string.

---

# 72. Shell Commands

Some tasks require shell syntax.

If supported, shell execution should be explicit:

```json
{
  "shell": true,
  "command": "..."
}
```

Do not treat every command as shell input by default.

---

# 73. `run_command` CWD

`cwd` must remain inside the project root.

Default:

```text
project root
```

---

# 74. Environment

Subprocesses may inherit the normal user environment as needed.

However:

> Provider credentials must not be injected into subprocesses by default.

For example, `NVIDIA_API_KEY` stored internally by Gocode should not automatically become visible to `cargo test`.

---

# 75. Command Output

Capture and stream:

```text
stdout
stderr
```

The TUI should receive output progressively.

---

# 76. Command Result

Suggested metadata:

```rust
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub duration_ms: u64,
}
```

---

# 77. Non-Zero Exit Code

Example:

```text
cargo test
```

returns:

```text
exit code 101
```

The tool itself may still return:

```text
ToolStatus::Success
```

because command execution succeeded.

The output should state:

```text
Process exited with code 101.
```

The agent then reasons about the failure.

---

# 78. Process Spawn Failure

Examples:

- executable not found;
- permission denied;
- invalid cwd.

These should return:

```text
ToolStatus::Failed
```

---

# 79. Command Cancellation

On cancellation:

1. request graceful termination;
2. wait briefly;
3. force termination if needed;
4. collect status;
5. return `Cancelled`.

Windows process-tree termination must be tested carefully.

---

# 80. Command Timeout

If the command exceeds its timeout:

- terminate it;
- return a clear result;
- mark timeout distinctly.

Example:

```text
Command timed out after 300 seconds.
```

---

# 81. Command Risk Evaluation

Before execution, the permission engine should classify the request.

Conceptual enum:

```rust
pub enum CommandRisk {
    Low,
    Medium,
    High,
}
```

---

# 82. Low-Risk Commands

Typical examples:

```text
cargo check
cargo test
cargo fmt --check
git status
git diff
npm test
pnpm test
pytest
go test ./...
```

This list is illustrative, not a hardcoded allowlist.

---

# 83. Higher-Risk Commands

Examples include operations that:

- delete files;
- write outside the workspace;
- change system configuration;
- install globally;
- require elevation;
- modify remote state;
- kill unrelated processes;
- alter credentials.

These should require explicit confirmation or be denied.

---

# 84. Destructive Shell Patterns

The MVP may use conservative heuristics for obvious destructive operations.

Do not attempt to build a perfect shell-security engine.

The architecture should allow improving risk classification later.

---

# 85. `run_command` Permissions

Policy:

```text
evaluate every call
```

The permission engine decides:

```text
Allow
Ask
Deny
```

based on command semantics and workspace context.

---

# 86. `git_status`

Purpose:

> Inspect the current Git working tree state without modifying it.

---

# 87. `git_status` Input

Suggested schema:

```json
{}
```

Optional future:

```json
{
  "path": "."
}
```

MVP can operate on the detected project repository.

---

# 88. `git_status` Behavior

Requirements:

- read-only;
- project repository only;
- return concise porcelain-like information;
- distinguish staged, modified, untracked, deleted.

Example:

```text
Modified:
  src/auth.rs

Untracked:
  src/new_module.rs
```

---

# 89. `git_status` Non-Git Project

Do not fail the whole agent.

Return:

```text
This project is not inside a Git repository.
```

---

# 90. `git_status` Permissions

Default:

```text
Allow
```

---

# 91. `git_diff`

Purpose:

> Inspect current changes without modifying the repository.

---

# 92. `git_diff` Input

Suggested schema:

```json
{
  "path": null,
  "staged": false
}
```

Possible fields:

```text
path    optional
staged  optional
```

---

# 93. `git_diff` Behavior

Requirements:

- read-only;
- workspace repository only;
- bounded output;
- allow file-scoped diff;
- mark truncation.

---

# 94. `git_diff` Permissions

Default:

```text
Allow
```

---

# 95. Git Tool Implementation

Git tools may initially invoke the local `git` executable or use a Rust Git library.

Decision criteria:

- implementation simplicity;
- behavior consistency;
- Windows support;
- diff fidelity.

The agent-facing interface should remain stable regardless of implementation.

---

# 96. No Commit Tool in v0.1.0

Do not expose:

```text
git_commit
git_push
git_reset
git_checkout
```

in the MVP.

The first release should focus on local code changes and validation.

---

# 97. Tool Visibility by Model Capability

If a model does not support tool calling:

```text
do not send tool definitions
```

The TUI should communicate that the selected model cannot operate as a full coding agent.

---

# 98. Tool Visibility by User Intent

Future optimization:

Expose only tools appropriate to the task.

Example read-only request:

```text
list_files
read_file
search
git_status
git_diff
```

Editing request:

```text
+ write_file
+ apply_patch
+ run_command
```

This is optional for the first implementation but desirable.

---

# 99. Tool Description Quality

Tool descriptions directly affect agent quality.

Bad:

```text
Reads files.
```

Better:

```text
Read UTF-8 text from a file inside the current project. Use line ranges for large files. Do not use this tool for binary files.
```

Descriptions should teach the model when and when not to use the tool.

---

# 100. Tool Schema Quality

Schemas should:

- prefer explicit fields;
- avoid loosely typed structures;
- use enums where appropriate;
- enforce minimum/maximum values;
- avoid hidden defaults that create risky behavior.

---

# 101. Tool Output Quality

Results should be optimized for both:

- model reasoning;
- human debugging.

Good outputs are:

- concise;
- deterministic;
- explicit about failures;
- explicit about truncation;
- explicit about paths.

---

# 102. Logging

Tool calls may be logged for local debugging.

Never log sensitive payloads without redaction.

Especially inspect:

- command environment;
- file content;
- tool arguments containing secrets.

---

# 103. Secret Redaction

At minimum, redact known provider credentials from:

- logs;
- TUI tool output;
- error reports.

Future versions may implement broader secret scanning.

---

# 104. File Content Privacy

File contents read by the agent may be sent to the selected model provider as part of tool results.

This is part of the core product behavior and should be documented clearly to users before public release.

---

# 105. Network Tool Policy

No generic internet/network tool in v0.1.0.

The agent should not be able to arbitrarily fetch URLs unless a future explicit tool is designed for that purpose.

Existing network consumers are separate system components:

```text
NVIDIA provider client
GitHub updater
```

---

# 106. Tool Isolation

Tools should not call each other implicitly unless clearly justified.

Example:

`apply_patch` should not silently run tests afterward.

The Agent decides the workflow.

This keeps behavior observable and predictable.

---

# 107. Atomicity

Prefer atomic file writes:

```text
write temp file
↓
flush
↓
replace target
```

where practical.

This reduces corruption risk if the process crashes.

---

# 108. Backups

The MVP does not need to create backup copies for every edit if Git/change tracking is sufficient.

However, architecture should not prevent adding rollback later.

---

# 109. Concurrent Tool Calls

MVP policy:

```text
execute sequentially
```

Even if the provider requests multiple calls.

Benefits:

- simpler permissions;
- clearer event ordering;
- safer writes;
- easier cancellation.

---

# 110. Future Parallelism

Future versions may parallelize clearly independent read-only tools.

Example:

```text
read_file A
read_file B
```

Never parallelize writes without explicit conflict management.

---

# 111. Event Ordering

For each tool:

```text
ToolRequested
↓
ToolStarted
↓
zero or more ToolOutput events
↓
ToolFinished
```

If permission is required:

```text
ToolRequested
↓
PermissionRequested
↓
PermissionResolved
↓
ToolStarted
...
```

---

# 112. Tool Call IDs

IDs must remain stable across:

- provider response;
- tool execution;
- conversation history;
- session persistence;
- TUI events.

---

# 113. Sessions

Persistable tool events should include:

```text
tool name
call id
arguments or safe representation
status
duration
affected files
exit code
```

Do not persist secrets unnecessarily.

---

# 114. Tool Result Context

The model should receive enough output to make the next decision.

The TUI may display a different, more concise representation.

Therefore separate:

```text
model-facing result
human-facing event summary
```

when useful.

---

# 115. Example: Read Flow

```text
Model
↓
read_file("src/auth.rs")
↓
validate path
↓
allow
↓
read lines
↓
ToolResult
↓
Model
```

TUI:

```text
● Reading src/auth.rs
✓ Read src/auth.rs
```

---

# 116. Example: Edit Flow

```text
Model
↓
apply_patch(...)
↓
validate
↓
permission engine
↓
apply atomically
↓
record FileChange
↓
ToolResult
↓
Model
```

TUI:

```text
● Editing src/auth.rs
✓ Modified src/auth.rs
```

---

# 117. Example: Command Flow

```text
Model
↓
run_command(cargo test)
↓
risk evaluation
↓
Allow
↓
spawn process
↓
stream output
↓
exit 101
↓
ToolResult
↓
Model analyzes failure
```

---

# 118. Testing Strategy

Each tool should have:

- unit tests;
- boundary tests;
- invalid input tests;
- cancellation tests where relevant;
- Windows-specific tests where relevant.

---

# 119. Filesystem Test Cases

Minimum coverage:

```text
valid relative path
nested path
missing file
directory instead of file
../ traversal
absolute outside path
symlink escape
ignored file
binary file
large file
Unicode filename
```

---

# 120. Search Test Cases

Minimum coverage:

```text
normal match
no match
many matches
ignored directory
binary files
glob filter
path scope
Unicode text
truncation
cancellation
```

---

# 121. Patch Test Cases

Minimum coverage:

```text
single-file edit
multiple edits
file creation
context mismatch
invalid patch
workspace escape
existing user changes
atomic failure
Unicode
CRLF
```

Windows CRLF behavior is especially important.

---

# 122. Command Test Cases

Minimum coverage:

```text
successful process
non-zero exit
missing executable
stdout
stderr
large output
timeout
cancellation
cwd validation
Unicode arguments
Windows process termination
```

---

# 123. Git Test Cases

Minimum coverage:

```text
clean repository
modified file
staged file
untracked file
non-Git directory
path-scoped diff
large diff truncation
```

---

# 124. Fake Tools

The Agent test suite should use fake tools where possible.

Example:

```rust
struct FakeReadFileTool {
    content: String,
}
```

This keeps Agent tests deterministic and independent of real filesystem behavior.

---

# 125. Tool Contract Tests

All tools should satisfy common invariants:

- never escape workspace unexpectedly;
- always preserve call ID;
- always return explicit status;
- never panic on user/model input;
- report cancellation consistently;
- report truncation explicitly;
- never leak provider secrets.

---

# 126. Performance Guidelines

Avoid:

- recursive scans without limits;
- loading entire huge files;
- buffering unlimited process output;
- cloning large strings unnecessarily;
- repeated canonicalization in tight loops when cacheable.

---

# 127. Large Output Strategy

For large command outputs:

Possible strategy:

```text
keep first N lines
+
keep last N lines
+
mark middle truncated
```

This is often more useful than keeping only the beginning.

---

# 128. Search Strategy

The initial implementation may use Rust libraries.

If performance becomes insufficient, Gocode may switch internally to `ripgrep`.

The tool contract must not change because of that implementation detail.

---

# 129. Process Runner Abstraction

Consider a separate internal abstraction:

```rust
pub trait ProcessRunner {
    async fn run(
        &self,
        request: CommandRequest,
        cancellation: CancellationToken,
    ) -> Result<ProcessResult, ProcessError>;
}
```

Benefits:

- tests;
- Windows-specific implementation;
- future sandboxing.

This trait is justified because process execution is an external-system boundary.

---

# 130. Filesystem Service Abstraction

Do not create a huge virtual filesystem abstraction prematurely.

A small shared path-validation/filesystem service is sufficient.

Centralize:

- root validation;
- normalization;
- binary detection;
- atomic writes.

---

# 131. Security Rule Summary

The MVP tool system must enforce:

1. workspace boundary;
2. no arbitrary network tool;
3. no provider secret inheritance to subprocesses;
4. risk-based command permission;
5. no automatic Git commit/push;
6. bounded outputs;
7. binary file rejection for text tools;
8. safe path normalization;
9. cancellation support;
10. explicit error reporting.

---

# 132. MVP Definition of Done

The tools layer is ready for v0.1.0 when the Agent can reliably perform:

```text
list project
↓
search symbol
↓
read relevant file
↓
apply targeted patch
↓
run validation command
↓
inspect Git diff/status
↓
return results to model
```

while maintaining:

- workspace safety;
- clear permissions;
- cancellation;
- bounded output;
- Windows compatibility;
- deterministic tool contracts.

---

# 133. Reference Workflow

User:

```text
Fix the authentication bug and run the tests.
```

Agent:

```text
search("validate_token")
↓
read_file("src/auth.rs")
↓
git_diff(path="src/auth.rs")
↓
apply_patch(...)
↓
run_command(program="cargo", args=["test"])
↓
git_diff()
↓
final response
```

This is the core workflow the MVP tool layer must support exceptionally well.

---

# 134. Final Rule

The tool layer should be powerful enough for real coding work while remaining narrow, explicit, and observable.

The model requests actions.

Gocode owns execution.

> No model request becomes a local side effect until Gocode has validated that the action exists, is well-formed, stays within the intended boundary, and satisfies the current permission policy.
