# Wonderblob Plan 1: Foundation + SFTP Vertical Slice

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A running Tauri app that connects to an SFTP server (agent-first auth), browses directories in a 1Password-8-style single-pane UI, uploads/downloads files, and stores bookmarks with secrets in the OS keychain.

**Architecture:** Cargo workspace with a UI-agnostic `wonderblob-core` crate (error taxonomy, `StorageBackend` VFS trait, SFTP implementation) and a thin `wonderblob-app` Tauri 2 crate exposing core over commands/events. Svelte frontend is a pure view layer. Integration tests run the VFS contract suite against a Dockerized OpenSSH server.

**Tech Stack:** Tauri 2.x, Rust (tokio, russh, russh-sftp, thiserror, keyring, serde), Svelte 5 + Vite, Docker (test SFTP server).

**Spec:** `docs/superpowers/specs/2026-06-07-wonderblob-design.md`

**Crate-API caveat:** `russh`/`russh-sftp` APIs move. Code below targets russh 0.46+/russh-sftp 2.x. If `cargo build` disagrees, consult docs.rs for the pinned version and adapt signatures — the *structure* (handler, agent auth flow, sftp session over channel) is stable.

---

### Task 1: Scaffold the Tauri app + Cargo workspace

**Files:**
- Create: `package.json`, `vite.config.ts`, `src/` (Svelte), `src-tauri/` (via scaffolder)
- Create: `Cargo.toml` (workspace root)
- Create: `crates/wonderblob-core/Cargo.toml`, `crates/wonderblob-core/src/lib.rs`
- Create: `.gitignore`, `rust-toolchain.toml`

- [ ] **Step 1: Scaffold Tauri 2 + Svelte + TypeScript**

```bash
cd ~/GitHub/wonderblob
npm create tauri-app@latest . -- --name wonderblob --identifier com.wonderblob.app --template svelte-ts --manager npm --yes
npm install
```

If the scaffolder refuses a non-empty dir, scaffold into `/tmp/wb` and `rsync -a /tmp/wb/ .` (docs/ is preserved; nothing else exists yet).

- [ ] **Step 2: Verify dev app launches**

Run: `npm run tauri dev`
Expected: a window opens with the template page. Close it. (On this Linux box: launch via `setsid -f` with sandbox disabled if the webview fails to start under the harness.)

- [ ] **Step 3: Convert to a Cargo workspace with a core crate**

Create root `Cargo.toml`:

```toml
[workspace]
members = ["src-tauri", "crates/wonderblob-core"]
resolver = "2"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
async-trait = "0.1"
```

Create `crates/wonderblob-core/Cargo.toml`:

```toml
[package]
name = "wonderblob-core"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { workspace = true }
serde = { workspace = true }
thiserror = { workspace = true }
async-trait = { workspace = true }
```

Create `crates/wonderblob-core/src/lib.rs`:

```rust
pub mod error;
pub mod vfs;
```

(Modules created in Tasks 2–3; for now add empty `error.rs` and `vfs.rs` containing only `//! placeholder module` so the crate compiles.)

In `src-tauri/Cargo.toml` add under `[dependencies]`:

```toml
wonderblob-core = { path = "../crates/wonderblob-core" }
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: scaffold Tauri 2 + Svelte app with wonderblob-core workspace crate"
```

---

### Task 2: Error taxonomy

**Files:**
- Create: `crates/wonderblob-core/src/error.rs`
- Test: inline `#[cfg(test)]` in `error.rs`

- [ ] **Step 1: Write the failing test**

In `crates/wonderblob-core/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_serialize_with_kind_for_frontend() {
        let e = StorageError::NotFound { path: "/x".into() };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["kind"], "notFound");
        assert_eq!(json["path"], "/x");
    }

    #[test]
    fn auth_failed_is_not_retryable_but_network_is() {
        assert!(!StorageError::AuthFailed { detail: "bad key".into() }.is_retryable());
        assert!(StorageError::Network { detail: "reset".into() }.is_retryable());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wonderblob-core`
Expected: FAIL — `StorageError` not found.

- [ ] **Step 3: Implement the taxonomy**

`crates/wonderblob-core/src/error.rs` (above the tests):

```rust
use serde::Serialize;
use thiserror::Error;

/// Common error taxonomy every backend maps into (spec: "Error handling").
#[derive(Debug, Error, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StorageError {
    #[error("authentication failed: {detail}")]
    AuthFailed { detail: String },
    #[error("not found: {path}")]
    NotFound { path: String },
    #[error("permission denied: {path}")]
    PermissionDenied { path: String },
    #[error("network error: {detail}")]
    Network { detail: String },
    #[error("conflict at {path}: {detail}")]
    Conflict { path: String, detail: String },
    #[error("quota exceeded")]
    QuotaExceeded,
    #[error("operation not supported by this backend: {op}")]
    Unsupported { op: String },
    #[error("{detail}")]
    Other { detail: String },
}

impl StorageError {
    /// Transient errors are retried with backoff; the rest surface immediately.
    pub fn is_retryable(&self) -> bool {
        matches!(self, StorageError::Network { .. })
    }

    pub fn other(e: impl std::fmt::Display) -> Self {
        StorageError::Other { detail: e.to_string() }
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;
```

Add `serde_json = { workspace = true }` to core's `[dev-dependencies]`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p wonderblob-core`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): StorageError taxonomy with frontend-serializable kinds"
```

---

### Task 3: VFS trait, entries, capabilities

**Files:**
- Create: `crates/wonderblob-core/src/vfs.rs`
- Test: inline `#[cfg(test)]` in `vfs.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_serializes_camel_case() {
        let e = Entry {
            name: "report.pdf".into(),
            path: "/docs/report.pdf".into(),
            kind: EntryKind::File,
            size: Some(1024),
            modified_ms: Some(1_700_000_000_000),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "file");
        assert_eq!(v["modifiedMs"], 1_700_000_000_000i64);
    }

    #[test]
    fn default_capabilities_are_conservative() {
        let c = Capabilities::default();
        assert!(!c.can_presign);
        assert!(c.can_rename); // most backends rename; opt out, not in
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core vfs`
Expected: FAIL — types not defined.

- [ ] **Step 3: Implement the VFS module**

`crates/wonderblob-core/src/vfs.rs`:

```rust
use crate::error::Result;
use async_trait::async_trait;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub modified_ms: Option<i64>,
}

/// What this backend can do; UI greys out the rest (spec: capability flags).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub can_presign: bool,
    pub can_rename: bool,
    pub can_set_mtime: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self { can_presign: false, can_rename: true, can_set_mtime: false }
    }
}

/// One implementation per protocol. Object-safe; held as `Arc<dyn StorageBackend>`.
#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    async fn list(&self, path: &str) -> Result<Vec<Entry>>;
    async fn stat(&self, path: &str) -> Result<Entry>;
    /// Reader over file contents starting at `offset` (ranged reads for preview/resume).
    async fn read(&self, path: &str, offset: u64)
        -> Result<Box<dyn AsyncRead + Send + Unpin>>;
    /// Writer that creates/replaces the file at `path`.
    async fn write(&self, path: &str)
        -> Result<Box<dyn AsyncWrite + Send + Unpin>>;
    async fn delete(&self, path: &str) -> Result<()>;
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    async fn mkdir(&self, path: &str) -> Result<()>;
    /// Time-limited share link; backends without support return Unsupported.
    async fn share_link(&self, path: &str, expiry_secs: u64) -> Result<String>;
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p wonderblob-core`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): StorageBackend VFS trait, Entry, Capabilities"
```

---

### Task 4: VFS contract test suite + Docker SFTP fixture

The reusable contract suite every backend must pass (spec: "Testing"). It runs only when `WONDERBLOB_TEST_SFTP=1`, so plain `cargo test` stays green without Docker.

**Files:**
- Create: `crates/wonderblob-core/tests/contract/mod.rs`
- Create: `crates/wonderblob-core/tests/sftp_contract.rs`
- Create: `scripts/test-sftp-up.sh`, `scripts/test-sftp-down.sh`

- [ ] **Step 1: Write the Docker fixture scripts**

`scripts/test-sftp-up.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
# Throwaway OpenSSH server for contract tests. User: wb / Password: wbpass / port 2222.
docker rm -f wonderblob-test-sftp >/dev/null 2>&1 || true
docker run -d --name wonderblob-test-sftp -p 2222:2222 \
  -e USER_NAME=wb -e USER_PASSWORD=wbpass -e PASSWORD_ACCESS=true \
  lscr.io/linuxserver/openssh-server:latest >/dev/null
echo "waiting for sshd..."
for i in $(seq 1 30); do
  if docker exec wonderblob-test-sftp pgrep sshd >/dev/null 2>&1; then
    sleep 1; echo "ready on localhost:2222 (wb/wbpass)"; exit 0
  fi
  sleep 1
done
echo "sshd never came up" >&2; exit 1
```

`scripts/test-sftp-down.sh`:

```bash
#!/usr/bin/env bash
docker rm -f wonderblob-test-sftp >/dev/null 2>&1 || true
```

Run: `chmod +x scripts/test-sftp-*.sh`

- [ ] **Step 2: Write the contract suite (generic over any backend)**

`crates/wonderblob-core/tests/contract/mod.rs`:

```rust
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wonderblob_core::error::StorageError;
use wonderblob_core::vfs::{EntryKind, StorageBackend};

/// Every backend must pass this. `root` is a writable empty directory path.
pub async fn run_contract(b: &dyn StorageBackend, root: &str) {
    let dir = format!("{root}/contract-dir");
    let file = format!("{dir}/hello.txt");
    let renamed = format!("{dir}/hello-renamed.txt");

    // mkdir + list shows it
    b.mkdir(&dir).await.expect("mkdir");
    let entries = b.list(root).await.expect("list root");
    assert!(entries.iter().any(|e| e.path == dir && e.kind == EntryKind::Dir));

    // write + read back
    let mut w = b.write(&file).await.expect("open write");
    w.write_all(b"hello wonderblob").await.expect("write bytes");
    w.shutdown().await.expect("flush/close");
    let mut r = b.read(&file, 0).await.expect("open read");
    let mut buf = String::new();
    r.read_to_string(&mut buf).await.expect("read bytes");
    assert_eq!(buf, "hello wonderblob");

    // ranged read
    let mut r = b.read(&file, 6).await.expect("ranged read");
    let mut buf = String::new();
    r.read_to_string(&mut buf).await.unwrap();
    assert_eq!(buf, "wonderblob");

    // stat
    let st = b.stat(&file).await.expect("stat");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.size, Some(16));

    // rename (if capable), delete, NotFound taxonomy
    if b.capabilities().can_rename {
        b.rename(&file, &renamed).await.expect("rename");
        assert!(matches!(
            b.stat(&file).await,
            Err(StorageError::NotFound { .. })
        ));
        b.delete(&renamed).await.expect("delete file");
    } else {
        b.delete(&file).await.expect("delete file");
    }
    b.delete(&dir).await.expect("delete dir");
    assert!(matches!(b.stat(&dir).await, Err(StorageError::NotFound { .. })));
}
```

- [ ] **Step 3: Write the SFTP harness that calls it (failing — no backend yet)**

`crates/wonderblob-core/tests/sftp_contract.rs`:

```rust
mod contract;

use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

#[tokio::test]
async fn sftp_passes_vfs_contract() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    let backend = SftpBackend::connect(SftpConfig {
        host: "localhost".into(),
        port: 2222,
        username: "wb".into(),
        auth: SftpAuth::Password("wbpass".into()),
    })
    .await
    .expect("connect");
    contract::run_contract(&backend, "/config").await;
}
```

- [ ] **Step 4: Verify it fails to compile**

Run: `cargo test -p wonderblob-core --test sftp_contract`
Expected: compile error — `wonderblob_core::sftp` doesn't exist. That's the red state for Task 5.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(core): VFS contract suite + dockerized SFTP fixture"
```

---

### Task 5: SFTP backend (password auth first)

**Files:**
- Create: `crates/wonderblob-core/src/sftp.rs`
- Modify: `crates/wonderblob-core/src/lib.rs` (add `pub mod sftp;`)
- Modify: `crates/wonderblob-core/Cargo.toml`

- [ ] **Step 1: Add dependencies**

In `crates/wonderblob-core/Cargo.toml`:

```toml
russh = "0.46"
russh-sftp = "2"
```

- [ ] **Step 2: Implement connect + handler + password auth**

`crates/wonderblob-core/src/sftp.rs`:

```rust
use crate::error::{Result, StorageError};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use russh::client;
use russh_sftp::client::SftpSession;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite};

pub enum SftpAuth {
    /// Try every identity in the SSH agent (SSH_AUTH_SOCK) — 1Password et al.
    Agent,
    KeyFile { path: String, passphrase: Option<String> },
    Password(String),
}

pub struct SftpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SftpAuth,
}

struct Handler;

impl client::Handler for Handler {
    type Error = russh::Error;
    // v1: accept any host key; host-key verification is a tracked follow-up
    // before any public release.
    async fn check_server_key(
        &mut self,
        _key: &russh::keys::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct SftpBackend {
    sftp: SftpSession,
    _session: client::Handle<Handler>, // keep the connection alive
}

impl SftpBackend {
    pub async fn connect(cfg: SftpConfig) -> Result<Self> {
        let config = Arc::new(client::Config::default());
        let mut session =
            client::connect(config, (cfg.host.as_str(), cfg.port), Handler)
                .await
                .map_err(|e| StorageError::Network { detail: e.to_string() })?;

        let authed = match &cfg.auth {
            SftpAuth::Password(pw) => session
                .authenticate_password(&cfg.username, pw)
                .await
                .map_err(|e| StorageError::Network { detail: e.to_string() })?
                .success(),
            SftpAuth::Agent => authenticate_agent(&mut session, &cfg.username).await?,
            SftpAuth::KeyFile { path, passphrase } => {
                authenticate_keyfile(&mut session, &cfg.username, path, passphrase.as_deref())
                    .await?
            }
        };
        if !authed {
            return Err(StorageError::AuthFailed {
                detail: format!("all auth methods rejected for {}", cfg.username),
            });
        }

        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| StorageError::Network { detail: e.to_string() })?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| StorageError::Network { detail: e.to_string() })?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| StorageError::other(e))?;

        Ok(Self { sftp, _session: session })
    }
}

/// Map russh-sftp status errors into the taxonomy.
fn map_sftp_err(path: &str, e: russh_sftp::client::error::Error) -> StorageError {
    let s = e.to_string().to_lowercase();
    if s.contains("no such file") {
        StorageError::NotFound { path: path.into() }
    } else if s.contains("permission denied") {
        StorageError::PermissionDenied { path: path.into() }
    } else {
        StorageError::Other { detail: e.to_string() }
    }
}
```

(Leave `authenticate_agent`/`authenticate_keyfile` as compile-blocking stubs returning `Err(StorageError::Unsupported { op: "agent".into() })` for now — implemented in Task 7. Define them so this compiles:)

```rust
async fn authenticate_agent(
    _session: &mut client::Handle<Handler>,
    _user: &str,
) -> Result<bool> {
    Err(StorageError::Unsupported { op: "agent auth (Task 7)".into() })
}

async fn authenticate_keyfile(
    _session: &mut client::Handle<Handler>,
    _user: &str,
    _path: &str,
    _passphrase: Option<&str>,
) -> Result<bool> {
    Err(StorageError::Unsupported { op: "keyfile auth (Task 7)".into() })
}
```

- [ ] **Step 3: Implement the StorageBackend trait for SFTP**

Append to `sftp.rs`:

```rust
fn entry_from(path_prefix: &str, name: &str, attrs: &russh_sftp::protocol::FileAttributes) -> Entry {
    let kind = if attrs.is_dir() {
        EntryKind::Dir
    } else if attrs.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    Entry {
        name: name.to_string(),
        path: format!("{}/{}", path_prefix.trim_end_matches('/'), name),
        kind,
        size: attrs.size,
        modified_ms: attrs.mtime.map(|t| (t as i64) * 1000),
    }
}

#[async_trait]
impl StorageBackend for SftpBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities { can_presign: false, can_rename: true, can_set_mtime: true }
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let dir = self.sftp.read_dir(path).await.map_err(|e| map_sftp_err(path, e))?;
        let mut out: Vec<Entry> = dir
            .filter(|f| f.file_name() != "." && f.file_name() != "..")
            .map(|f| entry_from(path, &f.file_name(), &f.metadata()))
            .collect();
        out.sort_by(|a, b| (b.kind == EntryKind::Dir).cmp(&(a.kind == EntryKind::Dir))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<Entry> {
        let attrs = self.sftp.metadata(path).await.map_err(|e| map_sftp_err(path, e))?;
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        Ok(entry_from(parent, &name, &attrs))
    }

    async fn read(&self, path: &str, offset: u64)
        -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        let mut f = self.sftp.open(path).await.map_err(|e| map_sftp_err(path, e))?;
        if offset > 0 {
            f.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(StorageError::other)?;
        }
        Ok(Box::new(f))
    }

    async fn write(&self, path: &str)
        -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        let f = self.sftp.create(path).await.map_err(|e| map_sftp_err(path, e))?;
        Ok(Box::new(f))
    }

    async fn delete(&self, path: &str) -> Result<()> {
        match self.stat(path).await?.kind {
            EntryKind::Dir => self.sftp.remove_dir(path).await,
            _ => self.sftp.remove_file(path).await,
        }
        .map_err(|e| map_sftp_err(path, e))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.sftp.rename(from, to).await.map_err(|e| map_sftp_err(from, e))
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        self.sftp.create_dir(path).await.map_err(|e| map_sftp_err(path, e))
    }

    async fn share_link(&self, _path: &str, _expiry: u64) -> Result<String> {
        Err(StorageError::Unsupported { op: "share_link".into() })
    }
}
```

Add `pub mod sftp;` to `lib.rs`.

- [ ] **Step 4: Build, fixing crate-API drift**

Run: `cargo build -p wonderblob-core`
If signatures don't match the pinned crate versions, check docs.rs (`russh`, `russh-sftp`) and adjust — keep the trait surface unchanged.

- [ ] **Step 5: Run the contract suite for real**

```bash
./scripts/test-sftp-up.sh
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_contract -- --nocapture
./scripts/test-sftp-down.sh
```

Expected: `sftp_passes_vfs_contract ... ok`. Debug against the live container until green.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(core): SFTP backend passing VFS contract (password auth)"
```

---

### Task 6: SSH agent + key file auth

The 1Password requirement. The agent flow: connect to `SSH_AUTH_SOCK`, enumerate identities, offer each public key until the server accepts one, sign with the agent.

**Files:**
- Modify: `crates/wonderblob-core/src/sftp.rs` (replace the two stubs)
- Test: `crates/wonderblob-core/tests/sftp_agent.rs`

- [ ] **Step 1: Write the failing test**

`crates/wonderblob-core/tests/sftp_agent.rs`:

```rust
//! Requires: scripts/test-sftp-up.sh running, a local ssh-agent with a key
//! loaded (`ssh-add`), and that key authorized for user wb in the container:
//!   docker exec wonderblob-test-sftp sh -c \
//!     'mkdir -p /config/.ssh && echo "<your pubkey line>" >> /config/.ssh/authorized_keys'
//! Gated by WONDERBLOB_TEST_SFTP_AGENT=1.

use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};
use wonderblob_core::vfs::StorageBackend;

#[tokio::test]
async fn agent_auth_connects_and_lists() {
    if std::env::var("WONDERBLOB_TEST_SFTP_AGENT").as_deref() != Ok("1") {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP_AGENT=1 (see file header)");
        return;
    }
    let b = SftpBackend::connect(SftpConfig {
        host: "localhost".into(),
        port: 2222,
        username: "wb".into(),
        auth: SftpAuth::Agent,
    })
    .await
    .expect("agent auth should succeed");
    b.list("/config").await.expect("list after agent auth");
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
./scripts/test-sftp-up.sh
ssh-add -L | head -1   # confirm an agent identity exists
docker exec wonderblob-test-sftp sh -c "mkdir -p /config/.ssh && echo '$(ssh-add -L | head -1)' >> /config/.ssh/authorized_keys && chown -R wb /config/.ssh"
WONDERBLOB_TEST_SFTP_AGENT=1 cargo test -p wonderblob-core --test sftp_agent -- --nocapture
```

Expected: FAIL with `Unsupported { op: "agent auth (Task 7)" }` (stub from Task 5).

- [ ] **Step 3: Implement agent auth**

Replace the `authenticate_agent` stub in `sftp.rs`:

```rust
async fn authenticate_agent(
    session: &mut client::Handle<Handler>,
    user: &str,
) -> Result<bool> {
    use russh::keys::agent::client::AgentClient;

    let mut agent = AgentClient::connect_env().await.map_err(|e| {
        StorageError::AuthFailed {
            detail: format!("cannot reach ssh-agent (SSH_AUTH_SOCK): {e}"),
        }
    })?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| StorageError::AuthFailed { detail: e.to_string() })?;
    if identities.is_empty() {
        return Err(StorageError::AuthFailed {
            detail: "ssh-agent has no identities loaded".into(),
        });
    }
    for key in identities {
        let result = session
            .authenticate_publickey_with(user, key, None, &mut agent)
            .await
            .map_err(|e| StorageError::Network { detail: e.to_string() })?;
        if result.success() {
            return Ok(true);
        }
    }
    Ok(false) // none accepted — caller raises AuthFailed
}
```

Replace the `authenticate_keyfile` stub:

```rust
async fn authenticate_keyfile(
    session: &mut client::Handle<Handler>,
    user: &str,
    path: &str,
    passphrase: Option<&str>,
) -> Result<bool> {
    use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};

    let key = load_secret_key(path, passphrase).map_err(|e| StorageError::AuthFailed {
        detail: format!("cannot load key {path}: {e}"),
    })?;
    let result = session
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .map_err(|e| StorageError::Network { detail: e.to_string() })?;
    Ok(result.success())
}
```

(API drift note applies: `authenticate_publickey_with` / `PrivateKeyWithHashAlg` are the russh 0.46-era names — verify on docs.rs for the pinned version.)

- [ ] **Step 4: Run the agent test until green**

Same command as Step 2. Expected: PASS. Also verify it passes with the **1Password agent** specifically (`SSH_AUTH_SOCK=~/.1password/agent.sock`) — that's the acceptance criterion.

- [ ] **Step 5: Run full core test suite**

Run: `cargo test -p wonderblob-core` (unit tests, no Docker needed)
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(core): SSH agent (1Password-compatible) and key file auth"
```

---

### Task 7: Tauri command layer

Bridge core to the frontend: open/close connections, list, download, upload. Connections live in a `HashMap<ConnectionId, Arc<dyn StorageBackend>>` behind Tauri managed state.

**Files:**
- Create: `src-tauri/src/state.rs`, `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs` (register state + commands)

- [ ] **Step 1: Implement managed state**

`src-tauri/src/state.rs`:

```rust
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;
use wonderblob_core::vfs::StorageBackend;

pub type ConnectionId = u64;

#[derive(Default)]
pub struct AppState {
    next_id: AtomicU64,
    pub connections: RwLock<HashMap<ConnectionId, std::sync::Arc<dyn StorageBackend>>>,
}

impl AppState {
    pub fn next_id(&self) -> ConnectionId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}
```

- [ ] **Step 2: Implement commands**

`src-tauri/src/commands.rs`:

```rust
use crate::state::{AppState, ConnectionId};
use serde::Deserialize;
use std::sync::Arc;
use tauri::State;
use tokio::io::AsyncWriteExt;
use wonderblob_core::error::StorageError;
use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};
use wonderblob_core::vfs::Entry;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthSpec {
    Agent,
    KeyFile { path: String, passphrase: Option<String> },
    Password { password: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpConnectArgs {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthSpec,
}

#[tauri::command]
pub async fn connect_sftp(
    state: State<'_, AppState>,
    args: SftpConnectArgs,
) -> Result<ConnectionId, StorageError> {
    let auth = match args.auth {
        AuthSpec::Agent => SftpAuth::Agent,
        AuthSpec::KeyFile { path, passphrase } => SftpAuth::KeyFile { path, passphrase },
        AuthSpec::Password { password } => SftpAuth::Password(password),
    };
    let backend = SftpBackend::connect(SftpConfig {
        host: args.host,
        port: args.port,
        username: args.username,
        auth,
    })
    .await?;
    let id = state.next_id();
    state.connections.write().await.insert(id, Arc::new(backend));
    Ok(id)
}

async fn backend(
    state: &State<'_, AppState>,
    id: ConnectionId,
) -> Result<Arc<dyn wonderblob_core::vfs::StorageBackend>, StorageError> {
    state.connections.read().await.get(&id).cloned().ok_or(StorageError::Other {
        detail: format!("no such connection {id}"),
    })
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, id: ConnectionId) -> Result<(), StorageError> {
    state.connections.write().await.remove(&id);
    Ok(())
}

#[tauri::command]
pub async fn list_dir(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
) -> Result<Vec<Entry>, StorageError> {
    backend(&state, id).await?.list(&path).await
}

#[tauri::command]
pub async fn download_file(
    state: State<'_, AppState>,
    id: ConnectionId,
    remote_path: String,
    local_path: String,
) -> Result<(), StorageError> {
    let b = backend(&state, id).await?;
    let mut r = b.read(&remote_path, 0).await?;
    let mut f = tokio::fs::File::create(&local_path).await.map_err(StorageError::other)?;
    tokio::io::copy(&mut r, &mut f).await.map_err(StorageError::other)?;
    f.flush().await.map_err(StorageError::other)?;
    Ok(())
}

#[tauri::command]
pub async fn upload_file(
    state: State<'_, AppState>,
    id: ConnectionId,
    local_path: String,
    remote_path: String,
) -> Result<(), StorageError> {
    let b = backend(&state, id).await?;
    let mut f = tokio::fs::File::open(&local_path).await.map_err(StorageError::other)?;
    let mut w = b.write(&remote_path).await?;
    tokio::io::copy(&mut f, &mut w).await.map_err(StorageError::other)?;
    w.shutdown().await.map_err(StorageError::other)?;
    Ok(())
}

#[tauri::command]
pub async fn delete_entry(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
) -> Result<(), StorageError> {
    backend(&state, id).await?.delete(&path).await
}

#[tauri::command]
pub async fn rename_entry(
    state: State<'_, AppState>,
    id: ConnectionId,
    from: String,
    to: String,
) -> Result<(), StorageError> {
    backend(&state, id).await?.rename(&from, &to).await
}

#[tauri::command]
pub async fn make_dir(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
) -> Result<(), StorageError> {
    backend(&state, id).await?.mkdir(&path).await
}
```

- [ ] **Step 3: Register in the Tauri builder**

In `src-tauri/src/lib.rs`, inside `run()`:

```rust
mod commands;
mod state;

// in the builder chain:
.manage(state::AppState::default())
.invoke_handler(tauri::generate_handler![
    commands::connect_sftp,
    commands::disconnect,
    commands::list_dir,
    commands::download_file,
    commands::upload_file,
    commands::delete_entry,
    commands::rename_entry,
    commands::make_dir,
])
```

`StorageError` already implements `Serialize`, which is all Tauri needs for command error returns.

- [ ] **Step 4: Verify it builds and the app still launches**

Run: `cargo build --workspace && npm run tauri dev` (close the window after it opens)
Expected: clean build, window opens.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(app): Tauri command layer for connections and file ops"
```

---

### Task 8: Frontend foundations — design system + app shell

The "must not look like a web app" task. Establish tokens and global resets before any feature UI exists, per the spec's design-language section (1Password 8 reference).

**Files:**
- Create: `src/lib/styles/tokens.css`, `src/lib/styles/app.css`
- Create: `src/lib/api.ts` (typed Tauri invoke wrappers)
- Modify: `src/App.svelte` (three-region shell)
- Delete: template demo components

- [ ] **Step 1: Create design tokens**

`src/lib/styles/tokens.css`:

```css
:root {
  /* type: platform-native stack, desktop density */
  --font-ui: system-ui, -apple-system, "Segoe UI", Cantarell, sans-serif;
  --font-mono: ui-monospace, "Cascadia Code", monospace;
  --text-base: 13px;
  --text-small: 11.5px;

  /* spacing & rows */
  --row-height: 30px;
  --sidebar-width: 240px;

  /* light theme (dark below) — neutral, one accent */
  --bg-app: #f5f5f4;
  --bg-sidebar: #ececeb;
  --bg-content: #ffffff;
  --bg-hover: rgba(0, 0, 0, 0.045);
  --bg-selected: rgba(59, 115, 235, 0.12);
  --fg-primary: #1c1c1e;
  --fg-secondary: #6e6e73;
  --border: rgba(0, 0, 0, 0.08);
  --accent: #3b73eb;
  --radius: 6px;
}

@media (prefers-color-scheme: dark) {
  :root {
    --bg-app: #1e1e20;
    --bg-sidebar: #252528;
    --bg-content: #2a2a2d;
    --bg-hover: rgba(255, 255, 255, 0.05);
    --bg-selected: rgba(94, 142, 245, 0.22);
    --fg-primary: #ededf0;
    --fg-secondary: #9b9ba1;
    --border: rgba(255, 255, 255, 0.09);
    --accent: #5e8ef5;
  }
}
```

- [ ] **Step 2: Global resets that kill webview tells**

`src/lib/styles/app.css`:

```css
@import "./tokens.css";

html, body {
  margin: 0;
  height: 100%;
  font-family: var(--font-ui);
  font-size: var(--text-base);
  color: var(--fg-primary);
  background: var(--bg-app);
  overscroll-behavior: none;          /* no scroll-bounce */
  -webkit-user-select: none;          /* chrome isn't selectable text */
  user-select: none;
  cursor: default;                    /* no text cursor on labels */
}

* { box-sizing: border-box; }

button, input, select { font: inherit; color: inherit; }
a { -webkit-user-drag: none; }
img { -webkit-user-drag: none; pointer-events: none; }

/* focus ring only for keyboard users */
:focus { outline: none; }
:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }

/* selectable where text genuinely is content */
.selectable { user-select: text; cursor: text; }
```

Also disable the webview context menu globally (real menus come per-component later) — in `src/main.ts` (or the Svelte entry):

```ts
if (!import.meta.env.DEV) {
  window.addEventListener("contextmenu", (e) => e.preventDefault());
}
window.addEventListener("keydown", (e) => {
  // block browser zoom / find / print chords
  if ((e.ctrlKey || e.metaKey) && ["+", "-", "=", "0", "p", "f"].includes(e.key)) {
    e.preventDefault();
  }
});
```

- [ ] **Step 3: Typed API wrapper**

`src/lib/api.ts`:

```ts
import { invoke } from "@tauri-apps/api/core";

export type EntryKind = "file" | "dir" | "symlink";
export interface Entry {
  name: string;
  path: string;
  kind: EntryKind;
  size: number | null;
  modifiedMs: number | null;
}
export type StorageErrorKind =
  | "authFailed" | "notFound" | "permissionDenied" | "network"
  | "conflict" | "quotaExceeded" | "unsupported" | "other";
export interface StorageError { kind: StorageErrorKind; [k: string]: unknown }

export type AuthSpec =
  | { type: "agent" }
  | { type: "keyFile"; path: string; passphrase?: string }
  | { type: "password"; password: string };

export const api = {
  connectSftp: (args: { host: string; port: number; username: string; auth: AuthSpec }) =>
    invoke<number>("connect_sftp", { args }),
  disconnect: (id: number) => invoke<void>("disconnect", { id }),
  listDir: (id: number, path: string) => invoke<Entry[]>("list_dir", { id, path }),
  downloadFile: (id: number, remotePath: string, localPath: string) =>
    invoke<void>("download_file", { id, remotePath, localPath }),
  uploadFile: (id: number, localPath: string, remotePath: string) =>
    invoke<void>("upload_file", { id, localPath, remotePath }),
  deleteEntry: (id: number, path: string) => invoke<void>("delete_entry", { id, path }),
  renameEntry: (id: number, from: string, to: string) =>
    invoke<void>("rename_entry", { id, from, to }),
  makeDir: (id: number, path: string) => invoke<void>("make_dir", { id, path }),
};
```

- [ ] **Step 4: App shell — sidebar / toolbar / content**

Replace `src/App.svelte`:

```svelte
<script lang="ts">
  import "./lib/styles/app.css";
</script>

<div class="shell">
  <aside class="sidebar">
    <div class="sidebar-section">Connections</div>
    <!-- BookmarkList mounts here (Task 9) -->
  </aside>
  <main class="content">
    <div class="toolbar">
      <!-- path breadcrumb + actions mount here (Task 10) -->
    </div>
    <div class="browser">
      <!-- FileList mounts here (Task 10) -->
    </div>
  </main>
</div>

<style>
  .shell { display: flex; height: 100vh; }
  .sidebar {
    width: var(--sidebar-width);
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
    padding: 8px;
    flex-shrink: 0;
  }
  .sidebar-section {
    font-size: var(--text-small);
    font-weight: 600;
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 6px 8px;
  }
  .content { flex: 1; display: flex; flex-direction: column; background: var(--bg-content); }
  .toolbar {
    height: 44px;
    border-bottom: 1px solid var(--border);
    display: flex;
    align-items: center;
    padding: 0 12px;
    gap: 8px;
    flex-shrink: 0;
  }
  .browser { flex: 1; overflow-y: auto; }
</style>
```

- [ ] **Step 5: Verify visually**

Run: `npm run tauri dev`
Expected: empty but *correct-feeling* shell — dark mode follows OS, no text cursor anywhere, no overscroll bounce, sidebar/toolbar layout matches the spec sketch.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(ui): design tokens, webview-tell resets, app shell"
```

---

### Task 9: Bookmarks + OS keychain

Bookmark metadata in a JSON file under the Tauri app-config dir; secrets only in the OS keychain via `keyring`, keyed by bookmark UUID.

**Files:**
- Create: `src-tauri/src/bookmarks.rs`
- Modify: `src-tauri/Cargo.toml` (add `keyring = "3"`, `uuid = { version = "1", features = ["v4", "serde"] }`)
- Modify: `src-tauri/src/lib.rs` (register commands)
- Modify: `src/lib/api.ts`
- Create: `src/lib/components/BookmarkList.svelte`, `src/lib/components/ConnectionSheet.svelte`
- Test: inline `#[cfg(test)]` in `bookmarks.rs`

- [ ] **Step 1: Write the failing test (store round-trip, no secrets in file)**

In `src-tauri/src/bookmarks.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_file_roundtrips_and_never_contains_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let store = BookmarkStore::new(dir.path().to_path_buf());
        let b = Bookmark {
            id: uuid::Uuid::new_v4(),
            label: "prod box".into(),
            protocol: Protocol::Sftp,
            host: "example.com".into(),
            port: 22,
            username: "jack".into(),
            auth_method: AuthMethod::Agent,
            initial_path: Some("/var/www".into()),
        };
        store.save(&b).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].label, "prod box");
        let raw = std::fs::read_to_string(store.file_path()).unwrap();
        assert!(!raw.to_lowercase().contains("password"));
    }
}
```

Add `tempfile = "3"` to src-tauri `[dev-dependencies]`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob` (the src-tauri package name from the scaffold — check its `Cargo.toml`; adjust `-p` accordingly)
Expected: FAIL — types not defined.

- [ ] **Step 3: Implement the store**

`src-tauri/src/bookmarks.rs` (above tests):

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;
use wonderblob_core::error::StorageError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Protocol { Sftp } // S3/AzBlob/OneDrive added in later plans

/// How to authenticate — the *method* only; secrets live in the keychain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthMethod {
    Agent,
    KeyFile { path: String }, // passphrase (if any) in keychain
    Password,                 // password in keychain
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub id: Uuid,
    pub label: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    pub initial_path: Option<String>,
}

pub struct BookmarkStore {
    dir: PathBuf,
}

impl BookmarkStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn file_path(&self) -> PathBuf {
        self.dir.join("bookmarks.json")
    }

    pub fn load_all(&self) -> Result<Vec<Bookmark>, StorageError> {
        match std::fs::read_to_string(self.file_path()) {
            Ok(s) => serde_json::from_str(&s).map_err(StorageError::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(StorageError::other(e)),
        }
    }

    pub fn save(&self, b: &Bookmark) -> Result<(), StorageError> {
        let mut all = self.load_all()?;
        all.retain(|x| x.id != b.id);
        all.push(b.clone());
        self.write_all(&all)
    }

    pub fn delete(&self, id: Uuid) -> Result<(), StorageError> {
        let mut all = self.load_all()?;
        all.retain(|x| x.id != id);
        self.write_all(&all)
    }

    fn write_all(&self, all: &[Bookmark]) -> Result<(), StorageError> {
        std::fs::create_dir_all(&self.dir).map_err(StorageError::other)?;
        let tmp = self.file_path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(all).map_err(StorageError::other)?)
            .map_err(StorageError::other)?;
        std::fs::rename(&tmp, self.file_path()).map_err(StorageError::other)
    }
}

/// Keychain wrapper. Service is constant; account is the bookmark UUID.
pub mod secrets {
    use wonderblob_core::error::StorageError;

    const SERVICE: &str = "com.wonderblob.app";

    pub fn set(bookmark_id: &str, secret: &str) -> Result<(), StorageError> {
        keyring::Entry::new(SERVICE, bookmark_id)
            .and_then(|e| e.set_password(secret))
            .map_err(StorageError::other)
    }

    pub fn get(bookmark_id: &str) -> Result<Option<String>, StorageError> {
        match keyring::Entry::new(SERVICE, bookmark_id).and_then(|e| e.get_password()) {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(StorageError::other(e)),
        }
    }

    pub fn delete(bookmark_id: &str) -> Result<(), StorageError> {
        match keyring::Entry::new(SERVICE, bookmark_id).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(StorageError::other(e)),
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p wonderblob` (adjust package name)
Expected: PASS.

- [ ] **Step 5: Add bookmark commands + wire into builder**

Append to `src-tauri/src/commands.rs`:

```rust
use crate::bookmarks::{secrets, Bookmark, BookmarkStore};
use tauri::Manager;

fn store(app: &tauri::AppHandle) -> Result<BookmarkStore, StorageError> {
    let dir = app.path().app_config_dir().map_err(StorageError::other)?;
    Ok(BookmarkStore::new(dir))
}

#[tauri::command]
pub async fn bookmarks_list(app: tauri::AppHandle) -> Result<Vec<Bookmark>, StorageError> {
    store(&app)?.load_all()
}

#[tauri::command]
pub async fn bookmark_save(
    app: tauri::AppHandle,
    bookmark: Bookmark,
    secret: Option<String>,
) -> Result<(), StorageError> {
    if let Some(s) = secret {
        secrets::set(&bookmark.id.to_string(), &s)?;
    }
    store(&app)?.save(&bookmark)
}

#[tauri::command]
pub async fn bookmark_delete(app: tauri::AppHandle, id: uuid::Uuid) -> Result<(), StorageError> {
    secrets::delete(&id.to_string())?;
    store(&app)?.delete(id)
}

/// Connect using a saved bookmark: resolves the secret from the keychain.
#[tauri::command]
pub async fn connect_bookmark(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: uuid::Uuid,
) -> Result<ConnectionId, StorageError> {
    use crate::bookmarks::AuthMethod;
    let b = store(&app)?
        .load_all()?
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| StorageError::Other { detail: "bookmark not found".into() })?;
    let auth = match b.auth_method {
        AuthMethod::Agent => SftpAuth::Agent,
        AuthMethod::KeyFile { path } => SftpAuth::KeyFile {
            path,
            passphrase: secrets::get(&b.id.to_string())?,
        },
        AuthMethod::Password => SftpAuth::Password(
            secrets::get(&b.id.to_string())?
                .ok_or(StorageError::AuthFailed { detail: "no saved password".into() })?,
        ),
    };
    let backend = SftpBackend::connect(SftpConfig {
        host: b.host,
        port: b.port,
        username: b.username,
        auth,
    })
    .await?;
    let cid = state.next_id();
    state.connections.write().await.insert(cid, Arc::new(backend));
    Ok(cid)
}
```

Add `mod bookmarks;` to `lib.rs` and the four new commands to `generate_handler![]`. Add to `src/lib/api.ts`:

```ts
export type AuthMethod =
  | { type: "agent" }
  | { type: "keyFile"; path: string }
  | { type: "password" };
export interface Bookmark {
  id: string; label: string; protocol: "sftp";
  host: string; port: number; username: string;
  authMethod: AuthMethod; initialPath: string | null;
}
// add to the `api` object:
//   bookmarksList: () => invoke<Bookmark[]>("bookmarks_list"),
//   bookmarkSave: (bookmark: Bookmark, secret?: string) =>
//     invoke<void>("bookmark_save", { bookmark, secret }),
//   bookmarkDelete: (id: string) => invoke<void>("bookmark_delete", { id }),
//   connectBookmark: (id: string) => invoke<number>("connect_bookmark", { id }),
```

- [ ] **Step 6: Build the sidebar + connection sheet UI**

`src/lib/components/BookmarkList.svelte` — sidebar list with rows (`height: var(--row-height)`, hover `--bg-hover`, selected `--bg-selected`), arrow-key navigation, Enter connects, a small `+` button in the section header opening the sheet. Double-click connects.

`src/lib/components/ConnectionSheet.svelte` — modal form (label, host, port, username, auth method select: Agent / Key file / Password; secret field shown only for the latter two; "SSH Agent" is the **default** selection). Saves via `api.bookmarkSave`; secret is passed separately and never stored in component state longer than needed. Style: compact rows, right-aligned primary button using `--accent`, Escape cancels, Enter submits.

Mount both in `App.svelte`'s sidebar region; on successful connect, store `{ connectionId, bookmark }` in a Svelte store `src/lib/stores/session.ts`:

```ts
import { writable } from "svelte/store";
import type { Bookmark } from "../api";
export const activeConnection = writable<{ id: number; bookmark: Bookmark } | null>(null);
export const currentPath = writable<string>("/");
```

- [ ] **Step 7: Verify end-to-end manually**

```bash
./scripts/test-sftp-up.sh
npm run tauri dev
```

Create a bookmark for `localhost:2222`, user `wb`, password auth `wbpass`. Connect. Expected: no errors in console; connection id stored; password visible in `secret-tool search service com.wonderblob.app` / KWallet — and **absent** from `~/.config/wonderblob/bookmarks.json` (check the actual app-config path; identifier may make it `com.wonderblob.app`).

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat: bookmarks with OS keychain secrets, connection sheet UI"
```

---

### Task 10: File browser pane

**Files:**
- Create: `src/lib/components/FileList.svelte`, `src/lib/components/Breadcrumb.svelte`
- Create: `src/lib/format.ts`
- Modify: `src/App.svelte`

- [ ] **Step 1: Formatting helpers + test**

`src/lib/format.ts`:

```ts
export function formatSize(bytes: number | null): string {
  if (bytes === null) return "—";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes, i = -1;
  do { v /= 1024; i++; } while (v >= 1024 && i < units.length - 1);
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[i]}`;
}

export function formatMtime(ms: number | null): string {
  if (ms === null) return "—";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium", timeStyle: "short",
  }).format(new Date(ms));
}
```

Add vitest: `npm i -D vitest`, script `"test": "vitest run"`, and `src/lib/format.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { formatSize } from "./format";

describe("formatSize", () => {
  it("handles null, bytes, and scales", () => {
    expect(formatSize(null)).toBe("—");
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(1536)).toBe("1.5 KB");
    expect(formatSize(157286400)).toBe("150 MB");
  });
});
```

Run: `npm test` — expected PASS (write test first, watch it fail on a stub if you prefer strict TDD; this is pure-function territory).

- [ ] **Step 2: FileList component**

`src/lib/components/FileList.svelte` — requirements (implement with Svelte 5 runes):

- Props: none; reads `activeConnection` + `currentPath` stores; loads `api.listDir` whenever either changes; shows entries in a table: name (with dir/file glyph), size (`formatSize`), modified (`formatMtime`)
- Rows: `height: var(--row-height)`, hover + selection backgrounds from tokens, single-select on click, range-select later (out of scope)
- **Keyboard:** ArrowUp/Down move selection, Enter on a dir descends (`currentPath.set(entry.path)`), Backspace goes to parent, type-ahead jumps to first name match (reset buffer after 700 ms)
- Double-click on a dir descends. Double-click on a file: **stub** — `console.info("open: EditSession in Plan 4")`
- Loading state: subtle 200 ms-delayed spinner (no flash on fast loads); error state: inline message with the `StorageError.kind`-specific text, e.g. `permissionDenied` → "You don't have permission to view this folder."
- Sort: dirs first, then case-insensitive name (backend already returns this — don't re-sort, trust the contract)

`src/lib/components/Breadcrumb.svelte` — splits `currentPath` on `/`, renders clickable segments in the toolbar; clicking a segment sets `currentPath`. Toolbar also gets: upload button (opens file picker via `@tauri-apps/plugin-dialog`, then `api.uploadFile` into `currentPath`), new-folder button, delete + rename in a context menu on rows (native menu via Tauri's menu API; fallback to a styled in-app menu if per-window context menus aren't wired yet — flag whichever was used in the PR notes).

- [ ] **Step 3: Wire into App.svelte**

Mount `Breadcrumb` in `.toolbar` and `FileList` in `.browser`; when `activeConnection` is null show an empty-state panel ("Connect to a server to get started" + button opening the connection sheet); when a bookmark with `initialPath` connects, set `currentPath` to it, else `/`.

- [ ] **Step 4: Verify the slice end-to-end**

```bash
./scripts/test-sftp-up.sh
npm run tauri dev
```

Manual checklist (this is the slice's acceptance test):
1. Connect to the test bookmark → file list renders `/config`
2. Keyboard: arrows, Enter into `.ssh`, Backspace out, type-ahead
3. New folder → appears; rename it; delete it
4. Upload a file via the toolbar → appears with correct size
5. Errors: stop Docker mid-session, click a folder → network error message, no crash
6. **Agent check:** create an agent-auth bookmark to a real host you have 1Password SSH access to; connect; browse
7. Feel check against spec design-language list: density, dark mode, no webview tells

- [ ] **Step 5: Run all tests**

```bash
cargo test --workspace
npm test
```

Expected: all green (Docker-gated tests skip without the env flag).

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(ui): file browser pane with keyboard nav, toolbar ops"
```

---

### Task 11: CI

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: CI
on:
  push: { branches: [main] }
  pull_request:

jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: System deps (Tauri on Linux)
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
            libayatana-appindicator3-dev librsvg2-dev libssl-dev
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace -- -D warnings
      - run: cargo test --workspace
      - name: SFTP contract tests
        run: |
          ./scripts/test-sftp-up.sh
          WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_contract
          ./scripts/test-sftp-down.sh

  frontend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 22, cache: npm }
      - run: npm ci
      - run: npm test
      - run: npx svelte-check
```

- [ ] **Step 2: Push and verify** (requires creating the GitHub repo — ask Jack for org/visibility before pushing)

Run: `gh repo create … && git push -u origin main`, then `gh run watch`
Expected: both jobs green.

- [ ] **Step 3: Commit any fixes**

```bash
git add -A && git commit -m "ci: rust + frontend pipelines with dockerized SFTP contract tests"
```

---

## Done criteria (Plan 1)

- `cargo test --workspace` + `npm test` green locally and in CI
- Contract suite passes against Dockerized OpenSSH
- App connects via **1Password SSH agent** to a real host and browses
- Bookmarks persist; secrets provably absent from `bookmarks.json`, present in keychain
- UI passes the spec's design-language checklist (density, dark mode, no webview tells)

## Explicitly deferred

S3/Azure backends (Plan 2), TransferEngine + progress events (Plan 3 — Task 7's download/upload are blocking one-shots by design), EditSession/preview (Plan 4), OneDrive + share links (Plan 5), drag & drop + packaging + host-key verification UX (Plan 6).
