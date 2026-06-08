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
    EngineConfig {
        max_workers,
        progress_interval_ms: 1,
        chunk_bytes: 16 * 1024,
        ..Default::default()
    }
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
    assert_eq!(
        store.get(id).unwrap().unwrap().transferred_bytes,
        500_000
    );
}

#[tokio::test]
async fn concurrency_cap_is_respected() {
    // With cap=1, a second enqueue can't start until the first finishes.
    // Use a large file so the first transfer is observably in-flight.
    let backend = Arc::new(MockBackend::new());
    backend.put("/a.bin", vec![1u8; 2_000_000]).await;
    backend.put("/b.bin", vec![2u8; 10]).await;
    // Slow A's chunks so it's observably in-flight while B waits for the slot.
    backend.set_read_delay_ms(3);
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(1),
    );
    let a = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: "/a.bin".into(),
            local_path: tmp.path().join("a.bin").to_string_lossy().into(),
            name: "a.bin".into(),
            total_bytes: Some(2_000_000),
        })
        .await
        .unwrap();
    let b = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: "/b.bin".into(),
            local_path: tmp.path().join("b.bin").to_string_lossy().into(),
            name: "b.bin".into(),
            total_bytes: Some(10),
        })
        .await
        .unwrap();
    // While a is running, b must still be queued (not running).
    settle(|| store.get(a).unwrap().unwrap().status == TransferStatus::Running).await;
    assert_eq!(
        store.get(b).unwrap().unwrap().status,
        TransferStatus::Queued
    );
    settle(|| store.get(b).unwrap().unwrap().status == TransferStatus::Completed).await;
}
