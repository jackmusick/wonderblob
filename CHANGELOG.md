# Changelog

All notable changes to Wonderblob are documented here. Versions are pre-1.0 (0.x).

## 0.1.2

OneDrive sign-in fix for Linux. On 0.1.1 the OAuth redirect opened a second app
window, left the sheet stuck on "Signing in…", and the saved connection fell
back to "Sign in again". Both symptoms had the same cause: the
`wonderblob://auth` callback never reached the running app.

### Fixed

- **OneDrive sign-in now completes on Linux.** Two compounding causes, both
  desktop-only:
  - Added `tauri-plugin-single-instance` (with the `deep-link` feature). On
    Linux/Windows the OS launches a *new* app instance for the
    `wonderblob://auth` callback rather than delivering it in-process; the
    plugin forwards the URL to the running instance and stops the second window.
  - The packaged desktop file's `Exec` line was missing the `%u` field code, so
    XDG dropped the redirect URL when launching the handler. The Flatpak now
    appends `%u`, which its desktop export preserves.

## 0.1.1

Linux Flatpak fixes. The 0.1.0 Flatpak ran sandboxed without the host access it
needed, so secrets and SFTP agent auth failed (both worked in unsandboxed
dev/native builds). Native `.deb`/`.rpm`/AppImage installs were unaffected.

### Fixed

- **Saved-connection secrets** now work in the Flatpak — granted the Secret
  Service D-Bus name (`org.freedesktop.secrets`); previously every keyring call
  failed with `org.freedesktop.DBus.Error.ServiceUnknown`.
- **SFTP SSH-agent auth** now works in the Flatpak — granted `ssh-auth`, the
  1Password and Bitwarden agent sockets, and read-only `~/.ssh` so `IdentityAgent`
  in `ssh_config` resolves (previously fell back to the empty default agent and
  failed with "Authentication failed"). Other custom agents: see the README for a
  one-time `flatpak override`.

### Changed

- Flatpak runtime bumped `org.gnome.Platform` 47 → 49 (47 reached end-of-life).

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
