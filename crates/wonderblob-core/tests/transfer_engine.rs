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
    assert_eq!(store.get(id).unwrap().unwrap().transferred_bytes, 500_000);
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

#[tokio::test]
async fn pause_keeps_partial_then_resume_completes_from_offset() {
    let backend = Arc::new(MockBackend::new());
    let body: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    backend.put("/r.bin", body.clone()).await;
    // Slow chunks so we can pause mid-file before it completes.
    backend.set_read_delay_ms(3);
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(1),
    );
    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: "/r.bin".into(),
            local_path: local.to_string_lossy().into(),
            name: "r.bin".into(),
            total_bytes: Some(1_000_000),
        })
        .await
        .unwrap();

    // Pause once some bytes have landed but before completion.
    settle(|| {
        let t = store.get(id).unwrap().unwrap();
        t.status == TransferStatus::Running && t.transferred_bytes > 0
    })
    .await;
    engine.pause(id).await.unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Paused).await;
    let partial = store.get(id).unwrap().unwrap().transferred_bytes;
    assert!(partial > 0 && partial < 1_000_000);

    // Resume → completes, byte-identical. Drop the delay so it finishes quickly.
    backend.set_read_delay_ms(0);
    engine.resume(id).await.unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(std::fs::read(&local).unwrap(), body);
}

#[tokio::test]
async fn cancel_stops_and_removes_partial_file() {
    let backend = Arc::new(MockBackend::new());
    backend.put("/r.bin", vec![7u8; 1_000_000]).await;
    backend.set_read_delay_ms(3); // observable mid-flight so cancel hits a running stream
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(1),
    );
    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: "/r.bin".into(),
            local_path: local.to_string_lossy().into(),
            name: "r.bin".into(),
            total_bytes: Some(1_000_000),
        })
        .await
        .unwrap();
    settle(|| {
        let t = store.get(id).unwrap().unwrap();
        t.status == TransferStatus::Running && t.transferred_bytes > 0
    })
    .await;
    engine.cancel(id).await.unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Canceled).await;
    // Give the cleanup a beat, then assert the partial is gone.
    settle(|| !local.exists()).await;
}

#[tokio::test]
async fn upload_completes_and_writes_remote() {
    let backend = Arc::new(MockBackend::new());
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("up.bin");
    std::fs::write(&local, vec![3u8; 300_000]).unwrap();
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(2),
    );
    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Up,
            remote_path: "/up.bin".into(),
            local_path: local.to_string_lossy().into(),
            name: "up.bin".into(),
            total_bytes: Some(300_000),
        })
        .await
        .unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(backend.get("/up.bin").await.unwrap().len(), 300_000);
}

#[tokio::test]
async fn transient_download_failure_is_retried_then_succeeds() {
    let backend = Arc::new(MockBackend::new());
    let body: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
    backend.put("/r.bin", body.clone()).await;
    backend.fail_read_after(50_000); // one injected mid-stream reset
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
    let mut cfg = fast_cfg(1);
    cfg.backoff_base_ms = 1; // keep the test fast
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        cfg,
    );
    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: "/r.bin".into(),
            local_path: local.to_string_lossy().into(),
            name: "r.bin".into(),
            total_bytes: Some(400_000),
        })
        .await
        .unwrap();
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(std::fs::read(&local).unwrap(), body); // resumed past the injected fault
}

#[tokio::test]
async fn restart_loads_incomplete_and_resume_rebinds_to_finish() {
    let body: Vec<u8> = (0..800_000u32).map(|i| (i % 251) as u8).collect();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("transfers.db");
    let local = dir.path().join("r.bin");

    // --- session 1: enqueue, pause mid-way, "crash" (drop engine) ---
    let id;
    {
        let backend = Arc::new(MockBackend::new());
        backend.put("/r.bin", body.clone()).await;
        backend.set_read_delay_ms(3);
        let store = Arc::new(TransferStore::open(&db).unwrap());
        let engine = TransferEngine::new(
            store.clone(),
            Arc::new(OneBackend(backend.clone())),
            Arc::new(CollectSink::default()),
            fast_cfg(1),
        );
        id = engine
            .enqueue(NewTransfer {
                connection_id: 1,
                direction: Direction::Down,
                remote_path: "/r.bin".into(),
                local_path: local.to_string_lossy().into(),
                name: "r.bin".into(),
                total_bytes: Some(800_000),
            })
            .await
            .unwrap();
        settle(|| {
            let t = store.get(id).unwrap().unwrap();
            t.status == TransferStatus::Running && t.transferred_bytes > 0
        })
        .await;
        engine.pause(id).await.unwrap();
        settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Paused).await;
    } // engine + store dropped

    // --- session 2: reopen DB, recover, reconnect (new conn id 99), rebind+resume ---
    let backend2 = Arc::new(MockBackend::new());
    backend2.put("/r.bin", body.clone()).await;
    let store2 = Arc::new(TransferStore::open(&db).unwrap());
    let engine2 = TransferEngine::new(
        store2.clone(),
        Arc::new(OneBackend(backend2.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(1),
    );
    let loaded = engine2.recover_on_start().unwrap();
    assert_eq!(loaded, 1); // the paused transfer was reloaded
    assert_eq!(
        store2.get(id).unwrap().unwrap().status,
        TransferStatus::Paused
    );

    engine2.resume_with(id, Some(99)).await.unwrap(); // rebind to new connection
    settle(|| store2.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    assert_eq!(std::fs::read(&local).unwrap(), body);
}

#[tokio::test]
async fn resume_while_running_does_not_double_spawn() {
    // A resume() (or any spawn) for a transfer whose worker is still live must be
    // a no-op: two workers on one local file = corruption + duplicate terminals.
    let backend = Arc::new(MockBackend::new());
    let body: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    backend.put("/r.bin", body.clone()).await;
    backend.set_read_delay_ms(3); // keep the worker observably in-flight
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let sink = Arc::new(CollectSink::default());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("r.bin");
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
            local_path: local.to_string_lossy().into(),
            name: "r.bin".into(),
            total_bytes: Some(1_000_000),
        })
        .await
        .unwrap();
    settle(|| {
        let t = store.get(id).unwrap().unwrap();
        t.status == TransferStatus::Running && t.transferred_bytes > 0
    })
    .await;
    // Hammer resume while the worker is alive — each must be rejected by the guard.
    for _ in 0..8 {
        engine.resume(id).await.unwrap();
    }
    assert!(
        engine.is_active(id),
        "the original worker should still own the slot"
    );
    backend.set_read_delay_ms(0); // let it finish
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Completed).await;
    // Exactly ONE terminal Completed state event for this id.
    let completed = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, TransferEvent::State(t) if t.id == id && t.status == TransferStatus::Completed))
        .count();
    assert_eq!(
        completed, 1,
        "expected exactly one Completed event, got {completed}"
    );
    assert_eq!(
        std::fs::read(&local).unwrap(),
        body,
        "output must be byte-identical"
    );
}

#[tokio::test]
async fn upload_cancel_cleans_partial_remote() {
    // SFTP leaves a real remote file behind on a cancelled upload; the engine must
    // best-effort delete it. Pre-seed the remote so a no-op would leave it present.
    let backend = Arc::new(MockBackend::new());
    backend.put("/up.bin", vec![0u8; 16]).await; // stand-in partial remote object
    backend.set_write_delay_ms(3); // observable mid-upload
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("up.bin");
    std::fs::write(&local, vec![3u8; 400_000]).unwrap();
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(CollectSink::default()),
        fast_cfg(1),
    );
    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Up,
            remote_path: "/up.bin".into(),
            local_path: local.to_string_lossy().into(),
            name: "up.bin".into(),
            total_bytes: Some(400_000),
        })
        .await
        .unwrap();
    settle(|| {
        let t = store.get(id).unwrap().unwrap();
        t.status == TransferStatus::Running && t.transferred_bytes > 0
    })
    .await;
    engine.cancel(id).await.unwrap();
    // The worker's Canceled branch deletes the remote *before* it records the
    // Canceled status, so once status settles the partial must already be gone.
    settle(|| store.get(id).unwrap().unwrap().status == TransferStatus::Canceled).await;
    assert!(
        backend.get("/up.bin").await.is_none(),
        "cancelled upload left a partial remote object behind"
    );
}
