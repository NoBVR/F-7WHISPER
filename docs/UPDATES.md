# Updates and releases

Added on the PC on 2026-11-22 (version 1.1.0). Both apps check GitHub Releases for a newer version and show an update bar at the top of the page with a round download button.

The repo is **public** so the apps can read releases without a sign-in. Its history was rewritten the same day so every commit uses GitHub's private no-reply address (see "Git identity" below).

## Releasing a new version

1. Bump the version in all of these (they must match):
   - `F7Whisper.csproj` `<Version>`
   - `src-tauri/tauri.conf.json` `version`
   - `src-tauri/Cargo.toml` `version` (then `cargo update -p f7whisper` refreshes `src-tauri/Cargo.lock`)
   - `package.json` and the two `version` fields at the top of `package-lock.json`
2. Commit and push to `main`.
3. Tag and push the tag: `git tag v1.2.0 && git push origin v1.2.0`.
4. `.github/workflows/release.yml` (it can also be run by hand from Actions to build and check both apps without publishing) checks the tag matches the csproj and Tauri versions, builds both apps and publishes the release with exactly these assets (the updaters look for these names):
   - `F-7WHISPER-windows-x64.zip`: `F-7WHISPER.exe`, `ui/` and `LICENSE.txt`
   - `F-7WHISPER-macos.dmg`: universal, with the license at `F-7WHISPER.app/Contents/Resources/LICENSE`

   The build fails if either download is missing the license (GPL-3.0 asks for it to come with every copy).
5. Every installed app on 1.1.0 or later sees the update within 6 hours, or on its next start.

Either machine can release; the workflow builds both apps on GitHub's runners.

## Contract (both hosts)

- **Check:** `GET https://api.github.com/repos/NoBVR/F-7WHISPER/releases/latest`, a few seconds after the page's `ready` and then every 6 hours. A release counts only if its tag (`v` stripped) is a higher `major.minor.patch` than the running app and it has this platform's asset. Errors are logged and nothing is shown.
- **Host → page:** `{ type: "update", version, status, progress }`
  - `status`: `available`, `downloading`, `installing`, `opened` (macOS) or `failed`
  - `progress`: 0 to 1 while downloading
- **Page → host:** `{ type: "installUpdate" }`. It is ignored with no known update or while a download runs. After `failed` the page lets the user try again.
- **Integrity:** when the release asset has a `digest` (`sha256:…`), the download is checked against it.

## Installing

- **Windows:** download the zip, unpack it to a staging folder, then a hidden helper waits for the app to close, copies the files over the running exe's folder, and starts the new exe. If copying fails it starts the old one.
- **macOS (from 2026-11-25; not yet tested end to end):** in place, like Windows (`src-tauri/src/update.rs`):
  1. Download the dmg to a hidden temp file in `~/Downloads` and check its size and SHA-256.
  2. Mount it hidden and read-only, and check the new `F-7WHISPER.app` satisfies the running app's designated requirement (`codesign --verify --deep --strict -R=…`). With the F-7WHISPER certificate that means: same bundle id, same certificate.
  3. Copy it with `ditto` into a private `.F-7WHISPER-update-*` folder next to the running app, strip quarantine and detach the dmg.
  4. A detached `/bin/sh` helper waits for F-7WHISPER to quit (it quits itself after showing "Installing…"), swaps the bundles, opens the new app, and deletes the backup and the dmg.
  - **Fallback**, which is the old behaviour: if the running app is ad-hoc signed, translocated or its folder isn't writable, or the check or the swap fails, the dmg is saved as `~/Downloads/F-7WHISPER-<version>.dmg` and opened (status `opened`) for the user to drag in. A 1.1.0 install updates this way once; after that, updates are in place.
  - Because every release is signed with the same certificate, the Accessibility permission carries over.

## Signing (macOS)

- Every Mac build that's installed is signed with one self-signed certificate, **"F-7WHISPER Self-Signed"** (SHA-1 `3e830f584bcdddbbfe7e5e2f437435f832c20692`, valid to 2046). macOS ties Accessibility to the signature; with this certificate the designated requirement is `identifier "local.f7whisper.desktop" and certificate leaf = H"3e83…"`, so a new build keeps the permission. Ad-hoc builds (`-`) get a new identity every time and lose it.
- **Where it lives:** the user's Mac login keychain, plus a backup `.p12` and its password in `~/.f7whisper-signing/` (outside the repo, mode 700). The repo is public: never commit the `.p12`, its password, or anything from that folder.
- **GitHub:** the secrets `MAC_CERT_P12` (base64 of the `.p12`) and `MAC_CERT_PASSWORD`. `release.yml` imports them into a temporary keychain, builds with `APPLE_SIGNING_IDENTITY="F-7WHISPER Self-Signed"`, and fails if the secrets are missing or the app in the dmg isn't signed with this certificate. So either machine can release.
- **Local Mac builds:** `APPLE_SIGNING_IDENTITY="F-7WHISPER Self-Signed" npx tauri build …` (HANDOFF §6). `tauri.conf.json` keeps `"signingIdentity": "-"` so `mac.yml` and machines without the certificate still build.
- **Gatekeeper** still warns on the first open of a downloaded dmg ("unidentified developer"): the certificate isn't from Apple. Updates installed in place don't go through that.
- **If the certificate is lost:** make a new one with the same name, update both secrets and `MAC_CERT_SHA1` in `release.yml`. Every install then needs Accessibility re-added once, and the in-place updater falls back to opening the dmg once, because the requirement changed.

## Page

- **Nothing downloads or installs on its own.** The bar asks "Update to version X?" and waits. Only the round button starts the download (the user asked for this on 2026-11-23: "I don't want it to auto update. It should prompt you").
- An X ("Not now") after the button hides the bar until F-7WHISPER next starts; the 6-hourly re-check of the same version stays hidden, a newer version shows again. The X shows when the bar asks, on the macOS drag hint and after a failure, not while downloading or installing (neither can be stopped).
- The bar sits under the title bar. Its text then says "Downloading…", "Installing…", the macOS drag hint, or "Download failed".
- The round button is primary white. While busy it turns grey and a ring around it shows progress; while installing the ring spins; on macOS it shows a check after the dmg opens.
- The page counts the bar's height in its `fit` message.
- The browser mock offers 1.2.0 and simulates a download.

## Git identity

Commits use `NoBVR <146703888+NoBVR@users.noreply.github.com>`, set per repo:

```
git config user.name NoBVR
git config user.email 146703888+NoBVR@users.noreply.github.com
```

After the rewrite, an older clone has to be reset once (it has no work of its own on it): `git fetch origin && git reset --hard origin/main`.
