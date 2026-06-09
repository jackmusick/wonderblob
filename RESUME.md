# Wonderblob — Resume Notes

_Last updated: 2026-06-08. Pause point after completing all six implementation plans._

## Where things stand

**All six plans are implemented, reviewed, and merged to `main`. CI is green on the
real GitHub runner.** The repo (`jackmusick/wonderblob`, private) is at a clean,
pushed state — nothing uncommitted, no in-flight work.

| Plan | Feature | Status |
|------|---------|--------|
| 1 | Foundation + SFTP (agent-first auth) | ✅ merged |
| 2 | S3 + Azure Blob backends | ✅ merged |
| 3 | TransferEngine (queue/resume/progress) | ✅ merged |
| 4 | EditSession (open/edit/save-back, preview) | ✅ merged |
| 5 | OneDrive for Business (Graph + OAuth) | ✅ merged |
| 6 | Drag & drop, packaging, SSH host-key verification | ✅ merged |

Design spec: `docs/superpowers/specs/2026-06-07-wonderblob-design.md`
Per-plan plans: `docs/superpowers/plans/`

## What's verified

- Every plan: implement → spec review → adversarial/security review → **live GUI smoke test**.
- SFTP, S3 (vs MinIO), Azure (vs Azurite), transfers, edit/save-back, conflict
  detection, host-key TOFU, and the "To Downloads" fallback all verified in the GUI.
- All backends pass a shared VFS contract suite incl. 20MB resumable transfers.
- CI runs the full suite + Dockerized SFTP/MinIO/Azurite fixtures on every push.

## What's left — NONE of it is code; all needs you

1. **OneDrive live sign-in (the only thing untestable headless).**
   - Entra app is registered: client ID `aaeb21a2-1c76-4c1d-92ab-28c6e611dcc2`,
     redirect `wonderblob://auth`, delegated scopes Files.ReadWrite.All +
     offline_access + User.Read, public-client flows enabled.
   - To test: `npm run tauri dev` → New Connection → Microsoft OneDrive →
     Sign in with Microsoft → browser consent → should deep-link back and list
     your real OneDrive.
   - **If it fails with `AADSTS50011` (redirect mismatch):** the `wonderblob://auth`
     URI has no path segment, which the Entra workforce platform *usually* accepts
     but occasionally rejects. Fix = register `wonderblob://auth/` (trailing slash)
     or `wonderblob://auth/callback` in Entra, then update `REDIRECT_URI` in
     `src-tauri/src/onedrive_auth.rs` to match byte-for-byte.

2. **Drag-in manual check.** Drag a file/folder from Dolphin onto the browser pane
   with a connection active — should show a drop highlight and enqueue uploads.
   (Unit-tested + wired; couldn't be simulated under the headless test display.)

3. **Cut the first release (optional, when ready).** Push a `v0.6.0` tag:
   `git tag v0.6.0 && git push origin v0.6.0`. This triggers `.github/workflows/release.yml`
   (macOS + Windows + Linux runners, ~uses runner minutes) and creates a **draft**
   GitHub Release with installers (deb/rpm/AppImage/dmg/msi/nsis). Builds are
   **unsigned** for v1 — README documents the Gatekeeper/SmartScreen workarounds.

## Known post-v1 polish (non-blocking, deferred deliberately)

- Recursive folder drag-in (currently top-level files + one dir level).
- Real OS drag-OUT / macOS file-promise drags (fallback "To Downloads" ships instead).
- Resumable UPLOAD sessions (downloads resume; uploads restart-from-0 today).
- Code-signing / notarization.
- `~/Downloads` name-collision overwrites on "To Downloads" (documented in README).
- Conflict detection degrades to size-only on S3/Azure objects without mtime.

## To run locally

```bash
npm install
npm run tauri dev        # dev app (needs the vite server, which this starts)
# Test fixtures (Docker):
./scripts/test-sftp-up.sh      # SFTP on :2222 (wb/wbpass)
./scripts/test-s3-up.sh        # MinIO on :9000 (minioadmin/minioadmin)
./scripts/test-azblob-up.sh    # Azurite on :10000
# Gated integration tests:
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_contract
```

## Install scripts — verify on first real release

The Flatpak/PowerShell install path (spec
`docs/superpowers/specs/2026-06-08-wonderblob-install-parity-design.md`) ships
unverified end-to-end — it needs a published release to test:

1. **`releases/latest` needs a PUBLISHED, non-prerelease release.** The current
   `release.yml` produces a *draft prerelease*; `Install-Wonderblob.ps1` and
   `install.sh` both query `/releases/latest`, which skips drafts/prereleases.
   Publish (and unflag prerelease on) the first release before advertising the
   one-liners.
2. **Flatpak live checks (can't be done headless):** after the tag build,
   `flatpak install --user wonderblob.flatpak` then `flatpak run com.wonderblob.app`
   — confirm (a) the window launches (webkit ABI vs GNOME runtime 47),
   (b) OneDrive sign-in's `wonderblob://auth` deep-link returns into the app,
   (c) EditSession opens a file in the editor and saves back. Fixes if not:
   bump `runtime-version`, add `x-scheme-handler/wonderblob` MimeType to the
   manifest desktop file, route the editor spawn through OpenURI.
3. **Windows:** `irm … | iex` on a real Windows box; confirm per-user silent
   install and that re-running updates in place.

## UX overhaul + SSH fix (2026-06-08, later session)

Big batch of UX work landed on `release-prep-v0.1.0` (commits `b7b54cc`→`f90b026`).
All committed; installed RPM is current.

**SSH Agent auth — fixed.** Root cause: agent auth used russh `connect_env()`
(reads only `$SSH_AUTH_SOCK`), but the user's key lives in 1Password selected via
`ssh_config` `IdentityAgent`. New `crates/wonderblob-core/src/ssh_agent.rs`
resolves the socket like `ssh` does (IdentityAgent wins over env). Tests use
`WONDERBLOB_SSH_CONFIG=/dev/null` to isolate. `errors.ts` now surfaces the auth
detail instead of a bare "Authentication failed."

**UX:** new `Icon.svelte` (inline-SVG set) + `ContextMenu.svelte`; file-type
icons + right-click menus (files & connections); connection protocol icons +
green connected-dot + single-click connect/disconnect toggle; icon toolbar
(Download icon → ~/Downloads, right-click → Download As); sortable + resizable +
show/hide file columns; tree view (expandable folders, lazy children);
resizable sidebar; transfers direction icons + "Clear finished" + per-row ×
force-remove (`clear_transfer` command) + fills width; `Settings.svelte`
(theme system/light/dark via `data-theme`, confirm-delete, column visibility),
backed by `src/lib/stores/prefs.ts`. Plan: `docs/superpowers/plans/2026-06-08-ux-pass.md`.

**Still open:** search (deferred); green-dot/single-click-connect kept as-is
(toggleable); transfers could get a floating-popover variant (currently a docked
panel toggled by the toolbar icon).

**⚠️ Dev caveat:** `npm run tauri dev` HMR is UNRELIABLE here — the webview
paints before Vite is ready and patches CSS inconsistently, so the dev window
misrenders (looked like broken layout when the source was fine). **Verify against
release builds** (`npm run tauri build --bundles rpm` + `sudo dnf reinstall`), not
the dev window. A clean fix for the dev race is a future task.
