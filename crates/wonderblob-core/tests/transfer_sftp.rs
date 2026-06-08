//! Real pause/resume against the Dockerized OpenSSH server (Plan 1 fixture).
//! Gated by WONDERBLOB_TEST_SFTP=1; run scripts/test-sftp-up.sh first.

mod sftp_support;

use sftp_support::{connect_accept_once, PASS};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use wonderblob_core::sftp::SftpAuth;
use wonderblob_core::transfer::engine::{
    BackendResolver, EngineConfig, EventSink, TransferEngine, TransferEvent,
};
use wonderblob_core::transfer::model::{Direction, TransferStatus};
use wonderblob_core::transfer::store::{NewTransfer, TransferStore};
use wonderblob_core::vfs::StorageBackend;

struct OneBackend(Arc<dyn StorageBackend>);
#[async_trait::async_trait]
impl BackendResolver for OneBackend {
    async fn resolve(&self, _id: u64) -> Option<Arc<dyn StorageBackend>> {
        Some(self.0.clone())
    }
}
struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _e: TransferEvent) {}
}

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

#[tokio::test]
async fn sftp_download_pauses_and_resumes_byte_identical() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    let backend: Arc<dyn StorageBackend> =
        Arc::new(connect_accept_once(SftpAuth::Password(PASS.into())).await);

    // Stage a multi-MiB remote file via the backend itself.
    let remote = "/config/wb-transfer-big.bin";
    let body: Vec<u8> = (0..20_000_000u32).map(|i| (i % 251) as u8).collect();
    {
        let mut w = backend.write(remote).await.expect("write");
        w.write_all(&body).await.unwrap();
        w.shutdown().await.unwrap();
    }

    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("big.bin");
    let store = Arc::new(TransferStore::open_in_memory().unwrap());
    // Small chunk + tiny progress interval so we can pause mid-stream.
    let cfg = EngineConfig {
        max_workers: 1,
        chunk_bytes: 32 * 1024,
        progress_interval_ms: 1,
        ..Default::default()
    };
    let engine = TransferEngine::new(
        store.clone(),
        Arc::new(OneBackend(backend.clone())),
        Arc::new(NullSink),
        cfg,
    );

    let id = engine
        .enqueue(NewTransfer {
            connection_id: 1,
            direction: Direction::Down,
            remote_path: remote.into(),
            local_path: local.to_string_lossy().into(),
            name: "big.bin".into(),
            total_bytes: Some(body.len() as u64),
        })
        .await
        .unwrap();

    // Pause after some bytes, before completion.
    for _ in 0..500 {
        let t = store.get(id).unwrap().unwrap();
        if t.status == TransferStatus::Running
            && t.transferred_bytes > 0
            && t.transferred_bytes < body.len() as u64
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    engine.pause(id).await.unwrap();
    for _ in 0..200 {
        if store.get(id).unwrap().unwrap().status == TransferStatus::Paused {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let partial = store.get(id).unwrap().unwrap().transferred_bytes;
    assert!(
        partial > 0 && partial < body.len() as u64,
        "expected a real mid-file pause, got {partial}"
    );

    engine.resume(id).await.unwrap();
    for _ in 0..2000 {
        if store.get(id).unwrap().unwrap().status == TransferStatus::Completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        store.get(id).unwrap().unwrap().status,
        TransferStatus::Completed
    );
    assert_eq!(
        std::fs::read(&local).unwrap(),
        body,
        "resumed download must be byte-identical"
    );

    let _ = backend.delete(remote).await; // cleanup
}
