# Release process

## Release contract

A release tag must be immutable and exactly match the `gocode` Cargo package
version with a leading `v` (for example, package `0.1.0` requires tag
`v0.1.0`). The release workflow only runs from such tags. It builds with the
committed `Cargo.lock`, packages only the expected platform binaries, produces
`SHA256SUMS`, and attaches all artifacts to the matching GitHub Release.

Expected assets are:

- `gocode-<version>-windows-x86_64.zip`
- `gocode-<version>-linux-x86_64.tar.gz`
- `SHA256SUMS`

The Windows archive contains root-level `gocode.exe` and `gocode-updater.exe`.
The Linux archive contains root-level `gocode` under its versioned directory.

## Required pre-release checks

Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo build --workspace --locked`, and `cargo test --workspace --locked` from a
clean checkout. Complete the manual matrix in `docs/RC_MANUAL_MATRIX.md` and
resolve every release-blocking issue before creating the tag.

The release job requires only GitHub `contents: write` permission to upload
artifacts. It uses the repository-scoped `GITHUB_TOKEN`; no provider credential
or third-party secret is needed or permitted.
