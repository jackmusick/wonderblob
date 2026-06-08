//! Persistent, resumable transfer queue (spec: "TransferEngine"). The engine and
//! its SQLite store live here so they're testable without Tauri; the app layer
//! injects a `BackendResolver` and an `EventSink`.

pub mod model;
pub mod store;
