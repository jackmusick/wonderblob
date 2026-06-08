mod contract;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wonderblob_core::azblob::{AzAuth, AzBlobBackend, AzBlobConfig};
use wonderblob_core::vfs::{EntryKind, StorageBackend};

/// Public, well-known Azurite development account key (not a secret).
const DEV_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_AZBLOB").as_deref() == Ok("1")
}

fn test_config() -> AzBlobConfig {
    AzBlobConfig {
        account: "devstoreaccount1".into(),
        // Azurite path-style endpoint includes the account name.
        endpoint: Some("http://127.0.0.1:10000/devstoreaccount1".into()),
        auth: AzAuth::AccountKey(DEV_KEY.into()),
    }
}

#[tokio::test]
async fn azblob_passes_vfs_contract() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_AZBLOB=1 and run scripts/test-azblob-up.sh");
        return;
    }
    let backend = AzBlobBackend::connect(test_config())
        .await
        .expect("connect");
    backend
        .ensure_test_container("wbtest")
        .await
        .expect("create container");
    contract::run_contract(&backend, "/wbtest").await;
}

#[tokio::test]
async fn azblob_root_lists_containers_as_dirs() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_AZBLOB=1");
        return;
    }
    let backend = AzBlobBackend::connect(test_config())
        .await
        .expect("connect");
    backend
        .ensure_test_container("wbtest")
        .await
        .expect("create container");
    let roots = backend.list("/").await.expect("list root");
    let c = roots
        .iter()
        .find(|e| e.name == "wbtest")
        .expect("wbtest container in root");
    assert_eq!(c.kind, EntryKind::Dir);
    assert_eq!(c.path, "/wbtest");
}

/// Deterministic, index-derived byte pattern (no RNG) so failures are reproducible.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Exercises the staged-block path: ~20 MiB (crosses the 8 MiB block boundary
/// multiple times) written in odd-sized chunks that straddle the boundary, then
/// read back and compared byte-for-byte. The 16-byte contract write only covers
/// the single-block case, so this protects the block-splitting + commit logic.
#[tokio::test]
async fn azblob_multiblock_roundtrip_20mb() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_AZBLOB=1 and run scripts/test-azblob-up.sh");
        return;
    }
    let backend = AzBlobBackend::connect(test_config())
        .await
        .expect("connect");
    backend
        .ensure_test_container("wbtest")
        .await
        .expect("create container");

    let path = "/wbtest/multiblock-20mb.bin";
    let _ = backend.delete(path).await;

    let total = 20 * 1024 * 1024 + 12_345; // ~20 MiB, not a multiple of PART_SIZE
    let data = pattern(total);

    let mut w = backend.write(path).await.expect("open write");
    // Odd chunk size that is not a divisor of the 8 MiB block size, so chunks
    // straddle block boundaries.
    let chunk = 100_003;
    let mut off = 0;
    while off < data.len() {
        let end = (off + chunk).min(data.len());
        w.write_all(&data[off..end]).await.expect("write chunk");
        off = end;
    }
    w.shutdown().await.expect("shutdown/commit");

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

/// Zero-byte upload: the writer stages one empty block and commits, producing a
/// 0-byte blob that stats as size 0 and reads back empty.
#[tokio::test]
async fn azblob_empty_file_roundtrip() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_AZBLOB=1 and run scripts/test-azblob-up.sh");
        return;
    }
    let backend = AzBlobBackend::connect(test_config())
        .await
        .expect("connect");
    backend
        .ensure_test_container("wbtest")
        .await
        .expect("create container");

    let path = "/wbtest/empty.bin";
    let _ = backend.delete(path).await;

    let mut w = backend.write(path).await.expect("open write");
    // No write_all calls at all — straight to shutdown.
    w.shutdown().await.expect("shutdown/commit");

    let st = backend.stat(path).await.expect("stat");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.size, Some(0), "empty file should stat as size 0");

    let mut r = backend.read(path, 0).await.expect("open read");
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.expect("read back");
    assert!(got.is_empty(), "empty file should read back empty");

    backend.delete(path).await.expect("cleanup");
}
