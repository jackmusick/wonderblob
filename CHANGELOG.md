# Changelog

All notable changes to Wonderblob are documented here. Versions are pre-1.0
(0.x); each minor maps to a development plan.

## 0.6.0

First distributable release.

### Added

- **SSH host-key verification** (replaces accept-any). First SFTP connect prompts
  with the server's `SHA256:` fingerprint (trust-on-first-use); trusted keys are
  stored in OpenSSH `known_hosts` format under the app config dir. A changed host
  key hard-stops with a man-in-the-middle warning and is never silently
  remembered.
- **Drag-in uploads** — drop files/folders from the OS file manager onto the
  browser pane to upload into the current directory (top-level files + one level
  of folder contents), with a drop-target highlight.
- **Drag-out fallback** — a one-click "To Downloads" action that downloads the
  selected file straight to `~/Downloads` (no save dialog). Deferred OS drag-out
  (incl. macOS file-promise drags) is post-v1.
- **Packaging** — per-OS bundle targets (deb/rpm/AppImage, dmg, msi/nsis) with
  full bundle metadata, and a tag-gated `tauri-action` release workflow that
  produces installers as a draft prerelease.

### Notes

- Builds are **unsigned** (no notarization / code-signing). See the README for
  Gatekeeper / SmartScreen caveats. Signing is post-v1.

## 0.1.0–0.5.0 (prior plans)

- SFTP browsing with SSH-agent / key-file / password auth, bookmarks with
  OS-keychain secrets, full keyboard navigation, OS-following dark mode.
- S3 (incl. S3-compatible endpoints) and Azure Blob Storage backends.
- Resumable transfer queue with pause/resume and progress.
- Open/edit remote files in local apps with save-back, and file preview.
- OneDrive for Business backend (Microsoft Graph + OAuth PKCE).
