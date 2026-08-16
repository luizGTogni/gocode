# Gocode — Updater Specification

**Status:** Implemented for Windows and Linux (v0.2.0+); packaged-flow validation should be repeated per release
**Product:** Gocode
**Target version:** v0.1.0 (Windows), extended to Linux in a later release
**Scope:** Update discovery, download, verification, self-update (Windows helper process, Linux in-place replace), rollback

---

# 1. Purpose

This document defines the Gocode update system.

The updater must allow a globally installed Gocode binary to discover and install newer stable releases with minimal user effort.

Required behavior:

```text
installed version = 0.1.0
latest stable release = 0.2.0
↓
Gocode asks user
↓
Yes / No
```

If the user declines, the same version should be offered again on the next startup.

---

# 2. Core Principle

Updates should be:

```text
safe
simple
non-blocking
recoverable
explicitly user-approved
```

Update checks must never prevent the user from opening Gocode.

---

# 3. Source of Truth

Use:

```text
GitHub Releases
```

with SemVer-compatible Git tags.

Example:

```text
v0.1.0
v0.2.0
v0.3.0
```

The updater should compare normalized versions:

```text
0.1.0
0.2.0
```

---

# 4. Stable Release Policy

Default update channel:

```text
stable
```

Ignore by default:

- draft releases;
- prereleases.

Future channels may support:

```text
beta
nightly
```

but are not implemented yet.

---

# 5. GitHub Release API

The update checker should query the repository's latest stable GitHub Release.

It should retrieve enough metadata to identify:

- release version;
- this platform's asset (Windows `.zip` or Linux `.tar.gz`);
- checksum asset/metadata;
- optional release notes.

The exact API client can use direct GitHub REST calls.

---

# 6. Update Architecture

Split the system into two logical components:

```text
UpdateChecker
UpdateInstaller
```

On Windows, installation involves two processes:

```text
gocode.exe
gocode-updater.exe
```

On Linux, installation happens inside the single running process — no helper binary is shipped or needed, because the installed executable can be replaced by an atomic rename while it is still running.

---

# 7. Why Windows Uses a Separate Updater

A running Windows executable cannot replace itself in place; the file is locked while it is executing.

Flow:

```text
gocode.exe
↓
download + verify + stage new version
↓
user confirms install ("Completed" screen → Close)
↓
launch gocode-updater.exe
↓
exit
↓
updater replaces gocode.exe
↓
updater relaunches gocode.exe
```

---

# 8. Why Linux Updates In Place

On Linux (and Unix generally), `rename()` only changes a directory entry — it doesn't require the target file to be closed. A running process keeps executing the old inode even after its path has been repointed at a new file.

This means Gocode can replace its own installed binary while it is still the process running that binary, with no separate helper process:

```text
gocode running
↓
download + verify + stage new version
↓
user confirms install ("Completed" screen → Close)
↓
rename the staged binary over the installed one (atomic; current process is unaffected)
↓
spawn the new binary as a child process
↓
send ExitForUpdate and exit
```

If the child spawn fails after the rename already succeeded, the binary on disk is updated even though the current process is still running the old code in memory — the user is told to reopen Gocode manually.

---

# 9. Crate Layout

Recommended:

```text
gocode-updater/
└── src/
    ├── lib.rs      (release discovery, download, checksum, archive extraction, replace/restart)
    └── main.rs     (the Windows-only gocode-updater helper binary)
```

The library is platform-agnostic where possible; only archive extraction (`extract_windows_archive` / `extract_linux_archive`) and the Windows helper binary are platform-specific.

---

# 10. Current Version

The running binary should expose its version at build time.

Conceptually:

```rust
const VERSION: &str = env!("CARGO_PKG_VERSION");
```

Parse with:

```text
semver
```

---

# 11. Update Check Timing

Recommended startup sequence:

```text
TUI opens
↓
normal initialization continues
↓
update check starts asynchronously
```

Do not:

```text
check GitHub
↓
wait
↓
then open TUI
```

Update checks are skipped entirely in debug builds and on platforms other than Windows and Linux, so local development never nags about updates.

---

# 12. Non-Blocking Failure

If GitHub is:

- offline;
- slow;
- rate limited;
- temporarily unavailable;

Gocode should continue normally.

Default user-facing behavior:

```text
show nothing
```

Update check failure is not a blocking application error.

---

# 13. Update Check Frequency

Current behavior:

```text
check on every startup
```

when:

```toml
[updates]
check_on_startup = true
```

No complex TTL policy is required.

A small internal cache is optional.

---

# 14. Update Configuration

Global config:

```toml
[updates]
check_on_startup = true
```

No `ignored_version` field in the MVP.

---

# 15. Version Comparison

Use SemVer rules.

Examples:

```text
current 0.1.0
latest 0.2.0
→ update available
```

```text
current 0.2.0
latest 0.2.0
→ no update
```

```text
current 0.3.0
latest 0.2.0
→ no downgrade
```

Never auto-downgrade.

---

# 16. Tag Normalization

Accept release tags such as:

```text
v0.2.0
```

Normalize to:

```text
0.2.0
```

before SemVer parsing.

Invalid release versions should be ignored with debug logging.

---

# 17. Release Metadata

Conceptual:

```rust
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    pub assets: Vec<ReleaseAsset>,
    pub release_notes: Option<String>,
}
```

---

# 18. Platform Asset Selection

The release pipeline publishes one deterministic archive name per supported platform:

```text
gocode-{version}-windows-x86_64.zip
gocode-{version}-linux-x86_64.tar.gz
```

The client picks the suffix for the platform it's running on (`current_platform_archive_suffix`) and looks for `gocode-{version}-{suffix}` among the release assets. Any other platform is unsupported and the update checker simply doesn't run.

---

# 19. Package Contents

Windows release archive:

```text
gocode.exe
gocode-updater.exe
LICENSE
INSTALL.md
install-windows.ps1
```

Linux release archive (inside one top-level `gocode-{version}-linux-x86_64/` directory):

```text
gocode
LICENSE
INSTALL.md
install-linux.sh
```

Linux ships no separate updater helper — only the `gocode` executable is extracted from the archive; the other files are ignored by the updater (they matter only for a fresh manual install).

---

# 20. Architecture Detection

Currently supported:

```text
x86_64 Windows
x86_64 Linux
```

If ARM64 is supported, asset selection must use runtime architecture.

Do not download an incompatible binary.

---

# 21. Update Available Event

The checker emits a normalized application event:

```rust
AppEvent::UpdateAvailable {
    current_version: String,
    version: String,
    notes: String,
}
```

`current_version` is included so the popup can show the "old → new" comparison. The TUI decides when to show the modal.

---

# 22. Active Agent Run

If an update is detected while an AgentRun is active:

```text
defer the update modal
```

until the task completes.

Do not interrupt coding work.

---

# 23. Update Prompt

The popup shows:

```text
New update

0.1.0  ->  0.2.0      (current version in red, new version in green)

<first line of release notes>

 Yes   No
```

`Yes`/`No` are highlighted buttons; Left/Right/Tab toggle which one is selected, Enter confirms the highlighted one, and `y`/`n`/`Esc` work as direct shortcuts regardless of the current selection.

---

# 24. Declining an Update

If the user selects `No` (or presses `Esc`):

- close the popup;
- continue normally;
- do not persist an ignored version.

Next startup:

```text
0.2.0 still latest
↓
ask again
```

---

# 25. Accepting an Update

Choosing `Yes` only downloads, verifies, and stages the update — it does **not** touch the installed binary yet:

```text
user selects Yes
↓
popup switches to a download-progress screen (percentage + message)
↓
download asset (streamed, with byte-level progress)
↓
verify checksum
↓
extract the platform archive into a staging directory
↓
popup switches to "Completed" with a Close button
```

The actual install + restart only happens once the user presses **Close** on the Completed screen (see §7 and §8 for the platform-specific replacement step that follows).

---

# 26. Download Location

Use a temporary/update staging directory.

Example:

```text
~/.gocode/cache/update/
```

or OS temp directory.

Staged download name:

```text
download.partial   (while in flight)
release.download   (once complete)
```

---

# 27. Partial Downloads

Never treat a partial download as valid.

Pattern:

```text
download to .partial
↓
flush/close
↓
rename to final staged file
```

---

# 28. HTTPS

All release and asset downloads must use HTTPS.

Do not support insecure HTTP for official updates.

---

# 29. Integrity Verification

The updater must verify downloaded artifacts before installation.

Minimum requirement:

```text
SHA-256 checksum
```

The release pipeline publishes checksums for every platform archive.

---

# 30. Checksum Distribution

GitHub Release asset:

```text
SHA256SUMS
```

containing one line per archive, e.g.:

```text
<sha256>  gocode-0.2.0-windows-x86_64.zip
<sha256>  gocode-0.2.0-linux-x86_64.tar.gz
```

---

# 31. Verification Flow

```text
download binary/archive
↓
download/read expected checksum
↓
compute SHA-256
↓
compare
↓
match → continue
mismatch → abort
```

---

# 32. Checksum Failure

If checksum mismatches:

```text
abort update
```

Do not touch the installed executable — at this point nothing has been staged for install yet.

User-facing message (on the popup's "Failed" screen):

```text
Gocode could not verify the update.

Your current installation was not changed.
```

---

# 33. Signature Verification

Cryptographic release signing is desirable future hardening.

Possible future technologies:

- Sigstore/cosign;
- minisign;
- platform code signing.

Not required while SHA-256 is implemented correctly, but Windows code signing should be considered before wider distribution. Linux has no equivalent trust-prompt friction to solve, so it isn't a near-term priority there.

---

# 34. Windows Code Signing

A production release should eventually sign:

```text
gocode.exe
gocode-updater.exe
```

This improves Windows trust UX.

It is a release engineering concern separate from checksum verification.

---

# 35. Update Installer Input

The Windows helper process receives explicit paths and expected version:

Conceptual CLI:

```text
gocode-updater.exe
  <pid>
  <staged-executable-path>
  <installed-executable-path>
```

Exact syntax is internal. Linux has no equivalent helper input — the running process performs the replace and restart itself (see §8).

---

# 36. Updater Trust Boundary

The updater must not accept arbitrary unsafe replacement targets from untrusted model/tool input.

Only the Gocode application invokes it with validated paths.

---

# 37. Process Ownership

The updater should verify the target path corresponds to the installed Gocode binary it expects to replace.

Avoid generic "replace any executable" behavior.

---

# 38. Windows Replacement Flow

```text
gocode.exe running
↓
new binary downloaded, verified, and staged (§25) — installed binary untouched so far
↓
"Completed" screen shown; user presses Close
↓
gocode-updater.exe launched
↓
gocode.exe exits
↓
updater waits for process release
↓
backup old binary
↓
replace
↓
verify replacement exists
↓
restart gocode.exe
↓
cleanup
↓
updater exits
```

---

# 39. Linux Replacement Flow

```text
gocode running
↓
new binary downloaded, verified, and staged (§25) — installed binary untouched so far
↓
"Completed" screen shown; user presses Close
↓
backup old binary (rename installed → installed.previous)
↓
rename staged binary → installed path (atomic; current process keeps running the old inode)
↓
remove backup
↓
spawn the newly installed binary as a child process
↓
send ExitForUpdate and exit
```

If the child spawn fails, the backup has already been removed (replacement succeeded) but no new process was started — the popup shows a "Failed" screen telling the user to reopen Gocode manually rather than attempting a rollback of a successful file replacement.

---

# 40. Waiting for Main Process Exit

The Windows updater helper may need to wait until the old process fully exits before the file lock is released.

Use:

- process handle if available;
- bounded retry/wait;
- clear failure behavior.

Never loop forever. (Not applicable on Linux — there is no lock to wait out.)

---

# 41. Backup Strategy

Before replacement, on either platform:

```text
<installed>
↓
<installed>.previous
```

Then place the new executable at the installed path. After a successful replacement:

```text
remove <installed>.previous
```

---

# 42. Rollback

If replacement fails after backup:

```text
restore <installed>.previous → <installed>
```

The user's existing installation should remain usable.

---

# 43. Rollback Scope

Rollback should cover:

- file replacement failure;
- missing new binary;
- rename/copy errors.

Full transactional update systems are unnecessary.

---

# 44. Restart

After successful replacement, start the newly installed executable as a child process (same binary path on both platforms, no arguments preserved yet).

Prefer preserving:

- current working directory.

Future enhancement may preserve project/session continuation.

---

# 45. Restart Arguments

If Gocode was started with useful CLI arguments, the restart may preserve them when safe.

Not required for the current implementation.

---

# 46. Update UX Progress

While downloading and preparing an update, the popup shows a progress bar with a percentage (when the server reports `Content-Length`) and a short message:

```text
Downloading update…   [███████░░░]  68%
Verifying update…     [██████████] 100%
Extracting update…    [██████████] 100%
```

Avoid exposing low-level file operations.

---

# 47. Terminal Shutdown Before Restart

Before exiting to install the update:

1. flush session state;
2. flush logs;
3. restore terminal;
4. (Windows) launch the updater helper, or (Linux) rename the binary and spawn the new process directly;
5. exit.

Do not leave terminal raw mode enabled.

---

# 48. Updater UI

The Windows helper does not need a TUI; it may run silently or print minimal console output.

Linux has no separate updater process at all — the replacement happens inside the main Gocode process between the Close click and process exit.

---

# 49. Update Failure UX

If update preparation or installation fails, the popup switches to a "Failed" screen:

```text
Failed

<error message>

 Close
```

Close dismisses the popup; the current session and installation are unaffected unless the failure happened after a successful file replacement (see §39), in which case the message tells the user to reopen Gocode manually.

---

# 50. Updater Failure After Main Exit

If the Windows updater helper fails after Gocode exits:

- restore old binary when possible;
- print a concise console error;
- preserve log location.

Example:

```text
Gocode update failed.
The previous version was restored.
```

---

# 51. Update Logs

Write updater diagnostics to:

```text
~/.gocode/logs/
```

Never include secrets.

Useful fields:

```text
current version
target version
asset name
checksum result
replacement stage
rollback result
```

---

# 52. GitHub Errors

Map:

```text
404 release
429 rate limit
5xx server error
network timeout
invalid JSON
```

to `UpdateError`.

Most check errors are silent/non-blocking.

Install errors are user-visible.

---

# 53. Update Error Types

Conceptual (matches `gocode_updater::UpdateError`):

```rust
pub enum UpdateError {
    Network(String),
    InvalidRelease(String),
    AssetNotFound,
    ChecksumMissing,
    ChecksumMismatch,
    UnsafeArchive(String),
    Io(String),
    Replace(String),
    Rollback(String),
    UnsupportedPlatform,
}
```

`UnsupportedPlatform` covers any OS other than Windows or Linux.

---

# 54. Update Checker Type

Conceptual:

```rust
pub struct UpdateChecker {
    source: Arc<dyn UpdateSource>,
    current_version: Version,
}
```

---

# 55. Update Source

A small abstraction is justified.

```rust
#[async_trait]
pub trait UpdateSource: Send + Sync {
    async fn latest_stable(
        &self,
    ) -> Result<Option<ReleaseInfo>, UpdateError>;
}
```

Implementation:

```text
GitHubReleaseSource
```

This enables deterministic tests.

---

# 56. Update Installer Type

Conceptual:

```rust
pub struct UpdateInstaller {
    downloader: Downloader,
    verifier: ChecksumVerifier,
    platform: PlatformInstaller,
}
```

Do not over-abstract internal helpers.

---

# 57. Download Progress

The downloader reports `(downloaded_bytes, total_bytes)` to a progress callback as each streamed chunk arrives; `total_bytes` is `None` when the server omits `Content-Length`. This is turned into the application event:

```rust
AppEvent::UpdateProgress {
    percent: Option<u8>,
    message: String,
}
```

The TUI shows `message` next to a progress bar driven by `percent` (an indeterminate bar when `percent` is `None`).

---

# 58. Cancellation

Before the user confirms install (i.e. while still on the download-progress screen), there is currently no cancel action — the download either completes or fails.

Once file replacement begins (§38/§39):

```text
do not expose arbitrary cancellation
```

A half-applied update is worse than finishing a short atomic operation.

---

# 59. Update During Permission Modal

Do not show an update modal over an active permission modal.

Queue the update notification.

---

# 60. Update During Onboarding

Recommended:

```text
finish onboarding first
```

Then show update prompt.

Do not distract the user before they can enter the product.

---

# 61. Update Check Privacy

The GitHub update request may reveal normal network metadata such as:

- IP;
- User-Agent.

Do not send:

- project path;
- user prompt;
- API key;
- model selection unless technically necessary.

---

# 62. GitHub Authentication

Public release checks should not require a GitHub token.

If unauthenticated rate limits become an issue, caching can be improved later.

Do not ask users for a GitHub token just to update Gocode.

---

# 63. Release Asset Naming

Standardized names (see `.github/workflows/release.yml`):

```text
gocode-{version}-windows-x86_64.zip
gocode-{version}-linux-x86_64.tar.gz
SHA256SUMS
```

Consistency simplifies updater logic.

---

# 64. Archive Extraction

Windows (`.zip`):

```text
download zip
↓
verify zip checksum
↓
extract gocode.exe and gocode-updater.exe into a staging directory
↓
validate both files were present
↓
install
```

Linux (`.tar.gz`):

```text
download tar.gz
↓
verify checksum
↓
extract only the `gocode` file, found one level under a single top-level directory
↓
validate it was present, then set its executable bit
↓
install
```

Never extract directly over the live installation on either platform.

---

# 65. Archive Safety

Extraction must prevent path traversal on both formats.

Windows ZIP entries are rejected unless they are a root-level `gocode.exe` or `gocode-updater.exe` (no `/`, no `\`, no enclosed-path mismatch).

Linux tar entries are rejected unless they resolve to exactly `<one-directory>/<file>` — anything with `..`, more path segments, or no wrapping directory is treated as unsafe. Non-`gocode` files inside that one directory (`LICENSE`, install scripts, …) are simply skipped, not rejected.

---

# 66. Expected Files

Windows release:

```text
gocode.exe
gocode-updater.exe
```

Both must exist before starting replacement.

Linux release: only `gocode` is required; other archive members are optional and ignored by the updater.

---

# 67. Updating the Updater

The Windows release may also contain a new `gocode-updater.exe`, but the current implementation does not use the freshly downloaded one — it reuses whatever `gocode-updater.exe` is already installed next to `gocode.exe`. If that file is missing, the update fails with a message asking the user to reinstall Gocode.

Linux has no separate updater binary to update — nothing to do here.

---

# 68. Simplified Updater Strategy

Windows:

- keep a small stable updater binary;
- update `gocode.exe` first;
- defer updater self-replacement.

Linux doesn't need this strategy at all — there is no second binary in the loop.

---

# 69. Installation Path

Typical per-user install locations:

```text
Windows: %USERPROFILE%\.gocode\bin\gocode.exe (+ gocode-updater.exe)
Linux:   ~/.local/bin/gocode
```

The updater must resolve the actual running binary path (`current_exe()`) rather than assuming a fixed default, since users may install elsewhere.

---

# 70. Portable/Alternate Installs

Future users may place Gocode elsewhere.

Use:

```text
current_exe()
```

to determine the active executable location.

Only update installations the process can write to.

---

# 71. Permission Failure

If the installation directory is not writable:

```text
abort safely
```

Show:

```text
Gocode does not have permission to update this installation.
```

Future installer methods may handle elevated/system installs separately.

---

# 72. Global User Install

Prefer per-user installation so updates do not require administrator/root privileges:

```text
Windows: %USERPROFILE%\.gocode\bin
Linux:   ~/.local/bin
```

---

# 73. Release Pipeline Contract

The updater depends on release engineering. GitHub Actions produces, per tag:

```text
gocode-{version}-windows-x86_64.zip
gocode-{version}-linux-x86_64.tar.gz
SHA256SUMS
GitHub Release (stable version tag)
```

Updater design and release pipeline must share the same asset naming contract.

---

# 74. Release Pipeline Concept

```text
git tag v0.2.0
↓
GitHub Actions
↓
cargo test
↓
cargo build --release
↓
package Windows and Linux assets
↓
SHA-256 (SHA256SUMS)
↓
publish GitHub Release
```

---

# 75. Release Notes

The update popup shows only the first line of the release notes.

Future option:

```text
View what's new
```

Not required currently.

---

# 76. Forced Updates

Do not implement forced updates.

The user must always be able to select:

```text
No
```

---

# 77. Automatic Silent Installation

Do not silently install updates without user approval.

Check automatically, install explicitly (and only after the user confirms the "Completed" screen — see §25).

---

# 78. Downgrade

Updater does not support downgrade through the normal flow.

Manual downgrade remains outside scope.

---

# 79. Prerelease Builds

A developer build with a version such as:

```text
0.2.0-alpha.1
```

should not automatically jump channels without a defined policy.

For stable public builds, use stable latest release only.

---

# 80. Development Builds

Update checks are disabled in debug builds (`cfg!(debug_assertions)`), so local development never nags about updates.

---

# 81. Debug Update Source

Tests may use a fake source.

Do not require GitHub network access for updater unit tests.

---

# 82. Fake Update Source

Example:

```rust
struct FakeUpdateSource {
    latest: Option<ReleaseInfo>,
}
```

Test:

```text
current 0.1.0
fake latest 0.2.0
→ update available
```

---

# 83. Fake Installer

Installer behavior should be testable with temporary directories.

Avoid replacing the test runner executable.

---

# 84. Version Tests

Minimum:

```text
same version
newer patch
newer minor
newer major
older release
v-prefix
invalid tag
prerelease
draft ignored
release missing this platform's asset
```

---

# 85. Download Tests

Test:

```text
success
timeout
partial failure
resume not required
wrong content length
temp file cleanup
progress callback receives increasing percentages
```

---

# 86. Checksum Tests

Test:

```text
correct checksum
wrong checksum
missing checksum
invalid checksum format
```

---

# 87. Platform Installer Tests

Test in isolated temp directories, for both platforms:

```text
replace binary
backup old
failure during replace
rollback
restart path construction
```

Windows-specific:

```text
locked file
missing staged updater helper
```

Linux-specific:

```text
extensionless binary name (no ".exe")
replace succeeds while a process still has the old file open
archive path-traversal / missing-wrapping-directory rejection
```

---

# 88. Integration Test

A safe integration scenario can simulate:

```text
fake current executable
↓
fake new executable
↓
updater replacement
↓
verify content
```

Do not replace the running test process.

---

# 89. Manual Release Verification

Before shipping a release, on **both** Windows and Linux:

1. install the previous version;
2. publish the new release;
3. launch Gocode;
4. receive prompt;
5. decline;
6. relaunch and confirm the prompt appears again;
7. accept — confirm the progress screen shows an increasing percentage;
8. confirm the "Completed" screen appears with a Close button;
9. press Close — confirm the process restarts automatically into the new version;
10. verify old binary/backup cleanup.

---

# 90. Recovery Manual Test

Simulate:

- no network;
- bad checksum;
- locked install directory (Windows) / read-only install directory (Linux);
- missing `gocode-updater.exe` (Windows);
- failed replacement;
- restart spawn failure right after a successful in-place replace (Linux) — confirm the "reopen manually" message appears and the binary on disk is the new version.

Current version must remain usable whenever possible.

---

# 91. App Event Integration

Events (`gocode_core::AppEvent`):

```rust
UpdateAvailable { current_version: String, version: String, notes: String }
UpdateProgress { percent: Option<u8>, message: String }
UpdateReady { message: String }
UpdateFailed(String)
ExitForUpdate
```

Commands (`gocode_core::AppCommand`):

```rust
AcceptUpdate       // Yes: download, verify, and stage the update
RejectUpdate       // No: dismiss for this startup
RestartForUpdate   // Close on the Completed screen: install and restart
```

---

# 92. Session Interaction

Before installation/restart:

```text
flush active session
```

Do not begin update replacement while an AgentRun is active.

---

# 93. Config Interaction

If:

```toml
check_on_startup = false
```

skip automatic check.

A future manual command may still allow:

```text
/check-update
```

Not required in the initial slash command set.

---

# 94. Updater Security Summary

The updater must enforce:

1. official GitHub release source;
2. HTTPS;
3. stable release filtering;
4. SemVer comparison;
5. deterministic platform asset selection;
6. SHA-256 verification;
7. staging before install — nothing is installed until the user confirms the Completed screen;
8. path validation;
9. backup before replacement;
10. rollback on failure;
11. no forced update;
12. no downgrade;
13. platform-appropriate replacement strategy (Windows: separate helper process; Linux: atomic in-place rename).

---

# 95. Definition of Done

The updater is ready when, on both Windows and Linux:

- update check runs after TUI startup, and is skipped in debug builds;
- GitHub failures do not block Gocode;
- latest stable release is parsed;
- SemVer comparison is correct;
- the popup appears only when a newer version exists for this platform;
- declining causes the prompt to reappear next startup;
- accepting downloads and verifies the correct platform artifact, showing live download progress;
- checksum is verified before anything is staged;
- partial downloads are not installed;
- the "Completed" screen appears only once the update is fully staged, with a Close button;
- pressing Close installs the update and restarts Gocode automatically;
- if automatic restart isn't possible, the user is told to reopen Gocode manually;
- rollback works on replacement failure;
- current installation remains unchanged after a verification/download failure.

---

# 96. Reference Successful Flow

```text
gocode 0.1.0 starts
↓
TUI opens
↓
background GitHub check
↓
0.2.0 found
↓
"New update" popup: Yes
↓
download (with progress %)
↓
SHA-256 verify
↓
extract to staging
↓
"Completed" screen: Close
↓
Windows: launch gocode-updater.exe, exit, helper replaces + restarts
Linux:   rename staged binary over installed, spawn new process, exit
↓
Gocode 0.2.0 starts
```

---

# 97. Reference Decline Flow

```text
0.2.0 available
↓
No
↓
continue using 0.1.0
↓
exit
↓
next startup
↓
0.2.0 still latest
↓
ask again
```

---

# 98. Reference Failure Flow

```text
0.2.0 available
↓
Yes
↓
download
↓
checksum mismatch
↓
"Failed" screen: Close
↓
0.1.0 remains installed
↓
continue using Gocode
```

---

# 99. Final Rule

The updater should make staying current easy without making the installation fragile.

> A failed update must be less disruptive than staying on the old version.
