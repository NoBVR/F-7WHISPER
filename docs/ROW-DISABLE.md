# Disabling a row

Added on the Mac on 2026-11-25, at the user's request: "When clicking the ... button it should have a disable/enable option. grays out the row when disabled. Both windows and macos versions."

**Status:**

- **Shared page (`ui/index.html`):** done. Checked in the browser mock, including an old host that doesn't send the flag.
- **Mac host (`src-tauri/`):** done on the Mac (see "Mac host" below).
- **Windows C# host (`src/*.cs`): not done; do it on the PC** ("Windows: what to change"). Until then the page hides the menu item on Windows (see "Page gate"), so the Windows app keeps working as before after a rebuild.

## Contract (both hosts)

### Row field

- A row has `disabled` (bool). In `settings.json` it is saved as `"disabled": true` and left out when false, so old files load with every row enabled.
- **Page gate:** a host that supports disabling sends `disabled` as a boolean on **every** row in `state.rows`. The page shows the Disable / Enable menu item only for a row whose `disabled` is a boolean.

### Message (page → host)

```json
{ "type": "setDisabled", "id": "<row id>", "disabled": true }
```

The host then:

- **When disabling:** stops the row if it's running, including a held Hold row. It drops the row's activate key binding, so that key reaches other apps normally. Then it saves and sends `state`.
- **When enabling:** re-registers the row's activate key under the usual rules (conflicts, two-key switch, overlap). It doesn't start the row. Then it saves and sends `state`.

### While a row is disabled

- `toggle` for that row is ignored.
- Start all skips it. Stop all doesn't need to.
- Its activate key isn't registered, so it never starts the row. `triggerOk` may be false; the page doesn't mark a disabled row red.
- Editing it (`edit`) keeps it disabled. New rows (`add`) start enabled. Removing it works as usual.
- Its press count stays as it was.

### Page

- The ⋯ menu shows Edit, **Disable** (or **Enable** on a disabled row), Delete.
- A disabled row is greyed out (opacity 0.4) except its ⋯ button. Its status pill reads "Disabled" in grey, and its Start button is disabled.
- Conflict checks in the Add / Edit dialog still count disabled rows' keys, so enabling a row never creates a clash.

## Mac host

- `settings.rs`: `RowSpec.disabled` (`#[serde(default, skip_serializing_if = …)]`).
- `engine.rs`: toggle and Start all skip disabled rows; `trigger_keys` leaves them out; a new method sets the flag and stops the row.
- `main.rs`: `Message::SetDisabled { id, disabled }` → engine, rebind, save, state. `state` rows include `disabled`.

## Windows: what to change (not done; do on the PC)

Read the current code first; line numbers drift. Nothing here was compiled on the Mac.

1. **`src/Settings.cs`:** the row type gets `bool Disabled`. Read `"disabled"` (missing → false); write it only when true (or always: the page only needs it in `state`).
2. **`src/MainForm.cs`:**
   - In `state`, send `disabled` (bool) on every row. This is what turns the menu item on in the page.
   - Handle `setDisabled`: find the row, set the flag, stop it if disabling (as `toggle` / Stop would, including a held Hold row), then `RegisterHotkeys()`, save, and send `state`.
   - `RegisterHotkeys` / `ApplyBindings`: leave disabled rows' activate keys out of `wanted`. That covers `RegisterHotKey`, the `WH_MOUSE_LL` side buttons and the two-key `WH_KEYBOARD_LL` combos.
3. **`src/KeyEngine.cs`:** `Toggle` and Start all ignore disabled rows. `Edit` keeps `Disabled`. `Add` creates enabled rows.
4. **Rebuild and install** (`docs/HANDOFF.md` §7), then test on Windows:
   - Disable a running row from ⋯: it stops and greys out.
   - Its activate key no longer starts it and reaches other apps.
   - Start all skips it.
   - Enable brings it back, stopped, with its activate key working.
   - Restart the app: the row stays disabled.
   - An old `settings.json` still loads.
5. Mark this file done and push.
