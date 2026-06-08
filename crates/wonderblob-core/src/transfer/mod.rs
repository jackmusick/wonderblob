//! Persistent, resumable transfer queue (spec: "TransferEngine"). The engine and
//! its SQLite store live here so they're testable without Tauri; the app layer
//! injects a `BackendResolver` and an `EventSink`.

pub mod engine;
/// In-memory, deterministic, failure-injectable `StorageBackend` used to drive
/// the engine from integration tests (in `tests/`, which compile against the
/// crate externally and so cannot see `#[cfg(test)]` items).
pub mod mock;
pub mod model;
pub mod store;

pub use engine::{
    BackendResolver, EngineConfig, EventSink, TransferEngine, TransferEvent, TransferUpdate,
};
pub use model::{Direction, Transfer, TransferId, TransferStatus};
pub use store::TransferStore;
