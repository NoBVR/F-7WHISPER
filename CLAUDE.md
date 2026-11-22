# F-7WHISPER

Read [`docs/HANDOFF.md`](docs/HANDOFF.md) before making changes: the open tasks, the user's working preferences, the architecture, build steps and what has and hasn't been verified. Source code is authoritative if it goes stale.

## GitHub is the shared source of truth

Claude works on this app on two machines: the user's Windows PC (Windows app, C# in `src/`) and their Mac (Mac app, Tauri in `src-tauri/`). The shared page (`ui/index.html`) and `blur-engine/` belong to both. GitHub (`NoBVR/F-7WHISPER`, branch `main`) is how the two stay the same:

1. **Before any work, pull:** `git pull --rebase origin main`. Then read `docs/HANDOFF.md` §0 for work the other machine left for this one, and do it.
2. **After every change, push:** commit (see the commit rule below) and `git push origin main`, so the other machine can pull it. Don't leave finished work only on one machine.
3. **When a change needs the other platform too**, write it down in `docs/`: what changed, the contract, and a to-do list for the other host (see `docs/TWO-KEY.md`). Link it from `docs/HANDOFF.md` §0 and push it with the change. When that work is done on the other machine, mark it done there and push.
4. **Then update this machine's installed app** (Mac: replace `/Applications/F-7WHISPER.app`; Windows: the `dotnet publish` in `docs/HANDOFF.md` §7). Tell the user what the other machine still has to do.

## Hard rules

- Never edit `blur-engine/src/engine/*.rs`, `blur-engine/src/error.rs`, `blur-engine/src/app_state.rs` or `blur-engine/src/settings/mod.rs` (byte-identical copies of Blur AutoClicker v3.10.2). The macOS versions live in `blur-engine/src/engine_mac/`.
- Don't post real key presses or clicks on the user's machine (examples, tests, computer use) without asking.
- Commits go through a Sonnet subagent with the exact files to stage; end messages with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
