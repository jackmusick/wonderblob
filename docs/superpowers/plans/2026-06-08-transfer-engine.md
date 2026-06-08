# Wonderblob Plan 3: TransferEngine (queue, resume, progress)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the blocking one-shot `download_file` / `upload_file` commands with a persistent, resumable, progress-reporting transfer queue. Transfers survive app restart (SQLite), run N at a time, stream through the existing `StorageBackend` trait, report progress as Tauri events, and support pause / resume / cancel / retry. The frontend grows a 1Password-8-density transfers panel and a toolbar activity indicator; the toolbar Upload action enqueues (multi-file) instead of blocking.

**Architecture:** The engine and its store live in the UI-agnostic `wonderblob-core` crate (new `transfer/` module: `model.rs`, `store.rs`, `engine.rs`) so the whole queue is testable with a mock backend and an in-memory SQLite DB — **no Tauri needed for the core tests**. Core stays Tauri-free by depending on two injected seams: a `BackendResolver` (maps a `connection_id` → `Arc<dyn StorageBackend>`) and an `EventSink` (emits progress/state). The `src-tauri` crate implements both — the resolver over `AppState`'s connection map, the sink over `AppHandle::emit` — constructs the engine in Tauri's `setup()` with the SQLite file under the app-data dir, and exposes commands + the `transfer://progress` / `transfer://state` events.

**Why core owns the engine:** the streaming/queue/resume/retry logic is protocol-agnostic and the highest-value thing to test deterministically (failure injection, restart recovery). Tauri only wires events, the SQLite path, and the connection map. Putting the engine in core keeps it driveable from a plain `#[tokio::test]` with a `MockBackend`, exactly as the contract suite drives backends today.

**Tech Stack:** Rust (`rusqlite` bundled, tokio, async-trait, `tokio-util` io, existing `bytes`/`futures`), Tauri 2.x events, Svelte 5 runes + `@tauri-apps/api/event`.

**Spec:** `docs/superpowers/specs/2026-06-07-wonderblob-design.md` (§ "TransferEngine", § "v1 scope")
**Builds on:** `docs/superpowers/plans/2026-06-07-foundation-sftp-slice.md` (Plan 1 — merged), `docs/superpowers/plans/2026-06-08-s3-azure-backends.md` (Plan 2 — merged)

**Crate-API caveat:** `rusqlite` 0.3x APIs are stable but check the current minor with `cargo add rusqlite --dry-run`; pin what you find. `rusqlite::Connection` is `Send` but **not `Sync`**, so the store wraps it in a `Mutex` (synchronous calls held briefly; the engine's slow work is the network I/O, not the DB). `tokio-util` is already a dependency with the `io` feature; this plan adds the `rt` feature for `CancellationToken` — verify the feature name on docs.rs for the pinned `tokio-util`.

**Trait constraint (do NOT change in this plan):** `StorageBackend` stays exactly as Plans 1–2 defined it — `read(&self, path, offset) -> Box<dyn AsyncRead + Send + Unpin>` and `write(&self, path) -> Box<dyn AsyncWrite + Send + Unpin>`. Downloads resume by re-`read`ing from `offset == transferred_bytes`. Uploads cannot resume (see the asymmetry note below) and the trait is not extended to make them.

**Upload-resume asymmetry (read before coding — this shapes the whole engine):**
- **Downloads resume from offset.** SFTP `read` seeks; S3/Azure `read` issues a ranged GET (`bytes={offset}-`). The worker re-opens at `transferred_bytes` and appends to the partial local file. Pause/crash/retry all resume mid-file.
- **Uploads restart from 0 in v1.** `write(path)` returns a *create/replace* `AsyncWrite`; the `S3MultipartWriter` / `AzBlockWriter` hold their multipart-upload-id / staged-block list **in memory only** (no persisted upload session), and SFTP `write` truncates. There is no resumable-upload-session persistence yet. So on pause/retry/restart an upload **resets `transferred_bytes` to 0 and re-streams the whole local file.** This is enforced in one place (`reset_upload_offset`) and surfaced honestly in the UI ("Uploads restart from the beginning") and in Explicitly-deferred. Resumable upload sessions (S3 persisted multipart-upload-id + part list, Azure staged uncommitted blocks, SFTP append) are a tracked post-v1 enhancement.

---

### Task 1: Transfer model — direction, status, record

The serializable domain types every other layer shares. Pure data + (de)serialization; no DB, no I/O yet.

**Files:**
- Create: `crates/wonderblob-core/src/transfer/mod.rs`
- Create: `crates/wonderblob-core/src/transfer/model.rs`
- Modify: `crates/wonderblob-core/src/lib.rs` (add `pub mod transfer;`)
- Test: inline `#[cfg(test)]` in `model.rs`

- [ ] **Step 1: Write the failing test**

In `crates/wonderblob-core/src/transfer/model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_serializes_camel_case_for_frontend() {
        let t = Transfer {
            id: 7,
            connection_id: 3,
            direction: Direction::Down,
            remote_path: "/wbtest/big.bin".into(),
            local_path: "/home/jack/Downloads/big.bin".into(),
            name: "big.bin".into(),
            total_bytes: Some(1024),
            transferred_bytes: 512,
            status: TransferStatus::Running,
            error: None,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_000_500,
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["connectionId"], 3);
        assert_eq!(v["direction"], "down");
        assert_eq!(v["status"], "running");
        assert_eq!(v["totalBytes"], 1024);
        assert_eq!(v["transferredBytes"], 512);
    }

    #[test]
    fn status_is_terminal_distinguishes_done_from_active() {
        assert!(TransferStatus::Completed.is_terminal());
        assert!(TransferStatus::Failed.is_terminal());
        assert!(TransferStatus::Canceled.is_terminal());
        assert!(!TransferStatus::Running.is_terminal());
        assert!(!TransferStatus::Queued.is_terminal());
        assert!(!TransferStatus::Paused.is_terminal());
    }

    #[test]
    fn status_round_trips_through_str() {
        for s in [
            TransferStatus::Queued,
            TransferStatus::Running,
            TransferStatus::Paused,
            TransferStatus::Completed,
            TransferStatus::Failed,
            TransferStatus::Canceled,
        ] {
            assert_eq!(TransferStatus::from_str(s.as_str()).unwrap(), s);
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core transfer::model`
Expected: FAIL — types not defined.

- [ ] **Step 3: Implement the model**

`crates/wonderblob-core/src/transfer/mod.rs`:

```rust
//! Persistent, resumable transfer queue (spec: "TransferEngine"). The engine and
//! its SQLite store live here so they're testable without Tauri; the app layer
//! injects a `BackendResolver` and an `EventSink`.

pub mod engine;
pub mod model;
pub mod store;

pub use engine::{
    BackendResolver, EngineConfig, EventSink, TransferEngine, TransferEvent, TransferUpdate,
};
pub use model::{Direction, Transfer, TransferStatus, TransferId};
pub use store::TransferStore;
```

(`engine`/`store` are created in Tasks 3–5; for now this `mod.rs` won't compile until they exist. To keep Task 1 self-contained, temporarily reduce `mod.rs` to just `pub mod model;` and add `pub mod engine; pub mod store;` + the re-exports in the tasks that create them. Track this so the re-export block lands with Task 5.)

`crates/wonderblob-core/src/transfer/model.rs` (above the tests):

```rust
use serde::Serialize;

pub type TransferId = i64;

/// Transfer direction. `Down` = remote→local, `Up` = local→remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Up,
    Down,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "up" => Some(Direction::Up),
            "down" => Some(Direction::Down),
            _ => None,
        }
    }
}

/// Lifecycle. Terminal states never transition again without an explicit
/// re-enqueue (resume/retry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferStatus {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Canceled,
}

impl TransferStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TransferStatus::Queued => "queued",
            TransferStatus::Running => "running",
            TransferStatus::Paused => "paused",
            TransferStatus::Completed => "completed",
            TransferStatus::Failed => "failed",
            TransferStatus::Canceled => "canceled",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => TransferStatus::Queued,
            "running" => TransferStatus::Running,
            "paused" => TransferStatus::Paused,
            "completed" => TransferStatus::Completed,
            "failed" => TransferStatus::Failed,
            "canceled" => TransferStatus::Canceled,
            _ => return None,
        })
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Canceled
        )
    }
}

/// One row of the `transfers` table; the unit the engine and UI exchange.
/// `transferred_bytes` doubles as the **resume offset** for downloads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: TransferId,
    pub connection_id: u64,
    pub direction: Direction,
    pub remote_path: String,
    pub local_path: String,
    /// Display name (basename of the file being moved).
    pub name: String,
    pub total_bytes: Option<u64>,
    pub transferred_bytes: u64,
    pub status: TransferStatus,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
```

Add `pub mod transfer;` to `crates/wonderblob-core/src/lib.rs`. For this task only, `transfer/mod.rs` should contain just `pub mod model;` (the `engine`/`store` lines and re-exports come with Tasks 3–5).

- [ ] **Step 4: Run tests**

Run: `cargo test -p wonderblob-core transfer::model`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): transfer model — Direction, TransferStatus, Transfer record"
```

---

### Task 2: TransferStore — SQLite (rusqlite bundled)

The persistent queue: schema + CRUD + `load_incomplete` (startup recovery) + throttle-friendly progress/status updates.

**Files:**
- Create: `crates/wonderblob-core/src/transfer/store.rs`
- Modify: `crates/wonderblob-core/src/transfer/mod.rs` (add `pub mod store;`)
- Modify: `crates/wonderblob-core/Cargo.toml`
- Test: inline `#[cfg(test)]` in `store.rs`

- [ ] **Step 1: Add the dependency**

In `crates/wonderblob-core/Cargo.toml` under `[dependencies]` (check current minor with `cargo add rusqlite --dry-run`):

```toml
rusqlite = { version = "0.32", features = ["bundled"] }
```

`bundled` compiles SQLite in — no system libsqlite needed (matches the cross-platform goal).

- [ ] **Step 2: Write the failing tests**

In `crates/wonderblob-core/src/transfer/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::model::{Direction, TransferStatus};

    fn mem() -> TransferStore {
        TransferStore::open_in_memory().expect("open :memory:")
    }

    fn sample(store: &TransferStore, conn: u64, dir: Direction) -> TransferId {
        store
            .insert(NewTransfer {
                connection_id: conn,
                direction: dir,
                remote_path: "/wbtest/a.bin".into(),
                local_path: "/tmp/a.bin".into(),
                name: "a.bin".into(),
                total_bytes: Some(1000),
            })
            .expect("insert")
    }

    #[test]
    fn insert_get_round_trips_as_queued() {
        let s = mem();
        let id = sample(&s, 1, Direction::Down);
        let t = s.get(id).unwrap().unwrap();
        assert_eq!(t.status, TransferStatus::Queued);
        assert_eq!(t.transferred_bytes, 0);
        assert_eq!(t.total_bytes, Some(1000));
        assert_eq!(t.name, "a.bin");
    }

    #[test]
    fn update_progress_and_status_persist() {
        let s = mem();
        let id = sample(&s, 1, Direction::Down);
        s.update_progress(id, 512, Some(1000)).unwrap();
        s.set_status(id, TransferStatus::Running, None).unwrap();
        let t = s.get(id).unwrap().unwrap();
        assert_eq!(t.transferred_bytes, 512);
        assert_eq!(t.status, TransferStatus::Running);
        // updated_at advances on writes.
        assert!(t.updated_at_ms >= t.created_at_ms);
    }

    #[test]
    fn set_status_records_error_text() {
        let s = mem();
        let id = sample(&s, 1, Direction::Up);
        s.set_status(id, TransferStatus::Failed, Some("network reset")).unwrap();
        let t = s.get(id).unwrap().unwrap();
        assert_eq!(t.status, TransferStatus::Failed);
        assert_eq!(t.error.as_deref(), Some("network reset"));
    }

    #[test]
    fn load_incomplete_returns_only_non_terminal() {
        let s = mem();
        let running = sample(&s, 1, Direction::Down);
        let done = sample(&s, 1, Direction::Down);
        let paused = sample(&s, 1, Direction::Up);
        s.set_status(running, TransferStatus::Running, None).unwrap();
        s.set_status(done, TransferStatus::Completed, None).unwrap();
        s.set_status(paused, TransferStatus::Paused, None).unwrap();
        let mut ids: Vec<_> = s.load_incomplete().unwrap().into_iter().map(|t| t.id).collect();
        ids.sort();
        assert_eq!(ids, vec![running, paused]); // completed excluded
    }

    #[test]
    fn list_orders_newest_first_and_clear_completed_prunes() {
        let s = mem();
        let a = sample(&s, 1, Direction::Down);
        let b = sample(&s, 1, Direction::Down);
        s.set_status(a, TransferStatus::Completed, None).unwrap();
        let listed: Vec<_> = s.list().unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(listed, vec![b, a]); // created_at desc (id desc as tiebreak)
        let pruned = s.clear_completed().unwrap();
        assert_eq!(pruned, 1);
        let remaining: Vec<_> = s.list().unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(remaining, vec![b]);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p wonderblob-core transfer::store`
Expected: FAIL — `TransferStore` not defined.

- [ ] **Step 4: Implement the store**

`crates/wonderblob-core/src/transfer/store.rs` (above the tests):

```rust
use crate::error::{Result, StorageError};
use crate::transfer::model::{Direction, Transfer, TransferId, TransferStatus};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;
use std::sync::Mutex;

/// Fields supplied at enqueue time; the store assigns id/status/timestamps.
pub struct NewTransfer {
    pub connection_id: u64,
    pub direction: Direction,
    pub remote_path: String,
    pub local_path: String,
    pub name: String,
    pub total_bytes: Option<u64>,
}

/// SQLite-backed persistent queue. `Connection` isn't `Sync`, so it lives behind
/// a `Mutex`; locks are held only for the (fast) row read/write, never across
/// network I/O.
pub struct TransferStore {
    conn: Mutex<Connection>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn map_db<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Other { detail: format!("transfer store: {e}") }
}

impl TransferStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_db)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(map_db)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS transfers (
                 id                INTEGER PRIMARY KEY AUTOINCREMENT,
                 connection_id     INTEGER NOT NULL,
                 direction         TEXT    NOT NULL,
                 remote_path       TEXT    NOT NULL,
                 local_path        TEXT    NOT NULL,
                 name              TEXT    NOT NULL,
                 total_bytes       INTEGER,
                 transferred_bytes INTEGER NOT NULL DEFAULT 0,
                 status            TEXT    NOT NULL,
                 error             TEXT,
                 created_at_ms     INTEGER NOT NULL,
                 updated_at_ms     INTEGER NOT NULL
             );",
        )
        .map_err(map_db)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn insert(&self, t: NewTransfer) -> Result<TransferId> {
        let conn = self.conn.lock().unwrap();
        let now = now_ms();
        conn.execute(
            "INSERT INTO transfers
               (connection_id, direction, remote_path, local_path, name,
                total_bytes, transferred_bytes, status, error, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 'queued', NULL, ?7, ?7)",
            params![
                t.connection_id as i64,
                t.direction.as_str(),
                t.remote_path,
                t.local_path,
                t.name,
                t.total_bytes.map(|b| b as i64),
                now,
            ],
        )
        .map_err(map_db)?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get(&self, id: TransferId) -> Result<Option<Transfer>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT * FROM transfers WHERE id = ?1", params![id], row_to_transfer)
            .optional()
            .map_err(map_db)
    }

    /// Newest-first; powers `list_transfers`.
    pub fn list(&self) -> Result<Vec<Transfer>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT * FROM transfers ORDER BY created_at_ms DESC, id DESC")
            .map_err(map_db)?;
        let rows = stmt.query_map([], row_to_transfer).map_err(map_db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_db)
    }

    /// Non-terminal rows, oldest-first (so startup re-enqueues in FIFO order).
    pub fn load_incomplete(&self) -> Result<Vec<Transfer>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT * FROM transfers
                 WHERE status IN ('queued','running','paused')
                 ORDER BY created_at_ms ASC, id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt.query_map([], row_to_transfer).map_err(map_db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_db)
    }

    pub fn update_progress(
        &self,
        id: TransferId,
        transferred: u64,
        total: Option<u64>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transfers
                SET transferred_bytes = ?2,
                    total_bytes = COALESCE(?3, total_bytes),
                    updated_at_ms = ?4
              WHERE id = ?1",
            params![id, transferred as i64, total.map(|b| b as i64), now_ms()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    pub fn set_status(
        &self,
        id: TransferId,
        status: TransferStatus,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transfers SET status = ?2, error = ?3, updated_at_ms = ?4 WHERE id = ?1",
            params![id, status.as_str(), error, now_ms()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// Upload resume is not supported (see plan header); rewind the offset so a
    /// re-run re-streams the whole file. Single source of the asymmetry.
    pub fn reset_upload_offset(&self, id: TransferId) -> Result<()> {
        self.update_progress(id, 0, None)
    }

    pub fn delete(&self, id: TransferId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM transfers WHERE id = ?1", params![id]).map_err(map_db)?;
        Ok(())
    }

    /// Remove completed rows; returns the number pruned.
    pub fn clear_completed(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM transfers WHERE status = 'completed'", [])
            .map_err(map_db)
    }
}

fn row_to_transfer(row: &Row<'_>) -> rusqlite::Result<Transfer> {
    let dir_s: String = row.get("direction")?;
    let status_s: String = row.get("status")?;
    Ok(Transfer {
        id: row.get("id")?,
        connection_id: row.get::<_, i64>("connection_id")? as u64,
        direction: Direction::from_str(&dir_s).unwrap_or(Direction::Down),
        remote_path: row.get("remote_path")?,
        local_path: row.get("local_path")?,
        name: row.get("name")?,
        total_bytes: row.get::<_, Option<i64>>("total_bytes")?.map(|b| b as u64),
        transferred_bytes: row.get::<_, i64>("transferred_bytes")? as u64,
        status: TransferStatus::from_str(&status_s).unwrap_or(TransferStatus::Failed),
        error: row.get("error")?,
        created_at_ms: row.get("created_at_ms")?,
        updated_at_ms: row.get("updated_at_ms")?,
    })
}
```

Add `pub mod store;` to `transfer/mod.rs`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p wonderblob-core transfer::store`
Expected: 5 passed. (If a `rusqlite` accessor name differs in the pinned minor — e.g. `OptionalExtension` path or `query_row` by column name — check docs.rs and adapt; keep behavior identical.)

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(core): TransferStore — SQLite persistent queue (CRUD, load_incomplete, clear_completed)"
```

---

### Task 3: Engine seams — BackendResolver, EventSink, config, events

The traits and value types the engine depends on instead of Tauri. Defined separately so Task 4's `MockBackend` and Task 5's engine compile against a stable surface.

**Files:**
- Create: `crates/wonderblob-core/src/transfer/engine.rs` (seams only this task; worker logic in Task 5)
- Modify: `crates/wonderblob-core/src/transfer/mod.rs` (add `pub mod engine;` + re-exports)
- Modify: `crates/wonderblob-core/Cargo.toml` (`tokio-util` `rt` feature)

- [ ] **Step 1: Add the cancellation feature**

In `crates/wonderblob-core/Cargo.toml`, extend the existing `tokio-util` line:

```toml
tokio-util = { version = "0.7.18", features = ["io", "rt"] }
```

(`CancellationToken` lives in `tokio_util::sync`; verify the gating feature on docs.rs for the pinned version — recent `tokio-util` exposes it under `rt`. If it's behind a different feature, use that.)

- [ ] **Step 2: Implement the seams**

`crates/wonderblob-core/src/transfer/engine.rs`:

```rust
use crate::transfer::model::{Transfer, TransferId, TransferStatus};
use crate::vfs::StorageBackend;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;

/// Resolves a live connection id to its backend. The app layer implements this
/// over `AppState`'s connection map; tests implement it over a `MockBackend`.
/// Returns `None` when the connection is gone (e.g. after a restart, before the
/// bookmark is reconnected) — the worker then parks the transfer as `Paused`.
#[async_trait]
pub trait BackendResolver: Send + Sync {
    async fn resolve(&self, connection_id: u64) -> Option<Arc<dyn StorageBackend>>;
}

/// What the engine emits. The app layer forwards these to the webview as
/// `transfer://progress` (Progress) and `transfer://state` (State).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferUpdate {
    pub id: TransferId,
    pub transferred_bytes: u64,
    pub total_bytes: Option<u64>,
    /// Derived instantaneous rate over the last throttle window.
    pub bytes_per_sec: u64,
}

/// The two event channels, kept as one enum so a single `EventSink` covers both.
#[derive(Debug, Clone)]
pub enum TransferEvent {
    /// Throttled byte progress (no status change).
    Progress(TransferUpdate),
    /// A status transition — carries the full record for the UI to reconcile.
    State(Transfer),
}

/// Where events go. The app layer maps these onto `AppHandle::emit`; tests use a
/// collecting sink.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: TransferEvent);
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Concurrent workers (spec: "N parallel transfer workers, configurable").
    pub max_workers: usize,
    /// Emit a progress event at most this often per transfer.
    pub progress_interval_ms: u64,
    /// Stream chunk size.
    pub chunk_bytes: usize,
    /// Retry attempts for *retryable* errors before giving up.
    pub max_retries: u32,
    /// Base backoff; attempt n waits base * 2^n (capped).
    pub backoff_base_ms: u64,
    pub backoff_cap_ms: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_workers: 3,
            progress_interval_ms: 200,
            chunk_bytes: 256 * 1024,
            max_retries: 4,
            backoff_base_ms: 500,
            backoff_cap_ms: 15_000,
        }
    }
}

/// Per-transfer cooperative control flag, checked between chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    Run,
    Pause,
    Cancel,
}

/// Outcome of one streaming attempt, so the worker loop can decide retry vs stop.
pub(crate) enum Outcome {
    Completed,
    Paused,
    Canceled,
    /// Failed; bool = retryable.
    Failed(String, bool),
}

// `TransferEngine` and its worker logic are implemented in Task 5.
pub struct TransferEngine;
```

Replace `transfer/mod.rs` with the full version from Task 1 Step 3 (the one that `pub mod engine;` + re-exports `BackendResolver, EngineConfig, EventSink, TransferEngine, TransferEvent, TransferUpdate, …`). The re-export of `TransferEngine` resolves now that the placeholder struct exists.

- [ ] **Step 3: Build**

Run: `cargo build -p wonderblob-core`
Expected: success (placeholder `TransferEngine` only).

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(core): transfer engine seams — BackendResolver, EventSink, events, config"
```

---

### Task 4: MockBackend — deterministic, failure-injectable test backend

A `StorageBackend` whose `read`/`write` are in-memory and deterministic, and which can inject a failure after exactly K bytes to exercise resume/retry. Reused by every engine test.

**Files:**
- Create: `crates/wonderblob-core/src/transfer/mock.rs`
- Modify: `crates/wonderblob-core/src/transfer/mod.rs` (add `#[cfg(test)] pub mod mock;` — test-only)
- Test: a smoke test inside `mock.rs`

- [ ] **Step 1: Write the failing smoke test**

In `crates/wonderblob-core/src/transfer/mock.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::StorageBackend;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn read_serves_bytes_and_honors_offset() {
        let b = MockBackend::new();
        b.put("/f.bin", vec![7u8; 100]).await;
        let mut r = b.read("/f.bin", 40).await.unwrap();
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf.len(), 60); // 100 - 40 offset
    }

    #[tokio::test]
    async fn read_fails_after_injected_byte_count() {
        let b = MockBackend::new();
        b.put("/f.bin", vec![1u8; 100]).await;
        b.fail_read_after(30); // first read attempt dies after 30 bytes
        let mut r = b.read("/f.bin", 0).await.unwrap();
        let mut buf = [0u8; 100];
        let mut total = 0;
        let err = loop {
            match r.read(&mut buf[total..]).await {
                Ok(0) => break None,
                Ok(n) => total += n,
                Err(e) => break Some(e),
            }
        };
        assert!(err.is_some());
        assert!(total <= 30);
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let b = MockBackend::new();
        let mut w = b.write("/out.bin").await.unwrap();
        w.write_all(&[9u8; 50]).await.unwrap();
        w.shutdown().await.unwrap();
        assert_eq!(b.get("/out.bin").await.unwrap().len(), 50);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core transfer::mock`
Expected: FAIL — `MockBackend` undefined.

- [ ] **Step 3: Implement the mock**

`crates/wonderblob-core/src/transfer/mock.rs` (above the tests). Files are `Arc<Mutex<HashMap<String, Vec<u8>>>>`; an injected `fail_read_after` makes the next `read` return an `io::Error` after K bytes (mapped by the engine to a retryable `StorageError::Network` via a marker the read returns). Implement the reader as a small `AsyncRead` over a byte cursor that errors at the threshold; the writer collects into a buffer committed on `poll_shutdown`.

```rust
use crate::error::{Result, StorageError};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Clone)]
pub struct MockBackend {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// >= 0 means "next read errors after this many bytes, then disarms".
    fail_read_after: Arc<AtomicI64>,
    /// Network up/down toggle: when false, read/write open() returns Network err.
    online: Arc<AtomicBool>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(HashMap::new())),
            fail_read_after: Arc::new(AtomicI64::new(-1)),
            online: Arc::new(AtomicBool::new(true)),
        }
    }
    pub async fn put(&self, path: &str, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(path.to_string(), bytes);
    }
    pub async fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned()
    }
    /// Arm a one-shot mid-read failure after `n` bytes (auto-disarms when it fires).
    pub fn fail_read_after(&self, n: u64) {
        self.fail_read_after.store(n as i64, Ordering::SeqCst);
    }
    pub fn set_online(&self, up: bool) {
        self.online.store(up, Ordering::SeqCst);
    }
}

struct MockReader {
    data: Vec<u8>,
    pos: usize,
    /// Remaining bytes before the armed failure fires; -1 disarmed.
    fail_after: Arc<AtomicI64>,
    served: usize,
}

impl AsyncRead for MockReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let threshold = self.fail_after.load(Ordering::SeqCst);
        if threshold >= 0 && self.served as i64 >= threshold {
            self.fail_after.store(-1, Ordering::SeqCst); // disarm: retry succeeds
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionReset, "injected")));
        }
        let remaining = self.data.len() - self.pos;
        if remaining == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut n = remaining.min(buf.remaining());
        if threshold >= 0 {
            n = n.min((threshold - self.served as i64).max(0) as usize);
            if n == 0 {
                self.fail_after.store(-1, Ordering::SeqCst);
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionReset, "injected")));
            }
        }
        buf.put_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        self.served += n;
        Poll::Ready(Ok(()))
    }
}

struct MockWriter {
    path: String,
    buf: Vec<u8>,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl AsyncWrite for MockWriter {
    fn poll_write(mut self: Pin<&mut Self>, _cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        self.buf.extend_from_slice(data);
        Poll::Ready(Ok(data.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.files.lock().unwrap().insert(this.path.clone(), std::mem::take(&mut this.buf));
        Poll::Ready(Ok(()))
    }
}

#[async_trait]
impl StorageBackend for MockBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn list(&self, _path: &str) -> Result<Vec<Entry>> {
        Ok(vec![])
    }
    async fn stat(&self, path: &str) -> Result<Entry> {
        let len = self.get(path).await.map(|b| b.len() as u64);
        match len {
            Some(size) => Ok(Entry {
                name: path.rsplit('/').next().unwrap_or(path).into(),
                path: path.into(),
                kind: EntryKind::File,
                size: Some(size),
                modified_ms: None,
            }),
            None => Err(StorageError::NotFound { path: path.into() }),
        }
    }
    async fn read(&self, path: &str, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        if !self.online.load(Ordering::SeqCst) {
            return Err(StorageError::Network { detail: "offline".into() });
        }
        let all = self.get(path).await.ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        let start = (offset as usize).min(all.len());
        Ok(Box::new(MockReader {
            data: all[start..].to_vec(),
            pos: 0,
            fail_after: self.fail_read_after.clone(),
            served: 0,
        }))
    }
    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        if !self.online.load(Ordering::SeqCst) {
            return Err(StorageError::Network { detail: "offline".into() });
        }
        Ok(Box::new(MockWriter { path: path.into(), buf: Vec::new(), files: self.files.clone() }))
    }
    async fn delete(&self, path: &str) -> Result<()> {
        self.files.lock().unwrap().remove(path);
        Ok(())
    }
    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Ok(())
    }
    async fn mkdir(&self, _path: &str) -> Result<()> {
        Ok(())
    }
    async fn share_link(&self, _path: &str, _expiry_secs: u64) -> Result<String> {
        Err(StorageError::Unsupported { op: "share_link".into() })
    }
}
```

In `transfer/mod.rs` add: `#[cfg(test)] pub mod mock;`

- [ ] **Step 4: Run tests**

Run: `cargo test -p wonderblob-core transfer::mock`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(core): MockBackend — deterministic, failure-injectable StorageBackend for engine tests"
```

---

### Task 5: TransferEngine — workers, download streaming, progress, concurrency cap

The heart: an engine that owns the store, resolver, sink, and config; spawns transfers as tokio tasks gated by a semaphore (the worker cap); streams downloads through `read(offset)` into the local file; throttles progress to the store + sink. Pause/resume/cancel/retry come in Tasks 6–8; this task does the happy-path download + cap + progress so its tests pin the core loop.

**Files:**
- Modify: `crates/wonderblob-core/src/transfer/engine.rs` (replace the placeholder `TransferEngine`)
- Test: `crates/wonderblob-core/tests/transfer_engine.rs` (integration test using `pub` engine API + a test `BackendResolver`/`EventSink`)

- [ ] **Step 1: Write the failing tests**

`crates/wonderblob-core/tests/transfer_engine.rs`:

```rust
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wonderblob_core::transfer::engine::{
    BackendResolver, EngineConfig, EventSink, TransferEngine, TransferEvent,
};
use wonderblob_core::transfer::mock::MockBackend;
use wonderblob_core::transfer::model::{Direction, TransferStatus};
use wonderblob_core::transfer::store::{NewTransfer, TransferStore};
use wonderblob_core::vfs::StorageBackend;

// --- test seams -----------------------------------------------------------

struct OneBackend(Arc<MockBackend>);
#[async_trait::async_trait]
impl BackendResolver for OneBackend {
    async fn resolve(&self, _id: u64) -> Option<Arc<dyn StorageBackend>> {
        Some(self.0.clone())
    }
}

#[derive(Default, Clone)]
struct CollectSink(Arc<Mutex<Vec<TransferEvent>>>);
impl EventSink for CollectSink {
    fn emit(&self, event: TransferEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn fast_cfg(max_workers: usize) -> EngineConfig {
    EngineConfig { max_workers, progress_interval_ms: 1, chunk_bytes: 16 * 1024, ..Default::default() }
}

async fn settle<F: Fn() -> bool>(pred: F) {
    for _ in 0..200 {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never settled");
}

// --- tests ----------------------------------------------------------------

#[tokio::test]
async fn download_completes_and_writes_local_file() {
    let backend = Arc::new(MockBackend::new());
    backend.put("/r.bin", vec![5u8; 200_000]).await;
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(2),
    );
    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: "/r.bin".into(),
            local_path: local.to_string_lossy().into(),
            name: "r.bin".into(),
            total_bytes: Some(200_000),
        })
        .await
        .unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(std::fs::metadata(&local).unwrap().len(), 200_000);
}

#[tokio::test]
async fn progress_is_monotonic_and_reaches_total() {
    let backend = Arc::new(MockBackend::new());
    backend.put("/r.bin", vec![1u8; 500_000]).await;
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let sink = Arc::new(CollectSink::default());
    let tmp = tempfile::tempdir().unwrap();
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        sink.clone(),
        fast_cfg(1),
    );
    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: "/r.bin".into(),
            local_path: tmp.path().join("r.bin").to_string_lossy().into(),
            name: "r.bin".into(),
            total_bytes: Some(500_000),
        })
        .await
        .unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    let mut last = 0u64;
    for ev in sink.0.lock().unwrap().iter() {
        if let TransferEvent::Progress(u) = ev {
            assert!(u.transferred_bytes >= last, "progress went backwards");
            last = u.transferred_bytes;
        }
    }
    assert_eq!(store.get(id).unwrap().unwrap().transferred_bytes, 500_000);
}

#[tokio::test]
async fn concurrency_cap_is_respected() {
    // With cap=1, a second enqueue can't start until the first finishes.
    // Use a large file so the first transfer is observably in-flight.
    let backend = Arc::new(MockBackend::new());
    backend.put("/a.bin", vec![1u8; 2_000_000]).await;
    backend.put("/b.bin", vec![2u8; 10]).await;
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(1),
    );
    let a = engine.enqueue(NewTransfer {
        connection_id: 1, direction: Direction::Down, remote_path: "/a.bin".into(),
        local_path: tmp.path().join("a.bin").to_string_lossy().into(), name: "a.bin".into(),
        total_bytes: Some(2_000_000),
    }).await.unwrap();
    let b = engine.enqueue(NewTransfer {
        connection_id: 1, direction: Direction::Down, remote_path: "/b.bin".into(),
        local_path: tmp.path().join("b.bin").to_string_lossy().into(), name: "b.bin".into(),
        total_bytes: Some(10),
    }).await.unwrap();
    // While a is running, b must still be queued (not running).
    settle(|| store.get(a).unwrap().unwrap().status == TransferStatus::Running).await;
    assert_eq!(store.get(b).unwrap().unwrap().status, TransferStatus::Queued);
    settle(|| store.get(b).unwrap().unwrap().status == TransferStatus::Completed).await;
}
```

Add `tempfile = "3"` to `crates/wonderblob-core/Cargo.toml` `[dev-dependencies]`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core --test transfer_engine`
Expected: compile error — `TransferEngine::new` / `enqueue` don't exist.

- [ ] **Step 3: Implement the engine**

Replace the placeholder `pub struct TransferEngine;` in `engine.rs` with the real engine. Key shape: a `Semaphore` caps concurrency; `enqueue` inserts the row then spawns a task that acquires a permit and runs `run_transfer`. A `controls: Mutex<HashMap<TransferId, Arc<AtomicU8>>>` lets pause/cancel (Task 6/7) flip a running transfer's flag, read between chunks.

```rust
use crate::error::StorageError;
use crate::transfer::model::{Direction, TransferStatus, TransferId};
use crate::transfer::store::{NewTransfer, TransferStore};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

const C_RUN: u8 = 0;
const C_PAUSE: u8 = 1;
const C_CANCEL: u8 = 2;

pub struct TransferEngine {
    store: Arc<TransferStore>,
    resolver: Arc<dyn BackendResolver>,
    sink: Arc<dyn EventSink>,
    cfg: EngineConfig,
    permits: Arc<Semaphore>,
    controls: Arc<Mutex<HashMap<TransferId, Arc<AtomicU8>>>>,
}

impl TransferEngine {
    pub fn new(
        store: Arc<TransferStore>,
        resolver: Arc<dyn BackendResolver>,
        sink: Arc<dyn EventSink>,
        cfg: EngineConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            permits: Arc::new(Semaphore::new(cfg.max_workers)),
            store,
            resolver,
            sink,
            cfg,
            controls: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn emit_state(&self, id: TransferId) {
        if let Ok(Some(t)) = self.store.get(id) {
            self.sink.emit(TransferEvent::State(t));
        }
    }

    /// Insert a queued transfer and spawn its worker task. Returns the new id.
    pub async fn enqueue(self: &Arc<Self>, new: NewTransfer) -> crate::error::Result<TransferId> {
        let id = self.store.insert(new)?;
        self.emit_state(id);
        self.clone().spawn(id);
        Ok(id)
    }

    /// Re-enqueue an existing (paused/failed/restart-loaded) transfer.
    pub fn spawn(self: Arc<Self>, id: TransferId) {
        tokio::spawn(async move {
            // Mark queued so the UI shows it waiting for a worker slot.
            let _ = self.store.set_status(id, TransferStatus::Queued, None);
            self.emit_state(id);
            let permit = self.permits.clone().acquire_owned().await.expect("semaphore");
            let control = Arc::new(AtomicU8::new(C_RUN));
            self.controls.lock().unwrap().insert(id, control.clone());
            self.run_transfer(id, control).await;
            self.controls.lock().unwrap().remove(&id);
            drop(permit);
        });
    }

    async fn run_transfer(self: &Arc<Self>, id: TransferId, control: Arc<AtomicU8>) {
        let mut attempt = 0u32;
        loop {
            let t = match self.store.get(id) {
                Ok(Some(t)) => t,
                _ => return,
            };
            // Resolve the backend; if the connection is gone, park as paused.
            let Some(backend) = self.resolver.resolve(t.connection_id).await else {
                let _ = self.store.set_status(id, TransferStatus::Paused, Some("connection not available; reconnect to resume"));
                self.emit_state(id);
                return;
            };
            let _ = self.store.set_status(id, TransferStatus::Running, None);
            self.emit_state(id);

            let outcome = match t.direction {
                Direction::Down => self.stream_download(&t, backend.as_ref(), &control).await,
                Direction::Up => self.stream_upload(&t, backend.as_ref(), &control).await, // Task 8
            };
            match outcome {
                Outcome::Completed => {
                    let _ = self.store.set_status(id, TransferStatus::Completed, None);
                    self.emit_state(id);
                    return;
                }
                Outcome::Paused => {
                    let _ = self.store.set_status(id, TransferStatus::Paused, None);
                    self.emit_state(id);
                    return;
                }
                Outcome::Canceled => {
                    // Cleanup handled in cancel(); just record + stop. (Task 7)
                    let _ = self.store.set_status(id, TransferStatus::Canceled, None);
                    self.emit_state(id);
                    return;
                }
                Outcome::Failed(msg, retryable) => {
                    if retryable && attempt < self.cfg.max_retries {
                        let backoff = (self.cfg.backoff_base_ms << attempt).min(self.cfg.backoff_cap_ms);
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(backoff)).await;
                        continue; // loop re-reads transferred_bytes → download resumes from offset
                    }
                    let _ = self.store.set_status(id, TransferStatus::Failed, Some(&msg));
                    self.emit_state(id);
                    return;
                }
            }
        }
    }

    /// Stream remote→local, appending from `transferred_bytes` (resume offset).
    async fn stream_download(
        &self,
        t: &crate::transfer::model::Transfer,
        backend: &dyn StorageBackend,
        control: &Arc<AtomicU8>,
    ) -> Outcome {
        use std::io::SeekFrom;
        let offset = t.transferred_bytes;
        let mut reader = match backend.read(&t.remote_path, offset).await {
            Ok(r) => r,
            Err(e) => return Outcome::Failed(e.to_string(), e.is_retryable()),
        };
        // Open the partial file, append (truncate to offset for safety).
        let mut file = match tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&t.local_path)
            .await
        {
            Ok(f) => f,
            Err(e) => return Outcome::Failed(e.to_string(), false),
        };
        if let Err(e) = tokio::io::AsyncSeekExt::seek(&mut file, SeekFrom::Start(offset)).await {
            return Outcome::Failed(e.to_string(), false);
        }
        let _ = file.set_len(offset).await; // drop any bytes past the resume point

        let mut transferred = offset;
        let mut chunk = vec![0u8; self.cfg.chunk_bytes];
        let mut last_emit = Instant::now();
        let mut window_bytes = 0u64;
        loop {
            match control.load(Ordering::SeqCst) {
                C_PAUSE => return Outcome::Paused,
                C_CANCEL => return Outcome::Canceled,
                _ => {}
            }
            let n = match reader.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = file.flush().await;
                    // io errors here are transient network resets → retryable.
                    return Outcome::Failed(e.to_string(), true);
                }
            };
            if let Err(e) = file.write_all(&chunk[..n]).await {
                return Outcome::Failed(e.to_string(), false);
            }
            transferred += n as u64;
            window_bytes += n as u64;
            if last_emit.elapsed() >= Duration::from_millis(self.cfg.progress_interval_ms) {
                let secs = last_emit.elapsed().as_secs_f64().max(0.001);
                let rate = (window_bytes as f64 / secs) as u64;
                let _ = self.store.update_progress(t.id, transferred, t.total_bytes);
                self.sink.emit(TransferEvent::Progress(TransferUpdate {
                    id: t.id,
                    transferred_bytes: transferred,
                    total_bytes: t.total_bytes,
                    bytes_per_sec: rate,
                }));
                last_emit = Instant::now();
                window_bytes = 0;
            }
        }
        if let Err(e) = file.flush().await {
            return Outcome::Failed(e.to_string(), false);
        }
        let _ = self.store.update_progress(t.id, transferred, Some(transferred));
        self.sink.emit(TransferEvent::Progress(TransferUpdate {
            id: t.id,
            transferred_bytes: transferred,
            total_bytes: Some(transferred),
            bytes_per_sec: 0,
        }));
        Outcome::Completed
    }

    // stream_upload + pause/resume/cancel/retry come in Tasks 6–8.
}
```

Need `use crate::vfs::StorageBackend;` at the top of `engine.rs` (the seams already import some of these — consolidate imports). Add a temporary `stream_upload` stub returning `Outcome::Failed("upload not yet implemented".into(), false)` so this compiles; Task 8 replaces it.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wonderblob-core --test transfer_engine`
Expected: the three download/progress/cap tests pass. Debug timing-sensitive `settle` loops with `--nocapture` if flaky (raise the iteration count, not the asserts).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): TransferEngine — workers, download streaming, throttled progress, concurrency cap"
```

---

### Task 6: Pause / resume (download resumes from offset)

Pause flips the running transfer's control flag → the loop returns `Paused`, leaving the partial local file and `transferred_bytes` intact. Resume re-spawns; `stream_download` re-opens at `transferred_bytes`.

**Files:**
- Modify: `crates/wonderblob-core/src/transfer/engine.rs` (add `pause`, `resume`)
- Test: extend `crates/wonderblob-core/tests/transfer_engine.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/transfer_engine.rs`:

```rust
#[tokio::test]
async fn pause_keeps_partial_then_resume_completes_from_offset() {
    let backend = Arc::new(MockBackend::new());
    let body: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    backend.put("/r.bin", body.clone()).await;
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(1),
    );
    let id = engine.enqueue(NewTransfer {
        connection_id: 1, direction: Direction::Down, remote_path: "/r.bin".into(),
        local_path: local.to_string_lossy().into(), name: "r.bin".into(),
        total_bytes: Some(1_000_000),
    }).await.unwrap();

    // Pause once some bytes have landed but before completion.
    settle(|| {
        let t = store.get(id).unwrap().unwrap();
        t.status == TransferStatus::Running && t.transferred_bytes > 0
    }).await;
    engine.pause(id).await.unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Paused).await;
    let partial = store.get(id).unwrap().unwrap().transferred_bytes;
    assert!(partial > 0 && partial < 1_000_000);

    // Resume → completes, byte-identical.
    engine.resume(id).await.unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(std::fs::read(&local).unwrap(), body);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core --test transfer_engine pause_keeps_partial`
Expected: FAIL — `pause`/`resume` undefined.

- [ ] **Step 3: Implement pause/resume**

In `engine.rs`:

```rust
impl TransferEngine {
    /// Signal a running transfer to stop at the next chunk boundary, preserving
    /// its partial file + offset. No-op if it isn't currently running.
    pub async fn pause(self: &Arc<Self>, id: TransferId) -> crate::error::Result<()> {
        if let Some(c) = self.controls.lock().unwrap().get(&id) {
            c.store(C_PAUSE, Ordering::SeqCst);
        } else {
            // Queued-but-not-started, or already parked: mark paused directly.
            self.store.set_status(id, TransferStatus::Paused, None)?;
            self.emit_state(id);
        }
        Ok(())
    }

    /// Re-enqueue a paused/failed transfer. Downloads resume from
    /// `transferred_bytes`; uploads reset to 0 first (see header). Optionally
    /// rebind to a fresh connection (restart recovery — Task 9).
    pub async fn resume(self: &Arc<Self>, id: TransferId) -> crate::error::Result<()> {
        let Some(t) = self.store.get(id)? else { return Ok(()); };
        if t.direction == Direction::Up {
            self.store.reset_upload_offset(id)?;
        }
        self.clone().spawn(id);
        Ok(())
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p wonderblob-core --test transfer_engine pause_keeps_partial`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): pause keeps partial + resume completes downloads from offset"
```

---

### Task 7: Cancel (stop + clean partial) + clear_completed

Cancel flips the flag, then deletes the partial local file (download) so no truncated artifact lingers. `clear_completed` removes finished rows.

**Files:**
- Modify: `crates/wonderblob-core/src/transfer/engine.rs` (add `cancel`, `clear_completed`, `list`)
- Test: extend `crates/wonderblob-core/tests/transfer_engine.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn cancel_stops_and_removes_partial_file() {
    let backend = Arc::new(MockBackend::new());
    backend.put("/r.bin", vec![7u8; 1_000_000]).await;
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(1),
    );
    let id = engine.enqueue(NewTransfer {
        connection_id: 1, direction: Direction::Down, remote_path: "/r.bin".into(),
        local_path: local.to_string_lossy().into(), name: "r.bin".into(),
        total_bytes: Some(1_000_000),
    }).await.unwrap();
    settle(|| {
        let t = store.get(id).unwrap().unwrap();
        t.status == TransferStatus::Running && t.transferred_bytes > 0
    }).await;
    engine.cancel(id).await.unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Canceled).await;
    // Give the cleanup a beat, then assert the partial is gone.
    settle(|| !local.exists()).await;
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core --test transfer_engine cancel_stops`
Expected: FAIL — `cancel` undefined.

- [ ] **Step 3: Implement cancel + clear_completed + list**

```rust
impl TransferEngine {
    pub async fn cancel(self: &Arc<Self>, id: TransferId) -> crate::error::Result<()> {
        let running = {
            let map = self.controls.lock().unwrap();
            map.get(&id).map(|c| { c.store(C_CANCEL, Ordering::SeqCst); }).is_some()
        };
        // If it wasn't running (queued/paused/failed), record canceled now.
        let t = self.store.get(id)?;
        if let Some(t) = t {
            if !running {
                self.store.set_status(id, TransferStatus::Canceled, None)?;
                self.emit_state(id);
            }
            // Clean the partial download artifact regardless of running state.
            if t.direction == Direction::Down {
                let _ = tokio::fs::remove_file(&t.local_path).await;
            }
        }
        Ok(())
    }

    pub fn clear_completed(&self) -> crate::error::Result<usize> {
        self.store.clear_completed()
    }

    pub fn list(&self) -> crate::error::Result<Vec<crate::transfer::model::Transfer>> {
        self.store.list()
    }
}
```

> **Cleanup-vs-running race:** when cancel fires on a *running* download, the worker sees `C_CANCEL` and returns `Outcome::Canceled`, which sets status. `cancel()` also removes the file. The worker may still flush one in-flight chunk after `remove_file`, recreating a stub; the `settle(|| !local.exists())` test tolerates one extra remove. To be deterministic, the worker's `Canceled` arm also removes the file: add `let _ = tokio::fs::remove_file(&t.local_path).await;` in the `Outcome::Canceled` branch of `run_transfer` (belt-and-suspenders, idempotent).

Add that remove to the `Outcome::Canceled` arm in `run_transfer`.

- [ ] **Step 4: Run the test**

Run: `cargo test -p wonderblob-core --test transfer_engine cancel_stops`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): cancel stops + cleans partial; clear_completed + list"
```

---

### Task 8: Upload streaming (restart-from-0) + retry-with-backoff

Implement `stream_upload` (local→remote via `write(path)`), enforcing the upload-restart asymmetry, and prove retry-with-backoff recovers a transient mid-download failure.

**Files:**
- Modify: `crates/wonderblob-core/src/transfer/engine.rs` (replace the `stream_upload` stub)
- Test: extend `crates/wonderblob-core/tests/transfer_engine.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn upload_completes_and_writes_remote() {
    let backend = Arc::new(MockBackend::new());
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("up.bin");
    std::fs::write(&local, vec![3u8; 300_000]).unwrap();
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(2),
    );
    let id = engine.enqueue(NewTransfer {
        connection_id: 1, direction: Direction::Up, remote_path: "/up.bin".into(),
        local_path: local.to_string_lossy().into(), name: "up.bin".into(),
        total_bytes: Some(300_000),
    }).await.unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(backend.get("/up.bin").await.unwrap().len(), 300_000);
}

#[tokio::test]
async fn transient_download_failure_is_retried_then_succeeds() {
    let backend = Arc::new(MockBackend::new());
    let body: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
    backend.put("/r.bin", body.clone()).await;
    backend.fail_read_after(50_000); // one injected mid-stream reset
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
    let mut cfg = fast_cfg(1);
    cfg.backoff_base_ms = 1; // keep the test fast
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        cfg,
    );
    let id = engine.enqueue(NewTransfer {
        connection_id: 1, direction: Direction::Down, remote_path: "/r.bin".into(),
        local_path: local.to_string_lossy().into(), name: "r.bin".into(),
        total_bytes: Some(400_000),
    }).await.unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(std::fs::read(&local).unwrap(), body); // resumed past the injected fault
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core --test transfer_engine upload_completes`
Expected: FAIL — upload still stubbed; retry test also fails until upload compiles.

- [ ] **Step 3: Implement upload streaming**

Replace the `stream_upload` stub:

```rust
    /// Stream local→remote. Uploads cannot resume (header asymmetry), so this
    /// always sends the whole file; callers reset `transferred_bytes` to 0 before
    /// a re-run via `reset_upload_offset`.
    async fn stream_upload(
        &self,
        t: &crate::transfer::model::Transfer,
        backend: &dyn StorageBackend,
        control: &Arc<AtomicU8>,
    ) -> Outcome {
        let mut file = match tokio::fs::File::open(&t.local_path).await {
            Ok(f) => f,
            Err(e) => return Outcome::Failed(e.to_string(), false),
        };
        let mut writer = match backend.write(&t.remote_path).await {
            Ok(w) => w,
            Err(e) => return Outcome::Failed(e.to_string(), e.is_retryable()),
        };
        let mut transferred = 0u64;
        let mut chunk = vec![0u8; self.cfg.chunk_bytes];
        let mut last_emit = Instant::now();
        let mut window_bytes = 0u64;
        loop {
            match control.load(Ordering::SeqCst) {
                // Pause/cancel mid-upload: the partial remote object is invalid and
                // a resume re-streams from 0, so treat both as their states; the
                // writer is dropped without shutdown (multipart upload never
                // completes → no partial object materializes for S3/Azure).
                C_PAUSE => return Outcome::Paused,
                C_CANCEL => return Outcome::Canceled,
                _ => {}
            }
            let n = match file.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return Outcome::Failed(e.to_string(), false),
            };
            if let Err(e) = writer.write_all(&chunk[..n]).await {
                return Outcome::Failed(e.to_string(), true);
            }
            transferred += n as u64;
            window_bytes += n as u64;
            if last_emit.elapsed() >= Duration::from_millis(self.cfg.progress_interval_ms) {
                let secs = last_emit.elapsed().as_secs_f64().max(0.001);
                let rate = (window_bytes as f64 / secs) as u64;
                let _ = self.store.update_progress(t.id, transferred, t.total_bytes);
                self.sink.emit(TransferEvent::Progress(TransferUpdate {
                    id: t.id, transferred_bytes: transferred, total_bytes: t.total_bytes, bytes_per_sec: rate,
                }));
                last_emit = Instant::now();
                window_bytes = 0;
            }
        }
        if let Err(e) = writer.shutdown().await {
            // shutdown finalizes the multipart/block upload (Plan 2) → retryable.
            return Outcome::Failed(e.to_string(), true);
        }
        let _ = self.store.update_progress(t.id, transferred, Some(transferred));
        self.sink.emit(TransferEvent::Progress(TransferUpdate {
            id: t.id, transferred_bytes: transferred, total_bytes: Some(transferred), bytes_per_sec: 0,
        }));
        Outcome::Completed
    }
```

> **Upload retry note:** on a retryable upload failure the `run_transfer` loop continues, re-reading the row. Because uploads don't resume, the loop must rewind first. Add to the retry arm: `if t.direction == Direction::Up { let _ = self.store.reset_upload_offset(id); }` immediately before `continue;`. (Downloads keep their offset; uploads restart.)

Add that rewind to the retry arm in `run_transfer`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wonderblob-core --test transfer_engine`
Expected: all engine tests pass (download, progress, cap, pause/resume, cancel, upload, retry).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): upload streaming (restart-from-0) + retry-with-backoff on transient errors"
```

---

### Task 9: Restart recovery — load incomplete, rebind on resume

On engine construction, load incomplete rows; any `running`/`queued` row from a prior session can't run until its connection is re-established, so park them `Paused`. `resume` gains an optional `connection_id` to rebind a restart-loaded transfer to the freshly reconnected backend.

**Files:**
- Modify: `crates/wonderblob-core/src/transfer/engine.rs` (`recover_on_start`, `resume` rebind param)
- Modify: `crates/wonderblob-core/src/transfer/store.rs` (add `rebind_connection`)
- Test: extend `crates/wonderblob-core/tests/transfer_engine.rs`

- [ ] **Step 1: Write the failing test**

Simulates a restart: build an engine on a **file-backed** store, pause a download, drop the engine, build a NEW engine on the same DB file with a fresh resolver, call `recover_on_start`, then `resume` with a rebind to finish.

```rust
#[tokio::test]
async fn restart_loads_incomplete_and_resume_rebinds_to_finish() {
    let body: Vec<u8> = (0..800_000u32).map(|i| (i % 251) as u8).collect();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("transfers.db");
    let local = dir.path().join("r.bin");

    // --- session 1: enqueue, pause mid-way, "crash" (drop engine) ---
    let id;
    {
        let backend = Arc::new(MockBackend::new());
        backend.put("/r.bin", body.clone()).await;
        let store = Arc::new(TransferStore::open(&db).unwrap());
        let engine = TransferEngine::new(
            store.clone(), Arc::new(OneBackend(backend.clone())),
            Arc::new(CollectSink::default()), fast_cfg(1),
        );
        id = engine.enqueue(NewTransfer {
            connection_id: 1, direction: Direction::Down, remote_path: "/r.bin".into(),
            local_path: local.to_string_lossy().into(), name: "r.bin".into(),
            total_bytes: Some(800_000),
        }).await.unwrap();
        settle(|| {
            let t = store.get(id).unwrap().unwrap();
            t.status == TransferStatus::Running && t.transferred_bytes > 0
        }).await;
        engine.pause(id).await.unwrap();
        settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Paused).await;
    } // engine + store dropped

    // --- session 2: reopen DB, recover, reconnect (new conn id 99), rebind+resume ---
    let backend2 = Arc::new(MockBackend::new());
    backend2.put("/r.bin", body.clone()).await;
    let store2 = Arc::new(TransferStore::open(&db).unwrap());
    let engine2 = TransferEngine::new(
        store2.clone(), Arc::new(OneBackend(backend2.clone())),
        Arc::new(CollectSink::default()), fast_cfg(1),
    );
    let loaded = engine2.recover_on_start().unwrap();
    assert_eq!(loaded, 1); // the paused transfer was reloaded
    assert_eq!(store2.get(id).unwrap().unwrap().status, TransferStatus::Paused);

    engine2.resume_with(id, Some(99)).await.unwrap(); // rebind to new connection
    settle(|| store2.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(std::fs::read(&local).unwrap(), body);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core --test transfer_engine restart_loads_incomplete`
Expected: FAIL — `recover_on_start` / `resume_with` undefined.

- [ ] **Step 3: Implement recovery + rebind**

In `store.rs`:

```rust
    pub fn rebind_connection(&self, id: TransferId, connection_id: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transfers SET connection_id = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id, connection_id as i64, now_ms()],
        )
        .map_err(map_db)?;
        Ok(())
    }
```

In `engine.rs`:

```rust
impl TransferEngine {
    /// Call once at startup. Any non-terminal row from a prior session is parked
    /// `Paused` (its connection is gone) so the UI can offer Resume once the
    /// matching bookmark reconnects. Returns the count reloaded.
    pub fn recover_on_start(&self) -> crate::error::Result<usize> {
        let incomplete = self.store.load_incomplete()?;
        for t in &incomplete {
            // Anything that claimed to be running/queued at crash time is parked.
            if t.status != TransferStatus::Paused {
                let _ = self.store.set_status(
                    t.id,
                    TransferStatus::Paused,
                    Some("interrupted by app restart; reconnect to resume"),
                );
            }
            self.sink.emit(TransferEvent::State(self.store.get(t.id)?.unwrap()));
        }
        Ok(incomplete.len())
    }

    /// Resume, optionally rebinding to a fresh connection id (restart recovery).
    pub async fn resume_with(
        self: &Arc<Self>,
        id: TransferId,
        connection_id: Option<u64>,
    ) -> crate::error::Result<()> {
        if let Some(cid) = connection_id {
            self.store.rebind_connection(id, cid)?;
        }
        self.resume(id).await
    }
}
```

Make the existing `resume(id)` delegate: `self.resume_with(id, None).await` — or keep both; the Tauri command (Task 11) calls `resume_with`.

- [ ] **Step 4: Run the test**

Run: `cargo test -p wonderblob-core --test transfer_engine restart_loads_incomplete`
Expected: PASS. Then full core suite: `cargo test -p wonderblob-core` — all green (Docker-gated backend tests skip).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): restart recovery — load incomplete, park as paused, rebind-on-resume"
```

---

### Task 10: Tauri wiring — resolver, sink, engine construction

Wire the engine into the app: a `BackendResolver` over the connection map, an `EventSink` over `AppHandle::emit`, the SQLite file under the app-data dir, constructed in `setup()` and managed alongside `AppState`.

**Files:**
- Modify: `src-tauri/src/state.rs` (share the connection map via `Arc`)
- Create: `src-tauri/src/transfers.rs` (resolver + sink + engine init)
- Modify: `src-tauri/src/lib.rs` (build engine in `setup`, manage it)
- Modify: `src-tauri/Cargo.toml` (no new deps — `tokio-util`/`rusqlite` are transitive via core; the engine type comes from `wonderblob-core`)

- [ ] **Step 1: Make the connection map shareable**

In `src-tauri/src/state.rs`, change the field so the engine's resolver can hold a clone:

```rust
pub type ConnMap = Arc<RwLock<HashMap<ConnectionId, Arc<dyn StorageBackend>>>>;

#[derive(Default)]
pub struct AppState {
    next_id: AtomicU64,
    pub connections: ConnMap,
}
```

`AppState::get`/`remove` and the command-layer `state.connections.write().await` already use the same surface — `Arc<RwLock<…>>` derefs transparently, so those call sites are unchanged. Run `cargo build -p wonderblob` to confirm. (If `register`/`get`/`remove` need a `.clone()` on the `Arc` anywhere, add it; behavior is identical.)

- [ ] **Step 2: Implement resolver, sink, and init**

`src-tauri/src/transfers.rs`:

```rust
use crate::state::ConnMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use wonderblob_core::transfer::engine::{
    BackendResolver, EngineConfig, EventSink, TransferEngine, TransferEvent,
};
use wonderblob_core::transfer::store::TransferStore;
use wonderblob_core::vfs::StorageBackend;

/// Resolves transfer connection ids against the live connection map.
pub struct AppResolver {
    pub conns: ConnMap,
}

#[async_trait::async_trait]
impl BackendResolver for AppResolver {
    async fn resolve(&self, connection_id: u64) -> Option<Arc<dyn StorageBackend>> {
        self.conns.read().await.get(&connection_id).cloned()
    }
}

/// Forwards engine events to the webview as `transfer://progress` / `transfer://state`.
pub struct AppSink {
    pub app: AppHandle,
}

impl EventSink for AppSink {
    fn emit(&self, event: TransferEvent) {
        match event {
            TransferEvent::Progress(u) => {
                let _ = self.app.emit("transfer://progress", u);
            }
            TransferEvent::State(t) => {
                let _ = self.app.emit("transfer://state", t);
            }
        }
    }
}

/// Build the engine, recover prior-session transfers, and return it to be managed.
pub fn init_engine(app: &AppHandle, conns: ConnMap) -> Arc<TransferEngine> {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir");
    let _ = std::fs::create_dir_all(&dir);
    let store = Arc::new(
        TransferStore::open(dir.join("transfers.db")).expect("open transfers.db"),
    );
    let engine = TransferEngine::new(
        store,
        Arc::new(AppResolver { conns }),
        Arc::new(AppSink { app: app.clone() }),
        EngineConfig::default(),
    );
    // Park interrupted transfers; the UI offers Resume after reconnect.
    let _ = engine.recover_on_start();
    engine
}
```

(`TransferUpdate` and `Transfer` both derive `Serialize`, which is all `Emitter::emit` needs. `Emitter` is the Tauri 2 trait providing `emit`; import it as shown.)

- [ ] **Step 3: Construct + manage the engine in `setup`**

In `src-tauri/src/lib.rs`, add `mod transfers;`, then add a `.setup(...)` to the builder chain that builds the engine after `AppState` is managed:

```rust
        .manage(state::AppState::default())
        .setup(|app| {
            let conns = app.state::<state::AppState>().connections.clone();
            let engine = transfers::init_engine(app.handle(), conns);
            app.manage(engine);
            Ok(())
        })
```

- [ ] **Step 4: Build + launch**

Run: `cargo build -p wonderblob && npm run tauri dev` (close the window after it opens).
Expected: clean build; `transfers.db` appears under the app-data dir on first run.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(app): wire TransferEngine — resolver over conn map, sink over AppHandle, init in setup"
```

---

### Task 11: Tauri commands — enqueue / control / list; remove blocking commands

Expose the engine to the frontend and **retire** the blocking `download_file` / `upload_file` commands.

**Files:**
- Modify: `src-tauri/src/commands.rs` (add transfer commands; delete `download_file`, `upload_file`)
- Modify: `src-tauri/src/lib.rs` (update `generate_handler![]`)

**Decision — remove the blocking commands, don't keep them as helpers.** The engine streams through `StorageBackend` directly inside its worker; there's no shared helper for the commands to be. Keeping them would leave a second, non-persistent, no-progress write path that bypasses the queue — exactly the divergence the engine exists to remove. The only other consumer would be Plan 4's EditSession "download to temp then open", which is a distinct small/blocking flow that will read via the backend (or enqueue a download) on its own; it does not need these commands. So both are deleted now.

- [ ] **Step 1: Add the transfer commands**

Append to `src-tauri/src/commands.rs`:

```rust
use std::sync::Arc as StdArc;
use wonderblob_core::transfer::engine::TransferEngine;
use wonderblob_core::transfer::model::{Direction, Transfer, TransferId};
use wonderblob_core::transfer::store::NewTransfer;

fn basename_of(path: &str) -> String {
    path.trim_end_matches('/').rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

#[tauri::command]
pub async fn enqueue_download(
    state: State<'_, AppState>,
    engine: State<'_, StdArc<TransferEngine>>,
    id: ConnectionId,
    remote_path: String,
    local_path: String,
    total_bytes: Option<u64>,
) -> Result<TransferId, StorageError> {
    // Best-effort size for the progress bar if the caller didn't supply it.
    let total = match total_bytes {
        Some(b) => Some(b),
        None => state.get(id).await.ok().and_then(|b| {
            futures::executor::block_on(async { b.stat(&remote_path).await.ok().and_then(|e| e.size) })
        }),
    };
    engine
        .enqueue(NewTransfer {
            connection_id: id,
            direction: Direction::Down,
            name: basename_of(&remote_path),
            remote_path,
            local_path,
            total_bytes: total,
        })
        .await
}

#[tauri::command]
pub async fn enqueue_upload(
    engine: State<'_, StdArc<TransferEngine>>,
    id: ConnectionId,
    local_path: String,
    remote_path: String,
) -> Result<TransferId, StorageError> {
    let total = tokio::fs::metadata(&local_path).await.ok().map(|m| m.len());
    engine
        .enqueue(NewTransfer {
            connection_id: id,
            direction: Direction::Up,
            name: basename_of(&local_path),
            remote_path,
            local_path,
            total_bytes: total,
        })
        .await
}

#[tauri::command]
pub async fn pause_transfer(
    engine: State<'_, StdArc<TransferEngine>>,
    transfer_id: TransferId,
) -> Result<(), StorageError> {
    engine.pause(transfer_id).await
}

#[tauri::command]
pub async fn resume_transfer(
    engine: State<'_, StdArc<TransferEngine>>,
    transfer_id: TransferId,
    connection_id: Option<u64>,
) -> Result<(), StorageError> {
    engine.resume_with(transfer_id, connection_id).await
}

#[tauri::command]
pub async fn cancel_transfer(
    engine: State<'_, StdArc<TransferEngine>>,
    transfer_id: TransferId,
) -> Result<(), StorageError> {
    engine.cancel(transfer_id).await
}

#[tauri::command]
pub async fn list_transfers(
    engine: State<'_, StdArc<TransferEngine>>,
) -> Result<Vec<Transfer>, StorageError> {
    engine.list()
}

#[tauri::command]
pub async fn clear_completed(
    engine: State<'_, StdArc<TransferEngine>>,
) -> Result<usize, StorageError> {
    engine.clear_completed()
}
```

> The `enqueue_download` size lookup via `futures::executor::block_on` inside an async command is awkward — prefer awaiting `stat` directly: replace the `total` block with a plain `async` lookup (`state.get(id).await.ok()` then `.stat(&remote_path).await.ok().and_then(|e| e.size)`), no `block_on`. Use whichever compiles cleanly under the borrow checker; the intent is "use caller-supplied total, else stat, else None". Drop the unused `futures` import if you take the direct path.

Delete the `download_file` and `upload_file` functions from `commands.rs` (and the now-unused `AsyncWriteExt` import if nothing else uses it).

- [ ] **Step 2: Update the handler registration**

In `src-tauri/src/lib.rs`, remove `commands::download_file` and `commands::upload_file` from `generate_handler![]` and add:

```rust
            commands::enqueue_download,
            commands::enqueue_upload,
            commands::pause_transfer,
            commands::resume_transfer,
            commands::cancel_transfer,
            commands::list_transfers,
            commands::clear_completed,
```

- [ ] **Step 3: Build**

Run: `cargo build -p wonderblob`
Expected: clean (no references to the deleted commands remain — the frontend wrappers go in Task 12).

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(app): transfer commands (enqueue/pause/resume/cancel/list/clear); retire blocking download_file/upload_file"
```

---

### Task 12: Frontend API + transfers store + event subscriptions

Typed wrappers for the new commands, a Svelte store reconciling `list_transfers` with the two event streams, and removal of the old `downloadFile`/`uploadFile` wrappers.

**Files:**
- Modify: `src/lib/api.ts` (transfer types + wrappers; remove `downloadFile`/`uploadFile`)
- Create: `src/lib/stores/transfers.ts`

- [ ] **Step 1: Add transfer types + wrappers**

In `src/lib/api.ts`, add:

```ts
export type TransferDirection = "up" | "down";
export type TransferStatus =
  | "queued" | "running" | "paused" | "completed" | "failed" | "canceled";

export interface Transfer {
  id: number;
  connectionId: number;
  direction: TransferDirection;
  remotePath: string;
  localPath: string;
  name: string;
  totalBytes: number | null;
  transferredBytes: number;
  status: TransferStatus;
  error: string | null;
  createdAtMs: number;
  updatedAtMs: number;
}

/** Payload of `transfer://progress`. */
export interface TransferProgress {
  id: number;
  transferredBytes: number;
  totalBytes: number | null;
  bytesPerSec: number;
}
```

In the `api` object, **remove** `downloadFile` and `uploadFile`, and add:

```ts
  enqueueDownload: (id: number, remotePath: string, localPath: string, totalBytes?: number) =>
    invoke<number>("enqueue_download", { id, remotePath, localPath, totalBytes: totalBytes ?? null }),
  enqueueUpload: (id: number, localPath: string, remotePath: string) =>
    invoke<number>("enqueue_upload", { id, localPath, remotePath }),
  pauseTransfer: (transferId: number) => invoke<void>("pause_transfer", { transferId }),
  resumeTransfer: (transferId: number, connectionId?: number) =>
    invoke<void>("resume_transfer", { transferId, connectionId: connectionId ?? null }),
  cancelTransfer: (transferId: number) => invoke<void>("cancel_transfer", { transferId }),
  listTransfers: () => invoke<Transfer[]>("list_transfers"),
  clearCompleted: () => invoke<number>("clear_completed"),
```

- [ ] **Step 2: Transfers store**

`src/lib/stores/transfers.ts` — a writable map keyed by id, reconciled on mount via `listTransfers`, kept live by subscribing to both events. Progress events patch bytes/speed only; state events replace the whole record.

```ts
import { writable, derived, get } from "svelte/store";
import { listen } from "@tauri-apps/api/event";
import { api, type Transfer, type TransferProgress } from "$lib/api";

/** id → Transfer, plus a transient bytesPerSec keyed alongside. */
export const transfers = writable<Map<number, Transfer>>(new Map());
export const transferSpeed = writable<Map<number, number>>(new Map());

/** Newest-first array for rendering. */
export const transferList = derived(transfers, ($t) =>
  [...$t.values()].sort((a, b) => b.createdAtMs - a.createdAtMs)
);

/** Count of active (queued/running/paused) transfers for the toolbar badge. */
export const activeTransferCount = derived(transferList, ($l) =>
  $l.filter((t) => t.status === "queued" || t.status === "running" || t.status === "paused").length
);

let started = false;

export async function initTransfers() {
  if (started) return;
  started = true;

  const initial = await api.listTransfers();
  transfers.set(new Map(initial.map((t) => [t.id, t])));

  await listen<Transfer>("transfer://state", (e) => {
    transfers.update((m) => {
      const next = new Map(m);
      next.set(e.payload.id, e.payload);
      return next;
    });
  });

  await listen<TransferProgress>("transfer://progress", (e) => {
    const p = e.payload;
    transfers.update((m) => {
      const cur = m.get(p.id);
      if (!cur) return m;
      const next = new Map(m);
      next.set(p.id, { ...cur, transferredBytes: p.transferredBytes, totalBytes: p.totalBytes ?? cur.totalBytes });
      return next;
    });
    transferSpeed.update((s) => {
      const next = new Map(s);
      next.set(p.id, p.bytesPerSec);
      return next;
    });
  });
}

export async function clearCompleted() {
  await api.clearCompleted();
  transfers.update((m) => {
    const next = new Map(m);
    for (const [id, t] of next) if (t.status === "completed") next.delete(id);
    return next;
  });
}
```

- [ ] **Step 3: Typecheck**

Run: `npm run check`
Expected: clean (no remaining references to `api.downloadFile`/`api.uploadFile` — Task 13 rewires the upload call site).

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(ui): transfer API wrappers + live transfers store over Tauri events"
```

---

### Task 13: Transfers panel + toolbar indicator; rewire Upload to enqueue (multi-file)

A bottom panel listing active+recent transfers (1Password-8 density, tokens only), per-row pause/resume/cancel, a "Clear completed" action, a toolbar toggle showing the active count, and the Upload action enqueuing every selected file.

**Files:**
- Create: `src/lib/components/TransfersPanel.svelte`
- Create: `src/lib/transfer-format.ts` (speed/percent helpers + test)
- Modify: `src/routes/+page.svelte` (init store, toolbar toggle + count, rewire `upload`, mount panel)

- [ ] **Step 1: Formatting helpers + test**

`src/lib/transfer-format.ts`:

```ts
import { formatSize } from "./format";

/** "1.4 MB/s"; 0 → "". */
export function formatSpeed(bytesPerSec: number): string {
  if (!bytesPerSec) return "";
  return `${formatSize(bytesPerSec)}/s`;
}

/** 0–100 integer; null total → indeterminate (-1). */
export function percent(transferred: number, total: number | null): number {
  if (total === null || total <= 0) return -1;
  return Math.min(100, Math.floor((transferred / total) * 100));
}
```

`src/lib/transfer-format.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { formatSpeed, percent } from "./transfer-format";

describe("transfer-format", () => {
  it("formatSpeed", () => {
    expect(formatSpeed(0)).toBe("");
    expect(formatSpeed(1536)).toBe("1.5 KB/s");
  });
  it("percent", () => {
    expect(percent(0, null)).toBe(-1);
    expect(percent(512, 1024)).toBe(50);
    expect(percent(2000, 1000)).toBe(100);
  });
});
```

Run: `npm test` — expected PASS.

- [ ] **Step 2: TransfersPanel component**

`src/lib/components/TransfersPanel.svelte` — Svelte 5 runes. Requirements:

- Reads `transferList`, `transferSpeed` stores; renders rows at `height: var(--row-height)`.
- Each row: a direction glyph (▼ down / ▲ up — plain text, no new color), the `name`, a progress bar, `%`/bytes (`formatSize(transferredBytes)` / `formatSize(totalBytes)`), and `formatSpeed`. Progress bar fill uses `--accent`; track uses `--bg-hover`. Indeterminate (`percent === -1`) → a subtle striped/animated track (CSS only, ≤150ms-feel, functional not decorative).
- Per-row actions by status: `running` → Pause + Cancel; `paused` → Resume + Cancel; `failed` → Retry (calls `resumeTransfer`) + Cancel; `completed`/`canceled` → no actions (just shows final state, with `error` text on failed). Resume passes the **current** `$activeConnection?.id` as the rebind `connectionId` so restart-loaded transfers re-bind to the live connection.
- Keyboard-accessible, consistent with `FileList`/`BookmarkList`: rows focusable (`tabindex`), ArrowUp/Down move focus, action buttons reachable by Tab; Escape collapses the panel (handled by parent toggle). Use the existing `describeError` for any action failure (surface via the parent's toast or an inline row error).
- Footer strip: "Clear completed" button (calls `clearCompleted()` from the store), right-aligned, ghost style.
- Empty state: "No transfers yet" centered in `--fg-secondary`.

Style strictly from tokens (`--bg-content`, `--bg-sidebar`, `--border`, `--fg-primary`, `--fg-secondary`, `--accent`, `--bg-hover`, `--radius`, `--row-height`, `--text-small`). **No new color tokens** — the progress bar is `--accent` on `--bg-hover`. (If, and only if, the accent-on-hover contrast proves too weak in dark mode during the visual check, add a single `--progress-track` token to `tokens.css` for both themes; prefer not to.)

- [ ] **Step 3: Toolbar toggle + count, rewire upload, mount panel**

In `src/routes/+page.svelte`:

```ts
  import TransfersPanel from "$lib/components/TransfersPanel.svelte";
  import { initTransfers, activeTransferCount } from "$lib/stores/transfers";

  let transfersOpen = $state(false);

  // Start the transfers store once (reconcile + subscribe).
  $effect(() => { initTransfers(); });
```

Replace the `upload()` body to allow multi-select and enqueue each file:

```ts
  async function upload() {
    const conn = $activeConnection;
    if (!conn || uploading) return;
    const selected = await open({ multiple: true, directory: false, title: "Upload files" });
    const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
    if (paths.length === 0) return;
    uploading = true;
    try {
      for (const p of paths) {
        const base = p.replace(/[\\/]+$/, "").split(/[\\/]/).pop();
        if (!base) continue;
        await api.enqueueUpload(conn.id, p, joinPath($currentPath, base));
      }
      transfersOpen = true; // reveal progress
    } catch (e) {
      showToast(opError(e, "Couldn't start upload"));
    } finally {
      uploading = false;
    }
  }
```

(Note: uploads no longer block the toolbar; `fileList?.refresh()` should run when a transfer for the current dir reaches `completed` — wire a small `$effect` that watches `transferList` for a just-completed upload whose remote dir equals `$currentPath` and calls `fileList?.refresh()`. Keep it simple: refresh on any upload completion.)

Add a toolbar button (in `.actions`, before Disconnect) showing the active count:

```svelte
          <button class="ghost" onclick={() => (transfersOpen = !transfersOpen)}>
            Transfers{#if $activeTransferCount > 0} ({$activeTransferCount}){/if}
          </button>
```

Mount the panel below `.browser` (collapsible), e.g.:

```svelte
      {#if transfersOpen}
        <div class="transfers">
          <TransfersPanel onerror={showToast} />
        </div>
      {/if}
```

with a token-only style (top border, `--bg-content`, fixed max-height ~38% with internal scroll).

- [ ] **Step 4: Verify end-to-end against the SFTP fixture**

```bash
./scripts/test-sftp-up.sh
npm run tauri dev
```

Manual checklist:
1. Connect; select multiple files; Upload → all appear in the panel, progress bars advance, count badge increments, files land remotely (refresh shows them).
2. Start a large upload, Cancel mid-way → row goes `canceled`, no broken remote object surfaces on refresh.
3. (Download path is exercised by the gated test in Task 14; if a download UI exists from Plan 4, repeat pause/resume here.)
4. Clear completed removes finished rows.
5. Feel check: density, dark mode, no webview tells, keyboard nav through rows + actions.

- [ ] **Step 5: Typecheck + tests**

```bash
npm run check && npm test
cargo test --workspace
```

Expected: all green (Docker-gated tests skip without env flags).

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(ui): transfers panel + toolbar indicator; Upload enqueues (multi-file)"
```

---

### Task 14: E2E SFTP resume test (gated) + CI

A real pause/resume download against the Dockerized OpenSSH server, asserting byte-identical output; plus the CI step. Gated by `WONDERBLOB_TEST_SFTP=1` so plain `cargo test` stays Docker-free.

**Files:**
- Create: `crates/wonderblob-core/tests/transfer_sftp_resume.rs`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the gated E2E test**

`crates/wonderblob-core/tests/transfer_sftp_resume.rs`:

```rust
//! Real pause/resume against the Dockerized OpenSSH server (Plan 1 fixture).
//! Gated by WONDERBLOB_TEST_SFTP=1; run scripts/test-sftp-up.sh first.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};
use wonderblob_core::transfer::engine::{
    BackendResolver, EngineConfig, EventSink, TransferEngine, TransferEvent,
};
use wonderblob_core::transfer::model::{Direction, TransferStatus};
use wonderblob_core::transfer::store::{NewTransfer, TransferStore};
use wonderblob_core::vfs::StorageBackend;

struct OneBackend(Arc<dyn StorageBackend>);
#[async_trait::async_trait]
impl BackendResolver for OneBackend {
    async fn resolve(&self, _id: u64) -> Option<Arc<dyn StorageBackend>> {
        Some(self.0.clone())
    }
}
struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _e: TransferEvent) {}
}

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

#[tokio::test]
async fn sftp_download_pauses_and_resumes_byte_identical() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    let backend: Arc<dyn StorageBackend> = Arc::new(
        SftpBackend::connect(SftpConfig {
            host: "localhost".into(),
            port: 2222,
            username: "wb".into(),
            auth: SftpAuth::Password("wbpass".into()),
        })
        .await
        .expect("connect"),
    );

    // Stage a multi-MiB remote file via the backend itself.
    let remote = "/config/wb-transfer-big.bin";
    let body: Vec<u8> = (0..4_000_000u32).map(|i| (i % 251) as u8).collect();
    {
        let mut w = backend.write(remote).await.expect("write");
        w.write_all(&body).await.unwrap();
        w.shutdown().await.unwrap();
    }

    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("big.bin");
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    // Small chunk + tiny progress interval so we can pause mid-stream.
    let cfg = EngineConfig { max_workers: 1, chunk_bytes: 32 * 1024, progress_interval_ms: 1, ..Default::default() };
    let engine = TransferEngine::new(store.clone(), Arc::new(OneBackend(backend.clone())), Arc::new(NullSink), cfg);

    let id = engine.enqueue(NewTransfer {
        connection_id: 1, direction: Direction::Down, remote_path: remote.into(),
        local_path: local.to_string_lossy().into(), name: "big.bin".into(),
        total_bytes: Some(body.len() as u64),
    }).await.unwrap();

    // Pause after some bytes, before completion.
    for _ in 0..500 {
        let t = store.get(id).unwrap().unwrap();
        if t.status == TransferStatus::Running && t.transferred_bytes > 0 && t.transferred_bytes < body.len() as u64 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    engine.pause(id).await.unwrap();
    for _ in 0..200 {
        if store.get(id).unwrap().unwrap().status == TransferStatus::Paused { break; }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let partial = store.get(id).unwrap().unwrap().transferred_bytes;
    assert!(partial > 0 && partial < body.len() as u64, "expected a real mid-file pause");

    engine.resume(id).await.unwrap();
    for _ in 0..2000 {
        if store.get(id).unwrap().unwrap().status == TransferStatus::Completed { break; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(store.get(id).unwrap().unwrap().status, TransferStatus::Completed);
    assert_eq!(std::fs::read(&local).unwrap(), body, "resumed download must be byte-identical");

    let _ = backend.delete(remote).await; // cleanup
}
```

- [ ] **Step 2: Run it against the live container**

```bash
./scripts/test-sftp-up.sh
WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test transfer_sftp_resume -- --nocapture
./scripts/test-sftp-down.sh
```

Expected: `sftp_download_pauses_and_resumes_byte_identical ... ok`. If the pause never catches a mid-file moment (transfer too fast), shrink `chunk_bytes` or grow the file; the assert guards against a false pass.

- [ ] **Step 3: Add the CI step**

In `.github/workflows/ci.yml`, inside the existing SFTP block of the `rust` job (where `WONDERBLOB_TEST_SFTP=1 cargo test … --test sftp_contract` already runs with the container up), add a second invocation under the same up/down so it reuses the running container:

```yaml
      - name: Transfer resume test (SFTP)
        run: |
          ./scripts/test-sftp-up.sh
          WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test sftp_contract
          WONDERBLOB_TEST_SFTP=1 cargo test -p wonderblob-core --test transfer_sftp_resume
          ./scripts/test-sftp-down.sh
```

(If the existing SFTP step already brings the container up/down, just append the `--test transfer_sftp_resume` line to it rather than adding a new up/down pair — avoid double-starting the container. The plain `cargo test --workspace` step already compiles + runs all the non-gated engine/store/model/mock tests, so a regression there fails CI even without Docker.)

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "test(core): gated SFTP pause/resume E2E (byte-identical) + CI step"
```

---

## Done criteria (Plan 3)

- `cargo test --workspace` + `npm run check` + `npm test` green locally and in CI (Docker-gated tests skip without env flags).
- Core engine tests (with `MockBackend`, no Tauri) assert: **queue ordering** + **concurrency cap**, **progress monotonic** reaching total, **pause keeps partial + resume completes download from offset**, **cancel cleans the partial**, **restart loads incomplete + resume rebinds to finish**, **retry-with-backoff** recovers an injected transient mid-download fault, **upload completes** (restart-from-0 semantics).
- `TransferStore` unit tests pass (CRUD, `load_incomplete`, `clear_completed`).
- Gated SFTP E2E proves a real download pauses mid-file and resumes **byte-identical**.
- The app constructs the engine at startup, opens `transfers.db` under the app-data dir, recovers prior-session transfers as `Paused`, and emits `transfer://progress` / `transfer://state`.
- The frontend transfers panel lists active+recent transfers with direction glyph, `--accent` progress bar, %/bytes, derived speed, per-row pause/resume/cancel/retry, and Clear completed; the toolbar shows an active-count indicator; Upload enqueues every selected file (multi-select).
- The blocking `download_file` / `upload_file` commands and their JS wrappers are gone; no second write path remains.
- UI is tokens-only (no new colors unless the documented `--progress-track` fallback was truly required), keyboard-accessible, consistent with `FileList`/`BookmarkList`.

## Explicitly deferred

- **Resumable UPLOAD sessions** (the asymmetry): persisting S3 multipart-upload-id + completed-part list, Azure staged-but-uncommitted block ids, and SFTP append-at-offset, so uploads resume mid-file instead of restarting from 0. v1 uploads restart; this is the single biggest tracked follow-up.
- **Bandwidth limits / throttling**, global and per-transfer.
- **Server-to-server / cross-backend copy** (also spec-deferred to post-v1).
- **Reorder / priority** within the queue; drag-to-reorder; "pause all" / "resume all" bulk actions.
- **Automatic resume on reconnect**: v1 parks restart-loaded transfers as `Paused` and resumes them when the user clicks Resume (rebinding to the live connection); auto-rebinding by matching bookmark id is a follow-up. (Requires persisting a stable bookmark id on the transfer row — currently only the ephemeral `connection_id` is stored.)
- **Checksum/integrity verification** of completed transfers (etag/mtime compare); conflict handling on download-over-existing-local-file (currently truncates to the resume offset).
- **Notifications** on completion/failure; transfer history retention policy / auto-prune.
- **EditSession** download-to-temp + open/watch/save-back (Plan 4) — it will reuse the backend or enqueue, not the retired blocking commands.

## Self-review (writing-plans checklist)

- **Spec coverage (§ TransferEngine):** persistent SQLite queue surviving restart ✓ (`TransferStore` bundled rusqlite + `load_incomplete`/`recover_on_start`); N configurable parallel workers ✓ (`EngineConfig.max_workers` + `Semaphore`); pause/crash resume ✓ for downloads (offset re-`read`), with the multipart/chunk-state limitation for uploads documented and centralized (`reset_upload_offset`); retry with exponential backoff for transient failures ✓ (`is_retryable` gate); progress streamed as Tauri events ✓ (`transfer://progress` / `transfer://state`). § Testing failure-injection (kill worker mid-chunk, restart, assert resume) ✓ (`MockBackend.fail_read_after` + restart test + gated SFTP E2E). § v1 scope "transfer queue with pause/resume" ✓.
- **No placeholders / no "same as task N":** every task has full code, exact paths, run-commands with expected output, and a commit. The two intentional stubs (`transfer/mod.rs` re-exports staged across Tasks 1/3; `stream_upload` stub in Task 5) are explicitly flagged with the task that replaces them.
- **Type/name consistency with the REAL symbols read:**
  - `StorageBackend` used exactly as defined in `vfs.rs`: `read(&self, path: &str, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>>`, `write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>>`, `stat`, `delete`; `Capabilities::default()`; `Entry { size: Option<u64> }`. Trait unchanged.
  - `StorageError` variants used as they exist: `Network{detail}`, `NotFound{path}`, `Unsupported{op}`, `Other{detail}`, plus the existing `is_retryable()` (Network-only) — the retry path relies on exactly that method.
  - `AppState` shape from `state.rs`: `connections` map of `ConnectionId (u64) → Arc<dyn StorageBackend>`, `next_id()`, `get`/`remove`; the only change is wrapping the map in `Arc` (`ConnMap`) so the resolver shares it — call sites unchanged. `register(...)` / `ConnectResult` in `commands.rs` untouched.
  - Event/command naming follows the existing `api.ts` `camelCase` + `snake_case` command convention (`enqueue_download`, `list_transfers`, …) and the spec-mandated `transfer://progress` / `transfer://state` event names; payloads `#[serde(rename_all = "camelCase")]` so `connectionId`/`transferredBytes`/`bytesPerSec` match the TS interfaces, exactly like `Entry`/`Capabilities`/`ConnectResult`.
  - Frontend mirrors Plan 1/2 idioms: `$lib/api`, `invoke<T>(name, args)` wrappers, Svelte `writable`/`derived` stores like `session.ts`, components keyboard-consistent with `FileList.svelte` (`onerror` prop, `describeError`, `--row-height` rows), `@tauri-apps/plugin-dialog` `open({ multiple: true })` for multi-select.
  - Engine holds backends via the injected `BackendResolver` (app impl reads the shared `ConnMap`) and emits via the injected `EventSink` (app impl calls `AppHandle::emit`) — core stays Tauri-free, matching the Plan 1/2 split (protocol logic in core, Tauri only wires).
