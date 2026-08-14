# Release notes

## v0.0.10 — Release Candidate

This release candidate packages the complete Gocode MVP for Windows x86_64 and
Linux x86_64. It introduces deterministic release archives, checksums, install
instructions, a tag-gated GitHub Release workflow, and the manual acceptance
matrix required before v0.1.0.

The coding-agent runtime includes NVIDIA NIM onboarding and streaming, validated
workspace tools, permission prompts, cancellation, bounded output, and the
Windows verified updater. Linux uses the documented manual-update path.

### Known limitations

- Manual Windows and Linux acceptance is still required before the v0.1.0 tag.
- Automatic self-update is Windows-only.
- Gocode is not an operating-system sandbox; review commands for untrusted
  repositories.
- Gocode does not collect product telemetry in the MVP.
