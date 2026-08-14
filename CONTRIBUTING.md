# Contributing to Gocode

Use Rust 1.88 or newer. Before proposing a change, run:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Keep changes focused, add deterministic tests for behavior changes, and do not
commit API keys, release artifacts, local logs, or generated state. Report
security issues privately using `docs/SECURITY.md`, not public issues.

By contributing, you agree that your contribution is licensed under the MIT
License.
