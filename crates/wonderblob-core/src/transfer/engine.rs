use crate::transfer::model::{Direction, Transfer, TransferId, TransferStatus};
use crate::transfer::store::{NewTransfer, TransferStore};
use crate::vfs::StorageBackend;
use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

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

/// Outcome of one streaming attempt, so the worker loop can decide retry vs stop.
enum Outcome {
    Completed,
    Paused,
    Canceled,
    /// Failed; bool = retryable.
    Failed(String, bool),
}

// Per-transfer cooperative control flag, checked between chunks.
const C_RUN: u8 = 0;
const C_PAUSE: u8 = 1;
const C_CANCEL: u8 = 2;

/// Persistent, resumable transfer queue. Owns the store, the injected resolver
/// and sink, a `Semaphore` capping concurrent workers, and a per-transfer
/// control flag map so pause/cancel can interrupt a running stream.
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
            let permit = self
                .permits
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore");
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
                let _ = self.store.set_status(
                    id,
                    TransferStatus::Paused,
                    Some("connection not available; reconnect to resume"),
                );
                self.emit_state(id);
                return;
            };
            let _ = self.store.set_status(id, TransferStatus::Running, None);
            self.emit_state(id);

            let outcome = match t.direction {
                Direction::Down => self.stream_download(&t, backend.as_ref(), &control).await,
                Direction::Up => self.stream_upload(&t, backend.as_ref(), &control).await,
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
                    // Belt-and-suspenders: also remove the partial download here so
                    // the artifact is gone even if cancel()'s remove raced an
                    // in-flight chunk flush.
                    if t.direction == Direction::Down {
                        let _ = tokio::fs::remove_file(&t.local_path).await;
                    }
                    let _ = self.store.set_status(id, TransferStatus::Canceled, None);
                    self.emit_state(id);
                    return;
                }
                Outcome::Failed(msg, retryable) => {
                    if retryable && attempt < self.cfg.max_retries {
                        let backoff =
                            (self.cfg.backoff_base_ms << attempt).min(self.cfg.backoff_cap_ms);
                        attempt += 1;
                        // Uploads can't resume — rewind before re-streaming.
                        if t.direction == Direction::Up {
                            let _ = self.store.reset_upload_offset(id);
                        }
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
        t: &Transfer,
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
            .truncate(false)
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

    /// Signal a running transfer to stop at the next chunk boundary, preserving
    /// its partial file + offset. No-op if it isn't currently running.
    pub async fn pause(self: &Arc<Self>, id: TransferId) -> crate::error::Result<()> {
        let signaled = {
            let map = self.controls.lock().unwrap();
            if let Some(c) = map.get(&id) {
                c.store(C_PAUSE, Ordering::SeqCst);
                true
            } else {
                false
            }
        };
        if !signaled {
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
        let Some(t) = self.store.get(id)? else {
            return Ok(());
        };
        if t.direction == Direction::Up {
            self.store.reset_upload_offset(id)?;
        }
        self.clone().spawn(id);
        Ok(())
    }

    /// Stop a transfer (running or not) and clean the partial download artifact.
    pub async fn cancel(self: &Arc<Self>, id: TransferId) -> crate::error::Result<()> {
        let running = {
            let map = self.controls.lock().unwrap();
            map.get(&id)
                .map(|c| {
                    c.store(C_CANCEL, Ordering::SeqCst);
                })
                .is_some()
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

    pub fn list(&self) -> crate::error::Result<Vec<Transfer>> {
        self.store.list()
    }

    /// Stream local→remote. Uploads cannot resume (header asymmetry), so this
    /// always sends the whole file; callers reset `transferred_bytes` to 0 before
    /// a re-run via `reset_upload_offset`.
    async fn stream_upload(
        &self,
        t: &Transfer,
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
                    id: t.id,
                    transferred_bytes: transferred,
                    total_bytes: t.total_bytes,
                    bytes_per_sec: rate,
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
            id: t.id,
            transferred_bytes: transferred,
            total_bytes: Some(transferred),
            bytes_per_sec: 0,
        }));
        Outcome::Completed
    }
}
