# Wonderblob

A fast, native-feeling remote file browser for Linux, macOS, and Windows — in the
spirit of Cyberduck, built with Tauri and Rust.

> **Status: v0.1.0 — first release (unsigned).** SFTP (with SSH
> host-key verification), S3, Azure Blob Storage, and OneDrive for Business all
> work end-to-end, with a resumable transfer queue, open-remote-files-locally
> editing, and OS drag & drop.

## Why

Cyberduck is great, but it doesn't run on Linux. Wonderblob aims to be the
cross-platform equivalent: a single-pane remote file browser that feels like a
real desktop app — keyboard-first, dense, quiet — not a web page in a frame.

## Features

- **SFTP** browsing, upload/download, rename, delete, new folder
- **SSH agent authentication** — works out of the box with 1Password,
  KeePassXC, or any agent exposing `SSH_AUTH_SOCK`; key files (with
  passphrase) and passwords also supported
- **Bookmarks with OS-keychain secrets** — passwords and passphrases live in
  KWallet/libsecret, macOS Keychain, or Windows Credential Manager; the
  bookmarks file on disk holds metadata only
- **OS drag & drop** — drag files/folders into the window to upload
- **SSH host-key verification** — trust-on-first-use with `SHA256:` fingerprint
  approval and a hard-stop on changed keys (MITM protection)
- Full keyboard navigation: arrows, Enter, Backspace, type-ahead, F2 rename,
  Delete with two-step confirm
- Dark mode that follows the OS

## Install

### Windows

```powershell
irm https://raw.githubusercontent.com/jackmusick/wonderblob/main/Install-Wonderblob.ps1 | iex
```

Installs the latest release silently, per-user, no admin. Re-run the same command
to update. Or download the `.msi` / NSIS `*-setup.exe` from
[Releases](https://github.com/jackmusick/wonderblob/releases) yourself.

> The installer isn't code-signed yet, so SmartScreen will interject —
> **More info → Run anyway**.

### Linux (one-liner)

```sh
curl -fsSL https://raw.githubusercontent.com/jackmusick/wonderblob/main/install.sh | sh
```

Installs the latest release per-user via Flatpak (real menu integration,
sandboxed, no sudo). Re-run the same command to update. Requires `flatpak` —
the script tells you the one-liner to install it if it's missing.

### Manual downloads

Or grab the asset for your platform from the
[Releases](https://github.com/jackmusick/wonderblob/releases) page:

- **Linux** — `wonderblob.flatpak` (`flatpak install --user wonderblob.flatpak`),
  the `.AppImage` (`chmod +x Wonderblob_*.AppImage && ./Wonderblob_*.AppImage`),
  or the `.deb` / `.rpm` for your distro.
- **macOS** — the `.dmg` (arm64 or x86_64). Drag Wonderblob to Applications.
- **Windows** — the `.msi` or the NSIS `*-setup.exe`.

### Unsigned-build caveats

v1 builds are **not code-signed or notarized**. Each OS gates unsigned apps on
first launch:

- **macOS** — Gatekeeper blocks the first open. Right-click the app → **Open**,
  or clear the quarantine flag:
  `xattr -dr com.apple.quarantine /Applications/wonderblob.app`. Signing +
  notarization (Apple Developer ID) is post-v1.
- **Windows** — SmartScreen shows "Windows protected your PC" → **More info** →
  **Run anyway**. An EV/OV signing cert is post-v1.
- **Linux** — unsigned AppImage/deb/rpm is normal; no OS gate. Remember to
  `chmod +x` the AppImage.

## Drag & drop

- **Drag in** — drag files (and folders) from your OS file manager **into** the
  window to upload them to the current directory. v1 handles top-level files
  plus **one level** of folder contents; deep recursive trees are a post-v1
  enhancement.
- **Drag out** — there is no deferred OS drag-out yet (incl. macOS file-promise
  drags — deferred). The fallback is the toolbar **To Downloads** button, which
  downloads the selected file straight to `~/Downloads` (no save dialog),
  alongside the regular **Download** button with a save dialog. A same-named
  file already in `~/Downloads` is overwritten; use **Download** for a save
  dialog if you want to choose the name.

## Host-key verification

On the first SFTP connection Wonderblob shows the server's `SHA256:` fingerprint
for **trust-on-first-use** approval (Connect & Remember / Connect Once / Cancel).
Trusted keys are stored in OpenSSH `known_hosts` format under the app config dir
(not your `~/.ssh/known_hosts`). If a known host later presents a **different**
key, Wonderblob hard-stops with a man-in-the-middle warning and refuses to
remember the new key.

## Roadmap

S3 (incl. S3-compatible endpoints) · Azure Blob Storage · OneDrive for
Business · transfer queue with pause/resume · open/edit remote files in your
local apps with save-back · share links (presigned URLs / SAS / OneDrive).

## Security notes

- SSH host keys are verified on first use and re-verified on every connect; a
  changed host key hard-stops with a MITM warning (see above).
- Secrets are stored exclusively in your OS keychain, never on disk or in logs.

## Development

```bash
npm install
npm run tauri dev
```

Rust workspace: `crates/wonderblob-core` holds all protocol logic behind a
`StorageBackend` trait; `src-tauri` is a thin Tauri command layer; the Svelte
frontend is a pure view. Backend acceptance is a shared contract test suite —
run it against a throwaway Docker OpenSSH server:

```bash
./scripts/test-sftp-up.sh
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_contract
./scripts/test-sftp-down.sh
# agent + keyfile auth, fully self-contained:
./scripts/test-sftp-auth.sh
```

Adding a protocol means implementing one Rust trait — see
`crates/wonderblob-core/src/vfs.rs`.

## License

[MIT](LICENSE)
