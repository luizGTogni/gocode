# Install and update Gocode

Release archives and their `SHA256SUMS` file are published together on GitHub
Releases. Verify the archive before installing it. Gocode v0.1.0 supports
Windows 10/11 (x86_64) and Ubuntu LTS-compatible Linux (x86_64).

## Windows

In PowerShell, download `gocode-<version>-windows-x86_64.zip` and `SHA256SUMS`.
Verify the archive hash matches the line for that file, extract the installer,
then run:

```powershell
Expand-Archive .\gocode-<version>-windows-x86_64.zip .\gocode-installer
powershell -ExecutionPolicy Bypass -File .\gocode-installer\install-windows.ps1 -ArchivePath .\gocode-<version>-windows-x86_64.zip
```

The installer places `gocode.exe` and `gocode-updater.exe` in
`%USERPROFILE%\.gocode\bin` and adds that directory to the user PATH. Open a
new terminal and run `gocode`. The first launch guides NVIDIA credential and
model setup.

To update, accept the in-product update prompt. The updater verifies the
download checksum, replaces the executable after Gocode exits, and restores the
previous binary if replacement fails. If an update is interrupted, keep the
existing installation and rerun Gocode; reinstall the verified release archive
if either executable is missing.

To uninstall, close Gocode, remove `%USERPROFILE%\.gocode\bin` from your user
PATH, then remove `%USERPROFILE%\.gocode`. This also removes local logs and
configuration; remove the NVIDIA credential separately from Windows Credential
Manager if desired.

## Linux

Download `gocode-<version>-linux-x86_64.tar.gz` and `SHA256SUMS`, verify the
matching SHA-256 hash, extract the archive, then run:

```sh
tar -xzf gocode-<version>-linux-x86_64.tar.gz
./gocode-<version>-linux-x86_64/install-linux.sh ./gocode-<version>-linux-x86_64.tar.gz
```

The installer copies `gocode` to `${XDG_BIN_HOME:-$HOME/.local/bin}`. If that
directory is not already in PATH, add it through your shell's normal startup
configuration and open a new terminal. Run `gocode` inside a project to begin
onboarding.

Linux self-update is intentionally unavailable in the MVP. To update, download
and verify a newer archive and rerun the install script. To uninstall, remove
`${XDG_BIN_HOME:-$HOME/.local/bin}/gocode`; remove XDG Gocode config, state,
and cache directories if you also want to remove local settings and logs.

## Verify an archive

On Linux:

```sh
sha256sum --check SHA256SUMS --ignore-missing
```

On Windows, compare `Get-FileHash -Algorithm SHA256 <archive>` with the entry
in `SHA256SUMS`. Never install an archive whose hash does not match.
