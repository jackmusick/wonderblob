use std::sync::{Arc, Mutex};
use std::time::Duration;
use wonderblob_core::transfer::engine::{
    BackendResolver, EngineConfig, EventSink, TransferEngine, TransferEvent,
};
use wonderblob_core::transfer::mock::MockBackend;
use wonderblob_core::transfer::model::{Direction, TransferStatus};
use wonderblob_core::transfer::store::{NewTransfer, TransferStore};
use wonderblob_core::vfs::StorageBackend;

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

// Probe: resume() called while the worker is still running -> double spawn.
#[tokio::test]
async fn resume_while_running_double_spawns_concurrent_writers() {
    let backend = Arc::new(MockBackend::new());
    let body: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
    backend.put("/r.bin", body.clone()).await;
    backend.set_read_delay_ms(2); // keep it observably in-flight
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
    let sink = Arc::new(CollectSink::default());
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        sink.clone(),
        EngineConfig {
            max_workers: 4, // allow 2nd worker to run concurrently
            progress_interval_ms: 1,
            chunk_bytes: 16 * 1024,
            ..Default::default()
        },
    );
    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: "/r.bin".into(),
            local_path: local.to_string_lossy().into(),
            name: "r.bin".into(),
            total_bytes: Some(body.len() as u64),
        })
        .await
        .unwrap();

    // wait until running with some bytes
    for _ in 0..500 {
        let t = store.get(id).unwrap().unwrap();
        if t.status == TransferStatus::Running && t.transferred_bytes > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // Fire a resume on the SAME id while it is still running. Keep the delay on
    // so BOTH workers are clearly in-flight concurrently.
    engine.resume(id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // now let both finish
    backend.set_read_delay_ms(0);
    for _ in 0..2000 {
        if store.get(id).unwrap().unwrap().status == TransferStatus::Completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Give any second worker time to also run to completion / clobber.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let completions = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(e, TransferEvent::State(t) if t.status == TransferStatus::Completed)
        })
        .count();
    println!("number of Completed state events for one id = {completions}");
    assert_eq!(
        completions, 1,
        "double-spawn: transfer ran/completed {completions} times for a single resume-while-running"
    );
}
