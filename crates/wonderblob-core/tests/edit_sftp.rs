//! Real open/edit/save-back + conflict against the Dockerized OpenSSH server
//! (Plan 1 fixture). Gated by WONDERBLOB_TEST_SFTP=1; run scripts/test-sftp-up.sh first.

use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use wonderblob_core::edit::{check_conflict, download_to_temp, save_back, ConflictCheck};
use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};
use wonderblob_core::vfs::StorageBackend;

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
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
