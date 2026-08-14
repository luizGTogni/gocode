# Foundation smoke checks

Run these checks from the repository root on each supported operating system.

## Automated baseline

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

## Linux (Ubuntu LTS x86_64)

Use disposable XDG directories so the check does not change an existing user profile:

```sh
export XDG_CONFIG_HOME="$(mktemp -d)"
export XDG_STATE_HOME="$(mktemp -d)"
export XDG_CACHE_HOME="$(mktemp -d)"
cargo run -p gocode
```

Verify that `gocode` creates `config/gocode/config.toml`, `state/gocode/logs`, and
`cache/gocode`. In a Git working tree, also verify `.gocode/project.toml`,
`.gocode/instructions.md`, and `.gocode/sessions`. Resize the terminal while Gocode is open,
then press `q` for a normal exit and repeat with `Ctrl+C`; terminal input must be restored in both
cases. Re-run the check from a directory whose name contains Unicode characters.

## Windows 10/11 x86_64

In PowerShell, start Gocode from a disposable Git working tree:

```powershell
cargo run -p gocode
```

Verify the global files under `%USERPROFILE%\.gocode` and the same per-project `.gocode` files.
Resize Windows Terminal (and repeat in PowerShell and `cmd`) while the TUI is open. Press `q` and
`Ctrl+C` in separate runs, confirming the console remains usable after each exit. Repeat from a
directory whose name contains Unicode characters.

## Cross-compilation guard

When the GNU Windows target is installed on Linux, run:

```sh
cargo check --workspace --target x86_64-pc-windows-gnu
```
