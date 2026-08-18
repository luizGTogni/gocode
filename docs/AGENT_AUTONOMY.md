# Agent autonomy and progress

## Objective

Enable Gocode to pursue a coding task autonomously: explore, change, validate, recover from a
failure, and finish with a useful result. It asks permission only for actions governed by the
existing risk policy, not for ordinary project reads, edits, or safe validation commands.

The runtime, rather than the model prompt alone, detects non-progressing work and produces a
recoverable result when execution cannot continue.

## Scope

This applies to one `AgentRun`. It does not add parallel or background agents, new providers, or
a planner/executor split.

## Architecture

Add a provider- and TUI-neutral `TaskProgress` value owned by `AgentRun`. For every completed
tool call, it records:

- action class: exploration, change, validation, or recovery;
- normalized action signature (tool name plus normalized arguments);
- affected files and validation obligations;
- whether the result introduced new evidence, repeated known state, or failed.

The loop uses internal phases:

```text
Explore -> Change -> Validate -> Recover -> Finalize
```

They are runtime controls, not a replacement for model reasoning. The TUI maps them to clear
activity labels such as exploring, validating, trying an alternative, and finalizing.

## Operational rules

1. A successful file-changing tool creates a validation obligation.
2. Normal completion is allowed only with no pending validation obligation, unless the model
   explicitly records that no safe or relevant validation command exists.
3. A failed validation enters `Recover`. The next productive action must differ from the failed
   action: inspect diagnostics, inspect the failure, search comparable code, or test a smaller
   hypothesis before another edit.
4. Repeated actions are judged by signature and evidence, not only by tool failure. A successful
   `read_file`, `search`, or command with unchanged output is non-progressing when repeated.
5. The runtime ends a run early after a bounded streak of non-progressing actions. Existing hard
   turn and tool-call limits remain the final resource boundary.
6. A tool or permission failure remains in the conversation as recovery evidence; it does not
   erase partial work or immediately terminate the run.

## Graceful finalization

When the progress policy or a hard limit stops further tools, enter `Finalize`:

1. Run one final model turn without tools.
2. Require a report of completed changes, validation evidence, the current blocker, and the best
   next action. It must not claim completion when validation failed or is absent.
3. Return `AgentCompletion` with a non-success termination reason such as `BudgetExhausted` or
   `NoProgress`, plus a visible warning.
4. If that final model turn fails, create a deterministic local fallback summary: files changed,
   commands and outcomes, last failure, and next suggested action.

Cancellation stays immediate and skips finalization. Existing permission enforcement remains
authoritative.

## Verification

Use `FakeProvider` and fake-tool tests for:

- edit followed by successful validation and normal completion;
- validation failure followed by a distinct recovery strategy and successful validation;
- repeated successful tool calls with unchanged output ending as `NoProgress`;
- attempted completion with pending validation, requiring validation or an explicit unavailability
  statement;
- max-turn and max-tool-call boundaries producing a partial final result;
- failure of the final model turn producing the local fallback summary;
- cancellation bypassing finalization.

## Non-goals

- automatic rollback of valid edits solely because validation failed;
- hidden background retries;
- treating incomplete work as successful completion;
- parallel task execution.
