# F-7WHISPER handoff: Windows PC → Mac

Written 2026-11-22 by Claude on the user's Windows PC so Claude on the user's Mac can continue locally. `main` was at `ea2f119` when this was written. Source code is authoritative if this file goes stale.

---

## 0. Start here: what the user asked for last

The user tested the Mac app (`F-7WHISPER.dmg`) on their real Mac and reported, in their words:

> "Its not registering macos keys and it also needs support for 2 keys so like ctrl + f7 as a hold keybind"

The Windows session was stopped before investigating, so **nothing has been changed for this yet**.

### Task A: "not registering macos keys" (unclear, ask first)

Ask the user which of these they mean before changing code:

1. Pressing a key in the Add dialog's **Key** box or **Activate** box does nothing.
2. The **Start all / Stop all** hotkeys (defaults F6 / F7) don't fire.
3. A row's **activate key** (Hold or Press) doesn't start the row.
4. A key row runs (the count goes up) but **other apps don't receive the presses**.

Likely causes to check on the real Mac:

- **Mac F-keys are media keys by default.** On Apple keyboards F1–F12 send brightness, volume, media and similar unless `fn` is held, or *System Settings → Keyboard → Keyboard Shortcuts → Function Keys → "Use F1, F2, etc. keys as standard function keys"* is on. The defaults F6 / F7 may therefore never arrive as F6 / F7.
- **macOS reserves Control+F1 … F8** for keyboard navigation. For example, Control+F7 toggles how Tab moves focus. These are under *System Settings → Keyboard → Keyboard Shortcuts → Keyboard*. The system may swallow them before the page or the hotkey API sees them. That directly affects the user's example, "ctrl + f7".
- **Accessibility permission.** Without it, macOS silently drops every posted key and click. Blur rows are also deliberately **paused** while `AXIsProcessTrusted()` is false (`src-tauri/src/engine.rs`, `trusted_cached`). The side-button listener (CGEventTap in `src-tauri/src/mouse_macos.rs`) may also need Input Monitoring. This is not verified.
- **Ad-hoc signing resets permissions.** The app is ad-hoc signed (`signingIdentity: "-"`), so every rebuild has a new code signature. macOS may then treat it as a new app, and the old Accessibility toggle no longer applies. Remove F-7WHISPER from the Accessibility list, then add it again. With `npx tauri dev`, the permission belongs to the terminal app that launched it (Terminal or iTerm), not to F-7.
- **Hotkeys whose code is `Vk<n>`** (keys without a DOM `code`) can't be registered on macOS: `plugin_shortcut` in `src-tauri/src/hotkeys.rs` returns `None` for them.

### Task B: two-key combos such as Ctrl+F7 as a Hold activate key

**Most of this already exists. Find what fails on the Mac before building anything new.**

- **UI:** the capture waits for a non-modifier key and records the modifiers with it. `hotkeyFrom(e)` in `ui/index.html` stores `{ code, vk, ctrl, alt, shift, meta, label }`. The Activate capture (`#trigCapture`) and the Settings hotkey capture both skip pure modifier presses (`MOD_CODE` test) and take the next key. Mac labels use ⌃⌥⇧⌘ (for example `⌃F7`); Windows labels use `Ctrl + F7`.
- **Mac host:** `src-tauri/src/hotkeys.rs` `plugin_shortcut()` builds a `tauri-plugin-global-shortcut` `Shortcut` with `Modifiers::CONTROL/ALT/SHIFT/SUPER`. Hold mode starts the row on `ShortcutState::Pressed` and stops it on `Released`.
- **Windows host (C#):** `src/MainForm.cs` calls `RegisterHotKey` with MOD flags (`src/Settings.cs` line ~63: Alt=1, Ctrl=2, Shift=4, Win=8). Hold release is detected by polling `GetAsyncKeyState(vk)` every 10 ms.
- **Settings model** (both hosts): `Hotkey = Key + ctrl/alt/shift/meta` (`src-tauri/src/settings.rs`, `src/Settings.cs`).

So on the Mac, test **Ctrl+F7 in Hold mode** and also **Ctrl+G in Hold mode**. If Ctrl+G works and Ctrl+F7 doesn't, the cause is the macOS reservation or media-key behaviour above, not missing support. If the user means **two ordinary keys** (for example `F7 + G`), that is genuinely new. Confirm it with the user, then change the UI capture, the settings model and both hosts. Any change to the `Hotkey` shape must stay readable by the Windows C# host (old settings files must keep loading).

---

## 1. How this user works (carry these over)

- **Plain, short explanations.** About 2 sentences per point. The user decides things in words. **Never** ask them to write code (no `TODO(human)` "Learn by Doing" requests).
- **No helper or subtext lines in the UI** unless the information is truly needed.
- **Real platform palettes only** (Vercel/Geist greys, Linear, Apple). Invented themed looks are rejected as "AI slop".
- **Commits:** done by a Sonnet subagent at medium effort, given the exact list of files to stage. End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Push to `main` on `NoBVR/F-7WHISPER` (private).
- **Model routing on the PC:**
  - Claude does all front-end work (`ui/index.html`).
  - Backend went to Codex `gpt-6.2-astra` at xhigh: one run at a time, short briefs naming exact files, `--sandbox read-only`, answering with a `git apply` diff.
  - If Codex isn't set up on the Mac, ask the user before substituting. Last time they chose "Claude writes it now" (Opus subagents).
- **Don't drive the user's screen** (computer use, synthetic input) for testing without asking. They often have Roblox or Discord open. Don't run anything that posts real keys or clicks on their Mac without asking: `blur-engine/examples/mac_rate` presses F20 and **left-clicks at the cursor** at 1,000 a second for 3 s.
- The user also runs **Blur AutoClicker** itself. Its events carry the same tag `0x800DA5A5`, so a listener can't tell the two apart.
- **After every F-7 change, update both deliverables unasked:**
  - On the Mac: rebuild and replace the app in `/Applications`.
  - Tell the user the Windows app also needs a rebuild on the PC, and list what changed for Windows.
- Never read `~/.codex/auth.json`. Treat the user's personal details as private.

---

## 2. What F-7WHISPER is

A small auto key presser and auto clicker (Windows + macOS). The window is a table of **rows**. Each row has:

- **Input:** a key (`kind: "key"`) or a click (`kind: "click"`). Clicks can be Left, Right, Middle, Back or Forward, at the cursor or at a picked point `at: {x, y}`.
- **Every:** the interval, 1 ms to 86,400,000 ms. 1 ms means 1,000 a second.
- **Only in** (optional): run only while a named app is in front.
- **Activate** key (optional): `mode: "hold"` (runs while held) or `"toggle"` (shown as "Press": each press starts or stops the row). Mouse 4 / Mouse 5 (side buttons) are allowed as activate keys and hotkeys: codes `MouseBack` / `MouseForward`, vk 5 / 6.
- Start / Stop button and a press counter.

Global **Start all / Stop all** hotkeys default to F6 / F7 and can be changed in Settings. Settings also has **"Stop when switching apps"**. A pin button keeps the window on top. Rows pause while F-7 itself is the frontmost app. The window is 800 px wide and resizes itself to fit up to 8 rows (no maximize or resize). The version shown is `v1.0.0`.

---

## 3. Repo layout

| Path | What |
|---|---|
| `ui/index.html` | The whole front end: one file of HTML/CSS/JS, no build step. Shared by both hosts. Opened in a plain browser, it runs a mock backend. |
| `ui/fonts/` | Bundled fonts (UI uses SF Pro on Mac via `-apple-system`, Geist fallback) |
| `src/*.cs`, `F7Whisper.csproj` | **Windows app** (the one the user installs on Windows): .NET 10 WinForms + full-window WebView2 |
| `src-tauri/` | **Mac app** (Tauri 2.10, Rust). Also builds on Windows, but there it's only a secondary build. |
| `blur-engine/` | Blur AutoClicker v3.10.2's engine (GPL-3.0) plus F-7's glue. Used by both apps. |
| `.github/workflows/mac.yml` | Manual CI: `dmg` job (universal dmg) and `engine` job (Blur tests + speed checks on macOS) |
| `assets/` | Icon sources: `icon-mac-1024.png` (824 px circle on the 1024 grid), `icon-win-1024.png`, `app.ico` |
| `src-tauri/icons/` | Generated icons (`icon.icns`, PNGs, `icon.ico`) |

Key Mac files in `src-tauri/src/`:

| File | Job |
|---|---|
| `main.rs` | Message handling, state, window, single instance |
| `engine.rs` | Row scheduler. Blur rows (`mod blur`, `process_blur`) plus F-7's own scheduler for Back/Forward |
| `hotkeys.rs` | Global hotkeys and activate keys via `tauri-plugin-global-shortcut` |
| `macos_keys.rs` | DOM `code` → mac keycode, posting keys, `trusted()`, `prompt()`, `open_settings()` (Accessibility) |
| `mouse_macos.rs` | CGEventTap for mouse side buttons |
| `desktop_macos.rs` | Front app (NSWorkspace), app list, clicks, point picking |
| `settings.rs` | Settings model and file |
| `window.rs` | Window sizing |

---

## 4. One page, two hosts: the contract

**Bridge:** the page prefers `window.__TAURI__`, using `invoke("page_message", …)` and `listen("host", …)`. Otherwise it uses WebView2 `chrome.webview.postMessage`.

**Page → host** (`send({type, …})`):

| Message | Fields |
|---|---|
| `ready` | |
| `add` | `input` |
| `edit` | `id`, `input` |
| `remove` | `id` |
| `toggle` | `id` |
| `startAll`, `stopAll` | |
| `pin` | `on` |
| `settings` | `hotkeys: {start, stop}`, `switchStop` |
| `pauseHotkeys` | `on`: true while a capture box is listening, so hotkeys don't fire |
| `listApps` | |
| `pickPoint`, `pickCancel` | |
| `fit` | `height`, `width` |
| `window` | `action`: `minimize`, `close` or `drag` |
| `openAccessSettings` | (Mac) |

**Input shape:**
- Key row: `{ kind: "key", key: {label, code, vk}, intervalMs, app, trigger }`
- Click row: `{ kind: "click", button: "left"|"right"|"middle"|"back"|"forward", at: {x,y}|null, intervalMs, app, trigger }`
- `trigger`: `null` or `{ hotkey: {label, code, vk, ctrl, alt, shift, meta}, mode: "hold"|"toggle" }`

**Host → page** (Tauri event `host`):

| Message | Fields |
|---|---|
| `state` | rows (each with `running`, `count`), `version`, `hotkeys`, `hotkeyOk: {start, stop}`, `switchStop`, `pinned`, `platform` (`"macos"` on Mac), `accessOk` |
| `counts` | |
| `apps` | `apps: string[]` |
| `point` | `x`, `y` |

Points are CoreGraphics global points (top-left of the main display).

**Conflicts:** a hotkey or activate key that clashes with Start/Stop or with a key a row presses gets a red ring and blocks Save. The hosts also report `hotkeyOk`.

**Window size:** the page sends `fit`.
- Height = `32 title + 52 toolbar + 36 table header + 48 × min(rows, 8)` (or 96 when there are no rows) `+ 16`.
- The Rust startup estimate is `136 + body`.
- On Mac the title bar is an overlay with traffic lights at `{x:16, y:16}` (`src-tauri/tauri.macos.conf.json`). `body.mac` hides the Windows caption buttons and pads the title 84 px.

**Settings file:**
- Mac: `~/Library/Application Support/F-7WHISPER/settings.json` (`dirs::config_dir()`)
- Windows: `%APPDATA%\F-7WHISPER\settings.json`

Back it up before tests that add rows.

---

## 5. Blur engine (`blur-engine/`)

The user asked for a 1:1 copy of Blur AutoClicker v3.10.2's engine (GitHub `Blur009/Blur-AutoClicker`, tag `v3.10.2`, commit `bfabfa9`, GPL-3.0; fine while F-7 stays private).

- **Never edit these.** They are byte-identical to Blur's release, and their git blob ids match GitHub's:
  - `blur-engine/src/engine/*.rs` (9 files)
  - `src/error.rs`, `src/app_state.rs`, `src/settings/mod.rs`
  - Check: `git hash-object blur-engine/src/engine/worker.rs` should give `38bee08a…`, the same as `gh api "repos/Blur009/Blur-AutoClicker/contents/src-tauri/src/engine?ref=v3.10.2" --jq '.[] | .name + " " + .sha'`.
- **macOS port:** `src/lib.rs` picks `engine/` on Windows and `#[path = "engine_mac/mod.rs"]` on macOS.
  - `engine_mac/mod.rs` pulls Blur's OS-free `cycle`, `failsafe`, `rng`, `stats` by path (the same files).
  - `engine_mac/{keyboard,mouse,process,worker,mod}.rs` are copies of Blur's files with only the Windows calls swapped:
    - Quartz `CGEvent` posts at the HID tap, with field 42 (`kCGEventSourceUserData`) set to Blur's tag `0x800DA5A5`;
    - a Windows-VK → mac-keycode table;
    - NSWorkspace for the front app;
    - `NSEvent` double-click and key-repeat values.
  - Each swapped spot is marked `// macOS:`.
- **Timing lesson.** On macOS, ordinary sleeps are coalesced: `sleep(1 ms)` took ~5 ms on GitHub's Mac, even at QoS user-interactive. The Mac `TimerResolutionGuard` therefore sets a **Mach `THREAD_TIME_CONSTRAINT_POLICY`** (1 ms period, 0.5 ms computation) on the click thread. This is the counterpart of Blur's `NtSetTimerResolution`. Result on CI: 997 presses/s and 996 clicks/s at 1 ms, versus ~510/s before. It has not been measured on the user's real Mac.
- **Fixes on top of the port:**
  - Clicks use the run's own last posted move point for 100 ms, because the cursor updates asynchronously.
  - The host caches `AXIsProcessTrusted()` for 500 ms.
- **Glue:**
  - `src/f7.rs`: `BlurRun::start(BlurInput, interval_ms)`, `set_paused`, `clicks`, `is_running`, `stop_reason`, `stop`; also a C ABI `f7_blur_*` used by the Windows C# app through `blur_engine.dll`.
  - `tauri-shim/` gives each run its own app state, so rows run concurrently.
  - The other files under `src/` are UI stand-ins.
- **Settings F-7 passes:** duty 45%, no speed randomization, failsafes, zones and process list off, one click point (radius 0, nudge size 1 px with 0% chance, so it re-aims every batch), `keyboard_uppercase` = Caps Lock state.
- **Routing:** key rows and Left/Right/Middle click rows use Blur. **Back/Forward** clicks stay on F-7's own scheduler (Blur has no side-button output).
- **Tests:** `cargo test --manifest-path blur-engine/Cargo.toml` should give 147 passed on Windows and macOS.
- **Examples (macOS):**
  - `mac_rate` posts **real** F20 presses and left clicks. Ask the user first.
  - `mac_timing` measures sleep accuracy and event-post cost; part of it also posts F20 and clicks.
  - Both run in CI.
- **Verification map** (private artifact, updated for the Mac port): https://claude.ai/artifact/1ZN3Vrxbfp2q1aTyA2CMKX

---

## 6. Build, run and ship on the Mac

**Prerequisites:**
- Xcode Command Line Tools: `xcode-select --install`
- Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Node 22
- GitHub CLI, signed in to the user's account (`gh auth login`). The repo is private.

```sh
git clone https://github.com/NoBVR/F-7WHISPER.git && cd F-7WHISPER
npm ci
npx tauri dev                                   # debug run (Accessibility goes to the terminal app)
npx tauri build --bundles dmg                   # this Mac's architecture only
# universal like CI: rustup target add aarch64-apple-darwin x86_64-apple-darwin
npx tauri build --target universal-apple-darwin --bundles dmg
cargo test --manifest-path blur-engine/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
```

- **Bundle output:** `src-tauri/target/<target>/release/bundle/{macos/F-7WHISPER.app, dmg/*.dmg}`. For the native build there's no `<target>` folder: `src-tauri/target/release/bundle/…`.
- **Install:** replace `/Applications/F-7WHISPER.app`, then re-grant Accessibility (see Task A).
- **Gatekeeper:** a downloaded or copied dmg/app shows "can't check it for malware". Either use *System Settings → Privacy & Security → Open Anyway*, or run `xattr -dr com.apple.quarantine /Applications/F-7WHISPER.app`.
- **CI:** `gh workflow run mac.yml --repo NoBVR/F-7WHISPER` (add `--ref <branch>` to test a branch first). The artifact is `F-7WHISPER-mac`: `gh run download <id> --repo NoBVR/F-7WHISPER --name F-7WHISPER-mac`.
- **Checking C# on the Mac (untested idea):** `dotnet build F7Whisper.csproj -p:EnableWindowsTargeting=true -p:SkipBlurEngineBuild=true` might compile-check the Windows host. It can't run there.

---

## 7. The Windows side (for reference; done on the PC)

- **Installed** at `%LOCALAPPDATA%\Programs\F-7WHISPER` (single-file exe + `ui` folder), with a Desktop shortcut.
- **Rebuild and install:** `dotnet publish -c Release -r win-x64 --self-contained false -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true -o %LOCALAPPDATA%\Programs\F-7WHISPER`. The csproj runs `cargo build --release` for `blur_engine.dll` first.
- **Windows code:**
  - keys use `SendInput` scancodes;
  - hotkeys use `RegisterHotKey`, plus a `WH_MOUSE_LL` hook for side buttons;
  - Blur rows use the DLL (`src/BlurEngine.cs`, `src/KeyEngine.cs` `ProcessBlur`).
- **Measured on Windows:** 999 presses/s from a 1 ms F24 row, every press tagged `0x800DA5A5`.

---

## 8. Verified vs not verified

**Verified:**
- Windows: UI and dialogs over CDP, key rows at 999/s.
- Blur's 147 tests on Windows and macOS.
- Mac CI builds; Blur on GitHub's macOS 26 VM at ~997/s with every event delivered.

**Not verified on a real Mac:**
- traffic-light position and title bar;
- key and click delivery into real apps;
- the Accessibility flow;
- Start/Stop hotkeys and activate keys (Hold / Press), modifier combos, mouse side buttons;
- point picking and "Only in";
- the real-hardware rate;
- whether `NSWorkspace` calls off the main thread misbehave.

The user's first real-Mac report is Task A / Task B above.

---

## 9. Recent history (`git log --oneline`)

| Commit | Change |
|---|---|
| `ea2f119` | macOS: real-time thread policy in place of Blur's 1 ms timer request |
| `ba4085e` | Add a macOS timing check for Blur's engine |
| `bf8c4f6` | Run Blur's engine on macOS too |
| `b9733f5` | Run rows on Blur AutoClicker's engine (v3.10.2) on Windows |
| `ac70257` / `181cf46` | Mac / Windows icon: circle |
| `95d60c4` | Tighten the gap under the title bar (title bar 32 px) |
