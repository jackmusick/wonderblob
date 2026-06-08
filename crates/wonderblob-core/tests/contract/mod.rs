use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wonderblob_core::error::StorageError;
use wonderblob_core::vfs::{EntryKind, StorageBackend};

/// Every backend must pass this. `root` is a writable empty directory path.
pub async fn run_contract(b: &dyn StorageBackend, root: &str) {
    let dir = format!("{root}/contract-dir");
    let file = format!("{dir}/hello.txt");
    let renamed = format!("{dir}/hello-renamed.txt");

    // idempotent pre-clean so reruns after a crash don't fail on mkdir
    let _ = b.delete(&file).await;
    let _ = b.delete(&renamed).await;
    let _ = b.delete(&dir).await;

    // mkdir + list shows it
    b.mkdir(&dir).await.expect("mkdir");
    let entries = b.list(root).await.expect("list root");
    assert!(entries.iter().any(|e| e.path == dir && e.kind == EntryKind::Dir));

    // write + read back
    let mut w = b.write(&file).await.expect("open write");
    w.write_all(b"hello wonderblob").await.expect("write bytes");
    w.shutdown().await.expect("flush/close");
    let mut r = b.read(&file, 0).await.expect("open read");
    let mut buf = String::new();
    r.read_to_string(&mut buf).await.expect("read bytes");
    assert_eq!(buf, "hello wonderblob");

    // ranged read
    let mut r = b.read(&file, 6).await.expect("ranged read");
    let mut buf = String::new();
    r.read_to_string(&mut buf).await.unwrap();
    assert_eq!(buf, "wonderblob");

    // stat
    let st = b.stat(&file).await.expect("stat");
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.size, Some(16));

    // rename (if capable), delete, NotFound taxonomy
    if b.capabilities().can_rename {
        b.rename(&file, &renamed).await.expect("rename");
        assert!(matches!(
            b.stat(&file).await,
            Err(StorageError::NotFound { .. })
        ));
        // verify the rename actually moved the file to the new path
        let moved = b.stat(&renamed).await.expect("stat renamed");
        assert_eq!(moved.kind, EntryKind::File);
        b.delete(&renamed).await.expect("delete file");
    } else {
        b.delete(&file).await.expect("delete file");
    }
    b.delete(&dir).await.expect("delete dir");
    assert!(matches!(b.stat(&dir).await, Err(StorageError::NotFound { .. })));
}
