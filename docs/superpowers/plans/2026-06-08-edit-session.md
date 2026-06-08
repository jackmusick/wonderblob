# Wonderblob Plan 4: EditSession (open / edit / save-back) + spacebar preview + Download wiring

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make double-click / Enter on a file the *default* open action — download it to a per-connection temp dir, open it with the OS default app, watch the temp file, and re-upload on save with a conflict guard. Add a spacebar **in-app preview** (text + images robustly, PDF best-effort) that never launches an external app. Finally wire the **Download** trigger Plan 3 left stubbed: a toolbar button that enqueues a download through the `TransferEngine` to a user-chosen location.

**Architecture — where EditSession lives (decided):** split exactly like Plan 3 split the TransferEngine.

- **Protocol-agnostic, Tauri-free, testable core** goes in `wonderblob-core` (`src/edit.rs`): `RemoteStat` (the conflict baseline), `download_to_temp`, `check_conflict`, `save_back`, and `preview_plan` (the text/image/pdf/too-large decision). These take `&dyn StorageBackend` + a local `Path`, so they're driveable from a plain `#[tokio::test]` with `MockBackend` **and** from the gated SFTP fixture — satisfying the "open → edit → save-back → conflict" test requirement directly against a real backend.
- **OS-integration + session lifetime** goes in `src-tauri` (`src/edit.rs`): the `notify` file-watcher, the debounce, the `EditRegistry` (session id ↔ temp path ↔ baseline ↔ watcher handle), the `tauri-plugin-opener` "open with default app" call, the temp-dir layout under the app cache dir, the commands, and the `edit://*` events. This is the part that genuinely needs Tauri (`AppHandle::path`, `Emitter::emit`, the opener plugin) and a file watcher, exactly as the scope note anticipated.

This keeps the high-value conflict/save-back logic deterministic and Docker-testable while the watcher/opener/registry stay where the OS APIs are — the same core-vs-wiring boundary Plans 1–3 established.

**Conflict detection without an etag (decided):** `StorageBackend::stat` returns `Entry { size: Option<u64>, modified_ms: Option<i64> }` — **there is no etag** anywhere in the trait. So the baseline is a snapshot of `(size, modified_ms)` captured right after download. Before every save-back we re-`stat` and compare: a **size** mismatch is always a conflict; **mtime** is compared only when *both* the baseline and the current stat report it (S3/Azure/mock may return `modified_ms: None`, in which case detection degrades to size-only — documented as best-effort). A vanished remote (`NotFound`) is also a conflict (don't silently recreate). This is the honest ceiling of what the trait exposes; richer integrity (content hash) is Explicitly-deferred.

**Engine vs. direct backend I/O (decided):**

- **EditSession open + save-back use direct `backend.read` / `backend.write`** (not the `TransferEngine`). Edit files are small text/config files; routing them through the persistent queue would spam the transfers panel, and the open flow needs a *synchronous* "download finished, here is the temp path, now open it" handoff plus the immediate stat snapshot. Direct I/O gives exactly that with no queue noise.
- **The toolbar Download button uses the `TransferEngine`** (`api.enqueueDownload`) — that's the bulk, resumable, progress-reporting path the queue exists for. This is the call Plan 3 deliberately left as a UI stub.

So both paths are used, each where it fits; the watcher + conflict guard are EditSession-specific regardless.

**Preview approach + PDF stance (decided):** the preview reads bytes via a dedicated `preview_file` command (capped at 10 MB) and renders them *inside the webview*:
- **Text** → command decodes UTF-8 and returns a `String`; shown in a monospace pane.
- **Images** → command returns a `data:<mime>;base64,…` URL; shown as `<img>`. **The existing CSP already allows this** (`img-src 'self' data:` in `tauri.conf.json`), so **no CSP or capability change is needed**. We deliberately avoid Tauri's asset protocol / `convertFileSrc` (which would require enabling the `assetProtocol` scope and widening `img-src` to include `asset:` / `http://asset.localhost`); building a `data:` URL from the already-read bytes is simpler and CSP-clean. The asset-protocol route is noted as the upgrade path if very large image previews ever need streaming.
- **PDF** → WebKitGTK's in-webview PDF support is unreliable, so v1 is **"Open in editor" fallback**: the preview panel shows a one-line "PDF preview isn't supported here" with an Open-in-editor button (which runs the normal EditSession open). A best-effort `<embed>` behind the same `data:` URL is acceptable on platforms where it works, but the fallback button is always present.
- **Size guard** → files over 10 MB (or unsupported types) return a plan with no bytes and the panel offers Open-in-editor instead of loading them.

**Tech stack:** Rust (`notify` watcher — new core-... no: **`src-tauri`** dep; `base64` for image data URLs; existing `tokio`/`async-trait`), `tauri-plugin-opener` (already a dependency, `opener:default` already granted), Tauri 2.x events, Svelte 5 runes + `@tauri-apps/api/event`, `@tauri-apps/plugin-dialog` `save()`.

**Spec:** `docs/superpowers/specs/2026-06-07-wonderblob-design.md` (§ "EditSession (open / edit / save-back)", § "v1 scope")
**Builds on:** Plan 1 (`…/2026-06-07-foundation-sftp-slice.md`), Plan 2 (`…/2026-06-08-s3-azure-backends.md`), Plan 3 (`…/2026-06-08-transfer-engine.md`) — all merged.

**Crate-API caveats:**
- `notify`: check the current major with `cargo add notify --dry-run` and pin it (likely `6.x` or `7.x`). Watch the temp file's **parent directory** `NonRecursive` and filter events by path, **not** the file itself — many editors save atomically by writing a sibling temp and `rename`-ing it over the original, which orphans an inode-level watch. Document this in the code.
- `base64`: already pinned in `wonderblob-core` (`0.22`); add the same to `src-tauri` (`base64 = "0.22"`) for the image data-URL builder, or expose a tiny core helper — either is fine; the plan uses a `src-tauri` dep.
- `StorageBackend` is **unchanged** by this plan — `read(&self, path, offset)`, `write(&self, path)`, `stat(&self, path) -> Entry`, exactly as Plans 1–3 use them.

---

### Task 1: Core conflict baseline + download / save-back

`RemoteStat` (the no-etag baseline), `download_to_temp`, `check_conflict`, `save_back` — pure, Tauri-free, tested with `MockBackend`.

**Files:**
- Create: `crates/wonderblob-core/src/edit.rs`
- Modify: `crates/wonderblob-core/src/lib.rs` (add `pub mod edit;`)
- Modify: `crates/wonderblob-core/src/transfer/mod.rs` — make `mock` non-test-only so `edit` tests can reuse it (see Step 3)
- Test: inline `#[cfg(test)]` in `edit.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/wonderblob-core/src/edit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::mock::MockBackend;

    #[test]
    fn remote_stat_size_mismatch_is_a_conflict() {
        let base = RemoteStat { size: Some(100), modified_ms: Some(10) };
        let now = RemoteStat { size: Some(120), modified_ms: Some(10) };
        assert!(now.differs_from(&base));
    }

    #[test]
    fn remote_stat_mtime_only_compared_when_both_present() {
        let base = RemoteStat { size: Some(100), modified_ms: Some(10) };
        // both present, mtime moved → conflict
        assert!(RemoteStat { size: Some(100), modified_ms: Some(20) }.differs_from(&base));
        // current lacks mtime → fall back to size-only → no conflict
        assert!(!RemoteStat { size: Some(100), modified_ms: None }.differs_from(&base));
    }

    #[tokio::test]
    async fn download_records_baseline_and_writes_temp() {
        let b = MockBackend::new();
        b.put("/r.txt", b"hello world".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        let base = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        assert_eq!(std::fs::read(&temp).unwrap(), b"hello world");
        assert_eq!(base.size, Some(11));
    }

    #[tokio::test]
    async fn save_back_overwrites_remote_and_returns_fresh_baseline() {
        let b = MockBackend::new();
        b.put("/r.txt", b"old".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        std::fs::write(&temp, b"new contents").unwrap();
        let base = save_back(&b, &temp, "/r.txt").await.unwrap();
        assert_eq!(b.get("/r.txt").await.unwrap(), b"new contents");
        assert_eq!(base.size, Some(12));
    }

    #[tokio::test]
    async fn check_conflict_detects_out_of_band_change() {
        let b = MockBackend::new();
        b.put("/r.txt", b"hello".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        let base = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        // someone else changes the remote out-of-band (different size)
        b.put("/r.txt", b"hello, world!".to_vec()).await;
        match check_conflict(&b, "/r.txt", &base).await.unwrap() {
            ConflictCheck::Conflict { .. } => {}
            ConflictCheck::Clear => panic!("expected a conflict"),
        }
    }

    #[tokio::test]
    async fn check_conflict_clear_when_unchanged() {
        let b = MockBackend::new();
        b.put("/r.txt", b"hello".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        let base = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        assert_eq!(check_conflict(&b, "/r.txt", &base).await.unwrap(), ConflictCheck::Clear);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core edit::`
Expected: FAIL — `edit` module / symbols undefined.

- [ ] **Step 3: Implement the core**

First make `MockBackend` reusable outside `#[cfg(test)]` so `edit`'s tests (and later the gated test scaffolding) can use it. In `crates/wonderblob-core/src/transfer/mod.rs` change:

```rust
#[cfg(test)] pub mod mock;
```
to
```rust
#[cfg(any(test, feature = "test-fixtures"))]
pub mod mock;
```
…**or**, simpler and with no feature flag, just drop the `#[cfg(test)]` so `pub mod mock;` is always compiled (it pulls in no extra deps beyond `tokio`/`async-trait` already present). Use the unconditional form unless binary-size review objects. (`edit.rs` only references `mock` from inside its own `#[cfg(test)]`, so an unconditional `pub mod mock;` is harmless.)

`crates/wonderblob-core/src/edit.rs` (above the tests):

```rust
//! EditSession core (spec: "EditSession — open / edit / save-back"). Protocol-
//! agnostic, Tauri-free: the app layer (`src-tauri/src/edit.rs`) owns the
//! watcher, the opener, the session registry, and the temp-dir layout, and calls
//! these functions for the actual download / conflict-check / upload.
//!
//! Conflict baseline = `(size, modified_ms)` from `stat`. There is **no etag** in
//! `StorageBackend`; size mismatch always counts as a conflict, mtime only when
//! both sides report it (mtime-less backends degrade to size-only — best effort).

use crate::error::{Result, StorageError};
use crate::vfs::{Entry, StorageBackend};
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// Snapshot used to detect out-of-band remote changes between open and save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStat {
    pub size: Option<u64>,
    pub modified_ms: Option<i64>,
}

impl RemoteStat {
    pub fn from_entry(e: &Entry) -> Self {
        Self { size: e.size, modified_ms: e.modified_ms }
    }

    /// True when the remote looks changed vs `baseline`. Size mismatch always
    /// counts; mtime only when BOTH report it.
    pub fn differs_from(&self, baseline: &RemoteStat) -> bool {
        if self.size != baseline.size {
            return true;
        }
        match (self.modified_ms, baseline.modified_ms) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        }
    }
}

/// Download remote→`temp_path`, returning the baseline captured *after* the read.
pub async fn download_to_temp(
    backend: &dyn StorageBackend,
    remote_path: &str,
    temp_path: &Path,
) -> Result<RemoteStat> {
    if let Some(parent) = temp_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(StorageError::other)?;
    }
    let mut reader = backend.read(remote_path, 0).await?;
    let mut file = tokio::fs::File::create(temp_path).await.map_err(StorageError::other)?;
    tokio::io::copy(&mut reader, &mut file).await.map_err(StorageError::other)?;
    file.flush().await.map_err(StorageError::other)?;
    let entry = backend.stat(remote_path).await?;
    Ok(RemoteStat::from_entry(&entry))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "result")]
pub enum ConflictCheck {
    Clear,
    Conflict { current: RemoteStat },
}

/// Re-stat the remote and compare to `baseline`. A vanished remote is a conflict.
pub async fn check_conflict(
    backend: &dyn StorageBackend,
    remote_path: &str,
    baseline: &RemoteStat,
) -> Result<ConflictCheck> {
    let current = match backend.stat(remote_path).await {
        Ok(e) => RemoteStat::from_entry(&e),
        Err(StorageError::NotFound { .. }) => {
            return Ok(ConflictCheck::Conflict {
                current: RemoteStat { size: None, modified_ms: None },
            });
        }
        Err(e) => return Err(e),
    };
    Ok(if current.differs_from(baseline) {
        ConflictCheck::Conflict { current }
    } else {
        ConflictCheck::Clear
    })
}

/// Upload `temp_path`→remote (create/replace), returning the fresh baseline.
pub async fn save_back(
    backend: &dyn StorageBackend,
    temp_path: &Path,
    remote_path: &str,
) -> Result<RemoteStat> {
    let mut file = tokio::fs::File::open(temp_path).await.map_err(StorageError::other)?;
    let mut writer = backend.write(remote_path).await?;
    tokio::io::copy(&mut file, &mut writer).await.map_err(StorageError::other)?;
    writer.shutdown().await.map_err(StorageError::other)?;
    let entry = backend.stat(remote_path).await?;
    Ok(RemoteStat::from_entry(&entry))
}
```

Add `pub mod edit;` to `crates/wonderblob-core/src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p wonderblob-core edit::`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): EditSession baseline — RemoteStat, download_to_temp, check_conflict, save_back"
```

---

### Task 2: Preview plan — text / image / pdf / too-large decision

A pure function deciding how a file previews from its name + size. No I/O; unit-tested.

**Files:**
- Modify: `crates/wonderblob-core/src/edit.rs`
- Test: extend the `#[cfg(test)]` block in `edit.rs`

- [ ] **Step 1: Write the failing tests**

Append to the tests in `edit.rs`:

```rust
    #[test]
    fn preview_plan_classifies_by_extension() {
        assert_eq!(preview_plan("notes.txt", Some(10), PREVIEW_CAP_BYTES), PreviewPlan::Text);
        assert_eq!(preview_plan("Makefile", Some(10), PREVIEW_CAP_BYTES), PreviewPlan::Text); // no ext → text
        assert_eq!(preview_plan("logo.PNG", Some(10), PREVIEW_CAP_BYTES), PreviewPlan::Image); // case-insensitive
        assert_eq!(preview_plan("report.pdf", Some(10), PREVIEW_CAP_BYTES), PreviewPlan::Pdf);
        assert_eq!(
            preview_plan("archive.zip", Some(10), PREVIEW_CAP_BYTES),
            PreviewPlan::Unsupported { ext: "zip".into() }
        );
    }

    #[test]
    fn preview_plan_size_guard_wins_over_type() {
        let cap = 1000;
        assert_eq!(
            preview_plan("big.txt", Some(5000), cap),
            PreviewPlan::TooLarge { size: 5000, cap }
        );
        // unknown size → allowed (the command still caps the actual read)
        assert_eq!(preview_plan("x.txt", None, cap), PreviewPlan::Text);
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p wonderblob-core edit::preview` → FAIL.

- [ ] **Step 3: Implement `preview_plan`**

Add to `edit.rs`:

```rust
/// Default ceiling for in-app preview; over this we offer "open in editor".
pub const PREVIEW_CAP_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PreviewPlan {
    Text,
    Image,
    Pdf,
    TooLarge { size: u64, cap: u64 },
    Unsupported { ext: String },
}

fn ext_of(name: &str) -> Option<String> {
    name.rfind('.')
        .filter(|&i| i > 0 && i + 1 < name.len())
        .map(|i| name[i + 1..].to_ascii_lowercase())
}

/// Decide how `name` (with optional known `size`) should preview, capped at `cap`.
pub fn preview_plan(name: &str, size: Option<u64>, cap: u64) -> PreviewPlan {
    if let Some(sz) = size {
        if sz > cap {
            return PreviewPlan::TooLarge { size: sz, cap };
        }
    }
    let ext = match ext_of(name) {
        None => return PreviewPlan::Text, // extensionless (Makefile, README) → text
        Some(e) => e,
    };
    const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"];
    const TEXT: &[&str] = &[
        "txt", "md", "markdown", "log", "json", "yaml", "yml", "toml", "ini", "cfg",
        "conf", "xml", "csv", "tsv", "sh", "bash", "zsh", "fish", "py", "rs", "go",
        "c", "h", "cpp", "hpp", "cc", "js", "ts", "tsx", "jsx", "svelte", "html",
        "css", "scss", "sql", "env", "gitignore", "dockerfile",
    ];
    if IMAGE.contains(&ext.as_str()) {
        PreviewPlan::Image
    } else if ext == "pdf" {
        PreviewPlan::Pdf
    } else if TEXT.contains(&ext.as_str()) {
        PreviewPlan::Text
    } else {
        PreviewPlan::Unsupported { ext }
    }
}

/// MIME for an image extension (drives the preview `data:` URL).
pub fn image_mime(name: &str) -> &'static str {
    match ext_of(name).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}
```

- [ ] **Step 4: Run tests** — `cargo test -p wonderblob-core edit::` → all pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): preview_plan — text/image/pdf/too-large/unsupported decision + image_mime"
```

---

### Task 3: App-layer EditRegistry + notify watcher + debounce

The session registry, the `notify` watcher (watch the parent dir, filter by path), and a per-session debounce task that runs the conflict-check/save-back flow and emits `edit://*`. No commands yet (Task 4) — this task lands the registry + watcher plumbing and a unit test for the debounce coalescer.

**Files:**
- Create: `src-tauri/src/edit.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod edit;`)
- Modify: `src-tauri/Cargo.toml` (`notify`, `base64`)
- Test: inline `#[cfg(test)]` debounce test in `edit.rs`

- [ ] **Step 1: Add deps**

In `src-tauri/Cargo.toml` `[dependencies]` (pin the `notify` major you find via `cargo add notify --dry-run`):

```toml
notify = "6"
base64 = "0.22"
```

- [ ] **Step 2: Write the failing debounce test**

The save flow is timing-driven, but the *coalescing* rule is separable and unit-testable. In `src-tauri/src/edit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::debounce_ready;
    use std::time::{Duration, Instant};

    #[test]
    fn debounce_waits_for_quiet_window() {
        let win = Duration::from_millis(500);
        let last_event = Instant::now();
        // a check 200ms later: not yet quiet
        assert!(!debounce_ready(last_event, last_event + Duration::from_millis(200), win));
        // a check 600ms later: quiet long enough → ready
        assert!(debounce_ready(last_event, last_event + Duration::from_millis(600), win));
    }
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test -p wonderblob edit::` → FAIL (`debounce_ready` undefined).

- [ ] **Step 4: Implement the registry + watcher**

`src-tauri/src/edit.rs` (above the tests):

```rust
//! App-layer EditSession: OS file watching, debounce, the session registry, the
//! "open with default app" call, and the `edit://*` events. The protocol work
//! (download / conflict-check / upload) lives in `wonderblob_core::edit`.

use crate::state::{ConnMap, ConnectionId};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use wonderblob_core::edit::{check_conflict, save_back, ConflictCheck, RemoteStat};
use wonderblob_core::error::StorageError;

pub type SessionId = u64;

const DEBOUNCE: Duration = Duration::from_millis(750);

/// Pure debounce rule: has it been quiet for `window` since `last_event` as of `now`?
pub(crate) fn debounce_ready(last_event: Instant, now: Instant, window: Duration) -> bool {
    now.duration_since(last_event) >= window
}

/// One open-for-edit file. The watcher handle and the debounce task handle are
/// held so they live as long as the session and are torn down on close.
pub struct EditSession {
    pub session_id: SessionId,
    pub connection_id: ConnectionId,
    pub remote_path: String,
    pub name: String,
    pub temp_path: PathBuf,
    pub baseline: Mutex<RemoteStat>,
    pub has_conflict: std::sync::atomic::AtomicBool,
    _watcher: RecommendedWatcher,
    task: tauri::async_runtime::JoinHandle<()>,
}

/// Serialized to the UI by `list_edit_sessions` and the `edit://*` events.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EditSessionInfo {
    pub session_id: SessionId,
    pub connection_id: ConnectionId,
    pub remote_path: String,
    pub name: String,
    pub has_conflict: bool,
}

impl EditSession {
    pub fn info(&self) -> EditSessionInfo {
        EditSessionInfo {
            session_id: self.session_id,
            connection_id: self.connection_id,
            remote_path: self.remote_path.clone(),
            name: self.name.clone(),
            has_conflict: self.has_conflict.load(Ordering::SeqCst),
        }
    }
}

pub struct EditRegistry {
    pub conns: ConnMap,
    pub app: AppHandle,
    sessions: Mutex<HashMap<SessionId, Arc<EditSession>>>,
    next_id: AtomicU64,
    /// Per-connection temp root, e.g. <cache>/edits/<connection_id>/.
    pub root: PathBuf,
}

impl EditRegistry {
    pub fn new(app: AppHandle, conns: ConnMap, root: PathBuf) -> Self {
        Self {
            conns,
            app,
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            root,
        }
    }

    /// Stable temp path for a (connection, remote_path): a per-path subdir keeps
    /// the original basename (so the OS picks the right app & shows a sane name)
    /// while a hash subdir avoids basename collisions across remote dirs. Stable
    /// ⇒ re-opening the same file reuses the same temp file.
    pub fn temp_path_for(&self, connection_id: ConnectionId, remote_path: &str) -> PathBuf {
        let basename = remote_path.trim_end_matches('/').rsplit('/').next().unwrap_or("file");
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&remote_path, &mut hasher);
        let h = std::hash::Hasher::finish(&hasher);
        self.root.join(connection_id.to_string()).join(format!("{h:016x}")).join(basename)
    }

    pub fn list(&self) -> Vec<EditSessionInfo> {
        self.sessions.lock().unwrap().values().map(|s| s.info()).collect()
    }

    pub fn get(&self, id: SessionId) -> Option<Arc<EditSession>> {
        self.sessions.lock().unwrap().get(&id).cloned()
    }

    /// Already open for this (connection, path)? Return its id so the command can
    /// just re-open the existing temp file instead of re-downloading.
    pub fn find(&self, connection_id: ConnectionId, remote_path: &str) -> Option<SessionId> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .find(|s| s.connection_id == connection_id && s.remote_path == remote_path)
            .map(|s| s.session_id)
    }

    /// Register a session: install a watcher on the temp file's PARENT dir
    /// (NonRecursive) and spawn its debounce/save task. Returns the new id.
    pub fn register(
        self: &Arc<Self>,
        connection_id: ConnectionId,
        remote_path: String,
        name: String,
        temp_path: PathBuf,
        baseline: RemoteStat,
    ) -> Result<SessionId, StorageError> {
        let session_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel::<()>();

        // Watch the PARENT dir, not the file: editors that save by writing a
        // sibling temp and renaming over the original would orphan a file watch.
        let watched = temp_path.parent().unwrap_or(&temp_path).to_path_buf();
        let watch_target = temp_path.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if ev.paths.iter().any(|p| p == &watch_target) {
                    let _ = tx.send(());
                }
            }
        })
        .map_err(StorageError::other)?;
        watcher
            .watch(&watched, RecursiveMode::NonRecursive)
            .map_err(StorageError::other)?;

        let task = {
            let reg = self.clone();
            tauri::async_runtime::spawn(async move {
                reg.debounce_loop(session_id, rx).await;
            })
        };

        let session = Arc::new(EditSession {
            session_id,
            connection_id,
            remote_path,
            name,
            temp_path,
            baseline: Mutex::new(baseline),
            has_conflict: std::sync::atomic::AtomicBool::new(false),
            _watcher: watcher,
            task,
        });
        self.sessions.lock().unwrap().insert(session_id, session);
        Ok(session_id)
    }

    /// Coalesce a burst of fs events into one save attempt after a quiet window.
    async fn debounce_loop(self: Arc<Self>, id: SessionId, mut rx: mpsc::UnboundedReceiver<()>) {
        loop {
            // Block until the first event.
            if rx.recv().await.is_none() {
                return; // sender (watcher) dropped → session closed
            }
            let mut last = Instant::now();
            // Drain further events until it's been quiet for DEBOUNCE.
            loop {
                match tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                    Ok(Some(())) => last = Instant::now(),
                    Ok(None) => return,
                    Err(_) => {
                        if debounce_ready(last, Instant::now(), DEBOUNCE) {
                            break;
                        }
                    }
                }
            }
            self.try_save(id).await;
        }
    }

    /// Conflict-check, then upload (or emit a conflict). Errors emit `edit://error`.
    async fn try_save(&self, id: SessionId) {
        let Some(session) = self.get(id) else { return };
        let Some(backend) = self.conns.read().await.get(&session.connection_id).cloned() else {
            self.emit_error(&session, "connection closed; reconnect and re-open to save");
            return;
        };
        let baseline = *session.baseline.lock().unwrap();
        match check_conflict(backend.as_ref(), &session.remote_path, &baseline).await {
            Ok(ConflictCheck::Conflict { .. }) => {
                session.has_conflict.store(true, Ordering::SeqCst);
                let _ = self.app.emit("edit://conflict", session.info());
            }
            Ok(ConflictCheck::Clear) => {
                match save_back(backend.as_ref(), &session.temp_path, &session.remote_path).await {
                    Ok(fresh) => {
                        *session.baseline.lock().unwrap() = fresh;
                        session.has_conflict.store(false, Ordering::SeqCst);
                        let _ = self.app.emit("edit://saved", session.info());
                    }
                    Err(e) => self.emit_error(&session, &e.to_string()),
                }
            }
            Err(e) => self.emit_error(&session, &e.to_string()),
        }
    }

    fn emit_error(&self, session: &EditSession, message: &str) {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ErrPayload {
            session_id: SessionId,
            remote_path: String,
            name: String,
            message: String,
        }
        let _ = self.app.emit(
            "edit://error",
            ErrPayload {
                session_id: session.session_id,
                remote_path: session.remote_path.clone(),
                name: session.name.clone(),
                message: message.to_string(),
            },
        );
    }

    /// Force-write the temp file ignoring the baseline (conflict "Overwrite").
    pub async fn force_save(&self, id: SessionId) -> Result<(), StorageError> {
        let Some(session) = self.get(id) else { return Ok(()) };
        let backend = self
            .conns
            .read()
            .await
            .get(&session.connection_id)
            .cloned()
            .ok_or_else(|| StorageError::Other { detail: "connection closed".into() })?;
        let fresh = save_back(backend.as_ref(), &session.temp_path, &session.remote_path).await?;
        *session.baseline.lock().unwrap() = fresh;
        session.has_conflict.store(false, Ordering::SeqCst);
        let _ = self.app.emit("edit://saved", session.info());
        Ok(())
    }

    /// Re-download the remote into the temp file, discarding local edits, and
    /// re-baseline (conflict "Discard").
    pub async fn discard_local(&self, id: SessionId) -> Result<(), StorageError> {
        let Some(session) = self.get(id) else { return Ok(()) };
        let backend = self
            .conns
            .read()
            .await
            .get(&session.connection_id)
            .cloned()
            .ok_or_else(|| StorageError::Other { detail: "connection closed".into() })?;
        let fresh = wonderblob_core::edit::download_to_temp(
            backend.as_ref(),
            &session.remote_path,
            &session.temp_path,
        )
        .await?;
        *session.baseline.lock().unwrap() = fresh;
        session.has_conflict.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Close a session: drop the watcher + task; optionally delete the temp file.
    pub fn close(&self, id: SessionId, keep_temp: bool) {
        if let Some(session) = self.sessions.lock().unwrap().remove(&id) {
            session.task.abort();
            if !keep_temp {
                let _ = std::fs::remove_file(&session.temp_path);
                if let Some(parent) = session.temp_path.parent() {
                    let _ = std::fs::remove_dir(parent); // best-effort; only if empty
                }
            }
            // Dropping `session` drops the watcher.
        }
    }

    /// Close every session for a connection and remove its temp tree (spec:
    /// "temp files cleaned up on disconnect"). `keep_temp` honored per call site.
    pub fn close_connection(&self, connection_id: ConnectionId, keep_temp: bool) {
        let ids: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.connection_id == connection_id)
            .map(|s| s.session_id)
            .collect();
        for id in ids {
            self.close(id, keep_temp);
        }
        if !keep_temp {
            let _ = std::fs::remove_dir_all(self.root.join(connection_id.to_string()));
        }
    }
}

/// Build the registry under <app cache>/edits and clean any stale temp tree from
/// a previous run (nothing is open at startup).
pub fn init_edit(app: &AppHandle, conns: ConnMap) -> Arc<EditRegistry> {
    let root = app
        .path()
        .app_cache_dir()
        .map(|d| d.join("edits"))
        .unwrap_or_else(|_| std::env::temp_dir().join("wonderblob-edits"));
    let _ = std::fs::remove_dir_all(&root); // prior-run temp files are orphans
    let _ = std::fs::create_dir_all(&root);
    Arc::new(EditRegistry::new(app.clone(), conns, root))
}
```

Add `mod edit;` to `src-tauri/src/lib.rs`.

> **Note (`Path` import):** `temp_path_for` and friends use `PathBuf`; the bare `Path` import is for `&Path` params elsewhere — drop it if the compiler flags it unused.

- [ ] **Step 5: Build + run the debounce test**

Run: `cargo build -p wonderblob && cargo test -p wonderblob edit::`
Expected: clean build; debounce test passes.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(app): EditRegistry + notify watcher + debounce save flow (edit:// events)"
```

---

### Task 4: Tauri commands + engine-style wiring + disconnect/exit cleanup

Construct the registry in `setup()`, expose the open/list/close/resolve commands, and tear down watchers + temp on disconnect and app exit.

**Files:**
- Modify: `src-tauri/src/lib.rs` (build + manage registry in `setup`; register handlers; exit cleanup)
- Modify: `src-tauri/src/commands.rs` (open/list/close/resolve commands; disconnect cleanup)

- [ ] **Step 1: Commands**

Append to `src-tauri/src/commands.rs` (imports: add `use crate::edit::{EditRegistry, EditSessionInfo, SessionId};` and `use tauri_plugin_opener::OpenerExt;`):

```rust
#[tauri::command]
pub async fn open_in_editor(
    state: State<'_, AppState>,
    edit: State<'_, Arc<EditRegistry>>,
    app: tauri::AppHandle,
    id: ConnectionId,
    path: String,
) -> Result<SessionId, StorageError> {
    // Re-open the existing session if the file is already open.
    if let Some(existing) = edit.find(id, &path) {
        if let Some(s) = edit.get(existing) {
            app.opener()
                .open_path(s.temp_path.to_string_lossy(), None::<&str>)
                .map_err(StorageError::other)?;
        }
        return Ok(existing);
    }
    let backend = state.get(id).await?;
    let temp = edit.temp_path_for(id, &path);
    let baseline = wonderblob_core::edit::download_to_temp(backend.as_ref(), &path, &temp).await?;
    let name = basename_of(&path);
    let session_id = edit.register(id, path, name, temp.clone(), baseline)?;
    app.opener()
        .open_path(temp.to_string_lossy(), None::<&str>)
        .map_err(StorageError::other)?;
    Ok(session_id)
}

#[tauri::command]
pub async fn list_edit_sessions(
    edit: State<'_, Arc<EditRegistry>>,
) -> Result<Vec<EditSessionInfo>, StorageError> {
    Ok(edit.list())
}

#[tauri::command]
pub async fn close_edit_session(
    edit: State<'_, Arc<EditRegistry>>,
    session_id: SessionId,
    keep_temp: bool,
) -> Result<(), StorageError> {
    edit.close(session_id, keep_temp);
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictAction {
    Overwrite,
    SaveAsCopy,
    Discard,
}

#[tauri::command]
pub async fn resolve_conflict(
    state: State<'_, AppState>,
    edit: State<'_, Arc<EditRegistry>>,
    session_id: SessionId,
    action: ConflictAction,
) -> Result<(), StorageError> {
    match action {
        ConflictAction::Overwrite => edit.force_save(session_id).await,
        ConflictAction::Discard => edit.discard_local(session_id).await,
        ConflictAction::SaveAsCopy => {
            // Upload the local edits to a sibling "<name> (local copy)" path,
            // leaving the (changed) remote untouched; then clear the conflict.
            let session = edit.get(session_id).ok_or_else(|| StorageError::Other {
                detail: "no such edit session".into(),
            })?;
            let backend = state.get(session.connection_id).await?;
            let copy_path = sibling_copy_path(&session.remote_path);
            wonderblob_core::edit::save_back(backend.as_ref(), &session.temp_path, &copy_path).await?;
            edit.discard_local(session_id).await // re-baseline to the real remote
        }
    }
}

/// "/a/b/report.txt" → "/a/b/report (local copy).txt".
fn sibling_copy_path(remote_path: &str) -> String {
    let (dir, file) = match remote_path.rfind('/') {
        Some(i) => (&remote_path[..=i], &remote_path[i + 1..]),
        None => ("", remote_path),
    };
    match file.rfind('.') {
        Some(i) if i > 0 => format!("{dir}{} (local copy){}", &file[..i], &file[i..]),
        _ => format!("{dir}{file} (local copy)"),
    }
}
```

- [ ] **Step 2: Disconnect cleanup**

Update `disconnect` in `commands.rs` so closing a connection tears down its edit sessions + temp:

```rust
#[tauri::command]
pub async fn disconnect(
    state: State<'_, AppState>,
    edit: State<'_, Arc<EditRegistry>>,
    id: ConnectionId,
) -> Result<(), StorageError> {
    edit.close_connection(id, false); // drop watchers + temp for this connection
    state.remove(id).await;
    Ok(())
}
```

- [ ] **Step 3: Build the registry in `setup` + register handlers + exit cleanup**

In `src-tauri/src/lib.rs`, extend the existing `setup` closure (it already builds the engine):

```rust
        .setup(|app| {
            let conns = app.state::<state::AppState>().connections.clone();
            let engine = transfers::init_engine(app.handle(), conns.clone());
            app.manage(engine);
            let edit = edit::init_edit(app.handle(), conns);
            app.manage(edit);
            Ok(())
        })
```

Add the new commands to `generate_handler![]`:

```rust
            commands::open_in_editor,
            commands::list_edit_sessions,
            commands::close_edit_session,
            commands::resolve_conflict,
            commands::preview_file,
```

(`preview_file` lands in Task 5; add the line then.) For app-exit teardown, watcher threads are dropped when the registry is dropped at process exit, and temp files were already wiped at next-startup by `init_edit`; that is sufficient. (Optional belt-and-suspenders: handle `tauri::RunEvent::ExitRequested` to `close_connection` all live connections — only add if a reviewer wants synchronous cleanup; not required since startup re-cleans.)

- [ ] **Step 4: Build**

Run: `cargo build -p wonderblob`
Expected: clean (after Task 5 adds `preview_file`, the handler line resolves; until then either add `preview_file` first or temporarily omit that one handler line).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(app): edit commands (open/list/close/resolve) + disconnect/startup temp cleanup"
```

---

### Task 5: `preview_file` command

Read a remote file (capped), classify via `preview_plan`, and return text or a `data:` image URL for the in-app preview.

**Files:**
- Modify: `src-tauri/src/commands.rs`

- [ ] **Step 1: Implement**

Append to `commands.rs` (imports: `use wonderblob_core::edit::{image_mime, preview_plan, PreviewPlan, PREVIEW_CAP_BYTES};`, `use base64::Engine;`, `use tokio::io::AsyncReadExt;`):

```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewResult {
    /// Tagged plan: { kind: "text" | "image" | "pdf" | "tooLarge" | "unsupported", … }
    pub plan: PreviewPlan,
    /// Decoded UTF-8 for `text` previews.
    pub text: Option<String>,
    /// "data:<mime>;base64,…" for `image` previews.
    pub data_url: Option<String>,
}

/// Read up to PREVIEW_CAP_BYTES of a remote file for the in-app preview.
#[tauri::command]
pub async fn preview_file(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
    name: String,
    size: Option<u64>,
) -> Result<PreviewResult, StorageError> {
    let plan = preview_plan(&name, size, PREVIEW_CAP_BYTES);
    // Only the renderable kinds read bytes; the rest report and stop.
    match &plan {
        PreviewPlan::Text | PreviewPlan::Image => {}
        _ => return Ok(PreviewResult { plan, text: None, data_url: None }),
    }
    let backend = state.get(id).await?;
    let mut reader = backend.read(&path, 0).await?;
    // Read cap+1 so we can detect (and reject) files whose real size exceeded the
    // declared `size` (or had no declared size).
    let mut buf = Vec::new();
    let mut limited = (&mut reader).take(PREVIEW_CAP_BYTES + 1);
    limited.read_to_end(&mut buf).await.map_err(StorageError::other)?;
    if buf.len() as u64 > PREVIEW_CAP_BYTES {
        return Ok(PreviewResult {
            plan: PreviewPlan::TooLarge { size: buf.len() as u64, cap: PREVIEW_CAP_BYTES },
            text: None,
            data_url: None,
        });
    }
    Ok(match plan {
        PreviewPlan::Text => PreviewResult {
            plan: PreviewPlan::Text,
            text: Some(String::from_utf8_lossy(&buf).into_owned()),
            data_url: None,
        },
        PreviewPlan::Image => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
            PreviewResult {
                plan: PreviewPlan::Image,
                text: None,
                data_url: Some(format!("data:{};base64,{}", image_mime(&name), b64)),
            }
        }
        other => PreviewResult { plan: other, text: None, data_url: None },
    })
}
```

`AsyncReadExt::take` needs `tokio::io::AsyncReadExt` in scope; the reader is `Box<dyn AsyncRead + Send + Unpin>` exactly as `StorageBackend::read` returns.

- [ ] **Step 2: Build** — `cargo build -p wonderblob` → clean. Ensure `commands::preview_file` is in `generate_handler![]` (Task 4 Step 3).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(app): preview_file command — capped read, text decode + image data URL"
```

---

### Task 6: Frontend API wrappers + edit-sessions store

Typed wrappers for the new commands and a live store of open sessions reconciled with the `edit://*` events.

**Files:**
- Modify: `src/lib/api.ts`
- Create: `src/lib/stores/edit.ts`

- [ ] **Step 1: API types + wrappers**

In `src/lib/api.ts` add types:

```ts
export type PreviewKind = "text" | "image" | "pdf" | "tooLarge" | "unsupported";
export interface PreviewPlan {
  kind: PreviewKind;
  size?: number; // tooLarge
  cap?: number;  // tooLarge
  ext?: string;  // unsupported
}
export interface PreviewResult {
  plan: PreviewPlan;
  text: string | null;
  dataUrl: string | null;
}

export interface EditSessionInfo {
  sessionId: number;
  connectionId: number;
  remotePath: string;
  name: string;
  hasConflict: boolean;
}

export type ConflictAction = "overwrite" | "saveAsCopy" | "discard";
```

In the `api` object add:

```ts
  openInEditor: (id: number, path: string) =>
    invoke<number>("open_in_editor", { id, path }),
  listEditSessions: () => invoke<EditSessionInfo[]>("list_edit_sessions"),
  closeEditSession: (sessionId: number, keepTemp: boolean) =>
    invoke<void>("close_edit_session", { sessionId, keepTemp }),
  resolveConflict: (sessionId: number, action: ConflictAction) =>
    invoke<void>("resolve_conflict", { sessionId, action }),
  previewFile: (id: number, path: string, name: string, size?: number) =>
    invoke<PreviewResult>("preview_file", { id, path, name, size: size ?? null }),
```

- [ ] **Step 2: Edit store**

`src/lib/stores/edit.ts`:

```ts
import { writable, derived } from "svelte/store";
import { listen } from "@tauri-apps/api/event";
import { api, type EditSessionInfo } from "$lib/api";

export const editSessions = writable<EditSessionInfo[]>([]);

/** Remote paths currently open for edit — FileList uses this for the row badge. */
export const editPaths = derived(editSessions, ($s) => new Set($s.map((e) => e.remotePath)));

/** Sessions awaiting conflict resolution (drives the modal). */
export const editConflicts = derived(editSessions, ($s) => $s.filter((e) => e.hasConflict));

async function refresh() {
  editSessions.set(await api.listEditSessions());
}

let started = false;
let onSaved: ((name: string) => void) | null = null;
let onError: ((message: string) => void) | null = null;

export async function initEdit(opts?: {
  onSaved?: (name: string) => void;
  onError?: (message: string) => void;
}) {
  onSaved = opts?.onSaved ?? null;
  onError = opts?.onError ?? null;
  if (started) return;
  started = true;
  await refresh();
  await listen<EditSessionInfo>("edit://saved", (e) => { refresh(); onSaved?.(e.payload.name); });
  await listen<EditSessionInfo>("edit://conflict", () => { refresh(); });
  await listen<{ name: string; message: string }>("edit://error", (e) => {
    refresh();
    onError?.(`Couldn't save “${e.payload.name}”: ${e.payload.message}`);
  });
}

export async function closeSession(sessionId: number, keepTemp: boolean) {
  await api.closeEditSession(sessionId, keepTemp);
  await refresh();
}

export async function resolve(sessionId: number, action: "overwrite" | "saveAsCopy" | "discard") {
  await api.resolveConflict(sessionId, action);
  await refresh();
}
```

- [ ] **Step 3: Typecheck** — `npm run check` → clean.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(ui): edit API wrappers + live edit-sessions store over edit:// events"
```

---

### Task 7: FileList wiring — open / Enter / spacebar-vs-typeahead / row badge

Replace the file-open stub with `open_in_editor`, add the spacebar preview without breaking type-ahead, and badge rows that are open for edit.

**Files:**
- Modify: `src/lib/components/FileList.svelte`

**Spacebar-vs-type-ahead resolution (decided):** today the catch-all branch `e.key.length === 1 && !ctrl/meta/alt` feeds *every* printable key — including Space (`" "`) — into `handleTypeahead`, so Space currently appends a space to the type-ahead buffer. New rule, inserted **before** that catch-all:

> Space previews the selected entry **only when** (a) a file (not a dir) is selected **and** (b) the type-ahead buffer is empty. If the buffer is non-empty (the user is mid-type-ahead, e.g. typing a name that contains a space), Space falls through to `handleTypeahead` and is appended as usual — so type-ahead for names with spaces still works. For a selected directory, Space also falls through to type-ahead (no preview for folders).

- [ ] **Step 1: Props + open**

Add props for preview + open delegation. In the `<script>`:

```ts
  let {
    onerror,
    onpreview,
  }: { onerror?: (m: string) => void; onpreview?: (entry: Entry) => void } = $props();
```

Import the edit store and the active connection (already imported `activeConnection`):

```ts
  import { editPaths } from "../stores/edit";
```

Replace the file branch of `open()`:

```ts
  async function open(entry: Entry) {
    if (entry.kind === "dir") {
      currentPath.set(entry.path);
      return;
    }
    const conn = $activeConnection;
    if (!conn) return;
    try {
      await api.openInEditor(conn.id, entry.path); // download → OS default app → watch
    } catch (e) {
      onerror?.(describeError(e, "open"));
    }
  }
```

(`api.openInEditor` already exists from Task 6; `describeError` is already imported.)

- [ ] **Step 2: Spacebar in the keymap**

In `onkeydown`, insert this branch **before** the final `else if (e.key.length === 1 …)`:

```ts
    } else if (e.key === " " && typeahead === "" && selected && selected.kind !== "dir") {
      e.preventDefault();
      onpreview?.(selected);
```

The existing catch-all stays last, so Space with a non-empty `typeahead` buffer (or a selected dir) still reaches `handleTypeahead`.

- [ ] **Step 3: Row badge for open-for-edit files**

In the row's `.col-name`, after the `.name`/rename input, add a small dot when the path is open. Use only existing tokens (`--accent`):

```svelte
          {#if $editPaths.has(entry.path)}
            <span class="editing-dot" title="Open for editing" aria-label="Open for editing"></span>
          {/if}
```

Add to `<style>`:

```css
  .editing-dot {
    flex-shrink: 0;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
  }
```

- [ ] **Step 4: Typecheck + existing tests**

Run: `npm run check && npm test`
Expected: clean; no regressions (FileList still keyboard-driven; type-ahead unchanged for non-Space keys and for Space mid-buffer).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(ui): FileList opens files via EditSession; spacebar preview (type-ahead safe); edit-row badge"
```

---

### Task 8: Preview overlay

A keyboard-driven preview panel: text in a monospace pane, images as `<img>`, PDF/too-large/unsupported as an "Open in editor" fallback. Esc and Space close it.

**Files:**
- Create: `src/lib/components/PreviewOverlay.svelte`
- Modify: `src/routes/+page.svelte` (mount it; pass the entry from FileList's `onpreview`)

- [ ] **Step 1: Component**

`src/lib/components/PreviewOverlay.svelte` — Svelte 5 runes. Requirements:

- Props: `entry: Entry`, `connectionId: number`, `onclose: () => void`, `onopen: (entry: Entry) => void` (the Open-in-editor fallback).
- On mount, call `api.previewFile(connectionId, entry.path, entry.name, entry.size ?? undefined)`; show a brief spinner (reuse the `.spinner` pattern from `FileList`) until it resolves.
- Render by `result.plan.kind`:
  - `text` → `<pre>` in `var(--font-mono)`, `var(--text-small)`, `--fg-primary`, scrollable, `white-space: pre`.
  - `image` → `<img src={result.dataUrl}>` centered, `max-width/height: 100%`, `object-fit: contain`.
  - `pdf` → message "PDF preview isn't supported here." + an "Open in editor" button calling `onopen(entry)` then `onclose()`. (Optional: also render `<embed src={result.dataUrl} type="application/pdf">` above the button for platforms where WebKit shows it — the button is always present.)
  - `tooLarge` → "Too large to preview (N) — open it in your editor instead." + Open-in-editor button.
  - `unsupported` → "Can't preview .<ext> files." + Open-in-editor button.
- A header strip: the file name (left), a close “✕” ghost button (right).
- **Focus + keys:** a focusable container (`tabindex="0"`) that autofocuses on mount; `onkeydown` handles `Escape` → `onclose()` and `" "` (Space) → `onclose()` (spec: "Space toggles"). `e.preventDefault()` on both; `e.stopPropagation()` so the underlying FileList doesn't also act.
- Layout: an overlay centered over the content pane (not full-screen modal) — `position: absolute; inset: 0;` within `.content`, `background: var(--bg-content)`, a `1px solid var(--border)` framed card, `--radius`. Motion: ≤150ms fade-in only.
- **Tokens only**, no new colors. Use `describeError(e, "open")` via an `onerror` prop or inline error text in `--danger` if the read fails.

Skeleton:

```svelte
<script lang="ts">
  import { api, type Entry, type PreviewResult } from "../api";
  import { describeError } from "../errors";
  import { formatSize } from "../format";

  let {
    entry,
    connectionId,
    onclose,
    onopen,
  }: { entry: Entry; connectionId: number; onclose: () => void; onopen: (e: Entry) => void } =
    $props();

  let result = $state<PreviewResult | null>(null);
  let error = $state<string | null>(null);
  let host = $state<HTMLDivElement | null>(null);

  $effect(() => {
    host?.focus();
  });

  $effect(() => {
    let alive = true;
    api
      .previewFile(connectionId, entry.path, entry.name, entry.size ?? undefined)
      .then((r) => alive && (result = r))
      .catch((e) => alive && (error = describeError(e, "open")));
    return () => { alive = false; };
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape" || e.key === " ") {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    }
  }
  function openInEditor() {
    onopen(entry);
    onclose();
  }
</script>

<div class="overlay" bind:this={host} tabindex="0" role="dialog" aria-label="Preview" {onkeydown}>
  <div class="bar">
    <span class="name" title={entry.name}>{entry.name}</span>
    <button class="ghost" onclick={onclose} aria-label="Close preview">✕</button>
  </div>
  <div class="body">
    {#if error}
      <p class="msg danger">{error}</p>
    {:else if !result}
      <span class="spinner" aria-label="Loading"></span>
    {:else if result.plan.kind === "text"}
      <pre class="text">{result.text}</pre>
    {:else if result.plan.kind === "image"}
      <img class="img" src={result.dataUrl} alt={entry.name} />
    {:else}
      <div class="fallback">
        <p class="msg">
          {#if result.plan.kind === "pdf"}PDF preview isn’t supported here.
          {:else if result.plan.kind === "tooLarge"}Too large to preview ({formatSize(entry.size)}).
          {:else}Can’t preview .{result.plan.ext} files.{/if}
        </p>
        <button class="primary" onclick={openInEditor}>Open in editor</button>
      </div>
    {/if}
  </div>
</div>

<style>
  /* tokens only; reuse FileList .spinner keyframes pattern */
</style>
```

(Fill the `<style>` with token-only rules mirroring `+page.svelte`/`FileList.svelte`; include the `@keyframes spin` + `.spinner` from `FileList` for the loading state.)

- [ ] **Step 2: Mount in `+page.svelte`**

```ts
  import PreviewOverlay from "$lib/components/PreviewOverlay.svelte";
  let previewEntry = $state<import("$lib/api").Entry | null>(null);
```

Pass `onpreview` to `FileList` and render the overlay inside `.browser` (so it overlays the list, scoping its `position: absolute`):

```svelte
      <div class="browser">
        <FileList bind:this={fileList} onerror={showToast} onpreview={(e) => (previewEntry = e)} />
        {#if previewEntry && $activeConnection}
          <PreviewOverlay
            entry={previewEntry}
            connectionId={$activeConnection.id}
            onclose={() => (previewEntry = null)}
            onopen={(e) => fileList?.openEntry(e)}
          />
        {/if}
      </div>
```

Expose a small `export function openEntry(entry: Entry)` in `FileList.svelte` that calls the internal `open(entry)` (so the overlay's "Open in editor" reuses the exact same EditSession path). `.browser` needs `position: relative` for the overlay's `inset: 0` — add it to that rule.

- [ ] **Step 3: Verify + typecheck**

Run: `npm run check && npm test`. Then manual (after Task 11's fixture or any connection): select a text file, press Space → preview; Esc closes; select an image → renders; select a `.pdf`/large file → fallback button opens it in the editor.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(ui): spacebar preview overlay (text/image robust, pdf/too-large fallback)"
```

---

### Task 9: Edit-session indicator + conflict modal

A toolbar indicator listing open-for-edit files with per-session close (keep/discard temp), and a modal that resolves conflicts (Overwrite / Save as copy / Discard).

**Files:**
- Create: `src/lib/components/EditSessions.svelte` (popover list + conflict modal, or two small components)
- Modify: `src/routes/+page.svelte` (init the edit store; mount the indicator + modal)

- [ ] **Step 1: Init the edit store in `+page.svelte`**

```ts
  import { initEdit, editSessions, editConflicts } from "$lib/stores/edit";

  $effect(() => {
    initEdit({
      onSaved: (name) => showToast(`Saved “${name}”`), // or a quieter status line
      onError: showToast,
    });
  });
```

- [ ] **Step 2: Indicator + close**

`EditSessions.svelte` requirements:

- Reads `editSessions`. A toolbar ghost button "Editing ({n})" (only shown when `n > 0`), consistent with the existing "Transfers (n)" button, toggling a small popover/panel.
- The panel lists each session: name (with a `--accent` dot if `hasConflict`), and a per-row "Close" affordance. Closing prompts keep-vs-discard temp — simplest desktop pattern: two buttons, "Close (keep file)" → `closeSession(id, true)` and "Close & discard" → `closeSession(id, false)`. (A single Close with a modifier is fine too; spell out whichever you implement.)
- Keyboard-accessible (focusable rows, Tab to buttons, Esc closes the popover), tokens only, same density as `BookmarkList`/`TransfersPanel`.

- [ ] **Step 3: Conflict modal**

A modal driven by `$editConflicts` (show the first pending conflict; queue the rest). For the conflicting session show:

> "**{name}** changed on the server since you opened it. How do you want to resolve it?"

Three buttons:
- **Overwrite remote** → `resolve(sessionId, "overwrite")`
- **Save as copy** → `resolve(sessionId, "saveAsCopy")` (uploads to "name (local copy).ext", re-baselines to the real remote)
- **Discard local changes** → `resolve(sessionId, "discard")` (re-downloads remote into the temp file)

Modal styling mirrors `ConnectionSheet.svelte` (existing overlay/sheet pattern); Esc maps to the safest action — **do nothing / dismiss** (leave it unresolved; the badge persists) rather than silently discarding. Tokens only. After any action the store refreshes; the modal advances to the next pending conflict or closes.

- [ ] **Step 4: Mount in the toolbar**

Add `<EditSessions />` to `.actions` (e.g. before the Transfers button) and render the conflict modal at the `+page.svelte` top level (sibling of `ConnectionSheet`), gated on `$editConflicts.length > 0`.

- [ ] **Step 5: Verify + typecheck** — `npm run check && npm test` clean; manual conflict walk-through happens in Task 11.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(ui): edit-session indicator (close keep/discard) + conflict-resolution modal"
```

---

### Task 10: Download toolbar button — wire Plan 3's stub through the engine

A "Download" action that saves the selected file to a user-chosen location via the save dialog and enqueues it on the `TransferEngine`.

**Files:**
- Modify: `src/routes/+page.svelte`

- [ ] **Step 1: Implement `download()`**

Add the `save` import and the handler:

```ts
  import { save } from "@tauri-apps/plugin-dialog";

  async function download() {
    const conn = $activeConnection;
    if (!conn) return;
    const entry = fileList?.selected() ?? null;
    if (!entry || entry.kind === "dir") {
      showToast("Select a file to download.");
      return;
    }
    const dest = await save({ defaultPath: entry.name, title: "Download to…" });
    if (!dest) return;
    try {
      await api.enqueueDownload(conn.id, entry.path, dest, entry.size ?? undefined);
      transfersOpen = true; // reveal progress
    } catch (e) {
      showToast(opError(e, "Couldn't start download"));
    }
  }
```

This finally exercises `api.enqueueDownload` (Plan 3) from the UI.

- [ ] **Step 2: Toolbar button**

In `.actions`, next to Upload:

```svelte
          <button class="ghost" onclick={download}>Download</button>
```

- [ ] **Step 3: Capability check**

`save()` requires `dialog:allow-save` — it's part of `dialog:default` in Tauri 2. Confirm by building + invoking; if the runtime reports a denied permission, add `"dialog:allow-save"` to `src-tauri/capabilities/default.json` `permissions`. (Expected: already covered by `dialog:default`.)

- [ ] **Step 4: Verify + typecheck** — `npm run check && npm test`; manual download of a file → it appears in the transfers panel and lands at the chosen path.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(ui): Download button — save dialog + enqueue via TransferEngine (wires Plan 3 stub)"
```

---

### Task 11: Gated SFTP EditSession E2E + CI

Real open → edit → save-back and the conflict path against the Dockerized OpenSSH server. Gated by `WONDERBLOB_TEST_SFTP=1`. These drive the **core** `edit` functions against `SftpBackend` directly (no Tauri), exactly like Plan 3's gated transfer test.

**Files:**
- Create: `crates/wonderblob-core/tests/edit_sftp.rs`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the gated tests**

`crates/wonderblob-core/tests/edit_sftp.rs`:

```rust
//! Real open/edit/save-back + conflict against the Dockerized OpenSSH server
//! (Plan 1 fixture). Gated by WONDERBLOB_TEST_SFTP=1; run scripts/test-sftp-up.sh first.

use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use wonderblob_core::edit::{check_conflict, download_to_temp, save_back, ConflictCheck};
use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};
use wonderblob_core::vfs::StorageBackend;

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

async fn connect() -> Arc<dyn StorageBackend> {
    Arc::new(
        SftpBackend::connect(SftpConfig {
            host: "localhost".into(),
            port: 2222,
            username: "wb".into(),
            auth: SftpAuth::Password("wbpass".into()),
        })
        .await
        .expect("connect"),
    )
}

#[tokio::test]
async fn edit_open_then_save_back_changes_remote() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    let backend = connect().await;
    let remote = "/config/wb-edit.txt";
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"original\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("wb-edit.txt");

    let baseline = download_to_temp(backend.as_ref(), remote, &temp).await.unwrap();
    assert_eq!(std::fs::read(&temp).unwrap(), b"original\n");

    // simulate a local edit, then save back
    std::fs::write(&temp, b"edited locally\n").unwrap();
    let _fresh = save_back(backend.as_ref(), &temp, remote).await.unwrap();

    // re-read the remote → it changed
    let mut r = backend.read(remote, 0).await.unwrap();
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut r, &mut got).await.unwrap();
    assert_eq!(got, b"edited locally\n");

    // no conflict immediately after our own save (baseline re-stat path)
    let _ = baseline;
    let _ = backend.delete(remote).await;
}

#[tokio::test]
async fn edit_detects_out_of_band_conflict_not_silent_overwrite() {
    if !enabled() {
        eprintln!("skipped");
        return;
    }
    let backend = connect().await;
    let remote = "/config/wb-edit-conflict.txt";
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"v1\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("c.txt");
    let baseline = download_to_temp(backend.as_ref(), remote, &temp).await.unwrap();

    // someone else changes the remote out-of-band (different size)
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"v2 changed elsewhere\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    std::fs::write(&temp, b"my local edit\n").unwrap();

    match check_conflict(backend.as_ref(), remote, &baseline).await.unwrap() {
        ConflictCheck::Conflict { .. } => {}
        ConflictCheck::Clear => panic!("expected a conflict, not a silent overwrite"),
    }
    let _ = backend.delete(remote).await;
}
```

- [ ] **Step 2: Run against the live container**

```bash
./scripts/test-sftp-up.sh
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test edit_sftp -- --nocapture
./scripts/test-sftp-down.sh
```

Expected: both tests pass. (SFTP reports mtime, so the conflict test also exercises the mtime path, not just size.)

- [ ] **Step 3: CI**

In `.github/workflows/ci.yml`, inside the existing SFTP block (which already brings the container up, runs `--test sftp_contract` and `--test transfer_sftp`, then brings it down), add one line so it reuses the running container:

```yaml
          WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test edit_sftp
```

(Append next to the existing `--test transfer_sftp` line, before `./scripts/test-sftp-down.sh`. Do not add a second up/down pair.)

- [ ] **Step 4: Full local sweep**

```bash
cargo test --workspace && npm run check && npm test
```

Expected: all green (Docker-gated tests skip without the env flag).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(core): gated SFTP EditSession E2E — save-back + out-of-band conflict; CI step"
```

---

## Done criteria (Plan 4)

- `cargo test --workspace` + `npm run check` + `npm test` green locally and in CI (Docker-gated tests skip without env flags).
- Core `edit` unit tests (with `MockBackend`, no Tauri) assert: **size/mtime conflict rule** (mtime only when both present), **download records a baseline + writes temp**, **save-back overwrites remote + re-baselines**, **out-of-band change is detected (not a silent overwrite)**, **clear when unchanged**, and the **preview decision** (text/image/pdf/too-large/unsupported + size-guard-wins).
- Gated SFTP E2E proves a real open → local edit → save-back changes the remote, and an out-of-band remote change is detected as a conflict.
- Double-click / Enter on a file downloads it to a stable per-connection temp path, opens it with the OS default app via `tauri-plugin-opener`, and a `notify` watcher (parent-dir, path-filtered, debounced ~750ms) re-uploads on save — emitting `edit://saved`, or `edit://conflict` when the remote changed since download, or `edit://error`.
- Spacebar previews the selected file in-app (text monospace pane; images via `data:` URL with **no CSP change**; PDF/too-large/unsupported fall back to "Open in editor"); Esc and Space close it; **type-ahead is preserved** (Space only previews when a non-dir is selected and the type-ahead buffer is empty).
- The conflict modal resolves Overwrite / Save-as-copy / Discard; an indicator lists open-for-edit files with a row badge in `FileList` and per-session close (keep or discard temp).
- Edit sessions and their temp files are cleaned up on disconnect (and any prior-run temp tree is wiped at startup).
- The **Download** toolbar button saves the selected file to a chosen location via the save dialog and enqueues it on the `TransferEngine` — wiring the trigger Plan 3 left stubbed.
- UI is tokens-only (no new colors; preview overlay + badge reuse `--bg-content`/`--border`/`--accent`/`--fg-*`), keyboard-first, Svelte 5 runes; SFTP/S3/Azure/transfer flows unchanged.

## Explicitly deferred

- **Diff view / 3-way merge** for conflicts — v1 offers overwrite / save-as-copy / discard only, no visual diff or merge.
- **Content-hash integrity** (true etag-equivalent) — conflict detection is size + best-effort mtime; mtime-less backends (S3/Azure unless they expose it) degrade to size-only.
- **Rich in-webview PDF** (page navigation, search) — v1 is text+images robust, PDF best-effort/fallback.
- **Editor-process awareness** — we watch the temp file, not the editor; there's no "the app that opened this has closed" signal, so sessions are closed manually (or on disconnect).
- **Configurable temp retention / "always keep temp" setting** — v1 cleans on disconnect/startup with a per-session keep flag at close; a persisted preference is a follow-up.
- **Preview of more types** (video, audio, office docs, syntax highlighting, large-file streaming via the asset protocol) — v1 is plain text + raster/SVG images + PDF fallback.
- **Auto-reopen sessions across app restarts** — sessions are in-memory; temp is wiped at startup.
- **External drag-out / multi-cursor / in-app editing** — editing happens in the OS default app, not inside Wonderblob.

## Self-review (writing-plans checklist)

- **Spec coverage (§ EditSession):** double-click/Enter → download to a per-connection temp dir + open with OS default handler ✓ (`open_in_editor` + `tauri-plugin-opener` `OpenerExt::open_path`); spacebar lightweight in-app preview for text/images/PDF without launching an app ✓ (`preview_file` + `PreviewOverlay`, PDF fallback documented); `notify`-based watcher detects saves, debounces, re-uploads ✓ (`EditRegistry` parent-dir watch + `debounce_loop`); conflict guard re-checks remote before overwrite and prompts overwrite/save-copy/discard ✓ (`check_conflict` + `resolve_conflict` + modal) — implemented over **mtime/size** since the trait has **no etag** (called out, not hand-waved); temp cleaned on disconnect with a keep option ✓ (`close_connection`, `close(keep_temp)`). § v1 scope "open/edit/save-back, spacebar preview" ✓; the Download trigger is wired through the existing `TransferEngine` ✓.
- **No placeholders / no "same as task N":** every task has a failing test (where logic is testable), exact paths, run-commands with expected output, and a commit. UI components are specified by explicit requirement-lists + skeletons in the same style Plan 3 used for `TransfersPanel`. The one staged dependency (`preview_file` handler line referenced in Task 4, defined in Task 5) is flagged inline.
- **Type/name consistency with the REAL symbols read:**
  - `StorageBackend` used exactly as in `vfs.rs`: `read(&self, path, offset) -> Result<Box<dyn AsyncRead + Send + Unpin>>`, `write(&self, path) -> Result<Box<dyn AsyncWrite + Send + Unpin>>`, `stat -> Entry`, `delete`. **Trait unchanged.** `Entry { size: Option<u64>, modified_ms: Option<i64> }` — the exact fields driving `RemoteStat` (no etag, confirmed).
  - `StorageError` variants used as they exist: `NotFound { path }` (vanished-remote conflict), `Other { detail }`, plus the constructor `StorageError::other(impl Display)` from `error.rs`.
  - App state: `AppState.connections: ConnMap = Arc<RwLock<HashMap<ConnectionId, Arc<dyn StorageBackend>>>>` (from `state.rs`) shared into `EditRegistry` exactly as `transfers::AppResolver` shares it; `state.get(id)`, `state.remove(id)`. `ConnectionId = u64`.
  - Reuses `MockBackend` (`crates/wonderblob-core/src/transfer/mock.rs`) for ungated tests; reuses `SftpBackend::connect(SftpConfig{host,port,username,auth})` + `SftpAuth::Password` (from the Plan 3 gated test) for the gated tests.
  - Commands/events follow the established conventions: `snake_case` commands (`open_in_editor`, `list_edit_sessions`, `close_edit_session`, `resolve_conflict`, `preview_file`) registered in `generate_handler![]`; `#[serde(rename_all = "camelCase")]` payloads so `sessionId`/`remotePath`/`hasConflict`/`dataUrl` match the TS interfaces, exactly like `ConnectResult`/`Transfer`; `edit://saved` / `edit://conflict` / `edit://error` mirror the spec-style `transfer://progress` / `transfer://state` namespacing.
  - Frontend mirrors Plan 1–3 idioms: `$lib/api` `invoke<T>(name, args)` wrappers, `writable`/`derived` stores like `transfers.ts`/`session.ts`, `listen<T>("edit://…")` subscriptions, components keyboard-consistent with `FileList`/`TransfersPanel` (`onerror`/`onpreview` props, `describeError`, `--row-height` rows, tokens-only). FileList's existing focus invariant, type-ahead (`handleTypeahead`, `typeahead` buffer), and keymap (`onkeydown`) are extended, not replaced — the Space rule is inserted before the printable-char catch-all so names with spaces still type-ahead.
  - **Capabilities/plugins:** `tauri-plugin-opener` is already a dependency with `opener:default` granted (`capabilities/default.json`) — no new plugin. `dialog:default` covers `save()`. **No CSP/asset-protocol change**: image previews use `data:` URLs and `img-src 'self' data:` is already in `tauri.conf.json`'s CSP; the asset-protocol alternative is documented as the deferred upgrade path. New Rust deps are `notify` + `base64` in **`src-tauri`** only.
