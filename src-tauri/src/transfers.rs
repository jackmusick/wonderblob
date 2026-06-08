//! App-layer wiring for the core `TransferEngine`: a `BackendResolver` over the
//! shared connection map, an `EventSink` that forwards engine events to the
//! webview, and the startup constructor that opens `transfers.db` and recovers
//! interrupted transfers from the prior session.

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
    let dir = app.path().app_data_dir().expect("app data dir");
    let _ = std::fs::create_dir_all(&dir);
    let store = Arc::new(TransferStore::open(dir.join("transfers.db")).expect("open transfers.db"));
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
