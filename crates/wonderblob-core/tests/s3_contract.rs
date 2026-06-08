mod contract;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wonderblob_core::s3::{S3Backend, S3Config};
use wonderblob_core::vfs::{EntryKind, StorageBackend};

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_S3").as_deref() == Ok("1")
}

fn test_config() -> S3Config {
    S3Config {
        access_key_id: "minioadmin".into(),
        secret_access_key: "minioadmin".into(),
        region: Some("us-east-1".into()),
        endpoint: Some("http://localhost:9000".into()),
        force_path_style: true, // MinIO requires path-style addressing
    }
}

#[tokio::test]
async fn s3_passes_vfs_contract() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_S3=1 and run scripts/test-s3-up.sh");
        return;
    }
    let backend = S3Backend::connect(test_config()).await.expect("connect");
    backend
        .ensure_test_bucket("wbtest")
        .await
        .expect("create bucket");
    // Contract root is a writable dir INSIDE a pre-created bucket.
    contract::run_contract(&backend, "/wbtest").await;
}

#[tokio::test]
async fn s3_root_lists_buckets_as_dirs() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_S3=1");
        return;
    }
    let backend = S3Backend::connect(test_config()).await.expect("connect");
    backend
        .ensure_test_bucket("wbtest")
        .await
        .expect("create bucket");
    let roots = backend.list("/").await.expect("list root");
    let bucket = roots
        .iter()
        .find(|e| e.name == "wbtest")
        .expect("wbtest bucket in root");
    assert_eq!(bucket.kind, EntryKind::Dir);
    assert_eq!(bucket.path, "/wbtest");
}

/// Deterministic, index-derived byte pattern (no RNG) so failures are reproducible.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Exercises the multipart path: ~20 MiB (crosses the 8 MiB part boundary
/// multiple times) written in odd-sized chunks that straddle the boundary, then
/// read back and compared byte-for-byte. The 16-byte contract write only covers
/// the single-part case, so this protects the part-splitting + complete logic.
#[tokio::test]
async fn s3_multipart_roundtrip_20mb() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_S3=1 and run scripts/test-s3-up.sh");
        return;
    }
    let backend = S3Backend::connect(test_config()).await.expect("connect");
    backend
        .ensure_test_bucket("wbtest")
        .await
        .expect("create bucket");

    let path = "/wbtest/multipart-20mb.bin";
    let _ = backend.delete(path).await;

    let total = 20 * 1024 * 1024 + 12_345; // ~20 MiB, not a multiple of PART_SIZE
    let data = pattern(total);

    let mut w = backend.write(path).await.expect("open write");
    // Odd chunk size that is not a divisor of the 8 MiB part size, so chunks
    // straddle part boundaries.
    let chunk = 100_003;
    let mut off = 0;
    while off < data.len() {
        let end = (off + chunk).min(data.len());
        w.write_all(&data[off..end]).await.expect("write chunk");
        off = end;
    }
    w.shutdown().await.expect("shutdown/complete");

    let st = backend.stat(path).await.expect("stat");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.size, Some(total as u64), "stat size mismatch");

    let mut r = backend.read(path, 0).await.expect("open read");
    let mut got = Vec::with_capacity(total);
    r.read_to_end(&mut got).await.expect("read back");
    assert_eq!(got.len(), total, "readback length mismatch");
    assert!(got == data, "readback bytes differ from written bytes");

    backend.delete(path).await.expect("cleanup");
}

/// Zero-byte upload: the writer must still issue one (empty) part and complete,
/// producing a 0-byte object that stats as size 0 and reads back empty.
#[tokio::test]
async fn s3_empty_file_roundtrip() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_S3=1 and run scripts/test-s3-up.sh");
        return;
    }
    let backend = S3Backend::connect(test_config()).await.expect("connect");
    backend
        .ensure_test_bucket("wbtest")
        .await
        .expect("create bucket");

    let path = "/wbtest/empty.bin";
    let _ = backend.delete(path).await;

    let mut w = backend.write(path).await.expect("open write");
    // No write_all calls at all — straight to shutdown.
    w.shutdown().await.expect("shutdown/complete");

    let st = backend.stat(path).await.expect("stat");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.size, Some(0), "empty file should stat as size 0");

    let mut r = backend.read(path, 0).await.expect("open read");
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.expect("read back");
    assert!(got.is_empty(), "empty file should read back empty");

    backend.delete(path).await.expect("cleanup");
}
