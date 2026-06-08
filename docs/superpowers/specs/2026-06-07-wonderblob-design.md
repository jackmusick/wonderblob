# Wonderblob — Design

**Date:** 2026-06-07
**Status:** Approved

## What

Wonderblob is an open-source, cross-platform (Linux/macOS/Windows) remote file
browser in the spirit of Cyberduck: a single-pane GUI for browsing and
transferring files across cloud storage and SFTP. Cyberduck itself is open
source but macOS/Windows only; Wonderblob exists primarily to bring that class
of tool to Linux while running identically on all three platforms.

**v1 backends:** S3 (incl. S3-compatible endpoints), Azure Blob Storage, SFTP,
OneDrive for Business.

## Stack

Tauri 2.x. The Rust core owns all protocol logic, credentials, transfers, and
the edit/watch machinery. The frontend (Svelte + desktop-grade custom
components) is purely a view layer driven over Tauri commands/events. Nothing
protocol-aware lives in JS.

**Hard design constraint:** the app must not look or feel like a web page.
Native window conventions, real context menus, platform keyboard shortcuts,
desktop information density, no scroll-bounce, no text cursor on labels, no
web-framework visual identity.

## Architecture

```
frontend (webview)          rust core
┌──────────────────┐   ┌─────────────────────────────┐
│ Browser pane     │   │ ConnectionManager (bookmarks)│
│ Transfers panel  │◄──┤ TransferEngine (queue/resume)│
│ Bookmark manager │   │ EditSession (open/watch/save)│
│ Connection sheet │   │ Keychain (keyring crate)     │
└──────────────────┘   │ VFS trait                    │
                       │  ├ s3      (aws-sdk-s3)     │
                       │  ├ azblob  (azure_storage)  │
                       │  ├ sftp    (russh, agent)   │
                       │  └ onedrive(Graph + PKCE)   │
                       └─────────────────────────────┘
```

## Components

### VFS trait (`StorageBackend`)

One async Rust trait implemented per protocol:

- `list`, `stat`, `read` (ranged), `write` (streaming), `delete`, `rename`,
  `mkdir`, `share_link(expiry)`
- Capability flags (`can_presign`, `can_rename`, `can_set_mtime`, …) so the UI
  greys out unsupported actions instead of faking them
- Buckets / containers / drives surface as the root directory listing

Backend implementations:

| Backend | Crate(s) | Notes |
|---|---|---|
| S3 | `aws-sdk-s3` | Presigned URLs, multipart upload, custom endpoints (MinIO, Wasabi, R2) |
| Azure Blob | `azure_storage_blobs` | SAS link generation, block-list uploads for resumable transfer |
| SFTP | `russh` + `russh-sftp` | **Agent-first auth** via `SSH_AUTH_SOCK` (1Password, KeePassXC, etc.), then key file, then password |
| OneDrive for Business | `reqwest` against Microsoft Graph | OAuth 2.0 PKCE in system browser, Graph upload sessions for resumable large files, native sharing links |

The trait is the contribution surface: adding a protocol later (WebDAV,
Backblaze B2, …) means implementing one trait.

### TransferEngine

- Persistent queue in SQLite (`rusqlite`) — survives app restart
- N parallel transfer workers (configurable)
- Chunked/multipart uploads on S3, Azure, and Graph upload sessions, with
  chunk state recorded so pause/crash resumes mid-file; SFTP resumes by offset
- Retry with exponential backoff for transient failures
- Progress streamed to the UI as Tauri events

### EditSession (open / edit / save-back)

Core UX, not an add-on:

- **Double-click / Enter:** download to a per-connection temp dir, open with
  the OS default handler (`xdg-open` / `open` / `start`)
- **Spacebar:** lightweight in-app preview (text, images, PDF in the webview)
  without launching an external app
- A `notify`-based watcher detects saves to the temp file, debounces, and
  re-uploads
- Conflict guard: remote etag/mtime checked before overwrite; on mismatch,
  prompt (overwrite / save copy / discard)
- Temp files cleaned up on disconnect with an option to keep

### Auth & credentials

- **S3:** access key/secret, or named AWS profiles
- **Azure:** connection string, account+key, or SAS token
- **SFTP:** agent (default), key file (with passphrase prompt), password
- **OneDrive:** OAuth PKCE using a shipped multi-tenant public client ID
  (registered once in Entra; delegated `Files.ReadWrite.All`); refresh tokens
  persisted. Advanced setting allows overriding the client ID per connection
  for orgs that require their own registration.
- All secrets stored in the OS keychain via the `keyring` crate
  (libsecret/KWallet, macOS Keychain, Windows Credential Manager). The
  bookmarks file stores metadata only — never secrets.

### Drag & drop

- **Drag in** (OS → app): full support on all platforms (Tauri native)
- **Drag out** (app → OS): macOS gets promise-file drags (file materializes on
  drop). Linux/Windows v1 fallback: drag-out triggers a download to
  `~/Downloads` (plus copy/paste support); true deferred drag-out is a tracked
  post-v1 enhancement. This limitation is documented honestly in the README.

## Error handling

All backend errors map to a small common taxonomy — `AuthFailed`, `NotFound`,
`PermissionDenied`, `Network`, `Conflict`, `QuotaExceeded`, `Unsupported`,
`Other` — so the UI can respond specifically. Transfer workers retry
transient errors with backoff; auth failures pause the queue and surface a
re-auth prompt rather than burning retries.

## Testing

- **Backend integration tests** run the VFS trait's contract suite against
  real services in CI: MinIO (S3), Azurite (Azure Blob), an OpenSSH container
  (SFTP)
- **OneDrive:** Graph-mock layer for CI plus a manual smoke checklist
  (interactive OAuth can't run headless)
- **TransferEngine:** failure-injection tests — kill a worker mid-chunk,
  restart, assert correct resume
- **EditSession:** unit tests for watch/debounce/conflict logic with a temp
  filesystem

## v1 scope

**In:** browse, transfer queue with pause/resume, bookmarks, open/edit/
save-back, spacebar preview, share links (presigned S3 / Azure SAS / OneDrive
sharing), drag-in everywhere, drag-out (macOS full, others fallback),
OS-keychain credential storage.

**Out (post-v1):** folder sync, server-to-server copy, dual-pane mode,
additional protocols, 1Password CLI integration beyond the SSH agent.
