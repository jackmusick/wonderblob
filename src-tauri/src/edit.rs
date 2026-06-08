//! App-layer EditSession: OS file watching, debounce, the session registry, the
//! "open with default app" call, and the `edit://*` events. The protocol work
//! (download / conflict-check / upload) lives in `wonderblob_core::edit`.

use crate::state::{ConnMap, ConnectionId};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use wonderblob_core::edit::{
    download_to_temp, flush_if_pending, save_back, temp_mtime, FlushResult, RemoteStat,
};
use wonderblob_core::error::StorageError;

pub type SessionId = u64;

const DEBOUNCE: Duration = Duration::from_millis(750);

/// Pure debounce rule: has it been quiet for `window` since `last_event` as of `now`?
pub(crate) fn debounce_ready(last_event: Instant, now: Instant, window: Duration) -> bool {
    now.duration_since(last_event) >= window
}

/// Result of a save attempt, used by teardown to decide whether the temp file is
/// safe to delete. `Dirty` ⇒ unflushed/conflicted work; preserve the temp file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveOutcome {
    Clean,
    Dirty,
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
    /// Temp-file mtime as of the last point the temp content matched the remote
    /// (after download or a successful save). A newer temp mtime ⇒ pending edits.
    /// Used to flush before teardown (C1) and to make Discard a no-op (I2).
    pub last_synced: Mutex<Option<SystemTime>>,
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
        let basename = remote_path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("file");
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&remote_path, &mut hasher);
        let h = std::hash::Hasher::finish(&hasher);
        self.root
            .join(connection_id.to_string())
            .join(format!("{h:016x}"))
            .join(basename)
    }

    pub fn list(&self) -> Vec<EditSessionInfo> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.info())
            .collect()
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

        // The temp file was just written by the caller's download, so its current
        // mtime is the "in sync with remote" marker.
        let last_synced = temp_mtime(&temp_path);
        let session = Arc::new(EditSession {
            session_id,
            connection_id,
            remote_path,
            name,
            temp_path,
            baseline: Mutex::new(baseline),
            last_synced: Mutex::new(last_synced),
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
            let Some(session) = self.get(id) else { return };
            self.attempt_save(&session).await;
        }
    }

    /// Conflict-aware save of any pending local edits, emitting the matching
    /// `edit://*` event. Shared by the debounce loop and by teardown flush, so the
    /// "check conflict before every save-back" guarantee holds on both paths.
    ///
    /// `Clean` means the temp file is safe to delete (nothing pending, or it was
    /// uploaded). `Dirty` means there is unflushed work (conflict, upload error,
    /// or connection gone) and the temp file MUST be preserved.
    async fn attempt_save(&self, session: &EditSession) -> SaveOutcome {
        let Some(backend) = self.conns.read().await.get(&session.connection_id).cloned() else {
            // Only surface an error if there is actually something to lose.
            let last_synced = *session.last_synced.lock().unwrap();
            if wonderblob_core::edit::temp_is_pending(&session.temp_path, last_synced) {
                self.emit_error(session, "connection closed; reconnect and re-open to save");
                return SaveOutcome::Dirty;
            }
            return SaveOutcome::Clean;
        };
        let baseline = *session.baseline.lock().unwrap();
        let last_synced = *session.last_synced.lock().unwrap();
        match flush_if_pending(
            backend.as_ref(),
            &session.remote_path,
            &session.temp_path,
            last_synced,
            &baseline,
        )
        .await
        {
            Ok(FlushResult::NothingPending) => SaveOutcome::Clean,
            Ok(FlushResult::Saved { stat, synced_at }) => {
                *session.baseline.lock().unwrap() = stat;
                *session.last_synced.lock().unwrap() = synced_at;
                session.has_conflict.store(false, Ordering::SeqCst);
                let _ = self.app.emit("edit://saved", session.info());
                SaveOutcome::Clean
            }
            Ok(FlushResult::Conflict { .. }) => {
                session.has_conflict.store(true, Ordering::SeqCst);
                let _ = self.app.emit("edit://conflict", session.info());
                SaveOutcome::Dirty
            }
            Err(e) => {
                self.emit_error(session, &e.to_string());
                SaveOutcome::Dirty
            }
        }
    }

    fn emit_error(&self, session: &EditSession, message: &str) {
        #[derive(Serialize, Clone)]
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
        let Some(session) = self.get(id) else {
            return Ok(());
        };
        let backend = self
            .conns
            .read()
            .await
            .get(&session.connection_id)
            .cloned()
            .ok_or_else(|| StorageError::Other {
                detail: "connection closed".into(),
            })?;
        // Capture the mtime of the bytes we're about to upload, so a concurrent
        // newer edit stays pending rather than being masked.
        let synced_at = temp_mtime(&session.temp_path);
        let fresh = save_back(backend.as_ref(), &session.temp_path, &session.remote_path).await?;
        *session.baseline.lock().unwrap() = fresh;
        *session.last_synced.lock().unwrap() = synced_at;
        session.has_conflict.store(false, Ordering::SeqCst);
        let _ = self.app.emit("edit://saved", session.info());
        Ok(())
    }

    /// Re-download the remote into the temp file, discarding local edits, and
    /// re-baseline (conflict "Discard"). Records the fresh temp mtime as the
    /// new last-synced point so the watcher event this re-download triggers is
    /// seen as "nothing pending" and does NOT re-upload identical bytes (I2).
    pub async fn discard_local(&self, id: SessionId) -> Result<(), StorageError> {
        let Some(session) = self.get(id) else {
            return Ok(());
        };
        let backend = self
            .conns
            .read()
            .await
            .get(&session.connection_id)
            .cloned()
            .ok_or_else(|| StorageError::Other {
                detail: "connection closed".into(),
            })?;
        let fresh =
            download_to_temp(backend.as_ref(), &session.remote_path, &session.temp_path).await?;
        *session.baseline.lock().unwrap() = fresh;
        *session.last_synced.lock().unwrap() = temp_mtime(&session.temp_path);
        session.has_conflict.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Flush a still-registered session, then drop its watcher + task; the temp
    /// file is deleted ONLY when `keep_temp` is false AND the flush was clean (no
    /// pending edits, conflict, or error). A dirty/conflicted temp is preserved
    /// regardless so nothing the user saved is silently lost (C1). Returns
    /// whether the session ended clean (used by `close_connection`).
    pub async fn close(&self, id: SessionId, keep_temp: bool) -> bool {
        // Pull the session out of the map first so no new debounce save can race;
        // abort the loop, then flush inline — the flush re-checks pending and does
        // the full conflict-aware save, so an aborted in-flight save is recovered.
        let Some(session) = self.sessions.lock().unwrap().remove(&id) else {
            return true;
        };
        session.task.abort();
        let clean = matches!(self.attempt_save(&session).await, SaveOutcome::Clean);
        if clean && !keep_temp {
            let _ = std::fs::remove_file(&session.temp_path);
            if let Some(parent) = session.temp_path.parent() {
                let _ = std::fs::remove_dir(parent); // best-effort; only if empty
            }
        }
        // Dropping `session` drops the watcher.
        clean
    }

    /// Close every session for a connection, flushing pending edits first (C1).
    /// The connection temp tree is removed only when `keep_temp` is false and
    /// every session flushed clean; a conflicted/unflushed temp is kept.
    pub async fn close_connection(&self, connection_id: ConnectionId, keep_temp: bool) {
        let ids: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.connection_id == connection_id)
            .map(|s| s.session_id)
            .collect();
        let mut all_clean = true;
        for id in ids {
            all_clean &= self.close(id, keep_temp).await;
        }
        // Only nuke the whole connection tree when nothing was left dirty —
        // otherwise we'd delete a temp `close` deliberately preserved.
        if !keep_temp && all_clean {
            let _ = std::fs::remove_dir_all(self.root.join(connection_id.to_string()));
        }
    }

    /// Flush pending edits for EVERY open session (app exit). Saves clean pending
    /// work to the remote and emits `edit://conflict` for sessions the remote
    /// changed under; never deletes temps. Returns true if every session flushed
    /// clean (no unresolved/unflushed work remains).
    pub async fn flush_all(&self) -> bool {
        let sessions: Vec<Arc<EditSession>> =
            self.sessions.lock().unwrap().values().cloned().collect();
        let mut all_clean = true;
        for session in sessions {
            all_clean &= matches!(self.attempt_save(&session).await, SaveOutcome::Clean);
        }
        all_clean
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

#[cfg(test)]
mod tests {
    use super::debounce_ready;
    use std::time::{Duration, Instant};

    #[test]
    fn debounce_waits_for_quiet_window() {
        let win = Duration::from_millis(500);
        let last_event = Instant::now();
        // a check 200ms later: not yet quiet
        assert!(!debounce_ready(
            last_event,
            last_event + Duration::from_millis(200),
            win
        ));
        // a check 600ms later: quiet long enough → ready
        assert!(debounce_ready(
            last_event,
            last_event + Duration::from_millis(600),
            win
        ));
    }
}
