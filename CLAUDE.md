# F-7WHISPER

Read [`docs/HANDOFF.md`](docs/HANDOFF.md) before making changes: the open tasks, the user's working preferences, the architecture, build steps and what has and hasn't been verified. Source code is authoritative if it goes stale.

Hard rules:
- Never edit `blur-engine/src/engine/*.rs`, `blur-engine/src/error.rs`, `blur-engine/src/app_state.rs` or `blur-engine/src/settings/mod.rs` (byte-identical copies of Blur AutoClicker v3.10.2). The macOS versions live in `blur-engine/src/engine_mac/`.
- Don't post real key presses or clicks on the user's machine (examples, tests, computer use) without asking.
- Commits go through a Sonnet subagent with the exact files to stage; end messages with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
