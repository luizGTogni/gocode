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
- **Nesting is allowed, but capped at one level.** Spawning is normally a supervisor-side
  operation triggered by a slash command; additionally, a depth-1 subagent (one the supervisor
  spawned directly) is given an `agent_spawn` tool it can call to delegate a bounded subtask to a
  depth-2 child and block until that child finishes. A depth-2 subagent never receives that tool,
  so there is no unbounded delegation chain — see §2.1.

## 2.1. Nesting

`MAX_SUBAGENT_DEPTH = 2` (`crates/gocode-agent/src/subagent_manager.rs`): depth `1` is a subagent
the supervisor spawned directly; depth `2` is one spawned by a depth-1 subagent's own
`agent_spawn` tool call. `SubagentManager::spawn` rejects any request above this
(`SubagentError::MaxDepthExceeded`), and a depth-2 subagent's own tool registry simply never gets
`agent_spawn` added to it (`SubagentWorker::tools_for`), so the limit isn't just a runtime check —
the model at max depth cannot even see the capability to try.

`agent_spawn` (`crates/gocode-agent/src/agent_spawn_tool.rs`, `AgentSpawnTool`) takes
`{task, mode, worktree?}` — the same shape as `/agent spawn`'s arguments — and **blocks** until the
child reaches a terminal status, returning its result summary as the tool's output. This is
deliberate: a tool call needs a result the model can act on, so `agent_spawn` is "delegate and
wait," not "fire and forget." The child's model, provider, project root, and worktree runner are
all inherited from the parent (not chosen by the model), and its permission mode is clamped to the
parent's own effective mode — so nesting cannot escalate permissions, matching the invariant every
top-level spawn already respects (§8).

Nested subagents draw from the **same** `SubagentManager` and the same concurrency pool as
top-level ones (`SubagentManager: Clone` — cloning shares the same `Arc`/`Mutex`-wrapped state,
so a clone is a handle, not an independent manager; `spawn` hands `Arc::new(self.clone())` to the
worker for exactly this purpose). Because a blocked parent still holds its own concurrency slot
while waiting on its child, nesting needs at least 2 slots to make progress — with `max_concurrent
= 1`, a parent holding the only slot would permanently block its own child from ever acquiring
one. Rather than silently deadlocking (bounded only by `per_task_timeout`) or quietly raising an
explicit `max_concurrent: 1` to 2, `agent_spawn` is simply not registered when the configured pool
is smaller than `MIN_CONCURRENCY_FOR_NESTING = 2` — a subagent then has no way to attempt a nested
spawn at all, and `/agent spawn`'s own top-level behavior with `max_concurrent: 1` is completely
unaffected (still genuinely serial, as configured).

The child's `SubagentRecord` carries `depth` and `parent_subagent_id` (both persisted,
schema-tolerant via `#[serde(default)]`), so `/agents` can show lineage and a restart can still
tell a nested subagent apart from a top-level one.

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
   linked worktree shares branches with the main one, so no directory change is needed). On a
   clean merge, that's it. On conflict, the merge is deliberately left in progress and a guided
   resolver popup opens (`AppState::pending_agent_conflict`, `handle_agent_conflict_event`,
   `render_agent_conflict_modal`): `o`/`t` per file calls `git checkout --ours`/`--theirs` then
   `git add`, and only once every file is resolved does `[Enter]` run `git commit --no-edit` to
   finish it — `[Esc]` aborts the merge at any point via `git merge --abort`, discarding every
   resolution made so far. Either way, the main workspace never sits in an *unexplained*
   half-merged state — it's either clean, or visibly mid-resolution with the popup open.
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
| `/agents` | Opens a scrollable popup listing id, mode, task, status, elapsed time, model, and worktree; Enter opens a subagent's full detail (status, messages, result), `r` refreshes, Esc closes/backs out. |
| `/agent status <id>` / `/agent result <id>` | Deep-links straight into the `/agents` popup's detail view for that subagent (status, messages, and structured result together) — no need to open `/agents` and navigate to it first. |
| `/agent message <id> <text>` | Queues a follow-up. If the subagent is `WaitingInput` (it asked a `NEEDS_INPUT:` question), this resumes it immediately; otherwise it's delivered at the next `Agent::run` step boundary (see §11.1). |
| `/agent stop <id>` | Cooperative stop; preserves partial result; never touches the worktree. |
| `/agent apply <id>` | Requests the diff. When it arrives, a modal shows it with `[y] Apply [n] Cancel`; `/agent apply <id> confirm` is an equivalent typed shortcut. On confirm, a clean merge applies immediately; a conflicting one opens the guided resolver (`o`/`t` per file, `[Enter]` to finish, `[Esc]` to abort — see §7). |
| `/agent cleanup <id>` | Requests the warning. When it arrives, a modal shows it with `[y] Remove [n] Cancel`; `/agent cleanup <id> confirm` is an equivalent typed shortcut. |

`<id>` accepts either the full UUID or the 8-character prefix `/agents` displays;
`SubagentManager::find` resolves it, refusing an ambiguous prefix.

Apply/cleanup confirmation reuses the same `pending_*` modal pattern `/worktree remove` already
uses (`AppState::pending_agent_confirm`, `handle_agent_confirm_event`,
`render_agent_confirm_modal`): the modal blocks composer input and every other pending prompt
gate in the TUI until the user presses `y`/Enter or `n`/Esc, so nothing is ever applied or removed
without an explicit keypress.

The `/agents` popup itself reuses the `/mcp`/`/skills` list-then-detail pattern
(`AppState::agents`/`agents_visible`/`agents_view`/`agents_selected`, `handle_agents_event`,
`render_agents_modal`): `handle_agents_event` is checked ahead of `handle_chat_event` in the
terminal loop's dispatch order, so composer input is implicitly blocked while the popup is open —
the same trick `/skills` and `/mcp` already rely on, no new gating conditions needed. Data comes
from `gocode_core::AppEvent::AgentListAvailable(Vec<SubagentRecord>)`, sent whenever `/agents`
opens or `r` is pressed inside it; `SubagentManager::list()` (disk-backed, see §9) is the source,
so the popup shows subagents from earlier sessions too.

`/agent status <id>`/`/agent result <id>` deep-link into the same popup's detail view rather than
answering inline: the runtime resolves `id` once (`SubagentManager::find`) and replies with
`AppEvent::AgentDetailAvailable { id, record }`, which the interface renders into
`AppState::agent_detail` (boxed — `SubagentRecord` is too large to keep unboxed in `AppEvent`
without bloating every other variant) and opens the popup straight to `AgentsView::Detail`. A
`None` record instead pushes a chat warning and leaves the popup closed. `agent_detail` is
decoupled from `agents`/`agents_selected` — it can hold a subagent that was never listed, e.g. a
prefix nobody has browsed to yet — but stays in sync with it: every `AgentListAvailable` refresh
re-matches by id and updates `agent_detail` if that subagent is still present, so pressing `r`
from either list or detail keeps both views current.

**Acting on `next_steps`.** A finished subagent's `SubagentResult::next_steps` are suggestions,
not automatic follow-up work — the supervisor (the user) always decides whether to act on them.
Two places surface that decision point instead of leaving the suggestions buried in a result blob:
the completion notice from `bridge_subagent_events` (`crates/gocode/src/main.rs`) appends "Suggests
N next step(s) — open /agents to review and spawn any of them." when `next_steps` is non-empty; and
inside the `/agents` detail view, pressing `n` prefills the composer with
`/agent spawn "<next step>"` for the current suggestion and closes the popup, ready for the user to
edit and submit (or discard). Repeated `n` presses cycle through every suggestion in order
(`AppState::agents_next_step_cursor`, reset to `0` each time a — possibly different — subagent's
detail is opened, so it never carries over between subagents). Nothing is ever spawned without the
user pressing Enter on the prefilled command themselves.

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
2. **The guided conflict resolver is ours/theirs only.** On a merge conflict, `/agent apply` now
   leaves the merge in progress (instead of aborting) and opens a popup listing every conflicting
   file; the user picks a whole side per file (`o` keeps the main workspace's version, `t` keeps
   the subagent's) until every file is resolved, then finishes or aborts the merge (see §10). There
   is still no hunk-level or line-level editor — a file that needs pieces of both sides has to be
   finished by hand outside the TUI before `git add`-ing it and returning to finish the merge (or
   the user aborts and starts over).
3. **`/agent message`/`stop` still reply inline.** `/agent status`/`result` now deep-link into the
   `/agents` popup's detail view (see §10), but `message` and `stop` are fire-and-forget mutations
   and stay as chat-log `Info`/`Warning` acknowledgements, consistent with how `/debug` already
   works — arguably the right call for a transient action rather than something to "view", but
   worth revisiting if usage shows otherwise.
4. **Nesting is capped at exactly one level, not configurable.** `MAX_SUBAGENT_DEPTH = 2` is a
   `const`, not a `SubagentLimits` field — deepening or disabling it means editing
   `subagent_manager.rs`, not a runtime setting. This was a deliberate choice to keep the first
   nesting increment small and easy to reason about; a configurable depth is a plausible follow-up
   once real usage shows whether one level is actually enough.
5. **`agent_spawn` has no per-parent fan-out limit.** A depth-1 subagent can call it repeatedly in
   sequence (each call blocks until that child finishes, so calls can't overlap from one parent),
   bounded only by the subagent's own `max_total_tool_calls` budget — there's no dedicated "at most
   N children" cap independent of that.
6. **Lineage is a marker, not a tree.** The `/agents` list prefixes a nested subagent's row with
   "↳" and its detail view shows "Depth: N (spawned by &lt;id&gt;)", but there's no indentation, no
   grouping of a parent with its children, and no way to jump from a child's detail straight to
   its parent's.

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
  asks `NEEDS_INPUT:` pausing at `WaitingInput` and resuming once `/agent message` answers it, and
  nesting (a depth-1 subagent spawning and blocking on a depth-2 child via `agent_spawn` end to
  end, `agent_spawn` present/absent from `tools_for` by depth and by concurrency-pool size,
  spawning beyond `MAX_SUBAGENT_DEPTH` rejected).
- `crates/gocode-tui/src/lib.rs` — `/agent`/`/agents` parsing (spawn flags in any order, apply
  confirm/no-confirm, message text joining, worktree-outside-implement-mode rejection), the
  apply/cleanup confirm modal (opens on `AgentDiffReady`/`AgentCleanupWarning`, resolves on y/n,
  blocks chat input while pending), the `/agents` popup (`AgentListAvailable` populates and
  clamps selection, list navigation stays in bounds, Enter/Esc move between list and detail, Esc
  on the list closes the popup, `r` requests a refresh from either view), and the guided conflict
  resolver (`AgentMergeConflict` opens it, `AgentConflictFileResolved` records the chosen side,
  `AgentMergeFinished` closes it and logs the outcome, navigation stays in bounds, `o`/`t` request
  resolving the selected file, Enter is a no-op until every file is resolved then requests
  finishing, Esc requests aborting even with unresolved files, and it blocks chat input while
  pending), and the `/agent status`/`result` deep link (`AgentDetailAvailable` opens the popup
  straight to detail with the matched record, a `None` match warns without opening it, entering
  detail from the list snapshots the selected record, and an `AgentListAvailable` refresh keeps an
  already-open detail in sync by id), and the `next_steps` prefill (`n` fills the composer with
  the current suggestion and closes the popup, repeated presses cycle through every suggestion in
  order, a result with no `next_steps` makes `n` a no-op, and opening a detail — including via the
  deep link — resets the cursor).
- `crates/gocode/src/main.rs` — diff computation; a clean merge applying immediately; a
  conflicting merge left in progress (not aborted) reporting a structured conflicting-files list;
  resolving every file and finishing completes the merge keeping the chosen side, verified against
  a real git repository fixture; aborting a conflicted merge leaves the workspace clean; and
  cleanup removing both the worktree and the persisted record.
