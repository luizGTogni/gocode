# Gocode — TUI Specification

**Status:** Initial technical draft  
**Product:** Gocode  
**Target version:** v0.1.0  
**Scope:** Terminal User Interface

---

# 1. Purpose

This document defines the Terminal User Interface (TUI) for Gocode.

The TUI is the primary user experience of the v0.1.0 product.

It must make a complex coding agent feel simple, predictable, and easy to control.

The interface should communicate:

- what the user asked;
- what the agent is doing;
- what files are being read or modified;
- what commands are being executed;
- when the model is thinking;
- when user permission is required;
- when an update is available;
- when something failed;
- what the final result was.

The TUI must not expose unnecessary provider or runtime complexity.

---

# 2. Core UX Principle

The TUI must optimize for:

```text
clarity
+
speed
+
low cognitive load
+
keyboard-first interaction
```

The user should not need to understand:

- tool schemas;
- provider request formats;
- stream protocols;
- reasoning parameters;
- internal event buses;
- config precedence;
- updater internals.

The interface should always answer three questions:

1. What is happening?
2. Do I need to do anything?
3. What changed?

---

# 3. Non-Negotiable Product Rule

Ease of use is a hard requirement.

The normal path should be:

```text
open terminal
↓
run gocode
↓
type request
↓
agent works
↓
review result
```

No manual config editing should be required for ordinary use.

---

# 4. Technology

Primary stack:

```text
Ratatui
Crossterm
Tokio
```

Responsibilities:

```text
Ratatui   → layout/rendering
Crossterm → terminal input/output
Tokio     → async runtime/events
```

---

# 5. TUI Architecture

The TUI must be event-driven.

Conceptually:

```text
keyboard input
agent events
tool events
provider events
update events
system events
        ↓
     AppEvent
        ↓
     AppState
        ↓
      render
```

Rendering must be derived from state.

---

# 6. TUI Boundary

The TUI may:

- render;
- collect input;
- move selection;
- open modals;
- send `AppCommand`s;
- display `AppEvent`s.

The TUI must not:

- call NVIDIA directly;
- execute tools;
- run commands;
- edit files;
- write credentials directly;
- install updates directly;
- contain model-specific reasoning logic.

---

# 7. App State

Conceptual structure:

```rust
pub struct AppState {
    pub screen: Screen,
    pub conversation: ConversationViewState,
    pub composer: ComposerState,
    pub agent: AgentViewState,
    pub provider: ProviderViewState,
    pub model: ModelViewState,
    pub modal: Option<ModalState>,
    pub notifications: Vec<Notification>,
    pub terminal: TerminalState,
}
```

The exact fields may evolve.

---

# 8. Screens

Top-level screens:

```rust
pub enum Screen {
    Boot,
    Onboarding,
    Chat,
    ModelPicker,
    ProviderPicker,
    Config,
    Help,
}
```

Most work happens in:

```text
Chat
```

---

# 9. Modals

Modals are overlays, not separate full screens.

```rust
pub enum ModalState {
    Permission(PermissionModal),
    Update(UpdateModal),
    Error(ErrorModal),
    ConfirmExit,
}
```

Potential future:

```text
SessionPicker
CommandPalette
```

---

# 10. Startup Philosophy

The TUI should appear as early as possible.

Do not wait for:

- GitHub update checks;
- full model metadata refresh;
- cache refresh;
- non-essential background work.

Preferred sequence:

```text
terminal setup
↓
render immediately
↓
load required config
↓
continue initialization asynchronously
```

---

# 11. Boot Screen

Boot should be short-lived.

Example:

```text
Gocode

Starting...
```

Avoid elaborate splash screens that slow access.

If startup is fast enough, the boot screen may be visually minimal.

---

# 12. First Launch Detection

If no valid provider configuration exists:

```text
Boot
↓
Onboarding
```

If configuration is valid:

```text
Boot
↓
Chat
```

---

# 13. Onboarding Goals

Onboarding should answer only what Gocode cannot infer.

For v0.1.0:

1. provider;
2. API key;
3. model.

Since NVIDIA is the only MVP provider, provider selection may initially be visually simple.

---

# 14. Onboarding — Welcome

Example:

```text
┌──────────────────────────────────────────────────────────────┐
│                                                              │
│                         G O C O D E                          │
│                                                              │
│              Your terminal coding agent                      │
│                                                              │
│  Let's connect your AI provider.                             │
│                                                              │
│  Provider                                                    │
│  > NVIDIA NIM                                                │
│                                                              │
│                         Continue                             │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

Do not overload this screen with documentation.

---

# 15. Onboarding — API Key

Example:

```text
NVIDIA API Key
────────────────────────────────────────────────────────

Paste your API key:

> nvapi-••••••••••••••••••••••

Enter connect   Esc back
```

Requirements:

- mask secret;
- paste must work;
- never echo raw secret after submission;
- validation state should be obvious.

---

# 16. Credential Validation State

While validating:

```text
Checking your NVIDIA API key...
```

On success:

```text
✓ Connected to NVIDIA NIM
```

On invalid key:

```text
Your NVIDIA API key was rejected.

Check the key and try again.

[ Try again ]
```

On network failure:

```text
Could not reach NVIDIA.

Your key was not changed.

[ Retry ]
```

Do not confuse connectivity errors with credential errors.

---

# 17. Onboarding — Model Picker

Example:

```text
Select a model

NVIDIA NIM

> Model A
  Tools       ✓
  Thinking    ✓
  Streaming   ✓

  Model B
  Tools       ✓
  Thinking    -
  Streaming   ✓

Enter select   ↑↓ move   Esc back
```

The picker must be capability-driven.

---

# 18. Model Information Density

Show only useful capability information.

Recommended:

```text
Tools
Thinking
Context
Vision
```

Only show fields that help selection.

Avoid exposing raw API metadata.

---

# 19. Recommended Model

If Gocode can confidently identify a suitable default:

```text
> Model A          Recommended
```

Do not mark a model as recommended unless the metadata/ranking is reliable.

---

# 20. Chat Screen

The Chat screen is the main product.

Recommended conceptual layout:

```text
┌───────────────────────────────────────────────────────────────┐
│ Gocode                                      NVIDIA • Model X  │
├───────────────────────────────────────────────────────────────┤
│                                                               │
│ You                                                           │
│ Fix the authentication bug and run the tests.                 │
│                                                               │
│ Gocode                                                        │
│ I'll inspect the authentication flow.                         │
│                                                               │
│ ● Searching "validate_token"                                  │
│ ✓ Read src/auth.rs                                            │
│ ✓ Modified src/auth.rs                                        │
│ ● Running cargo test                                          │
│                                                               │
│ ...                                                           │
│                                                               │
├───────────────────────────────────────────────────────────────┤
│ > Ask Gocode...                                               │
├───────────────────────────────────────────────────────────────┤
│ Enter send • Esc stop • / commands                           │
└───────────────────────────────────────────────────────────────┘
```

---

# 21. Layout Zones

The Chat screen should contain three primary zones:

```text
Header
Conversation / activity
Composer + status footer
```

Avoid adding side panels in the MVP.

---

# 22. Header

The header should communicate current context.

Recommended content:

```text
Gocode                         NVIDIA • Model Name
```

Optional indicators:

```text
Thinking: Auto
Git branch
```

Only add indicators if they remain visually lightweight.

---

# 23. Header Rules

The header should not become a dashboard.

Do not display all of:

- token counts;
- API endpoint;
- model ID;
- context usage;
- agent turn count;
- request latency;
- provider status;

by default.

Advanced details can exist in `/config` or future debug views.

---

# 24. Conversation Area

The conversation area shows:

- user messages;
- assistant text;
- agent activity;
- tool results at summary level;
- final output;
- errors related to the run.

---

# 25. User Message Rendering

Example:

```text
You
Fix the authentication bug and run the tests.
```

User messages must remain visually distinguishable from Gocode responses.

---

# 26. Assistant Message Rendering

Example:

```text
Gocode
I'll inspect the authentication flow first.
```

Streaming text should appear progressively.

---

# 27. Agent Activity Rendering

Observable actions should appear inline.

Examples:

```text
● Thinking
● Searching "validate_token"
● Reading src/auth.rs
● Editing src/auth.rs
● Running cargo test
```

Completed actions:

```text
✓ Read src/auth.rs
✓ Modified src/auth.rs
✓ cargo test
```

Failed actions:

```text
× cargo test
```

---

# 28. Activity Philosophy

The activity feed should make the agent transparent without becoming verbose.

Do not show raw internal events such as:

```text
ToolCallDelta index=0
ProviderChunk
State::Inference
```

Show meaningful actions.

---

# 29. Thinking Display

Thinking is a state, not a raw chain-of-thought viewer.

Recommended:

```text
● Thinking
```

Optionally:

```text
● Thinking about the authentication flow
```

only if derived from safe high-level status, not hidden reasoning text.

---

# 30. Thinking Completion

When thinking ends, either:

- remove transient spinner and continue;
- convert to a subtle completed state;
- collapse it.

Do not fill history with repeated `Thinking` rows.

---

# 31. Tool Display Model

Each tool should map to a user-friendly phrase.

Examples:

```text
list_files   → Listing files
read_file    → Reading <path>
search       → Searching "<query>"
apply_patch  → Editing <path>
write_file   → Writing <path>
run_command  → Running <command>
git_status   → Checking Git status
git_diff     → Reviewing changes
```

---

# 32. Tool Technical Details

The default TUI should hide raw JSON arguments.

Future expandable detail could show:

```text
read_file
path: src/auth.rs
lines: 1-200
```

But that is not required for the MVP.

---

# 33. Command Output

`run_command` may produce a lot of output.

Default behavior:

```text
● Running cargo test
  Compiling ...
  ...
```

The TUI should:

- stream recent output;
- cap displayed history;
- preserve final exit information;
- allow scrolling.

---

# 34. Command Completion

Success example:

```text
✓ cargo test
  18 tests passed
```

Failure example:

```text
× cargo test
  Process exited with code 101
```

The model can continue after a failed command.

---

# 35. File Changes

When a file changes:

```text
✓ Modified src/auth.rs
```

For creation:

```text
✓ Created src/token.rs
```

For deletion, if supported later:

```text
✓ Deleted src/old.rs
```

---

# 36. Existing User Changes

The UI should not imply that all Git changes were created by Gocode.

If relevant:

```text
src/auth.rs already had local changes
```

This should be surfaced only when it affects safety or understanding.

---

# 37. Composer

The composer is the text input area.

Example:

```text
> Ask Gocode...
```

Requirements:

- multiline input;
- paste;
- cursor navigation;
- deletion;
- Unicode;
- horizontal/vertical resize;
- clear focus state.

---

# 38. Enter Behavior

Recommended:

```text
Enter      → send
Shift+Enter → newline
```

If Shift+Enter detection is unreliable across target terminals, use:

```text
Enter      → send
Alt+Enter / Ctrl+Enter → newline
```

The final choice must be tested on Windows Terminal, PowerShell, and cmd environments.

Ease of use wins over convention purity.

---

# 39. Input During Active Agent Run

MVP recommendation:

While the agent is running:

- keep composer visible;
- disable submission or clearly indicate active run;
- `Esc` cancels current run.

Possible footer:

```text
Agent is working • Esc stop
```

Do not start two concurrent AgentRuns in one session.

---

# 40. Queued Messages

Queued input is not required for v0.1.0.

Future versions may allow typing the next request while the current one runs.

---

# 41. Keyboard Shortcuts

MVP shortcuts:

| Key | Action |
|---|---|
| `Enter` | Send / confirm |
| `Esc` | Cancel current operation / close modal |
| `Ctrl+C` | Safe exit |
| `↑` / `↓` | Navigate selection or history |
| `Tab` | Move focus when needed |
| `PageUp` / `PageDown` | Scroll conversation |
| `Home` / `End` | Context-sensitive navigation |

Avoid requiring users to memorize many shortcuts.

---

# 42. Footer

Footer should show only relevant shortcuts.

Normal chat:

```text
Enter send • Esc stop • / commands
```

Model picker:

```text
Enter select • ↑↓ move • Esc back
```

Permission modal:

```text
Enter run • Esc cancel
```

The footer should change with context.

---

# 43. Slash Commands

MVP:

```text
/model
/provider
/config
/clear
/help
/init
/skills
/exit
```

Slash commands should be discoverable.

Typing:

```text
/
```

may show lightweight suggestions.

---

# 44. Slash Command Autocomplete

Recommended behavior:

```text
/
↓
small suggestion list
```

Example:

```text
/model      Change model
/provider   Change provider
/config     Open settings
/clear      Clear conversation
/help       Show help
/init       Write an AGENTS.md overview of the project
/skills     List discovered skills
/exit       Exit Gocode
```

Not mandatory for the first internal build, but desirable before v0.1.0.

---

# 45. `/model`

Opens the model picker.

After selection:

```text
✓ Model changed to Model X
```

The header updates immediately.

---

# 46. `/provider`

Opens provider selection.

In v0.1.0 this may show only:

```text
NVIDIA NIM
```

The command still exists to preserve future architecture.

---

# 47. `/config`

Opens a simple configuration screen.

Do not expose every internal setting.

Recommended MVP options:

```text
Provider
Model
Thinking
Update checks
```

Possibly:

```text
Open config location
```

only if useful.

---

# 48. `/clear`

Clears the visible/current conversation after appropriate confirmation if needed.

Do not delete project files or global history unrelated to the active session.

---

# 49. `/help`

Shows:

- basic controls;
- slash commands;
- short explanation of what Gocode can do.

Keep it compact.

---

# 50. `/exit`

Safely exits.

Equivalent to graceful shutdown.

---

# 51. Provider Picker

General design:

```text
Select provider

> NVIDIA NIM
```

Future:

```text
  OpenAI
  Anthropic
  Gemini
```

If provider requires setup:

```text
Not connected
```

---

# 52. Config Screen

Example:

```text
Settings

Provider      NVIDIA NIM
Model         Model X
Thinking      Auto
Updates       Check on startup

Enter change   Esc back
```

The user should not need to know file paths or TOML syntax.

---

# 53. Thinking Settings UI

Capability-driven.

If unsupported:

```text
Thinking      Not supported
```

If toggle:

```text
Thinking
> Auto
  On
  Off
```

If effort:

```text
Thinking
> Auto
  Low
  Medium
  High
```

If model-specific values differ:

```text
> Auto
  None
  High
  Max
```

The TUI must display exactly the supported normalized choices.

---

# 54. Thinking Budget UI

If a model supports token budgets, avoid forcing the user into advanced numeric configuration during onboarding.

Default:

```text
Thinking      Auto
```

Advanced config may expose:

```text
Budget        8192
```

later.

For v0.1.0, Auto can hide complexity.

---

# 55. Permission Modal

Permission prompts must interrupt clearly but minimally.

Example:

```text
┌───────────────────────────────────────────────────────┐
│ Run command?                                          │
│                                                       │
│ cargo install some-package                            │
│                                                       │
│ Working directory:                                    │
│ C:\dev\project                                        │
│                                                       │
│ [ Run ]                          [ Cancel ]            │
└───────────────────────────────────────────────────────┘
```

---

# 56. Permission Modal Requirements

Must show:

- action;
- target/context;
- clear choices.

Do not show:

```text
RiskClass::Medium
PolicyEngine::Ask
```

unless debug mode exists.

---

# 57. Permission Denial

After denial:

```text
Command cancelled.
```

The Agent receives a denied tool result and may continue.

Do not make denial feel like an application error.

---

# 58. Update Check UX

Update checking happens asynchronously after startup.

If no update:

```text
show nothing
```

If update exists:

```text
show update modal
```

---

# 59. Update Modal

Example:

```text
┌──────────────────────────────────────────────────────┐
│ Gocode 0.2.0 is available                           │
│                                                      │
│ You're using 0.1.0                                  │
│                                                      │
│ [ Update now ]                 [ Not now ]           │
└──────────────────────────────────────────────────────┘
```

---

# 60. Update Rejection

If the user selects:

```text
Not now
```

close the modal and continue normally.

The same update may be offered again on the next startup.

---

# 61. Update Progress

When accepted:

```text
Updating Gocode...

Downloading 0.2.0
Verifying update
Restarting...
```

Avoid overly technical progress.

---

# 62. Update Failure

If download or validation fails:

```text
Gocode could not update.

Your current installation was not changed.

[ Continue ]
```

The application must remain usable.

---

# 63. Error Modal

Errors that require user attention should appear in a modal.

Example:

```text
┌──────────────────────────────────────────────────────┐
│ Could not reach NVIDIA                              │
│                                                      │
│ Check your connection and try again.                │
│                                                      │
│ [ Retry ]                      [ Close ]             │
└──────────────────────────────────────────────────────┘
```

---

# 64. Inline Errors

Not every error deserves a modal.

Tool failures during an AgentRun may appear inline:

```text
× cargo test
  2 tests failed
```

The agent can continue.

---

# 65. Error Severity

Conceptual:

```rust
enum ErrorSeverity {
    Info,
    Warning,
    Recoverable,
    Blocking,
}
```

The TUI presentation depends on severity.

---

# 66. Notifications

Small non-blocking status messages may appear as transient notifications.

Examples:

```text
Model changed
Configuration saved
Session cleared
```

Avoid excessive toast-like noise.

---

# 67. Loading States

Never leave the interface visually frozen.

Examples:

```text
● Connecting to NVIDIA
● Loading models
● Thinking
● Running tests
```

Use lightweight animation if desired.

---

# 68. Spinner Behavior

Use one consistent spinner style.

Do not animate every row.

Animation should communicate active work, not decorate the interface.

---

# 69. Terminal Resize

The TUI must handle resize events immediately.

Rules:

- never panic;
- preserve input;
- preserve scroll position when possible;
- adapt layout;
- collapse secondary details first.

---

# 70. Minimum Terminal Size

The app should define a minimum usable size.

If too small:

```text
Gocode needs a little more terminal space.

Resize the terminal to continue.
```

Do not render broken overlapping widgets.

---

# 71. Narrow Layout

On narrow terminals:

- hide less important header metadata;
- keep chat readable;
- wrap text;
- keep composer usable;
- keep modals within bounds.

---

# 72. Wide Layout

Do not automatically create sidebars just because space exists.

The MVP should preserve a centered/simple conversational layout.

---

# 73. Scrolling

Conversation history must be scrollable.

Requirements:

- mouse wheel optional;
- keyboard scrolling required;
- new streaming content should auto-follow only when user is already near the bottom.

---

# 74. Scroll Lock Behavior

If the user scrolls upward while the agent is streaming:

Do not snap them back to the bottom every frame.

Show a subtle indicator:

```text
↓ New activity
```

Future optional behavior.

---

# 75. Mouse Support

Mouse support is optional for v0.1.0.

Keyboard behavior must be complete without it.

If mouse support is implemented:

- scroll;
- click selection;
- no mouse-only actions.

---

# 76. Copy Behavior

Terminal-native text selection/copy behavior should remain usable.

Avoid unnecessary mouse capture if it prevents normal terminal copying.

This must be tested carefully with Windows Terminal.

---

# 77. Paste Behavior

Pasting multi-line prompts must work.

The composer should not accidentally submit each pasted line separately.

Bracketed paste support should be used where available.

---

# 78. Large Paste

For very large pasted text:

- accept reasonably;
- avoid UI freeze;
- optionally show length;
- do not silently truncate user input.

---

# 79. Unicode

The TUI must support:

- Unicode project names;
- Unicode file paths;
- Portuguese text;
- emoji where terminal supports them.

Do not rely on emoji for critical meaning.

Example:

```text
✓
×
●
```

should have text context where needed.

---

# 80. Color Philosophy

Color can help hierarchy but must not carry meaning alone.

For example, failure should show:

```text
× Failed
```

not just red text.

The MVP should work acceptably in terminals with limited color support.

---

# 81. Theme

MVP can initially use terminal/default theme behavior.

Potential setting:

```text
theme = system
```

Do not make theme customization a blocker for v0.1.0.

---

# 82. Accessibility

The TUI should avoid:

- low-contrast-only distinctions;
- color-only status;
- rapidly flashing animations;
- dense walls of metadata.

Keyboard-first operation is mandatory.

---

# 83. Focus Model

At most one primary input target should be active.

Normal Chat:

```text
composer focused
```

Modal open:

```text
modal owns focus
```

Picker open:

```text
picker owns focus
```

---

# 84. Modal Escape

`Esc` should normally close the topmost non-destructive modal.

Exception:

During an active AgentRun without modal:

```text
Esc → cancel AgentRun
```

Context must remain predictable.

---

# 85. Ctrl+C

`Ctrl+C` should perform safe shutdown.

If a destructive or active operation is in progress, Gocode may:

- cancel it;
- then exit;
- or require a second Ctrl+C only if absolutely necessary.

Avoid trapping users inside the TUI.

---

# 86. Terminal Restoration

The TUI must always attempt to restore:

```text
cursor
raw mode
alternate screen
```

on:

- normal exit;
- Ctrl+C;
- recoverable fatal error;
- panic hook.

---

# 87. Panic UX

If Gocode crashes after terminal restoration:

Print a concise message outside the alternate screen.

Example:

```text
Gocode encountered an unexpected error.

A local log was written to:
C:\Users\...\ .gocode\logs\...
```

Never print secrets.

---

# 88. Conversation Rendering Model

The conversation should use semantic blocks.

Conceptually:

```rust
enum ConversationItem {
    UserMessage,
    AssistantMessage,
    AgentActivity,
    ToolActivity,
    Error,
    CompletionSummary,
}
```

This is preferable to storing one preformatted string.

---

# 89. Streaming Assistant Message

While text is streaming:

```text
AssistantMessage {
    complete: false
}
```

When finished:

```text
complete: true
```

This makes cursor/spinner rendering easier.

---

# 90. Activity Lifecycle

Example:

```text
ToolRequested
↓
row created
↓
ToolStarted
↓
row active
↓
ToolFinished
↓
row completed
```

Prefer updating one row over appending duplicate rows.

---

# 91. Tool Output Expansion

MVP may show a small amount of inline output.

Future:

```text
Enter → expand tool details
```

Not required initially.

---

# 92. Conversation Density

The interface should feel compact.

Avoid large decorative gaps between every action.

The product is a coding tool, not a presentation.

---

# 93. Final Response Emphasis

The final response should visually stand apart from transient activity.

Example:

```text
Gocode

Fixed the token expiration check in `src/auth.rs`.

Validation:
✓ cargo test — 18 tests passed
```

---

# 94. Modified Files Summary

At run completion, optionally show:

```text
Changed files
  src/auth.rs
  src/middleware.rs
```

This can be part of the final response or a compact run summary.

---

# 95. Failed Validation Summary

Example:

```text
Validation
× cargo test — 2 tests still failing
```

The final response should never visually imply success when validation failed.

---

# 96. Session Startup Context

When opening Gocode in a project:

Header or initial system line may show:

```text
Project: my-app
```

Do not display the full absolute path unless useful.

---

# 97. Project Initialization

Creating local `.gocode` should normally be silent.

Do not interrupt the user with:

```text
Created .gocode directory
Created project.toml
Created sessions folder
```

unless something fails.

Automation should feel invisible.

---

# 98. Git Context

The header may eventually show branch:

```text
main
```

This is optional.

Do not delay MVP for Git decoration.

---

# 99. Model Unavailable State

If the saved model is no longer available:

```text
Your previous model is no longer available.

Choose another model to continue.
```

Open model picker automatically.

---

# 100. Provider Disconnected State

If credentials disappear or become invalid:

```text
NVIDIA needs to be reconnected.

[ Update API key ]
```

Preserve the current project/session state.

---

# 101. Offline Update Check

If GitHub cannot be reached:

```text
show nothing
```

Update check failure is non-essential.

Do not show scary startup warnings.

---

# 102. Offline Provider

If provider inference is unavailable:

The TUI should remain interactive.

User should be able to:

- retry;
- open config;
- change provider/model in future;
- exit safely.

---

# 103. App Commands

The TUI should emit commands instead of calling services directly.

Conceptual:

```rust
pub enum AppCommand {
    SubmitPrompt(String),
    CancelAgent,
    SelectModel(ModelId),
    SelectProvider(ProviderId),
    SaveCredential(...),
    AcceptPermission(...),
    RejectPermission(...),
    AcceptUpdate,
    RejectUpdate,
    ClearConversation,
    Exit,
}
```

---

# 104. App Events

The TUI consumes normalized events.

Conceptual:

```rust
pub enum AppEvent {
    BootCompleted,
    AgentStarted,
    AgentTextDelta(String),
    AgentThinkingState(...),
    ToolRequested(...),
    ToolStarted(...),
    ToolOutput(...),
    ToolFinished(...),
    FileChanged(...),
    UpdateAvailable(...),
    Error(...),
    AgentCompleted(...),
}
```

---

# 105. Event Ordering

The UI may assume logical ordering such as:

```text
AgentStarted
↓
ToolRequested
↓
ToolStarted
↓
ToolFinished
↓
AgentCompleted
```

The runtime must preserve this contract.

---

# 106. Event Coalescing

Streaming may generate many tiny events.

The TUI runtime may coalesce text deltas before rendering.

Goal:

- smooth output;
- low CPU;
- no visible lag.

Do not redraw thousands of times per second.

---

# 107. Render Tick

Use event-driven redraw plus a modest tick for:

- spinner animation;
- transient notifications.

Do not use an aggressive fixed refresh rate without need.

---

# 108. Performance

The TUI must avoid:

- rendering entire unbounded history every frame;
- cloning large conversation buffers;
- blocking on filesystem;
- blocking on network;
- expensive markdown parsing per frame.

Use viewport-based rendering.

---

# 109. Markdown Rendering

Assistant responses may contain Markdown.

MVP should support useful basics:

```text
paragraphs
inline code
code blocks
lists
bold emphasis if practical
```

Do not attempt full browser-grade Markdown rendering.

---

# 110. Code Blocks

Code blocks must remain readable.

Example:

```text
fn validate_token(...) {
    ...
}
```

Horizontal overflow strategy should be predictable.

Wrapping code is acceptable if needed for MVP, but preserving indentation is important.

---

# 111. File Paths

Render project-relative file paths whenever possible.

Example:

```text
src/auth.rs
```

instead of:

```text
C:\Users\alice\projects\foo\src\auth.rs
```

---

# 112. Command Rendering

Prefer reconstructing a readable command:

```text
cargo test
```

from structured process input.

Quote arguments only when needed.

---

# 113. Secret Rendering

Secrets must never be displayed after submission.

API key input:

```text
••••••••••••
```

Logs and errors must be redacted.

---

# 114. Help Screen

Example:

```text
Gocode Help

Enter       Send
Esc         Stop current task / go back
Ctrl+C      Exit
/model      Change model
/provider   Change provider
/config     Settings
/clear      Clear conversation
/help       Help
/init       Write an AGENTS.md overview
/skills     List discovered skills
/exit       Exit
```

Keep the screen short enough to scan quickly.

---

# 115. Config Persistence Feedback

After changing a setting:

```text
✓ Saved
```

Do not require an explicit Save button for each field if changes can safely persist immediately.

---

# 116. Destructive Settings

If a future setting can destroy data, require explicit confirmation.

No such setting is required in the v0.1.0 config screen.

---

# 117. Empty Chat State

After onboarding:

```text
What do you want to build?

> Ask Gocode...
```

Optional examples may be shown subtly:

```text
Fix this test
Explain this module
Add an endpoint
Refactor authentication
```

Avoid making the empty state noisy.

---

# 118. First Prompt Experience

The first successful prompt should quickly demonstrate:

- streaming;
- visible activity;
- tool use;
- clear final result.

This is a major product-quality target.

---

# 119. Chat-Only Model State

If selected model lacks tools:

Header or notice:

```text
Chat-only model

This model cannot read, edit, or run your project.
```

Provide:

```text
[ Choose another model ]
```

The user can still chat if desired.

---

# 120. Tool-Capable Model State

No special banner needed.

The user should simply experience the coding agent.

---

# 121. Thinking Capability Display

Model picker may show:

```text
Thinking ✓
```

Config controls depend on normalized capability.

Do not display raw provider field names such as:

```text
reasoning_effort
chat_template_kwargs
```

---

# 122. Provider Branding

Use textual provider naming:

```text
NVIDIA NIM
```

Avoid relying on vendor logos in the TUI.

This keeps rendering portable.

---

# 123. Update Version Display

Use normalized semantic version:

```text
0.1.0 → 0.2.0
```

Git tag formatting such as `v0.2.0` may remain internal.

---

# 124. Status Bar Priority

Footer/status space is limited.

Priority:

1. current actionable shortcut;
2. active operation status;
3. provider/model context;
4. secondary metadata.

Do not crowd it.

---

# 125. Modal Priority

Only one modal should own interaction at a time.

Priority examples:

```text
permission prompt > update prompt
blocking error > update prompt
```

An update notification should never interrupt a critical permission decision.

---

# 126. Update Timing During Agent Run

If update availability is detected while the agent is actively working:

Recommended MVP behavior:

- store pending update notification;
- show modal after the AgentRun completes.

Do not interrupt the coding task.

---

# 127. Error During Permission Prompt

If the underlying run fails while permission modal is open:

- close/invalidate the modal;
- show the relevant error;
- prevent stale permission acceptance.

---

# 128. Stale Events

Events should carry IDs where necessary.

Examples:

```text
AgentRunId
ToolCallId
```

The TUI should ignore events belonging to obsolete runs when appropriate.

---

# 129. Testing Strategy

TUI tests should focus primarily on state transitions, not pixel-perfect terminal snapshots.

Categories:

```text
state reducer tests
keyboard tests
modal tests
layout tests
event ordering tests
resize tests
```

---

# 130. State Tests

Examples:

```text
AgentStarted → composer disabled
AgentCompleted → composer enabled
UpdateAvailable → modal queued
PermissionRequested → permission modal
Esc during run → CancelAgent command
```

---

# 131. Keyboard Tests

Minimum:

```text
Enter send
Esc cancel
Ctrl+C exit
picker navigation
modal acceptance
modal rejection
slash command navigation
multiline input
```

---

# 132. Resize Tests

Test:

```text
wide
normal
narrow
below minimum
```

No panic or invalid geometry.

---

# 133. Snapshot Tests

Snapshot tests can be useful for key screens:

```text
Onboarding
Chat idle
Chat running
Permission modal
Update modal
Error modal
Model picker
```

Do not make snapshots so brittle that every wording change breaks the suite.

---

# 134. Windows Manual Test Matrix

Before v0.1.0, manually validate:

```text
Windows Terminal + PowerShell
Windows Terminal + cmd
standalone PowerShell window
Unicode project path
paste
resize
Ctrl+C
Esc cancellation
API key input
command output
updater flow
```

---

# 135. UX Acceptance Criteria

The TUI is ready for v0.1.0 when:

- a new user can complete onboarding without editing files;
- normal chat is immediately understandable;
- active work is visible;
- tool activity is understandable;
- permissions are clear;
- errors are actionable;
- streaming does not freeze;
- resize works;
- cancellation works;
- secrets remain hidden;
- model/thinking options are capability-driven;
- updates do not block or interrupt active work;
- the app remains fully keyboard-usable.

---

# 136. Reference First-Run Flow

```text
gocode
↓
Welcome
↓
NVIDIA API key
↓
Connected
↓
Model picker
↓
Chat
↓
"What do you want to build?"
```

---

# 137. Reference Coding Flow

```text
User
"Fix authentication and run the tests."
↓
● Thinking
↓
● Searching "validate_token"
↓
✓ Read src/auth.rs
↓
✓ Modified src/auth.rs
↓
● Running cargo test
↓
✓ 18 tests passed
↓
Gocode final response
```

---

# 138. Reference Permission Flow

```text
Agent requests higher-risk command
↓
Permission modal
↓
User selects Run
↓
Tool execution
↓
Agent continues
```

or:

```text
User selects Cancel
↓
Denied ToolResult
↓
Agent continues or explains limitation
```

---

# 139. Reference Update Flow

```text
Chat opens
↓
background update check
↓
new version detected
↓
if agent idle → show modal
if agent active → defer
↓
Update now / Not now
```

---

# 140. `/init`

Asks the agent to explore the project and write a complete `AGENTS.md` at the project root:
purpose, structure, build/test/lint commands, and conventions an AI coding agent should follow.

Implemented as a canned prompt sent through the normal chat path, the same way a user would
type the request by hand — the agent uses its existing file tools, no dedicated tool needed.

Distinct from automatic `.gocode/` project bootstrap, which happens unconditionally at every
startup and needs no command.

---

# 141. `/skills`

Lists skills discovered from `~/.agents/skills/` (global) and the project's `.agents/skills/`
(or `.gocode/skills/` fallback when `.agents/` doesn't exist). Each skill's name and description
are also surfaced to the model as a system message so it can read the full file on demand via
the existing read-file tool.

---

# 142. Final Rule

The Gocode TUI should not feel like an interface for an AI API.

It should feel like a focused development tool.

> Every screen, status line, modal, and shortcut should reduce uncertainty and help the user move from intent to working code with as little friction as possible.
