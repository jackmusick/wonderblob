//! Env-gated real-tenant OneDrive round-trip. Interactive OAuth can't run in CI,
//! so this test is SKIPPED unless all of:
//!   WONDERBLOB_TEST_ONEDRIVE=1
//!   WONDERBLOB_ONEDRIVE_REFRESH_TOKEN=<a valid refresh token>
//!   WONDERBLOB_ONEDRIVE_CLIENT_ID=<the app's client id>
//! are set. When present it refreshes against the REAL
//! login.microsoftonline.com/organizations/oauth2/v2.0 authority, points the
//! backend at real Graph, and runs write→stat→read→share_link→delete under a
//! `/wonderblob-test/` folder. Mirrors the WONDERBLOB_TEST_S3/_AZBLOB gating.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wonderblob_core::onedrive::{OneDriveBackend, OneDriveConfig, RefreshingTokenProvider};
use wonderblob_core::vfs::StorageBackend;

const AUTH_BASE: &str = "https://login.microsoftonline.com/organizations/oauth2/v2.0";
const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

#[tokio::test]
async fn live_onedrive_roundtrip() {
    if std::env::var("WONDERBLOB_TEST_ONEDRIVE").as_deref() != Ok("1") {
        eprintln!("skipping live OneDrive test (set WONDERBLOB_TEST_ONEDRIVE=1 + refresh token + client id)");
        return;
    }
    let (Ok(refresh), Ok(client_id)) = (
        std::env::var("WONDERBLOB_ONEDRIVE_REFRESH_TOKEN"),
        std::env::var("WONDERBLOB_ONEDRIVE_CLIENT_ID"),
    ) else {
        eprintln!("skipping live OneDrive test (missing refresh token / client id)");
        return;
    };

    let token = Arc::new(RefreshingTokenProvider::new(
        reqwest::Client::new(),
        AUTH_BASE.to_string(),
        client_id,
        refresh,
        Arc::new(|_rotated: String| { /* test: ignore rotation */ }),
    ));
    let b = OneDriveBackend::new(OneDriveConfig {
        base_url: GRAPH_BASE.to_string(),
        token,
    });

    let dir = "/wonderblob-test";
    let file = "/wonderblob-test/hello.txt";
    let _ = b.mkdir(dir).await; // may already exist

    let mut w = b.write(file).await.expect("write");
    w.write_all(b"hello onedrive").await.expect("write_all");
    w.shutdown().await.expect("shutdown");

    let st = b.stat(file).await.expect("stat");
    assert_eq!(st.size, Some(14));

    let mut r = b.read(file, 0).await.expect("read");
    let mut buf = String::new();
    r.read_to_string(&mut buf).await.expect("read body");
    assert_eq!(buf, "hello onedrive");

    let link = b.share_link(file, 3600).await.expect("share_link");
    assert!(link.starts_with("https://"), "share link: {link}");

    b.delete(file).await.expect("delete");
    eprintln!("live OneDrive round-trip OK");
}
