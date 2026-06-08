use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use wonderblob_core::error::StorageError;
use wonderblob_core::vfs::StorageBackend;

pub type ConnectionId = u64;

#[derive(Default)]
pub struct AppState {
    next_id: AtomicU64,
    pub connections: RwLock<HashMap<ConnectionId, Arc<dyn StorageBackend>>>,
}

impl AppState {
    pub fn next_id(&self) -> ConnectionId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Look up a live connection by id.  Extracted so unit tests can drive the
    /// HashMap logic without needing Tauri's `State<'_>` wrapper.
    pub async fn get(&self, id: ConnectionId) -> Result<Arc<dyn StorageBackend>, StorageError> {
        self.connections
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| StorageError::Other { detail: format!("no such connection {id}") })
    }

    /// Remove a connection; returns true if it existed.
    pub async fn remove(&self, id: ConnectionId) -> bool {
        self.connections.write().await.remove(&id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_backend::FakeBackend;

    #[tokio::test]
    async fn get_and_remove_round_trip() {
        let state = AppState::default();

        // Nothing yet → error.
        assert!(state.get(0).await.is_err());

        // Insert a fake backend.
        let id = state.next_id();
        state.connections.write().await.insert(id, Arc::new(FakeBackend));

        // Now it's there.
        assert!(state.get(id).await.is_ok());

        // Remove it.
        assert!(state.remove(id).await);
        assert!(!state.remove(id).await); // second removal → false

        // Gone.
        assert!(state.get(id).await.is_err());
    }
}
