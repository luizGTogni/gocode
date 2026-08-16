# Gocode

Gocode is a terminal coding agent for Windows and Linux, powered by NVIDIA NIM.
It can inspect a project, propose and apply targeted changes, and run validation
commands with explicit permission controls.

## Install

Download the archive for your platform from the GitHub Releases page, verify its
SHA-256 value against the published `SHA256SUMS`, then follow
[the installation guide](docs/INSTALL.md). No Rust toolchain is required.

## Privacy and safety

Gocode sends the prompt and the project content it selects for a request to the
chosen NVIDIA provider. API credentials are stored separately from ordinary
configuration and are removed from child-process environments. File edits and
commands remain inside the detected workspace and follow permission policy.

Gocode has no telemetry in v0.1.0. See [security](docs/SECURITY.md),
[provider details](docs/NVIDIA_NIM.md), [configuration](docs/CONFIG.md), and
[tool permissions](docs/TOOLS.md) before using it with sensitive projects. Gocode can also
connect to [MCP servers](docs/MCP.md) — read that guide before adding one, since a server's
tools run with the same permissions as Gocode's own.

## Support

For setup, recovery, and known limitations, see [troubleshooting](docs/TROUBLESHOOTING.md).
To report a security issue privately, follow [the security policy](docs/SECURITY.md).
See the [release notes](docs/RELEASE_NOTES.md) and [release process](docs/RELEASE.md)
for distribution details. Contributions are welcome under [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Gocode is distributed under the [MIT License](LICENSE).
