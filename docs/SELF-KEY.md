# Activate keys that a row presses ("hold F to spam F")

Asked for on the PC on 2026-11-27. The user's words: "I'm trying to spam F in my game instead of having to do it myself". They set up a key row that presses F with activate key F (Hold), and the page refused it.

**Status:**

- **Windows C# host (`src/*.cs`) and shared page (`ui/index.html`): done on the PC on 2026-11-27.**
- **Mac host (`src-tauri/`): not done.** The Mac doesn't send `selfKey`, so its page still blocks this setup, as before. "Mac: what to change" below is the to-do.

## Why it was blocked

A one-key activate key went to the OS hotkey API (`RegisterHotKey` on Windows, `tauri-plugin-global-shortcut` on Mac). That API can't tell the user's F from F-7's own posted F, which breaks this in two ways:

- The hotkey also catches F-7's posted presses, so they never reach the game.
- Hold release asked the OS whether F is still down (`GetAsyncKeyState`). The OS never records the swallowed physical F as down, and F-7's own key-ups make F look released, so the row stops right away.

## Contract (both hosts)

- **Shared key:** a row's one-key keyboard activate key whose main key is also the key that any key row presses. That includes the row itself, and rows that are disabled. Modifiers on the activate key don't matter: `Ctrl + F` with a row pressing F is shared too.
  - It excludes two-key keys (`with`), mouse buttons, and modifier keys.
- **Handled by the host's low-level listener**, never by the OS hotkey API and never falling back to it. That's the `WH_KEYBOARD_LL` hook on Windows (`src/KeyboardHook.cs`) and the `CGEventTap` on Mac, the same ones two-key keys use ([`TWO-KEY.md`](TWO-KEY.md)).
  - It works whether the two-key switch is on or off.
  - Start all / Stop all still win a clash.
- **Firing:**
  - A physical, fresh key-down with exactly the activate key's modifiers is a down edge. It is swallowed, along with its auto-repeats and its matching key-up, so only F-7's presses reach apps.
  - The physical key-up is the up edge.
  - Releasing a modifier doesn't end it.
- **Synthetic input is ignored:** F-7's own posted keys (own process id on Mac, `LLKHF_INJECTED` on Windows) and anything tagged `0x800DA5A5` never change state, and always pass through to apps.
- **Rows:**
  - Hold rows run from the down edge until the up edge, never by polling key state.
  - Press rows toggle on each down edge.
- **`triggerOk`:** true when the listener is running and the key is bound. If the listener can't run, it is `false` (red ring); never fall back.
- **Release safety:** running Hold rows on a shared key stop when it stops being bound. That covers:
  - a row being edited, removed or disabled;
  - a capture box opening (`pauseHotkeys`);
  - the listener dying;
  - the key moving between the listener and the OS hotkey API because rows changed.
- **Page gate:** a host that supports this sends `selfKey: true` in every `state` message. Only then does the page drop its "activate key is a key a row presses" conflicts, in both directions. Without it the page blocks them as before.
- `settings.json` doesn't change.

## Windows: how it's done

- `KeyEngine.TriggerChords` also returns the VKs that key rows press (`row.Stroke.Vk`, the VK actually sent).
- `MainForm.RegisterHotkeys` sends a one-key trigger to the hook when its VK is in that set, it isn't Start/Stop, and it's an ordinary key (`KeyChoice.IsOrdinaryKey`). `Registration.ViaKeyboardHook` records which way a binding went, because `Chord.UsesHook` could no longer tell from the chord's shape alone, so it was removed.
- `KeyboardHook.KeyDown` matches a one-key binding (`Chord(vk, mods)`) before looking for combos. On start, the hook marks shared keys that are already down as held, so their auto-repeats pass through instead of firing.
- `KeyEngine.Activate(chord, viaKeyboardHook)` leaves hook chords out of `PollHolds`, and `ReleaseHookHold` stops them on the up edge. When a key moves from `RegisterHotKey` to the hook, a polled Hold on it stops first.

## Mac: what to change

1. `main.rs`: send `selfKey: true` in `state` once the rest is done. That is the switch that turns the feature on in the page.
2. `hotkeys.rs`: compute the shared keys from the rows (every key row's key code), and register those activate keys through the event tap instead of `tauri-plugin-global-shortcut`, whatever the two-key switch says.
3. Event tap (`mouse.rs` / `mouse_macos.rs`): match a one-key binding on a key-down that isn't F-7's own (own pid) or tagged `0x800DA5A5`, with exactly matching modifiers.
   - Post the down edge and swallow the down, its repeats and its up.
   - Post the up edge on the key-up.
4. `engine.rs`: Hold rows on a shared key release on the tap's up edge (like combos and side buttons), not by polling.
5. Test on the Mac:
   - In TextEdit, a row pressing F at 10/s with activate key F (Hold) types F only while F is held, and stops on release.
   - With Press, each press of F toggles it.
   - F typed in TextEdit with no such row still types normally.
