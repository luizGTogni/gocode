# v0.0.8 Stability Design

## Scope

This milestone makes the existing hardened feature set predictable through
repeatable, offline automated validation. Manual Windows and Linux terminal
acceptance remains a v0.0.9 release-candidate gate.

## Test strategy

- Keep unit and contract tests in their owning crates: `gocode-core`,
  `gocode-provider-nvidia`, `gocode-agent`, `gocode-tools`, `gocode-tui`, and
  `gocode-updater`.
- Add scripted agent flows using deterministic provider and tool doubles. They
  must cover normal completion, retryable provider disconnects, cancellation,
  bounded output, and persisted-session failures without a live service.
- Add cross-crate integration tests at the application boundary for startup,
  provider failure recovery, tool invocation, cancellation, and update-check
  degradation. Tests must use temporary directories and local responses only.
- Preserve platform-independent assertions; platform-specific terminal checks
  are documented rather than emulated in CI.

## Recovery behavior

Recoverable provider and update-check failures return a user-facing event and
leave the application usable. Cache or session read/write failures fall back to
safe defaults or report a non-fatal error; they never overwrite a known-good
file. Cancellation stops active work, drains or discards stale events, and
leaves no active command or pending permission in state. Panic and normal
shutdown retain terminal restoration guarantees already owned by the TUI.

## Bounds and performance

Existing output, channel, search, and model-cache limits are tested directly.
The suite uses bounded fixtures to ensure large output is truncated, repeated
stream events cannot grow state without limit, and ignored large project files
do not make search unresponsive. No benchmark-derived numeric budget is added
until a stable cross-platform baseline exists.

## Defect and release policy

`docs/HARDENING.md` records reproducible issue expectations, severity, and
known limitations. A crash in a normal flow, data corruption, secret exposure,
workspace escape, or failed update rollback is release-blocking. The final
validation gate is `cargo fmt --check`, `cargo clippy --workspace --all-targets
-- -D warnings`, and `cargo test --workspace`.

Report a defect with the command or prompt, a minimal project fixture, expected
and actual behavior, platform and terminal, Gocode revision, and any
secret-redacted diagnostic output. Classify it as release-blocking when it
matches the conditions above; otherwise record its severity and deterministic
reproduction before deferring it.

## Non-goals

This milestone does not add providers, tools, release packaging, networked
tests, telemetry, or the manual Windows/Linux matrix.

## Known limitations

- Windows Terminal and Linux terminal restoration remain manual v0.0.9 checks.
- Automatic self-update is Windows-only; Linux continues to use the documented
  manual archive replacement path.
- The default 64 KiB streamed assistant-response limit and 200 search-result
  limit favor a responsive TUI over retaining unbounded model output.
