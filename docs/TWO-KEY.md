# Two-key activate keys

Added on the Mac on 2026-11-22. A row's activate key (Hold or Press) can be two ordinary keys held together, for example `F7 + G`, optionally with modifiers (`⌃F7 + G`). A Settings switch, **Two-key activate keys**, turns the feature off.

**Status:**

- **Mac host (`src-tauri/`) and shared page (`ui/index.html`):** written, type-checked and installed on the user's Mac. Page capture and conflicts were checked in the browser mock. **Not yet tested with real key presses**, and the `cfg(windows)` paths of the Tauri build couldn't be compiled on the Mac.
- **The Windows C# host (`src/*.cs`) is not updated yet.** The page hides the feature from a host that doesn't report it (see "Page gate"), so the current Windows app keeps working as before after a rebuild. Section "Windows: what to change" is the to-do list for the PC.

## Contract (both hosts)

### Hotkey shape

A hotkey can have an optional second key, `with`:

```json
{ "label": "F7 + G", "code": "F7", "vk": 118,
  "ctrl": false, "alt": false, "shift": false, "meta": false,
  "with": { "label": "G", "code": "KeyG", "vk": 71 } }
```

- `label` is the full label the page shows: `F7 + G` on Windows, `⌃F7 + G` on Mac (modifier symbols before the first key).
- `with` is missing or `null` for a one-key hotkey. Old settings files have no `with` and load unchanged.
- `with` never names a modifier (`ShiftLeft`, `ControlRight`, ...), a mouse button (`MouseBack`, `MouseForward`), or the same key as `code`. A host rejects such a hotkey.
- Only row activate keys (`trigger.hotkey`) use `with`. Start all / Stop all stay one key. The page never sends `with` for them.
- **Identity is unordered:** `F7 + G` and `G + F7` (same modifiers) are the same binding.

### Settings

- New setting `twoKey` (bool, default `true`), saved in `settings.json` as `"twoKey"`.
- The page sends it in the `settings` message: `{ type: "settings", hotkeys, switchStop, twoKey }`.
- **Page gate:** a host that supports two-key keys sends `twoKey` (a boolean) in every `state` message. If `state.twoKey` is missing, the page hides the switch and captures one key, as before.

### When it fires

- The combo is **down** when both keys are physically held, in either order, and the modifiers (Ctrl/Alt or Option/Shift/Win or Cmd) at the moment the second key goes down match exactly. Caps Lock and fn don't count.
- It is **up** as soon as either key is released.
- **Hold:** the row runs while the combo is down. **Press:** each time the combo goes down toggles the row.
- **Swallowing:** while the combo is down, the key that completed it is swallowed: its down, its auto-repeats and its matching up. The key that was already held passed through to apps as normal. Each key on its own always reaches apps normally. Never register either key alone with the OS hotkey API.
- **Synthetic input is ignored:** F-7's own posted keys (own process id on Mac, `LLKHF_INJECTED` on Windows) and any event tagged `0x800DA5A5` (Blur's tag, also used by Blur AutoClicker) never change combo state.

### When it can't work

- The combo is not registered, and the row reports `triggerOk: false` (red ring on its chip), when:
  - the switch is off;
  - the listener can't run (Mac: no Accessibility);
  - a key has no mapping (for example `Vk<n>` codes on Mac).
- **Never fall back to the first key alone.**
- **Release safety:** Hold rows bound to a combo stop when:
  - a capture box opens (`pauseHotkeys`);
  - the switch is turned off;
  - Accessibility is lost;
  - the listener dies;
  - the row is edited or removed.

### Page capture (Activate box)

- The first non-modifier key is held as pending, and the box shows `F7 + …`.
- With the switch on, a second, different non-modifier key pressed while the first is still held finishes as a two-key combo.
- These finish as one key, the first:
  - releasing the first key;
  - releasing any modifier (WebKit drops some key-ups while ⌘ is held);
  - the box losing focus.
- With the switch off, or when the host doesn't report `twoKey`, the first key finishes the capture at once (old behaviour).
- Mouse 4 / Mouse 5 still finish at once as a one-key activate key.
- **Conflicts:** a combo never conflicts with a row's pressed key, because posted keys are ignored. A row's single key still conflicts with a one-key activate key as before.
- **Overlap:** a combo conflicts with a one-key hotkey that is one of its two keys with the same modifiers. That covers Start all / Stop all and other rows' one-key activate keys, in both directions. With the default Stop all = F7, `F7 + G` is refused until Stop all is changed, because F7 alone would stop everything first.
  - The page marks it red and blocks Save.
  - The host also refuses to register such a combo (`triggerOk: false`), which covers a Start/Stop change made later in Settings.

## Mac host: how it's done

- `settings.rs`: `Hotkey.with: Option<Key>`, unordered `same_binding`, `Settings.two_key`.
- The side-button event tap (`mouse.rs` / `mouse_macos.rs`) now also watches key down/up when combos are bound:
  - It is an active `CGEventTap` at the session level, so it needs Accessibility only, like the side buttons.
  - It keeps a held-key bitmap, posts `ShortcutEdge` edges for combo tokens and swallows the completing key.
  - After a tap timeout it re-checks the held keys with `CGEventSourceKeyState`.
  - Before a combo fires, it confirms the other key is really down with `CGEventSourceKeyState`. A key-up the tap never saw (for example under Secure Input) can't leave a stale key that completes a combo on its own.
  - A fresh, non-repeat press of a key still marked held settles the missed key-up first.
- `hotkeys.rs`: combo hotkeys register only through the tap (`Registration::Combo`), never through `tauri-plugin-global-shortcut`. A combo that overlaps a one-key binding (`Hotkey::overlaps`) is dropped before registering.
- `main.rs`: `twoKey` in `state`, the `settings` message and the save.
- `engine.rs` `Rows::edit`: a running Hold row stops when its activate key is changed or removed. The old key's release would no longer reach the row, so before this it kept running. Editing anything else, including Hold → Press with the same key, keeps it running, as before.

## Windows: what to change (not done; do on the PC)

Nothing here has been compiled: this Mac has no `dotnet`. Read the current code first, because the line numbers drift.

1. **`src/Settings.cs`**
   - `Hotkey` gets an optional second key: `KeyChoice? With`. `Hotkey.Read` parses `with` with `KeyChoice.Read`. It rejects modifier VKs (0x10–0x12, 0x5B, 0x5C, 0xA0–0xA5), mouse VKs 5/6 and `With.Vk == Vk`, and treats missing or `null` as one key.
   - `Chord` must include the second key, or `F7 + G` collides with `F7` in `_registered`, `wanted` and `_triggerOk`. For example `Chord(int Vk, uint Modifiers, int With = 0)`, stored unordered (smaller VK first when `With != 0`) so `F7+G == G+F7`.
   - `Settings` gets `bool TwoKey` (default `true`), read from `"twoKey"` and saved by the camelCase serializer. Old files without it load as `true`.
   - Serialising `Hotkey` must write `with` (or `null`) back out, so the page and the file keep the second key.
2. **`src/MainForm.cs`**
   - In `state`, send `twoKey = _twoKey`. This is what turns the feature on in the page.
   - In `settings`, read `twoKey` (bool; keep the current value if missing), store it, and call `RegisterHotkeys()`.
   - In `RegisterHotkeys` / `ApplyBindings`, never pass a two-key chord to `RegisterHotKey`. `RegisterHotKey` can't express two plain keys, and registering one of them would swallow it everywhere.
   - Give two-key chords to a keyboard hook (below) only while `_twoKey` is on, and report `_triggerOk` for them as "hook running and switch on".
   - Skip a two-key chord that overlaps a one-key binding in `wanted`, i.e. one of its keys with the same modifiers is Start, Stop or another row's one-key activate key. Report it as not OK. (Mac: `Hotkey::overlaps` in `src-tauri/src/settings.rs`.)
3. **New keyboard hook**, modelled on `src/MouseHotkeys.cs` (`WH_MOUSE_LL` on its own thread with an edge queue):
   - `WH_KEYBOARD_LL`. Skip events with `LLKHF_INJECTED` or `dwExtraInfo == 0x800DA5A5`.
   - Keep a set of held VKs. Map left/right modifier VKs to Ctrl/Alt/Shift/Win and don't treat them as combo keys.
   - On a key-down that completes a bound combo with matching modifiers, first confirm the other key with `GetAsyncKeyState` (a missed key-up must not leave it "held"). Then queue a down edge, mark that VK swallowed and return 1. Swallow its repeats, and its key-up when it comes.
   - On either key's up: queue an up edge.
   - Install the hook only while at least one combo is bound. On dispose, queue up edges for any active combo.
4. **`src/KeyEngine.cs`**
   - Hold release for combos must come from the hook's up edge, the way `ReleaseMouseHold` works for side buttons.
   - The `GetAsyncKeyState(vk)` polling in `PollHolds` only knows one VK. Either treat combos like mouse holds (`_mouseHolds`-style set released by the edge), or poll both VKs.
   - To match the Mac: when a running Hold row is edited and its activate key changes or is removed, stop that row.
5. **Rebuild and install:** the `dotnet publish` line in `docs/HANDOFF.md` §7. Then test on Windows:
   - `F7 + G` Hold runs only while both are held.
   - `G` alone still types `G` in Notepad.
   - `F7` alone does nothing extra.
   - Press mode toggles once per combo.
   - Turning the switch off turns the chip red and the row no longer starts.
   - Old `settings.json` files still load.
