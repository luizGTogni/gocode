---
description: Validate the current changes with the project's CI checks
---

Validate the current working tree changes by running the same checks CI runs
(see .github/workflows/ci.yml), in this order, stopping to report the first failure:

1. `cargo fmt --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo build --workspace`
4. `cargo test --workspace $ARGUMENTS`

If everything passes, summarize what was checked. If something fails, show the
relevant error output and propose a fix before re-running the failing step.
