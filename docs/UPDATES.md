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
4. `.github/workflows/release.yml` checks the tag matches the csproj and Tauri versions, builds both apps and publishes the release with exactly these assets (the updaters look for these names):
   - `F-7WHISPER-windows-x64.zip`: `F-7WHISPER.exe` and `ui/`
   - `F-7WHISPER-macos.dmg`: universal
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
- **macOS:** download the dmg to `~/Downloads/F-7WHISPER-<version>.dmg` and open it. The user drags the app into Applications and reopens it. With ad-hoc signing, the Accessibility toggle has to be re-added afterwards.

## Page

- The bar sits under the title bar. Its text says the version, then "Downloading…", "Installing…", the macOS drag hint, or "Download failed".
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
