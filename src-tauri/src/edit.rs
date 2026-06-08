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
        let fresh = save_back(backend.as_ref(), &session.temp_path, &session.remote_path).await?;
        *session.baseline.lock().unwrap() = fresh;
        session.has_conflict.store(false, Ordering::SeqCst);
        let _ = self.app.emit("edit://saved", session.info());
        Ok(())
    }

    /// Re-download the remote into the temp file, discarding local edits, and
    /// re-baseline (conflict "Discard").
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
