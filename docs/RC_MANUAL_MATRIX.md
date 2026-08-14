# v0.0.9 release-candidate manual matrix

Record the runner or device, date, Gocode revision, terminal, and pass/fail
result for every row before v0.1.0. A failure involving crash, data corruption,
credential exposure, workspace escape, or updater rollback is release-blocking.

| Platform | Scenario | Result | Evidence / notes |
| --- | --- | --- | --- |
| Windows 10 | Windows Terminal + PowerShell: install, onboarding, Unicode path, coding flow, Ctrl+C, update/rollback/relaunch | Pending | |
| Windows 11 | Windows Terminal + PowerShell: install, onboarding, Unicode path, coding flow, Ctrl+C, update/rollback/relaunch | Pending | |
| Windows 10/11 | standalone PowerShell and `cmd`: install, PATH, resize, terminal restoration | Pending | |
| Linux x86_64 | xterm-compatible terminal: archive install, onboarding, coding flow, Ctrl+C, manual update | Pending | |
| Linux x86_64 | default XDG paths and overridden XDG paths | Pending | |
| Both | clean machine without Rust, credential persistence, cancellation/retry/restart | Pending | |
