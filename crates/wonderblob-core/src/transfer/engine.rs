use crate::transfer::model::{Transfer, TransferId};
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
