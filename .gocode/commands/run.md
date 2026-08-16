---
description: Build and launch the Gocode TUI app to try it out
---

Build and run the Gocode binary so I can see it working.

Steps:
1. Run `cargo build --workspace` and report any build errors.
2. Launch the app with `cargo run -p gocode -- $ARGUMENTS` (or plain `cargo run -p gocode`
   if no arguments were given) so I can interact with the TUI.
3. If the app needs a terminal to run interactively, tell me the exact command to run myself
   instead of trying to drive the TUI non-interactively.
