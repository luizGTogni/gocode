# v0.0.7 — Security and Platform Hardening Record

## Automated verification

The v0.0.7 security boundary is covered by the workspace test suite. The checks include traversal
and symlink containment, invalid tool input, model-controlled tool validation, permission denial,
cancellation, bounded command output, credential removal from child environments, masked
credentials, updater checksum/archive/replacement checks, and terminal-state handling.

The additional hardening in this release makes atomic writes fail closed when a temporary path is
already occupied, rejects non-HTTPS release metadata endpoints, bounds unterminated SSE events,
and refuses oversized or excessive streamed tool-call fragments before they can be executed.

Run locally:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Platform matrix

| Area | Linux | Windows |
| --- | --- | --- |
| Build/type validation | `x86_64-unknown-linux-gnu` | `x86_64-pc-windows-gnu` cross-check required in CI or a Windows runner |
| Workspace containment | Automated traversal, absolute-path, missing-target, and Unix symlink tests | Run the same suite on Windows to cover drive-letter, UNC, junction, and reparse-point semantics |
| Commands | Automated timeout, cancellation, bounded output, and credential-environment tests | Manually exercise Windows Terminal, PowerShell, and `cmd`, including Unicode paths and quoting |
| Persistent data | XDG resolution and private Unix directory tests | Verify Credential Manager unavailability, executable locks, replacement rollback, and user-profile paths |
| Terminal | TUI resize, cancellation, and panic-restoration tests | Verify restoration after Ctrl+C, panic, update hand-off, and resize in supported terminals |

## Manual Windows release gate

Before a public release, record the Windows runner, version, terminal, and result for each item
in the Windows column. A failure is release-blocking if it permits a workspace escape, exposes a
credential, bypasses permission validation, leaves a child process running after cancellation, or
replaces an unverified update.

Linux automatic self-update remains outside the MVP; users must use the documented manual-update
flow. This is a product limitation, not a fallback to an unverified updater.
