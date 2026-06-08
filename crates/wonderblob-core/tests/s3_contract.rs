mod contract;

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
