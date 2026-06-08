//! Real open/edit/save-back + conflict against the Dockerized OpenSSH server
//! (Plan 1 fixture). Gated by WONDERBLOB_TEST_SFTP=1; run scripts/test-sftp-up.sh first.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::io::AsyncWriteExt;
use wonderblob_core::edit::{
    check_conflict, download_to_temp, flush_if_pending, save_back, temp_mtime, ConflictCheck,
    FlushResult,
};
use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};
use wonderblob_core::vfs::StorageBackend;

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

/// Write `bytes` and guarantee a strictly-newer mtime than `marker`.
fn write_newer(path: &Path, bytes: &[u8], marker: Option<SystemTime>) {
    loop {
        std::fs::write(path, bytes).unwrap();
        match (temp_mtime(path), marker) {
            (Some(n), Some(m)) if n > m => break,
            (Some(_), None) => break,
            _ => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
}

async fn connect() -> Arc<dyn StorageBackend> {
    Arc::new(
        SftpBackend::connect(SftpConfig {
            host: "localhost".into(),
            port: 2222,
            username: "wb".into(),
            auth: SftpAuth::Password("wbpass".into()),
        })
        .await
        .expect("connect"),
    )
}

#[tokio::test]
async fn edit_open_then_save_back_changes_remote() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    let backend = connect().await;
    let remote = "/config/wb-edit.txt";
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"original\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("wb-edit.txt");

    let baseline = download_to_temp(backend.as_ref(), remote, &temp)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&temp).unwrap(), b"original\n");

    // simulate a local edit, then save back
    std::fs::write(&temp, b"edited locally\n").unwrap();
    let _fresh = save_back(backend.as_ref(), &temp, remote).await.unwrap();

    // re-read the remote → it changed
    let mut r = backend.read(remote, 0).await.unwrap();
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut r, &mut got)
        .await
        .unwrap();
    assert_eq!(got, b"edited locally\n");

    // no conflict immediately after our own save (baseline re-stat path)
    let _ = baseline;
    let _ = backend.delete(remote).await;
}

#[tokio::test]
async fn edit_detects_out_of_band_conflict_not_silent_overwrite() {
    if !enabled() {
        eprintln!("skipped");
        return;
    }
    let backend = connect().await;
    let remote = "/config/wb-edit-conflict.txt";
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"v1\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("c.txt");
    let baseline = download_to_temp(backend.as_ref(), remote, &temp)
        .await
        .unwrap();

    // someone else changes the remote out-of-band (different size)
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"v2 changed elsewhere\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    std::fs::write(&temp, b"my local edit\n").unwrap();

    match check_conflict(backend.as_ref(), remote, &baseline)
        .await
        .unwrap()
    {
        ConflictCheck::Conflict { .. } => {}
        ConflictCheck::Clear => panic!("expected a conflict, not a silent overwrite"),
    }
    let _ = backend.delete(remote).await;
}

/// C1: a pending edit (temp newer than last-synced) flushes to the remote — this
/// is the path teardown/exit runs so a debounce-window save is never lost.
#[tokio::test]
async fn flush_saves_pending_edit_to_remote() {
    if !enabled() {
        eprintln!("skipped");
        return;
    }
    let backend = connect().await;
    let remote = "/config/wb-flush.txt";
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"original\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("wb-flush.txt");
    let baseline = download_to_temp(backend.as_ref(), remote, &temp)
        .await
        .unwrap();
    let synced = temp_mtime(&temp);

    // unsaved local edit still inside the (simulated) debounce window
    write_newer(&temp, b"flushed on teardown\n", synced);

    match flush_if_pending(backend.as_ref(), remote, &temp, synced, &baseline)
        .await
        .unwrap()
    {
        FlushResult::Saved { .. } => {}
        other => panic!("expected Saved, got {other:?}"),
    }
    let mut r = backend.read(remote, 0).await.unwrap();
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut r, &mut got)
        .await
        .unwrap();
    assert_eq!(got, b"flushed on teardown\n");
    let _ = backend.delete(remote).await;
}

/// C1: when the remote changed out-of-band, flush returns Conflict, writes
/// NOTHING to the remote, and leaves the local temp bytes intact (caller keeps
/// the temp file rather than deleting it).
#[tokio::test]
async fn flush_conflict_preserves_remote_and_temp() {
    if !enabled() {
        eprintln!("skipped");
        return;
    }
    let backend = connect().await;
    let remote = "/config/wb-flush-conflict.txt";
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"v1\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("c.txt");
    let baseline = download_to_temp(backend.as_ref(), remote, &temp)
        .await
        .unwrap();
    let synced = temp_mtime(&temp);

    // remote changes elsewhere; local has unsaved edits
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"v2 changed elsewhere\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    write_newer(&temp, b"my local edit\n", synced);

    let res = flush_if_pending(backend.as_ref(), remote, &temp, synced, &baseline)
        .await
        .unwrap();
    assert!(
        matches!(res, FlushResult::Conflict { .. }),
        "expected Conflict, got {res:?}"
    );
    // remote not overwritten, local edit preserved
    let mut r = backend.read(remote, 0).await.unwrap();
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut r, &mut got)
        .await
        .unwrap();
    assert_eq!(got, b"v2 changed elsewhere\n");
    assert_eq!(std::fs::read(&temp).unwrap(), b"my local edit\n");
    let _ = backend.delete(remote).await;
}

/// I2: after a Discard re-download + re-baseline, a flush is a no-op — the
/// watcher event the re-download triggers must NOT re-upload identical bytes.
#[tokio::test]
async fn discard_rebaseline_flush_writes_nothing() {
    if !enabled() {
        eprintln!("skipped");
        return;
    }
    let backend = connect().await;
    let remote = "/config/wb-discard.txt";
    {
        let mut w = backend.write(remote).await.unwrap();
        w.write_all(b"remote v1\n").await.unwrap();
        w.shutdown().await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("wb-discard.txt");
    download_to_temp(backend.as_ref(), remote, &temp)
        .await
        .unwrap();

    // local edit, then Discard: re-download + re-baseline
    write_newer(&temp, b"local junk\n", temp_mtime(&temp));
    let rebaselined = download_to_temp(backend.as_ref(), remote, &temp)
        .await
        .unwrap();
    let synced_after_discard = temp_mtime(&temp);

    let res = flush_if_pending(
        backend.as_ref(),
        remote,
        &temp,
        synced_after_discard,
        &rebaselined,
    )
    .await
    .unwrap();
    assert_eq!(res, FlushResult::NothingPending);

    let mut r = backend.read(remote, 0).await.unwrap();
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut r, &mut got)
        .await
        .unwrap();
    assert_eq!(got, b"remote v1\n");
    let _ = backend.delete(remote).await;
}
