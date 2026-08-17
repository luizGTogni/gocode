# Gocode — Subagents Architecture

**Status:** MVP implemented (engine, persistence, TUI commands)
**Product:** Gocode
**Target version:** v0.5.x
**Scope:** Supervisor-delegated subagents

---

# 1. Purpose

This document describes the subagent system: how the main agent (the "supervisor") delegates
bounded subtasks to isolated workers ("subagents"), how those subagents are tracked, and how their
results and file changes flow back into the main session.

The core principle: subagents increase parallelism without expanding permissions or creating
conflicts. By default a subagent investigates and reports back; only `--mode implement` may write
files, and then only inside a worktree created for it.

# 2. Model

Supervisor + isolated workers, entirely in-process:

- **Supervisor** — the running Gocode session (TUI + its own `Agent`). It is the only thing that
  talks to the user. It creates, tracks, messages, stops, and consolidates subagents via
  `/agent ...` commands.
- **Subagent** — a short-lived `gocode_agent::Agent` instance driven by `SubagentManager`, given a
  minimal, explicit context (not the supervisor's full conversation), that runs to completion (or
  timeout/stop) and returns a structured result.
- **No nesting in the MVP.** Spawning a subagent is a supervisor-side operation triggered by a
  slash command; it is never exposed as an LLM-callable tool, so a subagent's own tool registry has
  no way to spawn another subagent.

# 3. Crates involved

| Crate | Responsibility |
|---|---|
| `gocode-core` | `SubagentRecord`/`SubagentResult`/`SubagentMode`/`SubagentStatus` data model, JSON persistence, restart recovery (`crates/gocode-core/src/subagent.rs`); `AppCommand`/`AppEvent` variants (`crates/gocode-core/src/lib.rs`). |
| `gocode-agent` | `SubagentManager`: spawn, concurrency, timeout, cooperative stop, message delivery, worktree claiming, structured-result parsing (`crates/gocode-agent/src/subagent_manager.rs`). Reuses `Agent`/`AgentRequest`/`AgentLimits` unchanged. |
| `gocode-tools` | Reused as-is: `worktree` (create/list/remove), `permissions` (`PermissionContext`/`PermissionPolicy`), `process::redact_secrets`. No changes needed. |
| `gocode-tui` | `SlashCommand::Agent`/`Agents`, `parse_agent_command`, dispatch, and rendering of `AgentNotice`/`AgentProgress`/`AgentDiffReady`/`AgentCleanupWarning` events as chat entries. |
| `gocode` (bin) | Boots `SubagentManager`, bridges its events into `AppEvent`s, implements every `AppCommand::Agent*` handler (including the `git diff`/`git merge`/`git worktree remove` orchestration for apply/cleanup). |

# 4. Lifecycle

```
Queued -> Running -> Completed
        <-------->  WaitingInput   (subagent ended a message with `NEEDS_INPUT: <question>`;
                                     resumes as Running once `/agent message <id> <answer>` lands)
Running -> Failed
Running -> Stopped        (via /agent stop)
Running -> TimedOut       (per_task_timeout exceeded, bounds WaitingInput too)

Any of {Queued, Running, WaitingInput} found on disk at startup -> Interrupted
```

`SubagentManager::spawn` persists a `Queued` record immediately, then a `tokio::spawn`ed task
acquires a `tokio::sync::Semaphore` permit — this **is** the concurrency queue: over-capacity
spawns simply wait for a permit while their status stays `Queued`. Once running, the task drives a
scoped `Agent` and wraps the whole thing in `tokio::select!` against a timeout `sleep` and a
per-subagent `CancellationToken` (fired by `/agent stop`), so both cases cut the run off by
dropping its future rather than requiring the provider to cooperate.

# 5. Context passed to a subagent

Built by `render_objective` (`subagent_manager.rs`), **not** the supervisor's conversation history:

- the objective, in the supervisor's own words;
- the project's `AGENTS.md` / `.gocode/instructions.md` content (same as the supervisor loads);
- the work mode and a request to end the final message with a fenced ` ```json ` block matching
  `SubagentResult`'s shape;
- instructions for pausing instead of guessing: end the message with `NEEDS_INPUT: <question>`
  (no JSON block) when clarification is needed before the task can continue.

Explicitly **not** included: the supervisor's chat history, secrets, or other subagents' results.

# 6. Structured result

```rust
struct SubagentResult {
    summary: String,
    findings: Vec<String>,
    files_read: Vec<String>,
    files_changed: Vec<String>,
    commands_run: Vec<String>,
    tests_run: Vec<String>,
    risks: Vec<String>,
    next_steps: Vec<String>,
    worktree_path: Option<PathBuf>,
    error: Option<String>,
}
```

`parse_result` looks for a fenced ` ```json ` block in the subagent's final message and parses it
into this shape; if none is found (or it doesn't parse), it falls back to `summary: <raw text>`
with no error, rather than losing the subagent's work.

# 7. Worktree isolation

Only `--mode implement` may write files, and only inside a worktree:

1. `spawn()` calls the existing `gocode_tools::worktree::create_worktree`, landing at
   `<repo>-worktrees/subagent-<id-prefix>` on a fresh branch off the current one — the same
   mechanism `/worktree` already uses.
2. The subagent's `Agent` gets a `PermissionContext` whose `project_root` is that worktree path.
   It never sees, and cannot write to, the main workspace.
3. `SubagentManager` tracks claimed worktree paths so two implement-mode subagents can never target
   the same one.
4. Changes only reach the main workspace through `/agent apply <id> confirm`, which runs
   `git diff base...branch` for review, then `git merge --no-ff` from the main workspace root (a
   linked worktree shares branches with the main one, so no directory change is needed). On
   conflict, `git merge --abort` runs immediately and the conflict is reported — nothing is ever
   left half-merged.
5. `/agent cleanup <id> confirm` only runs for a terminal-status subagent; it removes the worktree
   via `git worktree remove` (never `--force`) before deleting the persisted record, so a failed
   removal never leaves an orphaned worktree with no record pointing at it.

# 8. Permissions

`SubagentManager::spawn` computes an effective mode that is never more permissive than the parent
session's own `PermissionMode`:

- **Plan mode** forces every subagent to a read-only mode (`Research`) regardless of the requested
  `--mode`, and rejects `--worktree`/`--mode implement` outright.
- **Approve mode** — an implement-mode subagent's writes and `/agent apply`'s merge both go through
  the same `PermissionResolver` confirmation path the main session already uses; no new prompt UI.
- **Auto mode** — implement-mode subagents get `DefaultPermissionPolicy::editing()` scoped strictly
  to their worktree; high-risk commands and anything outside that path still deny via the existing
  risk-based policy in `gocode-tools`.

Every `SubagentEvent::Progress` line and every field persisted into `SubagentResult`/
`SubagentMessage` passes through `gocode_tools::process::redact_secrets` before being persisted or
shown.

# 9. Persistence

Mirrors `SessionRecord`'s pattern exactly: one JSON file per subagent under
`<state_dir>/subagents/<id>.json`, written with the same `atomic_write` helper. Every field is
`#[serde(default)]`-tolerant so an older or newer schema still loads.

`SubagentManager::get`/`list` read from disk (not an in-memory cache), so subagents from earlier
sessions — including ones marked `Interrupted` at startup — show up in `/agents` immediately.
`recover_interrupted` runs once at boot, before the command loop starts: any record still
`Queued`/`Running`/`WaitingInput` is rewritten to `Interrupted`. Nothing is ever auto-resumed; the
user reviews via `/agents` and discards via `/agent cleanup <id>`.

# 10. Commands

| Command | Effect |
|---|---|
| `/agent spawn <task> [--mode research\|plan\|implement\|review] [--model <id>] [--worktree]` | Creates a subagent; prints id/mode/permissions/location once it starts. |
| `/agents` | Lists id, task, status, elapsed time, model, worktree. |
| `/agent status <id>` | Current status plus the last few messages. |
| `/agent message <id> <text>` | Queues a follow-up. If the subagent is `WaitingInput` (it asked a `NEEDS_INPUT:` question), this resumes it immediately; otherwise it's delivered at the next `Agent::run` step boundary (see §11.1). |
| `/agent stop <id>` | Cooperative stop; preserves partial result; never touches the worktree. |
| `/agent result <id>` | The structured `SubagentResult`. |
| `/agent apply <id>` | Requests the diff. When it arrives, a modal shows it with `[y] Apply [n] Cancel`; `/agent apply <id> confirm` is an equivalent typed shortcut. |
| `/agent cleanup <id>` | Requests the warning. When it arrives, a modal shows it with `[y] Remove [n] Cancel`; `/agent cleanup <id> confirm` is an equivalent typed shortcut. |

`<id>` accepts either the full UUID or the 8-character prefix `/agents` displays;
`SubagentManager::find` resolves it, refusing an ambiguous prefix.

Apply/cleanup confirmation reuses the same `pending_*` modal pattern `/worktree remove` already
uses (`AppState::pending_agent_confirm`, `handle_agent_confirm_event`,
`render_agent_confirm_modal`): the modal blocks composer input and every other pending prompt
gate in the TUI until the user presses `y`/Enter or `n`/Esc, so nothing is ever applied or removed
without an explicit keypress.

# 11. Known MVP limitations and recommended next increments

1. **Mid-run messaging is still not mid-turn.** `Agent::run` is one bounded prompt-to-completion
   call with no injection hook, so a message sent while the subagent is mid-tool-call-sequence is
   only picked up once that whole run finishes producing a final text response. What *is* handled
   now: a subagent can explicitly pause (see `WaitingInput` below) by ending its message with
   `NEEDS_INPUT: <question>` instead of guessing or stalling — `/agent message <id> <answer>`
   resumes it immediately from there, without waiting for any timeout. A subagent that doesn't ask
   still only sees a queued message after its current run completes. A future increment could add
   a cancellable "check for new input" point inside `Agent::drive` itself for the fully general
   case.
2. **Apply-time conflict resolution is still manual.** `/agent apply` now parses git's own
   `CONFLICT (...): Merge conflict in <path>` lines into an explicit "Conflicting files:" list
   (`parse_conflicting_files` in `crates/gocode/src/main.rs`) rather than dumping raw stdout/stderr,
   but there is still no guided in-TUI resolver — the user resolves manually in the worktree and
   re-runs `/agent apply`.
3. **No dedicated popup/list view.** Every `/agent ...` response (other than the apply/cleanup
   confirm modal) renders as a chat-log `Info`/`Warning` entry, consistent with how `/debug`
   already works, rather than a scrollable popup like `/mcp` or `/skills`.
4. **No nested subagents**, by construction (see §2) — not a limitation to lift casually, since the
   spec explicitly requires it stay out of the MVP.

# 12. Example session

```
> /agent spawn "summarize how login sessions expire" --mode research
Subagent a1b2c3d4 created — mode: research, read-only, location: main workspace (read-only).
Subagent a1b2c3d4 finished: completed. Sessions expire via a 30-minute sliding TTL in redis...

> /agent spawn "add a doc comment to SessionStore::renew" --mode implement --worktree
Subagent e5f6a7b8 created — mode: implement, editing, location: /code/gocode-worktrees/subagent-e5f6a7b8.
Subagent e5f6a7b8 finished: completed. Added a doc comment explaining the renewal window.

> /agent apply e5f6a7b8
[modal] Subagent e5f6a7b8 (branch subagent-e5f6a7b8), merging into main:
--- a/src/session_store.rs
+++ b/src/session_store.rs
@@ ...
[y] Apply   [n] Cancel
# user presses 'y'
Applied subagent e5f6a7b8's changes (merged branch subagent-e5f6a7b8).

> /agent cleanup e5f6a7b8
[modal] This removes the worktree at /code/gocode-worktrees/subagent-e5f6a7b8 (branch
subagent-e5f6a7b8). Any changes not already applied via `/agent apply e5f6a7b8` will be discarded.
[y] Remove   [n] Cancel
# user presses 'y'
Removed subagent e5f6a7b8.
```

# 13. Test coverage

- `crates/gocode-core/src/subagent.rs` — data model, persistence round-trip, restart recovery.
- `crates/gocode-agent/src/subagent_manager.rs` — read-only lifecycle, two subagents running
  concurrently, concurrency-cap queueing, timeout, stop-preserves-partial-result, Plan-mode
  permission denial, structured-result parsing (with and without a valid block), a subagent that
  asks `NEEDS_INPUT:` pausing at `WaitingInput` and resuming once `/agent message` answers it.
- `crates/gocode-tui/src/lib.rs` — `/agent`/`/agents` parsing (spawn flags in any order, apply
  confirm/no-confirm, message text joining, worktree-outside-implement-mode rejection), and the
  apply/cleanup confirm modal (opens on `AgentDiffReady`/`AgentCleanupWarning`, resolves on y/n,
  blocks chat input while pending).
- `crates/gocode/src/main.rs` — diff computation, a clean merge applying successfully, a
  conflicting merge aborting without leaving the workspace half-merged and reporting a structured
  conflicting-files list, and cleanup removing both the worktree and the persisted record.
