# Wonderblob

A fast, native-feeling remote file browser for Linux, macOS, and Windows — in the
spirit of Cyberduck, built with Tauri and Rust.

> **Status: early development.** SFTP works end-to-end (including SSH-agent
> auth). S3, Azure Blob Storage, and OneDrive for Business are on the roadmap,
> along with a resumable transfer queue and open-remote-files-locally editing.

## Why

Cyberduck is great, but it doesn't run on Linux. Wonderblob aims to be the
cross-platform equivalent: a single-pane remote file browser that feels like a
real desktop app — keyboard-first, dense, quiet — not a web page in a frame.

## Features (today)

- **SFTP** browsing, upload/download, rename, delete, new folder
- **SSH agent authentication** — works out of the box with 1Password,
  KeePassXC, or any agent exposing `SSH_AUTH_SOCK`; key files (with
  passphrase) and passwords also supported
- **Bookmarks with OS-keychain secrets** — passwords and passphrases live in
  KWallet/libsecret, macOS Keychain, or Windows Credential Manager; the
  bookmarks file on disk holds metadata only
- Full keyboard navigation: arrows, Enter, Backspace, type-ahead, F2 rename,
  Delete with two-step confirm
- Dark mode that follows the OS

## Roadmap

S3 (incl. S3-compatible endpoints) · Azure Blob Storage · OneDrive for
Business · transfer queue with pause/resume · open/edit remote files in your
local apps with save-back · share links (presigned URLs / SAS / OneDrive) ·
drag & drop with the OS.

## Security notes

- Wonderblob currently **does not verify SSH host keys** (accept-on-first-use
  verification is planned before the first release). Don't point it at hosts
  you don't trust yet.
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
