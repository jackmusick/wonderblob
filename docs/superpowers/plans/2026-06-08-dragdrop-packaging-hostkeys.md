# Wonderblob Plan 6: Drag & Drop, Packaging, SSH Host-Key Verification

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three deferred-to-v1-release items from the spec and ship a distributable build. (A) Replace SFTP's **accept-any** host key with real verification — an OpenSSH-format `known_hosts` store, a trust-on-first-use approval flow surfaced to the user, and a hard-stop on key-changed (MITM). (B) **Drag & drop**: drag-in from the OS file manager → enqueue uploads everywhere; drag-out → a download-to-`~/Downloads` fallback on all platforms (macOS promise-files deferred, documented honestly). (C) **Packaging**: per-OS bundle targets, a tag-triggered `tauri-action` release workflow producing installers, version/README/changelog, with code-signing explicitly out of scope (unsigned-build caveats documented).

**Architecture:** Host-key store logic lives in `wonderblob-core` (`hostkey.rs`, testable with a temp `known_hosts` file) and reuses `russh-keys` 0.46's `known_hosts` module — `check_known_hosts_path`, `learn_known_hosts_path`, `Error::KeyChanged`, `PublicKey::fingerprint()` — which already ship the OpenSSH format **plus key-changed detection**. The `Handler` is wired to a pre-resolved decision (known/unknown/changed) computed *before* the handshake so `check_server_key` never blocks on a browser dialog. The connect command becomes **two-phase**: an unknown key returns a `HostKeyUnverified { fingerprint, host, port, keyB64 }` decision-needed result; the frontend shows an approval dialog; a retry carries the user's decision (`accept-and-remember` writes to `known_hosts`, `accept-once` trusts only this session). Drag-in is a thin frontend handler over `onDragDropEvent` → `api.enqueueUpload` (the existing, tested command). Packaging is config + a workflow; no new Rust.

**Tech Stack:** Rust (`russh` 0.46, `russh-keys` 0.46 `known_hosts` module — already deps), Tauri 2.x (`getCurrentWebview().onDragDropEvent`, bundle config), Svelte 5 runes, GitHub Actions + `tauri-apps/tauri-action@v0`.

**Spec:** `docs/superpowers/specs/2026-06-07-wonderblob-design.md` (§ "Drag & drop", § "Auth & credentials", § "v1 scope"; the "host-key verification is a tracked follow-up before any public release" note in `sftp.rs:32`).
**Builds on:** Plans 1–5 (merged). Touches `crates/wonderblob-core/src/sftp.rs`, `src-tauri/src/{commands.rs,lib.rs,state.rs}`, `src/lib/api.ts`, `src/routes/+page.svelte`, `src-tauri/tauri.conf.json`, `.github/workflows/`.

**Researched sources (decisions cite these):**
- Tauri drag-drop: [`onDragDropEvent` / webview namespace](https://v2.tauri.app/reference/javascript/api/namespacewebview/), [`dragDropEnabled` semantics — issue #14373](https://github.com/tauri-apps/tauri/issues/14373) (the native layer intercepts OS drops; DOM `ondrop` does **not** fire — must use `onDragDropEvent`; `dragDropEnabled` defaults to `true`).
- Release pipeline: [Tauri GitHub distribute guide](https://v2.tauri.app/distribute/pipelines/github/), [`tauri-apps/tauri-action`](https://github.com/tauri-apps/tauri-action).
- `known_hosts` format + SHA256 fingerprint: reused directly from `russh-keys` 0.46 (`src/known_hosts.rs`, `src/key.rs::fingerprint` → `BASE64_NOPAD(SHA256(pubkey))`).

**Crate-API caveat:** `russh`/`russh-keys` APIs move. Code below targets the pinned `russh`/`russh-keys` 0.46 already in `crates/wonderblob-core/Cargo.toml`. `Handler::check_server_key` takes `&russh::keys::key::PublicKey` (as in the current `sftp.rs`). If `cargo build` disagrees, consult docs.rs for the pinned version and adapt signatures — the *structure* (pre-resolve decision → handler returns it → two-phase connect) is stable.

---

## Decision log (read before implementing)

1. **Store format = OpenSSH `known_hosts`, not app JSON.** `russh-keys` 0.46 already exposes `check_known_hosts_path(host, port, &pubkey, path) -> Result<bool, Error>` (returns `Err(Error::KeyChanged { line })` on mismatch — exactly SSH's behavior) and `learn_known_hosts_path(...)`. Reusing it gives power-user-compatible files, free key-changed detection, and far less parsing than rolling JSON. The file lives at `<app-config>/known_hosts` (app-managed, **not** `~/.ssh/known_hosts` — we don't touch the user's OpenSSH file in v1). Hashed-host (`HashKnownHosts`) entries are **not** written (russh-keys writes plaintext host lines); reading hashed entries another tool wrote is out of scope (deferred).

2. **TOFU mechanism = two-phase connect, NOT a blocking callback.** `check_server_key` is called mid-handshake on russh's connection task; it cannot `await` a browser dialog without deadlocking the IPC. Instead we compute the decision **before** building the session: a `HostKeyDecision` resolved against the store. The `Handler` carries the decision and the handler simply returns `Ok(true/false)` — but for an *unknown* key the first connect attempt is made with a `Handler` that **captures the key and rejects** (so no data flows to an untrusted server), the connect returns `HostKeyUnverified { fingerprint, host, port, keyB64 }`, the frontend prompts, and a second `connect_sftp` carries `hostKeyDecision: { keyB64, remember }` → the store is updated (if remember) and the handler now trusts that exact key. This keeps the handshake non-blocking and never streams bytes to an unverified host.

3. **Drag-in = `onDragDropEvent`, top-level files + one-level-deep dirs.** OS drops never reach DOM `ondrop` in a Tauri webview (native interception — see issue #14373); the payload gives **filesystem paths**, not bytes, so we hand each path straight to `api.enqueueUpload` (already tested, streams from disk). Directories: enqueue their **immediate file children** (one level). Full recursion is deferred (the engine enqueues per-file; recursive tree-walk + remote `mkdir` is a tracked enhancement). A drop-target highlight shows on the FileList while `type === "over"`.

4. **Drag-out = download-to-`~/Downloads` fallback, all platforms; macOS promise-files deferred.** Tauri 2 has no stable cross-platform deferred drag-out (start-drag-with-promise). Honest v1: dragging a remote row **out of the window** is not wired (the webview can't originate an OS file-promise drag reliably); instead the existing Download button stays the primary path and we add a **"Download to ~/Downloads"** one-click action (no save dialog) so the fallback the spec promises exists. macOS promise-file drag is explicitly deferred. This matches the spec's "Linux/Windows v1 fallback" wording and avoids overpromising.

5. **Ephemeral-fixture-key CI problem.** The Dockerized OpenSSH fixture regenerates its host key every container start, so a committed `known_hosts` can't pin it. The SFTP contract/transfer/edit gated tests connect with an **`accept-once`** decision (trust this session's key without writing it) via a test-only `SftpConfig.host_key = HostKeyDecision::AcceptOnce`. Plain `cargo test` is unaffected (those tests are env-gated). CI stays green with no committed key.

---

### Task 1: Host-key store in core (`hostkey.rs`)

The testable store: check a key against the app `known_hosts`, classify it (Known / Unknown / Changed), remember it. Wraps `russh-keys::known_hosts` so the OpenSSH format and key-changed logic are reused, not reimplemented.

**Files:**
- Create: `crates/wonderblob-core/src/hostkey.rs`
- Modify: `crates/wonderblob-core/src/lib.rs` (add `pub mod hostkey;`)
- Test: inline `#[cfg(test)]` in `hostkey.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway pubkey for tests: parse an ed25519 known-hosts line russh can read.
    fn sample_key() -> russh::keys::key::PublicKey {
        // ssh-keygen-style line; parse via russh_keys::parse_public_key_base64.
        russh_keys::parse_public_key_base64(
            "AAAAC3NzaC1lZDI1NTE5AAAAIK8...REPLACE_WITH_A_REAL_ED25519_BLOB",
        )
        .expect("parse test pubkey")
    }

    #[test]
    fn unknown_then_remembered_then_known() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostKeyStore::new(dir.path().join("known_hosts"));
        let k = sample_key();
        assert!(matches!(store.classify("h.example.com", 22, &k).unwrap(), HostKeyStatus::Unknown));
        store.remember("h.example.com", 22, &k).unwrap();
        assert!(matches!(store.classify("h.example.com", 22, &k).unwrap(), HostKeyStatus::Known));
    }

    #[test]
    fn changed_key_is_flagged_not_silently_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostKeyStore::new(dir.path().join("known_hosts"));
        let k1 = sample_key();
        store.remember("h.example.com", 22, &k1).unwrap();
        // A *different* key of the same type for the same host → Changed.
        let k2 = other_key_same_type();
        assert!(matches!(store.classify("h.example.com", 22, &k2).unwrap(), HostKeyStatus::Changed));
    }

    #[test]
    fn fingerprint_is_sha256_base64() {
        let k = sample_key();
        let fp = fingerprint(&k);
        // russh-keys formats SHA256 fingerprints as BASE64_NOPAD (no "SHA256:" prefix);
        // we add the prefix for display parity with OpenSSH.
        assert!(fp.starts_with("SHA256:"));
    }

    fn other_key_same_type() -> russh::keys::key::PublicKey { /* second ed25519 blob */ unimplemented!() }
}
```

(When implementing, generate two real ed25519 public-key base64 blobs with `ssh-keygen -t ed25519` and paste the `AAAA…` field. Keep them as test constants.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core hostkey`
Expected: FAIL — `HostKeyStore` not defined.

- [ ] **Step 3: Implement the store over `russh-keys::known_hosts`**

`crates/wonderblob-core/src/hostkey.rs`:

```rust
//! App-managed SSH host-key store (spec § Auth: host-key verification is a
//! pre-release requirement). Wraps `russh-keys`' OpenSSH `known_hosts` reader so
//! the on-disk format is power-user-compatible and key-changed (MITM) detection
//! is reused, not reimplemented. We never touch ~/.ssh/known_hosts in v1.

use crate::error::{Result, StorageError};
use russh::keys::key::PublicKey;
use std::path::PathBuf;

/// Where a host's key sits relative to what we've already trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// Matches a recorded key — proceed silently.
    Known,
    /// No record for this host — surface for TOFU approval.
    Unknown,
    /// A record exists but the key DIFFERS — hard stop (possible MITM).
    Changed,
}

pub struct HostKeyStore {
    path: PathBuf,
}

impl HostKeyStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Classify a presented server key against the store. Reuses
    /// `check_known_hosts_path`, which returns `Err(KeyChanged)` on mismatch.
    pub fn classify(&self, host: &str, port: u16, key: &PublicKey) -> Result<HostKeyStatus> {
        if !self.path.exists() {
            return Ok(HostKeyStatus::Unknown);
        }
        match russh_keys::check_known_hosts_path(host, port, key, &self.path) {
            Ok(true) => Ok(HostKeyStatus::Known),
            Ok(false) => Ok(HostKeyStatus::Unknown),
            Err(russh_keys::Error::KeyChanged { .. }) => Ok(HostKeyStatus::Changed),
            Err(e) => Err(StorageError::other(e)),
        }
    }

    /// Append this host+key to the store in OpenSSH known_hosts format.
    pub fn remember(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(StorageError::other)?;
        }
        russh_keys::learn_known_hosts_path(host, port, key, &self.path).map_err(StorageError::other)
    }
}

/// SHA256 base64 fingerprint with the OpenSSH "SHA256:" display prefix.
/// `PublicKey::fingerprint()` returns BASE64_NOPAD(SHA256(pubkey)) without prefix.
pub fn fingerprint(key: &PublicKey) -> String {
    format!("SHA256:{}", key.fingerprint())
}

/// Base64 of the raw public key bytes — the opaque token the frontend round-trips
/// through the two-phase connect so the retry trusts the *exact* key it approved.
pub fn key_to_base64(key: &PublicKey) -> String {
    use russh::keys::PublicKeyBase64;
    key.public_key_base64()
}

pub fn key_from_base64(b64: &str) -> Result<PublicKey> {
    russh_keys::parse_public_key_base64(b64).map_err(StorageError::other)
}
```

Add `pub mod hostkey;` to `crates/wonderblob-core/src/lib.rs`. (`russh-keys` is already a dependency; if `parse_public_key_base64`/`PublicKeyBase64` live under a slightly different path in the pinned minor, check docs.rs.)

- [ ] **Step 4: Run tests** → `cargo test -p wonderblob-core hostkey` — expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): host-key store over OpenSSH known_hosts (classify/remember/fingerprint)"
```

---

### Task 2: Thread the host-key decision through `SftpBackend::connect`

The `Handler` must (a) never trust an unknown key, (b) capture the presented key so the caller can surface it, (c) trust exactly the key the user approved on retry. Done by computing the decision pre-handshake and giving the `Handler` a verdict + a capture slot.

**Files:**
- Modify: `crates/wonderblob-core/src/sftp.rs`
- Test: `crates/wonderblob-core/tests/sftp_hostkey.rs` (gated by `WONDERBLOB_TEST_SFTP`)

- [ ] **Step 1: Replace the accept-any `Handler` + extend `SftpConfig`**

In `sftp.rs`, add a decision type and a capturing handler. Replace the current `struct Handler;` / `check_server_key -> Ok(true)`:

```rust
use crate::hostkey::{key_from_base64, key_to_base64, fingerprint, HostKeyStatus, HostKeyStore};
use std::sync::Mutex as StdMutex;

/// What to do about the server's host key on THIS connect attempt.
pub enum HostKeyDecision {
    /// Verify against the store; unknown/changed keys are rejected and the
    /// presented key is captured for the caller to surface (TOFU phase 1).
    Verify(HostKeyStore),
    /// Trust exactly this key (its base64), and remember it if `remember`.
    /// Used by the connect retry after the user approves (TOFU phase 2), and by
    /// gated tests (`remember: false` = accept-once, for the ephemeral fixture key).
    Trust { key_b64: String, remember: bool, store: Option<HostKeyStore> },
}

/// Connect failed because the host key is unverified — NOT an error, a
/// decision-needed state the frontend turns into an approval dialog.
pub struct HostKeyUnverified {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub key_b64: String,
    /// true => a DIFFERENT key is already recorded (MITM warning), false => first-seen.
    pub changed: bool,
}

struct Handler {
    host: String,
    port: u16,
    decision: HostKeyDecisionInner,
    /// Filled with the presented key on rejection so `connect` can report it.
    captured: Arc<StdMutex<Option<(String, String, bool)>>>, // (fingerprint, key_b64, changed)
}

enum HostKeyDecisionInner {
    Verify(HostKeyStore),
    Trust { key_b64: String },
}

#[async_trait]
impl client::Handler for Handler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &russh::keys::key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        let presented_b64 = key_to_base64(key);
        match &self.decision {
            HostKeyDecisionInner::Trust { key_b64 } => Ok(&presented_b64 == key_b64),
            HostKeyDecisionInner::Verify(store) => {
                match store.classify(&self.host, self.port, key) {
                    Ok(HostKeyStatus::Known) => Ok(true),
                    Ok(status) => {
                        *self.captured.lock().unwrap() =
                            Some((fingerprint(key), presented_b64, status == HostKeyStatus::Changed));
                        Ok(false) // reject: handshake aborts, no bytes to an untrusted host
                    }
                    Err(_) => Ok(false),
                }
            }
        }
    }
}
```

Add `pub host_key: HostKeyDecision` to `SftpConfig`. `connect` builds the `Handler` from it and, on rejection-due-to-capture, returns the unverified state. Make `connect` return a richer result:

```rust
/// Either a connected backend or a TOFU decision-needed state.
pub enum SftpConnectOutcome {
    Connected(SftpBackend),
    HostKeyUnverified(HostKeyUnverified),
}

impl SftpBackend {
    pub async fn connect(cfg: SftpConfig) -> Result<SftpConnectOutcome> {
        let captured = Arc::new(StdMutex::new(None));
        let (inner, remember_store) = match cfg.host_key {
            HostKeyDecision::Verify(store) => (HostKeyDecisionInner::Verify(store), None),
            HostKeyDecision::Trust { key_b64, remember, store } =>
                (HostKeyDecisionInner::Trust { key_b64 }, remember.then_some(store).flatten()),
        };
        let handler = Handler {
            host: cfg.host.clone(), port: cfg.port,
            decision: inner, captured: captured.clone(),
        };
        let config = Arc::new(client::Config::default());
        let mut session = match client::connect(config, (cfg.host.as_str(), cfg.port), handler).await {
            Ok(s) => s,
            Err(e) => {
                // A rejected host key surfaces here as a handshake error; if we
                // captured a key, report the decision-needed state instead.
                if let Some((fp, key_b64, changed)) = captured.lock().unwrap().take() {
                    return Ok(SftpConnectOutcome::HostKeyUnverified(HostKeyUnverified {
                        host: cfg.host, port: cfg.port, fingerprint: fp, key_b64, changed,
                    }));
                }
                return Err(StorageError::Network { detail: e.to_string() });
            }
        };
        // Phase-2 trust that asked to remember: persist now that the handshake passed.
        if let Some(store) = remember_store {
            if let Ok(k) = key_from_base64(/* the trusted key */ &/* key_b64 captured above */ String::new()) {
                let _ = store.remember(&cfg.host, cfg.port, &k);
            }
        }
        // ... existing auth + sftp-subsystem setup, then:
        Ok(SftpConnectOutcome::Connected(Self { sftp, _session: session }))
    }
}
```

Implementation note for the `remember` write: keep the trusted `key_b64` in scope (it's `cfg.host_key`'s `key_b64`) so phase-2 can `key_from_base64` + `remember` after a successful handshake. Don't write on a failed handshake.

- [ ] **Step 2: Gated integration test (uses accept-once for the ephemeral fixture)**

`crates/wonderblob-core/tests/sftp_hostkey.rs`:

```rust
use wonderblob_core::hostkey::HostKeyStore;
use wonderblob_core::sftp::{HostKeyDecision, SftpAuth, SftpBackend, SftpConfig, SftpConnectOutcome};

fn enabled() -> bool { std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1") }

#[tokio::test]
async fn first_connect_is_unverified_then_remember_then_known() {
    if !enabled() { eprintln!("skipped"); return; }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");

    // Phase 1: Verify against an empty store → Unverified with a fingerprint.
    let out = SftpBackend::connect(SftpConfig {
        host: "localhost".into(), port: 2222, username: "wb".into(),
        auth: SftpAuth::Password("wbpass".into()),
        host_key: HostKeyDecision::Verify(HostKeyStore::new(path.clone())),
    }).await.unwrap();
    let unv = match out { SftpConnectOutcome::HostKeyUnverified(u) => u, _ => panic!("expected unverified") };
    assert!(unv.fingerprint.starts_with("SHA256:"));
    assert!(!unv.changed);

    // Phase 2: Trust + remember → Connected, and the key lands in the file.
    let out = SftpBackend::connect(SftpConfig {
        host: "localhost".into(), port: 2222, username: "wb".into(),
        auth: SftpAuth::Password("wbpass".into()),
        host_key: HostKeyDecision::Trust {
            key_b64: unv.key_b64.clone(), remember: true,
            store: Some(HostKeyStore::new(path.clone())),
        },
    }).await.unwrap();
    assert!(matches!(out, SftpConnectOutcome::Connected(_)));
    assert!(std::fs::read_to_string(&path).unwrap().contains("localhost"));

    // Now a Verify connect is Known (no prompt).
    let out = SftpBackend::connect(SftpConfig {
        host: "localhost".into(), port: 2222, username: "wb".into(),
        auth: SftpAuth::Password("wbpass".into()),
        host_key: HostKeyDecision::Verify(HostKeyStore::new(path.clone())),
    }).await.unwrap();
    assert!(matches!(out, SftpConnectOutcome::Connected(_)));
}
```

- [ ] **Step 3: Migrate every other SFTP-connecting test/call to the new signature**

`SftpConfig` now requires `host_key`. Update the existing gated suites (`sftp_contract.rs`, `sftp_agent.rs`, `transfer_sftp.rs`, `edit_sftp.rs`, `scripts/test-sftp-auth.sh` callers) to pass `host_key: HostKeyDecision::Trust { key_b64: <captured>, remember: false, store: None }` — i.e. accept-once. Simplest pattern for those tests: a tiny helper that does a phase-1 `Verify` against a temp store to capture `key_b64`, then connects accept-once. Add that helper to a shared test module so each suite is a one-liner. This is what keeps CI green against the ephemeral fixture key (Decision 5).

- [ ] **Step 4: Run gated tests** (Docker up)

```bash
./scripts/test-sftp-up.sh
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_hostkey
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_contract
./scripts/test-sftp-down.sh
cargo test -p wonderblob-core   # ungated still green
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): SFTP host-key verification with two-phase TOFU connect"
```

---

### Task 3: Tauri connect commands — surface the decision, store path in app-config

Wire the two-phase connect into `connect_sftp` and `connect_bookmark`. The `known_hosts` path is `<app-config>/known_hosts` (only `src-tauri` knows the path; core is path-agnostic).

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Test: none new (the core test covers the logic; this is thin wiring)

- [ ] **Step 1: Add a host-key helper + a richer connect result**

In `commands.rs`:

```rust
use wonderblob_core::hostkey::HostKeyStore;
use wonderblob_core::sftp::{HostKeyDecision, SftpConnectOutcome};

fn known_hosts_store(app: &tauri::AppHandle) -> Result<HostKeyStore, StorageError> {
    let dir = app.path().app_config_dir().map_err(StorageError::other)?;
    Ok(HostKeyStore::new(dir.join("known_hosts")))
}

/// The frontend's host-key decision, mirrored from `api.ts`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyApproval {
    pub key_b64: String,
    /// accept-and-remember (true) vs accept-once (false).
    pub remember: bool,
}

/// connect_sftp now returns either a connection or a host-key decision-needed.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SftpConnectResponse {
    Connected { id: ConnectionId, capabilities: Capabilities },
    HostKeyUnverified { host: String, port: u16, fingerprint: String, key_b64: String, changed: bool },
}
```

Add an optional `host_key: Option<HostKeyApproval>` to `SftpConnectArgs`. Build the `HostKeyDecision`:
- `None` → `HostKeyDecision::Verify(known_hosts_store(app)?)` (TOFU phase 1)
- `Some(a)` → `HostKeyDecision::Trust { key_b64: a.key_b64, remember: a.remember, store: Some(known_hosts_store(app)?) }`

`connect_sftp` signature gains `app: tauri::AppHandle` (already used by other commands). On `SftpConnectOutcome::HostKeyUnverified(u)` return `SftpConnectResponse::HostKeyUnverified { … }`; on `Connected(b)` register and return `Connected { id, capabilities }`. Keep the existing `CONNECT_TIMEOUT` wrapper.

- [ ] **Step 2: Same two-phase for `connect_bookmark` (SFTP arm only)**

The SFTP arm of `connect_bookmark` returns `SftpConnectResponse` too (cloud arms always `Connected`). Add `host_key: Option<HostKeyApproval>` to the command. The frontend's bookmark-connect flow gains the same retry step — only for `Protocol::Sftp`. S3/Azure/OneDrive arms are unchanged (no host keys).

> Type note: `connect_bookmark` currently returns `ConnectResult`. To avoid breaking the cloud callers, return a unified `SftpConnectResponse`-style enum where cloud arms produce `Connected`. The frontend treats `Connected` exactly as today.

- [ ] **Step 3: Build + the app still launches**

Run: `cargo build --workspace` then `npm run tauri dev` (close window). Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(app): two-phase SFTP host-key approval in connect commands"
```

---

### Task 4: Frontend host-key approval dialog + retry flow

The new SFTP connect step: on `hostKeyUnverified`, show a dialog (fingerprint, host, accept-and-remember / accept-once / cancel); on a `changed` key show the scary MITM warning and disable "remember". Tokens only.

**Files:**
- Modify: `src/lib/api.ts`
- Create: `src/lib/components/HostKeyDialog.svelte`
- Modify: `src/routes/+page.svelte` + `src/lib/components/ConnectionSheet.svelte` (wherever SFTP connect is invoked)

- [ ] **Step 1: Types + API in `api.ts`**

```ts
export interface HostKeyApproval { keyB64: string; remember: boolean; }

export type SftpConnectResponse =
  | { kind: "connected"; id: number; capabilities: Capabilities }
  | { kind: "hostKeyUnverified"; host: string; port: number;
      fingerprint: string; keyB64: string; changed: boolean };

// connectSftp/connectBookmark now return SftpConnectResponse; add optional hostKey arg:
//   connectSftp: (args, hostKey?: HostKeyApproval) =>
//     invoke<SftpConnectResponse>("connect_sftp", { args: { ...args, hostKey: hostKey ?? null } }),
//   connectBookmark: (id, hostKey?) =>
//     invoke<SftpConnectResponse>("connect_bookmark", { id, hostKey: hostKey ?? null }),
```

Cloud `connect*` commands keep returning `ConnectResult`; only SFTP-capable paths use `SftpConnectResponse`. The caller narrows on `kind`.

- [ ] **Step 2: `HostKeyDialog.svelte`**

Props: `{ host, port, fingerprint, changed }`, callbacks `onaccept(remember: boolean)` / `oncancel()`. Layout (compact, token-driven):
- First-seen (`changed === false`): title "Unknown host key", body "The server **{host}:{port}** presented a host key Wonderblob hasn't seen before.", a monospace `fingerprint` row (use `.selectable`), and three buttons: **Connect & Remember** (primary, `--accent`), **Connect Once**, **Cancel**.
- Changed (`changed === true`): a warning style (red accent token; add `--danger` if not present), title "⚠ HOST KEY CHANGED", body matching OpenSSH's warning ("This could mean someone is eavesdropping (man-in-the-middle attack)…"), **only** **Cancel** + **Connect Once** (no Remember — never silently overwrite a changed key in v1). Escape = cancel.

- [ ] **Step 3: Retry loop at the connect call site**

Wrap the SFTP connect so an `hostKeyUnverified` response opens the dialog and, on accept, re-invokes with `{ keyB64, remember }`; on a second `connected` proceed as before; on cancel, abort quietly. Pseudo:

```ts
async function connectSftpWithHostKey(args): Promise<ConnectResult | null> {
  let res = await api.connectSftp(args);
  if (res.kind === "hostKeyUnverified") {
    const decision = await showHostKeyDialog(res); // resolves {remember} or null
    if (!decision) return null;
    res = await api.connectSftp(args, { keyB64: res.keyB64, remember: decision.remember });
  }
  if (res.kind !== "connected") return null;
  return { id: res.id, capabilities: res.capabilities };
}
```

Apply the same wrapper to the bookmark-connect path (SFTP bookmarks only; cloud bookmarks skip the dialog).

- [ ] **Step 4: Manual verify**

```bash
./scripts/test-sftp-up.sh && npm run tauri dev
```
Connect to `localhost:2222` (wb/wbpass): first connect shows the unknown-host dialog with a `SHA256:` fingerprint → Remember → browses. Reconnect: no dialog (Known). To test changed-key: stop the container, `./scripts/test-sftp-up.sh` again (new host key), reconnect → MITM warning, no Remember offered.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(ui): SSH host-key approval dialog + MITM-changed warning"
```

---

### Task 5: Drag-in — OS files dropped onto the FileList enqueue uploads

`onDragDropEvent` gives filesystem paths; hand each to the tested `enqueue_upload`. Directories → enqueue immediate file children. Drop-target highlight while hovering.

**Files:**
- Create: `src-tauri/src/dropfiles.rs` (expand a dropped dir to its immediate file children — testable)
- Modify: `src-tauri/src/{commands.rs,lib.rs}` (an `enqueue_dropped` command)
- Modify: `src/routes/+page.svelte` (subscribe to `onDragDropEvent`, highlight, dispatch)
- Modify: `src/lib/api.ts`

- [ ] **Step 1: Failing test for the path-expansion helper**

`src-tauri/src/dropfiles.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_dir_to_immediate_files_only() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), b"a").unwrap();
        std::fs::write(d.path().join("b.txt"), b"b").unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/c.txt"), b"c").unwrap(); // one level deeper: skipped
        let mut got = expand_dropped(&[d.path().to_string_lossy().into()]);
        got.sort();
        assert_eq!(got.len(), 2); // a.txt + b.txt, NOT sub/c.txt
    }
    #[test]
    fn passes_plain_files_through() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.bin");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(expand_dropped(&[f.to_string_lossy().into()]), vec![f.to_string_lossy().to_string()]);
    }
}
```

- [ ] **Step 2: Implement `expand_dropped`**

```rust
//! Map dropped OS paths to a flat list of file paths to upload.
//! v1: top-level files + the immediate file children of dropped directories
//! (one level). Recursive trees are a tracked post-v1 enhancement.

pub fn expand_dropped(paths: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in paths {
        let path = std::path::Path::new(p);
        if path.is_dir() {
            if let Ok(rd) = std::fs::read_dir(path) {
                for e in rd.flatten() {
                    if e.path().is_file() {
                        out.push(e.path().to_string_lossy().into_owned());
                    }
                }
            }
        } else if path.is_file() {
            out.push(p.clone());
        }
    }
    out
}
```

- [ ] **Step 3: `enqueue_dropped` command**

Enqueues uploads for every expanded path into `dest_dir` on connection `id`, reusing the existing `enqueue_upload` body (basename + `joinPath`). Returns the new transfer ids.

```rust
#[tauri::command]
pub async fn enqueue_dropped(
    engine: State<'_, Arc<TransferEngine>>,
    id: ConnectionId,
    dest_dir: String,
    paths: Vec<String>,
) -> Result<Vec<TransferId>, StorageError> {
    let mut ids = Vec::new();
    for local in crate::dropfiles::expand_dropped(&paths) {
        let remote = format!("{}/{}", dest_dir.trim_end_matches('/'), basename_of(&local));
        let total = tokio::fs::metadata(&local).await.ok().map(|m| m.len());
        ids.push(engine.enqueue(NewTransfer {
            connection_id: id, direction: Direction::Up,
            name: basename_of(&local), remote_path: remote, local_path: local, total_bytes: total,
        }).await?);
    }
    Ok(ids)
}
```

Add `mod dropfiles;` + register `commands::enqueue_dropped` in `lib.rs`. Add to `api.ts`:
`enqueueDropped: (id, destDir, paths: string[]) => invoke<number[]>("enqueue_dropped", { id, destDir, paths })`.

- [ ] **Step 4: Wire `onDragDropEvent` in `+page.svelte`**

`dragDropEnabled` defaults to `true` (per Tauri docs / issue #14373) — no config change needed; DOM `ondrop` will NOT fire, so we must use the native event. In `$effect` (mount):

```ts
import { getCurrentWebview } from "@tauri-apps/api/webview";
let dragOver = $state(false);
$effect(() => {
  const un = getCurrentWebview().onDragDropEvent(async (e) => {
    if (e.payload.type === "over") { dragOver = true; return; }
    if (e.payload.type === "leave") { dragOver = false; return; }
    if (e.payload.type === "drop") {
      dragOver = false;
      const conn = $activeConnection;
      if (!conn) return;
      try { await api.enqueueDropped(conn.id, $currentPath, e.payload.paths); }
      catch (err) { showToast(opError(err, "Couldn't upload dropped files")); }
    }
  });
  return () => { un.then((f) => f()); };
});
```

Bind a `.drop-target` class on the `.browser` region when `dragOver` (a token-driven inset accent ring). Refresh the listing on drop completion (reuse the existing transfer-complete refresh that already watches the active dir).

- [ ] **Step 5: Run tests + manual**

`cargo test -p wonderblob` (dropfiles unit). Manual: `npm run tauri dev` against the SFTP fixture, drag a file and a folder from the OS file manager onto the list → uploads appear in Transfers, files land in the current dir; the highlight shows while hovering.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: drag-in uploads via onDragDropEvent (files + one-level dirs)"
```

---

### Task 6: Drag-out fallback — "Download to ~/Downloads"

Honest v1 per Decision 4: no deferred OS drag-out; instead a one-click download to `~/Downloads` (no save dialog), complementing the existing Download button. macOS promise-files deferred.

**Files:**
- Modify: `src-tauri/src/commands.rs` (resolve `~/Downloads` and enqueue)
- Modify: `src/lib/api.ts`, `src/routes/+page.svelte` (a "Download to Downloads" action / context-menu item)

- [ ] **Step 1: `enqueue_download_to_downloads` command**

```rust
#[tauri::command]
pub async fn enqueue_download_to_downloads(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    engine: State<'_, Arc<TransferEngine>>,
    id: ConnectionId,
    remote_path: String,
    total_bytes: Option<u64>,
) -> Result<TransferId, StorageError> {
    let downloads = app.path().download_dir().map_err(StorageError::other)?;
    let local = downloads.join(basename_of(&remote_path));
    // reuse enqueue_download's stat-fallback for total
    // ...
}
```

Uses Tauri's `PathResolver::download_dir()` (cross-platform `~/Downloads`). Register in `lib.rs`; add `enqueueDownloadToDownloads` to `api.ts`.

- [ ] **Step 2: Frontend action**

Add a "Download to Downloads" item (toolbar overflow or the row context menu next to the existing Download) that calls the new command for the selected entry. Toast on success ("Downloading to ~/Downloads").

- [ ] **Step 3: README honesty paragraph** — drafted here, written in Task 9: drag-out is download-to-Downloads on all platforms; deferred OS drag-out (incl. macOS promise-files) is post-v1.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat: drag-out fallback — one-click download to ~/Downloads"
```

---

### Task 7: Bundle config — per-OS targets + metadata

**Files:**
- Modify: `src-tauri/tauri.conf.json`

- [ ] **Step 1: Expand the `bundle` block**

`targets: "all"` already produces per-OS installers, but pin them explicitly and add metadata:

```json
"bundle": {
  "active": true,
  "targets": ["deb", "rpm", "appimage", "app", "dmg", "msi", "nsis"],
  "category": "Utility",
  "copyright": "Copyright © 2026 Wonderblob contributors",
  "shortDescription": "Cross-platform remote file browser",
  "longDescription": "Wonderblob is an open-source remote file browser for S3, Azure Blob, SFTP, and OneDrive — Cyberduck for Linux, macOS, and Windows.",
  "licenseFile": "../LICENSE",
  "icon": [
    "icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png",
    "icons/icon.icns", "icons/icon.ico"
  ],
  "linux": { "deb": { "depends": ["libwebkit2gtk-4.1-0", "libgtk-3-0"] } }
}
```

(Tauri ignores targets not buildable on the current OS, so `targets` listing all is fine; the matrix builds each on its native runner.) Confirm `icons/` already has all five (they're referenced today). Add a `LICENSE` file at repo root if absent (MIT, matching "open-source" in the spec) — ask Jack to confirm the license if unsure.

- [ ] **Step 2: Bump version**

Set `"version": "0.6.0"` in `tauri.conf.json` (and keep `package.json` in sync — Tauri reads `tauri.conf.json` version for `__VERSION__`). 0.x signals pre-1.0; this is the sixth plan's release.

- [ ] **Step 3: Local bundle smoke**

```bash
npm run tauri build
ls src-tauri/target/release/bundle/   # appimage/ deb/ rpm/ on this Linux box
```
Expected: an AppImage + .deb + .rpm produced. (macOS/Windows artifacts come from CI runners.)

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "build: per-OS bundle targets + metadata, version 0.6.0"
```

---

### Task 8: Release workflow — `tauri-action` on version tags

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Write the workflow** (matrix + tauri-action, per the [Tauri GitHub guide](https://v2.tauri.app/distribute/pipelines/github/))

```yaml
name: Release
on:
  push:
    tags: ["v*"]

jobs:
  release:
    permissions:
      contents: write
    strategy:
      fail-fast: false
      matrix:
        include:
          - platform: macos-latest   # Apple Silicon
            args: "--target aarch64-apple-darwin"
          - platform: macos-latest   # Intel
            args: "--target x86_64-apple-darwin"
          - platform: ubuntu-22.04
            args: ""
          - platform: windows-latest
            args: ""
    runs-on: ${{ matrix.platform }}
    steps:
      - uses: actions/checkout@v4

      - name: Linux deps (Tauri + keyring)
        if: matrix.platform == 'ubuntu-22.04'
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            libwebkit2gtk-4.1-dev libgtk-3-dev \
            libayatana-appindicator3-dev librsvg2-dev libssl-dev \
            libdbus-1-dev patchelf

      - uses: actions/setup-node@v4
        with: { node-version: 22, cache: npm }

      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.platform == 'macos-latest' && 'aarch64-apple-darwin,x86_64-apple-darwin' || '' }}

      - uses: Swatinem/rust-cache@v2

      - run: npm ci

      - uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        with:
          tagName: ${{ github.ref_name }}
          releaseName: "Wonderblob ${{ github.ref_name }}"
          releaseBody: "Unsigned builds — see the README for Gatekeeper / SmartScreen notes. Download the asset for your platform."
          releaseDraft: true
          prerelease: true
          args: ${{ matrix.args }}
```

Notes baked in:
- Linux deps mirror `ci.yml` (`libwebkit2gtk-4.1-dev` etc) + `patchelf` (AppImage needs it).
- `prerelease: true` + `releaseDraft: true` for 0.x — Jack reviews before publishing.
- No signing env vars (`APPLE_CERTIFICATE`, `TAURI_SIGNING_PRIVATE_KEY`, etc.) — Decision: code-signing out of scope (Task 9 documents the implications).

- [ ] **Step 2: Validate the YAML + a dry tag (optional, gated on Jack)**

`actionlint .github/workflows/release.yml` if available. A real release run requires pushing a `v0.6.0` tag — **ask Jack** before tagging (it creates a public draft release and consumes runner minutes; macOS/Windows runners are needed).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "ci: tauri-action release workflow on v* tags (unsigned, draft)"
```

---

### Task 9: README install instructions + unsigned-build caveats + changelog

**Files:**
- Modify (or create): `README.md`
- Create: `CHANGELOG.md` (light)

- [ ] **Step 1: README — Install section**

Add:
- **Install**: download the platform asset from [Releases]; Linux → AppImage (`chmod +x`, run) / `.deb` / `.rpm`; macOS → `.dmg`; Windows → `.msi` or NSIS `-setup.exe`.
- **Unsigned-build caveat (honest):**
  - macOS: builds are **not notarized/signed**. Gatekeeper blocks first launch — right-click → Open, or `xattr -dr com.apple.quarantine /Applications/wonderblob.app`. Signing/notarization is post-v1 (needs an Apple Developer ID — **Jack**).
  - Windows: **unsigned**; SmartScreen shows "Windows protected your PC" → More info → Run anyway. An EV/OV cert is post-v1 (**Jack**).
  - Linux: unsigned AppImage/deb/rpm is normal; no OS gate.
- **Drag & drop**: drag files/folders from your file manager **into** the window to upload (top-level files + one level of folder contents in v1). Drag-**out** is a one-click "Download to ~/Downloads"; deferred OS drag-out (incl. macOS file-promise drags) is post-v1.
- **Host keys**: on first SFTP connect Wonderblob shows the server's `SHA256:` fingerprint for approval (trust-on-first-use) and hard-stops if a known host's key changes (MITM protection), storing trusted keys in OpenSSH `known_hosts` format under the app config dir.

- [ ] **Step 2: `CHANGELOG.md`** — a `## 0.6.0` entry summarizing Plans 1–6 highlights (or just the Plan 6 deltas if a changelog already exists). Keep it short.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "docs: install instructions, unsigned-build caveats, changelog 0.6.0"
```

---

### Task 10: Full verification pass + CI green

**Files:** none (verification only)

- [ ] **Step 1: Local full suite**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm test && npm run check
./scripts/test-sftp-up.sh
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_hostkey
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_contract
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test transfer_sftp
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test edit_sftp
./scripts/test-sftp-down.sh
```
Expected: all green; the gated SFTP suites pass via accept-once against the ephemeral fixture key (Decision 5).

- [ ] **Step 2: Manual acceptance**
1. SFTP first-connect → host-key dialog (`SHA256:` fp) → Remember → browse; reconnect = no prompt.
2. Restart container (new key) → reconnect → MITM warning, no Remember.
3. Drag a file + a folder from the OS into the list → uploads enqueue, land in current dir, highlight shows on hover.
4. Select a remote file → "Download to ~/Downloads" → lands in `~/Downloads`.
5. `npm run tauri build` → AppImage/deb/rpm exist.

- [ ] **Step 3: Push, watch CI**

```bash
git push
gh run watch
```
Expected: existing `ci.yml` green (release workflow only runs on tags). Fix any clippy/fmt drift.

---

## Done criteria (Plan 6)

- `cargo test --workspace` + `npm test` + `npm run check` green locally and in CI; gated SFTP suites green via accept-once.
- SFTP **accept-any host key is gone**: unknown keys prompt (TOFU), trusted keys persist in OpenSSH `known_hosts` under app-config, changed keys hard-stop with a MITM warning. Bookmarks/`connect_bookmark`/the connection sheet still work, with the host-key dialog as a new step on the SFTP path only.
- Drag-in uploads work on all platforms via `onDragDropEvent` (files + one-level dirs), with a drop highlight.
- Drag-out fallback ("Download to ~/Downloads") works on all platforms.
- `npm run tauri build` produces AppImage/deb/rpm locally; `release.yml` is in place to produce .dmg (×2 arch) / .msi+NSIS / Linux installers on a `v*` tag, drafted as a prerelease.
- README documents install + the unsigned-build (Gatekeeper/SmartScreen) caveats + drag/host-key behavior honestly.

## Explicitly deferred (tracked post-v1)

- **Code-signing & notarization** (macOS Developer ID, Windows EV/OV cert) — builds ship unsigned; needs certs/secrets from **Jack**.
- **Auto-update** (Tauri updater + `latest.json` + signing key) — not wired.
- **Recursive folder drag-in** — v1 does top-level files + one level of dir contents only; deep trees + remote `mkdir` mirroring is deferred.
- **macOS promise-file drag-out** and true deferred OS drag-out on any platform — fallback is download-to-Downloads only.
- **OpenSSH hashed-host (`HashKnownHosts`) entries** — the store writes/reads plaintext host lines; reading hashed entries written by other tools is out of scope.
- **Importing/sharing the user's `~/.ssh/known_hosts`** — Wonderblob keeps its own app-config store; it doesn't read or write the system file.

## Self-review (writing-plans checklist)

- **Spec coverage:** drag-in everywhere ✓, drag-out fallback ✓ (§ Drag & drop); host-key verification as the pre-release requirement ✓ (§ Auth, `sftp.rs:32` note); packaging/distribution for the v1 release ✓.
- **No placeholders:** every task has concrete files, code, and run/expected lines. (Test pubkey blobs are intentionally `REPLACE_…` — the implementer generates real ones with `ssh-keygen`; flagged inline.)
- **Real-symbol consistency:** reuses `StorageBackend`, `Capabilities`, `AppState`/`ConnectionId`, `TransferEngine::enqueue`/`NewTransfer`/`Direction::Up`, `basename_of`, `connect_sftp`/`connect_bookmark`/`SftpConfig`/`SftpAuth`, the `Handler::check_server_key(&russh::keys::key::PublicKey)` signature from the current `sftp.rs`, `app.path().app_config_dir()`/`download_dir()`, `keychain()` helper, `api.ts`'s `ConnectResult`/`Capabilities`, the `transfer://progress` event convention, and the `default.json` capability set. New symbols (`HostKeyStore`, `HostKeyStatus`, `HostKeyDecision`, `SftpConnectOutcome`, `SftpConnectResponse`, `enqueue_dropped`, `expand_dropped`) are named to match existing conventions.
- **Cited research:** Tauri `onDragDropEvent`/`dragDropEnabled` (webview API + issue #14373), `tauri-action`/GitHub release guide, and `russh-keys` 0.46's `known_hosts` API (verified in the local crate source) all drive concrete decisions above.
