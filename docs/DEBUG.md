# Guided debugging

`/debug <description>` starts an evidence-first investigation. The agent is instructed to show
Triagem, Reproduzindo, Investigando, Hipótese, Corrigindo, and Validando; it must not invent
errors or edit before it has a plausible, evidence-backed cause.

Run `/debug` without a description to answer, one at a time: expected behavior, actual behavior,
reproduction steps, error/log/stack trace, affected environment, and when the problem started.

## Commands

- `/debug status` shows the active hypothesis, collected evidence, and recorded command count.
- `/debug stop` cancels the active investigation while preserving its session state.
- `/debug summary` produces a copyable issue/PR/support summary.

The investigation is persisted with its session, including when a session is resumed or forked.

## Safety limits

The existing permission mode remains authoritative. Plan mode blocks source edits; Approve asks
before every edit and command; Auto still blocks high-risk commands and asks for the configured
medium-risk actions. Long-running commands inherit the process timeout and cancellation token;
`/debug stop` cancels an active run. Agent edits remain one normal agent transaction, so existing
`/undo` and `/redo` apply unchanged.

Process output and guided answers redact common token, password, secret, API-key, bearer-token,
and cookie forms before they are streamed or persisted. Gocode does not send diagnostics to an
additional service: the normal selected model provider is the only remote participant.
