# Troubleshooting

## Gocode is not found

Open a new terminal after installation. On Windows, confirm
`%USERPROFILE%\.gocode\bin` is in the user PATH. On Linux, confirm
`${XDG_BIN_HOME:-$HOME/.local/bin}` is in PATH.

## NVIDIA onboarding fails

Check that the API key is valid and that the terminal can reach
`https://integrate.api.nvidia.com`. Gocode does not store API keys in
configuration files. You can retry onboarding without deleting project files.

## An edit or command did not run

Gocode validates all model-requested tools. Writes require editing intent;
medium-risk commands require approval; high-risk commands are refused. Read
the displayed permission prompt and retry with an appropriate explicit task.

## Cancellation or terminal recovery

Use Ctrl+C to cancel active work or exit. If a terminal is left in an unusual
state after a crash, run `reset` on Linux or open a new Windows Terminal tab.
Keep the terminal and operating-system details for a bug report.

## Update or installation recovery

Do not bypass a checksum failure. Re-download the release archive and verify
it against `SHA256SUMS`. On Windows, rerun the installer if the updater was
interrupted. On Linux, install the verified archive manually.

## Known limitations

- Linux uses manual updates in the MVP.
- Gocode is not an OS sandbox; carefully review commands for untrusted projects.
- Provider use sends selected prompt and project context to NVIDIA NIM.
- Maximum streamed response and search result limits preserve TUI responsiveness.
