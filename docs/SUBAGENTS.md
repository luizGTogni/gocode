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
                   -> Failed
                   -> Stopped        (via /agent stop)
                   -> TimedOut       (per_task_timeout exceeded)
        (WaitingInput reserved for future turn-based intake; unused in the MVP)

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
  `SubagentResult`'s shape.

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
| `/agent message <id> <text>` | Queues a follow-up, delivered at the next `Agent::run` step boundary (see §11.1). |
| `/agent stop <id>` | Cooperative stop; preserves partial result; never touches the worktree. |
| `/agent result <id>` | The structured `SubagentResult`. |
| `/agent apply <id>` / `/agent apply <id> confirm` | Shows the diff, then merges on explicit confirmation. |
| `/agent cleanup <id>` / `/agent cleanup <id> confirm` | Shows a warning, then removes the worktree + metadata on explicit confirmation. |

`<id>` accepts either the full UUID or the 8-character prefix `/agents` displays;
`SubagentManager::find` resolves it, refusing an ambiguous prefix.

# 11. Known MVP limitations and recommended next increments

1. **Mid-run messaging is not truly mid-turn.** `Agent::run` is one bounded prompt-to-completion
   call with no injection hook. A subagent's work is modeled as a sequence of `Agent::run` calls;
   `/agent message` is delivered as the next call's prompt once the current one finishes, not
   instantly. Acceptable for the MVP; a future increment could add a cancellable
   "check for new input" point inside `Agent::drive`.
2. **Apply-time conflict handling is detect-and-abort only.** On a merge conflict, `/agent apply`
   aborts and reports git's own conflict summary; there is no guided in-TUI resolver. The user
   resolves manually in the worktree and re-runs `/agent apply`.
3. **Confirmation is a typed subcommand, not a keyboard modal.** `/agent apply <id> confirm` and
   `/agent cleanup <id> confirm` require retyping the command rather than a y/n keypress (unlike
   `/worktree remove`). This avoided touching the several modal-gating call sites spread across
   `gocode-tui/src/lib.rs`; a follow-up could add a dedicated confirm modal for parity.
4. **No dedicated popup/list view.** Every `/agent ...` response renders as a chat-log `Info`/
   `Warning` entry, consistent with how `/debug` already works, rather than a scrollable popup like
   `/mcp` or `/skills`.
5. **`WaitingInput` is unused.** The status exists in the model for a future turn-based intake
   flow but no code currently transitions into it.
6. **No nested subagents**, by construction (see §2) — not a limitation to lift casually, since the
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
Diff for subagent e5f6a7b8 (branch subagent-e5f6a7b8):
--- a/src/session_store.rs
+++ b/src/session_store.rs
@@ ...
Run `/agent apply e5f6a7b8 confirm` to merge into main, or ignore to cancel.

> /agent apply e5f6a7b8 confirm
Applied subagent e5f6a7b8's changes (merged branch subagent-e5f6a7b8).

> /agent cleanup e5f6a7b8
This removes the worktree at /code/gocode-worktrees/subagent-e5f6a7b8 (branch subagent-e5f6a7b8).
Any changes not already applied via `/agent apply e5f6a7b8` will be discarded. Run
`/agent cleanup e5f6a7b8 confirm` to proceed.

> /agent cleanup e5f6a7b8 confirm
Removed subagent e5f6a7b8.
```

# 13. Test coverage

- `crates/gocode-core/src/subagent.rs` — data model, persistence round-trip, restart recovery.
- `crates/gocode-agent/src/subagent_manager.rs` — read-only lifecycle, two subagents running
  concurrently, concurrency-cap queueing, timeout, stop-preserves-partial-result, Plan-mode
  permission denial, structured-result parsing (with and without a valid block).
- `crates/gocode-tui/src/lib.rs` — `/agent`/`/agents` parsing (spawn flags in any order, apply
  confirm/no-confirm, message text joining, worktree-outside-implement-mode rejection).
- `crates/gocode/src/main.rs` — diff computation, a clean merge applying successfully, a
  conflicting merge aborting without leaving the workspace half-merged, and cleanup removing both
  the worktree and the persisted record.
