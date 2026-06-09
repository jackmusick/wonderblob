# Changelog

All notable changes to Wonderblob are documented here. Versions are pre-1.0 (0.x).

## 0.1.0

First release. Wonderblob is a cross-platform remote file browser — Cyberduck for
Linux, macOS, and Windows.

### Backends

- **SFTP** browsing with SSH-agent / key-file / password auth, and SSH host-key
  verification (trust-on-first-use): the first connect prompts with the server's
  `SHA256:` fingerprint, trusted keys are stored in OpenSSH `known_hosts` format,
  and a changed host key hard-stops with a man-in-the-middle warning.
- **S3** (including S3-compatible endpoints) and **Azure Blob Storage**.
- **OneDrive for Business** (Microsoft Graph + OAuth PKCE).

### Features

- Bookmarks with secrets stored in the OS keychain, full keyboard navigation, and
  OS-following dark mode.
- Resumable transfer queue with pause/resume and progress.
- Open/edit remote files in local apps with save-back, plus file preview.
- **Drag-in uploads** — drop files/folders from the OS file manager onto the
  browser pane to upload into the current directory (top-level files + one level of
  folder contents), with a drop-target highlight.
- **Drag-out fallback** — a one-click "To Downloads" action that downloads the
  selected file straight to `~/Downloads`.

### Packaging

- Per-OS installers (deb/rpm/AppImage on Linux, msi/nsis on Windows, dmg on macOS)
  with full bundle metadata, produced by a tag-gated `tauri-action` release workflow.

### Notes

- Builds are **unsigned** (no notarization / code-signing). See the README for
  Gatekeeper / SmartScreen caveats. Signing is post-1.0.
