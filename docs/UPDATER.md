# Gocode — Updater Specification

**Status:** Initial technical draft  
**Product:** Gocode  
**Target version:** v0.1.0  
**Scope:** Update discovery, download, verification, Windows self-update, rollback

---

# 1. Purpose

This document defines the Gocode update system.

The updater must allow a globally installed Gocode binary to discover and install newer stable releases with minimal user effort.

Required v0.1.0 behavior:

```text
installed version = 0.1.0
latest stable release = 0.2.0
↓
Gocode asks user
↓
Update now / Not now
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

but not in v0.1.0.

---

# 5. GitHub Release API

The update checker should query the repository's latest stable GitHub Release.

It should retrieve enough metadata to identify:

- release version;
- Windows asset;
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

And on Windows, two processes:

```text
gocode.exe
gocode-updater.exe
```

---

# 7. Why a Separate Updater

A running Windows executable should not rely on replacing itself in place.

Preferred flow:

```text
gocode.exe
↓
download new version
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

# 8. Crate Layout

Recommended:

```text
gocode-updater/
└── src/
    ├── checker.rs
    ├── github.rs
    ├── release.rs
    ├── version.rs
    ├── download.rs
    ├── checksum.rs
    ├── installer.rs
    ├── windows.rs
    ├── errors.rs
    └── lib.rs
```

A separate binary target may live in the same crate.

---

# 9. Current Version

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

# 10. Update Check Timing

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

---

# 11. Non-Blocking Failure

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

# 12. Update Check Frequency

v0.1.0 behavior:

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

# 13. Update Configuration

Global config:

```toml
[updates]
check_on_startup = true
```

No `ignored_version` field in the MVP.

---

# 14. Version Comparison

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

# 15. Tag Normalization

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

# 16. Release Metadata

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

# 17. Windows Asset Selection

Release pipeline should publish a deterministic Windows artifact name.

Example:

```text
gocode-x86_64-pc-windows-msvc.zip
```

or:

```text
gocode-windows-x86_64.zip
```

The exact name must be standardized before updater implementation.

---

# 18. Package Contents

Recommended Windows release archive:

```text
gocode.exe
gocode-updater.exe
```

Optionally:

```text
LICENSE
```

The updater should know exactly which executable must replace the installed binary.

---

# 19. Architecture Detection

v0.1.0 may initially support:

```text
x86_64 Windows
```

If ARM64 is supported, asset selection must use runtime architecture.

Do not download an incompatible binary.

---

# 20. Update Available Event

The checker emits normalized application event:

```rust
AppEvent::UpdateAvailable(UpdateInfo)
```

The TUI decides when to show the modal.

---

# 21. Active Agent Run

If an update is detected while an AgentRun is active:

```text
defer the update modal
```

until the task completes.

Do not interrupt coding work.

---

# 22. Update Prompt

Example:

```text
Gocode 0.2.0 is available

You're using 0.1.0

[ Update now ] [ Not now ]
```

Keep the interaction simple.

---

# 23. Declining an Update

If user selects:

```text
Not now
```

Gocode should:

- close the modal;
- continue normally;
- not persist an ignored version.

Next startup:

```text
0.2.0 still latest
↓
ask again
```

---

# 24. Accepting an Update

Flow:

```text
user selects Update now
↓
download asset
↓
verify integrity
↓
prepare updater
↓
launch updater
↓
exit current process
```

---

# 25. Download Location

Use a temporary/update staging directory.

Example:

```text
~/.gocode/cache/update/
```

or OS temp directory.

Recommended staged names:

```text
gocode-0.2.0.zip.part
gocode-0.2.0.zip
```

---

# 26. Partial Downloads

Never treat a partial download as valid.

Pattern:

```text
download to .part
↓
flush/close
↓
rename to final staged file
```

---

# 27. HTTPS

All release and asset downloads must use HTTPS.

Do not support insecure HTTP for official updates.

---

# 28. Integrity Verification

The updater must verify downloaded artifacts before installation.

Minimum MVP requirement:

```text
SHA-256 checksum
```

The release pipeline should publish checksums.

---

# 29. Checksum Distribution

Recommended GitHub Release asset:

```text
SHA256SUMS
```

or per-file:

```text
gocode-windows-x86_64.zip.sha256
```

The format must be standardized.

---

# 30. Verification Flow

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

# 31. Checksum Failure

If checksum mismatches:

```text
abort update
```

Do not replace the installed executable.

User-facing message:

```text
Gocode could not verify the update.

Your current installation was not changed.
```

---

# 32. Signature Verification

Cryptographic release signing is desirable future hardening.

Possible future technologies:

- Sigstore/cosign;
- minisign;
- platform code signing.

Not required to block v0.1.0 if SHA-256 is implemented correctly, but Windows code signing should be considered before wider distribution.

---

# 33. Windows Code Signing

A production release should eventually sign:

```text
gocode.exe
gocode-updater.exe
```

This improves Windows trust UX.

It is a release engineering concern separate from checksum verification.

---

# 34. Update Installer Input

The updater process should receive explicit paths and expected version.

Conceptual CLI:

```text
gocode-updater.exe
  --current <path>
  --new <path>
  --version 0.2.0
  --restart
```

Exact syntax is internal.

---

# 35. Updater Trust Boundary

The updater must not accept arbitrary unsafe replacement targets from untrusted model/tool input.

Only the Gocode application invokes it with validated paths.

---

# 36. Process Ownership

The updater should verify the target path corresponds to the installed Gocode binary it expects to replace.

Avoid generic "replace any executable" behavior.

---

# 37. Main Windows Flow

```text
gocode.exe running
↓
new binary staged
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

# 38. Waiting for Main Process Exit

The updater may need to wait until the old process fully exits.

Use:

- process handle if available;
- bounded retry/wait;
- clear failure behavior.

Never loop forever.

---

# 39. Backup Strategy

Before replacement:

```text
gocode.exe
↓
gocode.exe.old
```

Then place new executable:

```text
new → gocode.exe
```

After successful restart/verification:

```text
remove .old
```

---

# 40. Rollback

If replacement fails after backup:

```text
restore gocode.exe.old
```

The user's existing installation should remain usable.

---

# 41. Rollback Scope

MVP rollback should cover:

- file replacement failure;
- missing new binary;
- rename/copy errors.

Full transactional update systems are unnecessary.

---

# 42. Restart

After successful replacement:

```text
start gocode.exe
```

Prefer preserving:

- current working directory.

Future enhancement may preserve project/session continuation.

v0.1.0 can simply reopen Gocode in the same working directory if practical.

---

# 43. Restart Arguments

If Gocode was started with useful CLI arguments, updater may preserve them when safe.

Not required for first implementation.

---

# 44. Update UX Progress

During download/preparation, TUI may show:

```text
Updating Gocode...

Downloading 0.2.0
Verifying update
Preparing restart
```

Avoid exposing low-level file operations.

---

# 45. Terminal Shutdown Before Updater

Before exiting to updater:

1. flush session state;
2. flush logs;
3. restore terminal;
4. launch updater;
5. exit.

Do not leave terminal raw mode enabled.

---

# 46. Updater UI

The updater does not need a TUI.

It may run silently or print minimal console output.

Example:

```text
Updating Gocode...
Updated to 0.2.0.
```

If immediately restarting, even this may be unnecessary.

---

# 47. Update Failure UX

If update preparation fails while Gocode is still running:

```text
Gocode could not update.

Your current installation was not changed.

[ Continue ]
```

The user should remain in the current session.

---

# 48. Updater Failure After Main Exit

If updater fails after Gocode exits:

- restore old binary when possible;
- print a concise console error;
- preserve log location.

Example:

```text
Gocode update failed.
The previous version was restored.
```

---

# 49. Update Logs

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

# 50. GitHub Errors

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

# 51. Update Error Types

Conceptual:

```rust
pub enum UpdateError {
    Network(String),
    Timeout,
    InvalidRelease(String),
    InvalidVersion(String),
    AssetNotFound,
    ChecksumMissing,
    ChecksumMismatch,
    Download(String),
    Io(String),
    Replace(String),
    Rollback(String),
    Restart(String),
}
```

---

# 52. Update Checker Type

Conceptual:

```rust
pub struct UpdateChecker {
    source: Arc<dyn UpdateSource>,
    current_version: Version,
}
```

---

# 53. Update Source

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

# 54. Update Installer Type

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

# 55. Download Progress

Downloader may emit:

```rust
pub enum UpdateProgress {
    Starting,
    Downloading {
        downloaded: u64,
        total: Option<u64>,
    },
    Verifying,
    PreparingInstall,
    Restarting,
}
```

TUI can simplify the display.

---

# 56. Cancellation

Before the updater process starts, user cancellation may be allowed.

Once binary replacement begins:

```text
do not expose arbitrary cancellation
```

A half-applied update is worse than finishing a short atomic operation.

---

# 57. Update During Permission Modal

Do not show an update modal over an active permission modal.

Queue the update notification.

---

# 58. Update During Onboarding

Recommended:

```text
finish onboarding first
```

Then show update prompt.

Do not distract the user before they can enter the product.

---

# 59. Update Check Privacy

The GitHub update request may reveal normal network metadata such as:

- IP;
- User-Agent.

Do not send:

- project path;
- user prompt;
- API key;
- model selection unless technically necessary.

---

# 60. GitHub Authentication

Public release checks should not require a GitHub token.

If unauthenticated rate limits become an issue, caching can be improved later.

Do not ask users for a GitHub token just to update Gocode.

---

# 61. Release Asset Naming

Standardize naming early.

Recommended example:

```text
gocode-v0.2.0-x86_64-pc-windows-msvc.zip
gocode-v0.2.0-x86_64-pc-windows-msvc.zip.sha256
```

Consistency simplifies updater logic.

---

# 62. Archive Extraction

If distributing ZIP:

```text
download zip
↓
verify zip checksum
↓
extract into staging directory
↓
validate expected files
↓
install
```

Never extract directly over the live installation.

---

# 63. Archive Safety

ZIP extraction must prevent path traversal.

Reject entries such as:

```text
../../evil.exe
```

Only accept expected file names/paths.

---

# 64. Expected Files

For Windows release:

```text
gocode.exe
gocode-updater.exe
```

Validate they exist before starting replacement.

---

# 65. Updating the Updater

The release may also contain a new:

```text
gocode-updater.exe
```

The update strategy must account for replacing the updater itself safely.

One simple approach:

1. current updater performs main binary replacement;
2. updater replacement is staged for next launch or copied under a temporary name;
3. cleanup occurs after process exit.

The exact implementation should be designed carefully for Windows file-lock behavior.

---

# 66. Simplified MVP Updater Strategy

For first implementation, acceptable approach:

- keep a small stable updater binary;
- update `gocode.exe` first;
- defer updater self-replacement if necessary.

Do not let updater self-update complexity block the MVP.

---

# 67. Installation Path

Expected default:

```text
%USERPROFILE%\.gocode\bin\
```

Containing:

```text
gocode.exe
gocode-updater.exe
```

Updater must resolve the actual running binary path rather than blindly assuming the default.

---

# 68. Portable/Alternate Installs

Future users may place Gocode elsewhere.

Use:

```text
current_exe()
```

to determine the active executable location.

Only update installations the process can write to.

---

# 69. Permission Failure

If installation directory is not writable:

```text
abort safely
```

Show:

```text
Gocode does not have permission to update this installation.
```

Future installer methods may handle elevated/system installs separately.

---

# 70. Global User Install

The MVP should prefer per-user installation so updates do not require administrator privileges.

This aligns with:

```text
%USERPROFILE%\.gocode\bin
```

---

# 71. Release Pipeline Contract

The updater depends on release engineering.

GitHub Actions should produce:

```text
Windows binary/archive
checksum
GitHub Release
stable version tag
```

Updater design and release pipeline must share the same asset naming contract.

---

# 72. Release Pipeline Concept

```text
git tag v0.2.0
↓
GitHub Actions
↓
cargo test
↓
cargo build --release
↓
package Windows assets
↓
SHA-256
↓
publish GitHub Release
```

---

# 73. Release Notes

The update modal does not need to display full release notes.

Future option:

```text
View what's new
```

Not required for v0.1.0.

---

# 74. Forced Updates

Do not implement forced updates in v0.1.0.

The user must be able to select:

```text
Not now
```

---

# 75. Automatic Silent Installation

Do not silently install updates without user approval in v0.1.0.

Check automatically, install explicitly.

---

# 76. Downgrade

Updater does not support downgrade through normal flow.

Manual downgrade remains outside MVP.

---

# 77. Prerelease Builds

A developer build with version such as:

```text
0.2.0-alpha.1
```

should not automatically jump channels without a defined policy.

For stable public builds, use stable latest release only.

---

# 78. Development Builds

Local development binaries may disable update checks by:

- build flag;
- config;
- repository detection;
- version convention.

The exact policy should prevent developers from accidentally overwriting debug builds.

---

# 79. Debug Update Source

Tests may use a fake source.

Do not require GitHub network access for updater unit tests.

---

# 80. Fake Update Source

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

# 81. Fake Installer

Installer behavior should be testable with temporary directories.

Avoid replacing the test runner executable.

---

# 82. Version Tests

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
```

---

# 83. Download Tests

Test:

```text
success
timeout
partial failure
resume not required
wrong content length
temp file cleanup
```

---

# 84. Checksum Tests

Test:

```text
correct checksum
wrong checksum
missing checksum
invalid checksum format
```

---

# 85. Windows Installer Tests

Test in isolated temp directory:

```text
replace binary
backup old
failure during replace
rollback
locked file
missing staged file
restart path construction
```

---

# 86. Integration Test

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

# 87. Real Windows Manual Test

Before v0.1.0:

1. install `0.0.x`;
2. publish test release;
3. launch Gocode;
4. receive prompt;
5. decline;
6. relaunch and confirm prompt appears again;
7. accept;
8. verify checksum;
9. verify process restart;
10. verify new version;
11. verify old binary cleanup.

---

# 88. Recovery Manual Test

Simulate:

- no network;
- bad checksum;
- locked install directory;
- missing updater;
- failed replacement.

Current version must remain usable whenever possible.

---

# 89. App Event Integration

Events:

```rust
AppEvent::UpdateAvailable(UpdateInfo)
AppEvent::UpdateProgress(UpdateProgress)
AppEvent::UpdateFailed(UpdateError)
```

Commands:

```rust
AppCommand::AcceptUpdate
AppCommand::RejectUpdate
```

---

# 90. Session Interaction

Before installation/restart:

```text
flush active session
```

Do not begin update replacement while an AgentRun is active.

---

# 91. Config Interaction

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

# 92. Updater Security Summary

The updater must enforce:

1. official GitHub release source;
2. HTTPS;
3. stable release filtering;
4. SemVer comparison;
5. deterministic platform asset selection;
6. SHA-256 verification;
7. staging before install;
8. path validation;
9. backup before replacement;
10. rollback on failure;
11. no forced update;
12. no downgrade.

---

# 93. Definition of Done

The updater is ready for v0.1.0 when:

- update check runs after TUI startup;
- GitHub failures do not block Gocode;
- latest stable release is parsed;
- SemVer comparison is correct;
- update modal appears only when newer version exists;
- declining causes the prompt to reappear next startup;
- accepting downloads the correct Windows artifact;
- checksum is verified;
- partial downloads are not installed;
- Gocode exits cleanly;
- updater replaces the binary;
- rollback works on replacement failure;
- new Gocode restarts successfully;
- current installation remains unchanged after verification/download failure.

---

# 94. Reference Successful Flow

```text
gocode 0.1.0 starts
↓
TUI opens
↓
background GitHub check
↓
0.2.0 found
↓
Update modal
↓
Update now
↓
download
↓
SHA-256 verify
↓
stage
↓
launch updater
↓
restore terminal
↓
gocode exits
↓
backup 0.1.0 binary
↓
install 0.2.0
↓
restart
↓
Gocode 0.2.0 starts
```

---

# 95. Reference Decline Flow

```text
0.2.0 available
↓
Not now
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

# 96. Reference Failure Flow

```text
0.2.0 available
↓
Update now
↓
download
↓
checksum mismatch
↓
abort
↓
0.1.0 remains installed
↓
show non-destructive error
↓
continue using Gocode
```

---

# 97. Final Rule

The updater should make staying current easy without making the installation fragile.

> A failed update must be less disruptive than staying on the old version.
