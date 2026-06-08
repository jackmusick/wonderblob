//! Mock-Graph VFS coverage for the OneDrive backend (CI coverage — Graph isn't
//! self-hostable like MinIO/Azurite). A single in-process `wiremock` server
//! impersonates a small Graph drive; no network, no env flag, no fixture.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{body_string_contains, header, header_exists, method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use wonderblob_core::onedrive::{OneDriveBackend, OneDriveConfig, StaticToken};
use wonderblob_core::vfs::{EntryKind, StorageBackend};

fn backend(uri: &str) -> OneDriveBackend {
    OneDriveBackend::new(OneDriveConfig {
        base_url: uri.to_string(),
        token: Arc::new(StaticToken::new("T")),
    })
}

#[tokio::test]
async fn vfs_roundtrip_list_stat_read_delete_mkdir_share() {
    let s = MockServer::start().await;

    // list("/")
    Mock::given(method("GET"))
        .and(path("/me/drive/root/children"))
        .and(header("authorization", "Bearer T"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "value": [
                {"name": "Reports", "folder": {}},
                {"name": "notes.txt", "file": {}, "size": 5,
                 "lastModifiedDateTime": "2024-01-02T03:04:05Z", "eTag": "\"E1\""}
            ]
        })))
        .mount(&s)
        .await;

    // stat("/notes.txt")
    Mock::given(method("GET"))
        .and(path("/me/drive/root:/notes.txt:"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "I1", "name": "notes.txt", "file": {}, "size": 5,
            "lastModifiedDateTime": "2024-01-02T03:04:05Z", "eTag": "\"E1\"",
            "@microsoft.graph.downloadUrl": format!("{}/dl/notes", s.uri())
        })))
        .mount(&s)
        .await;

    // read("/notes.txt", 0) via /content
    Mock::given(method("GET"))
        .and(path("/me/drive/root:/notes.txt:/content"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
        .mount(&s)
        .await;

    // mkdir("/New")
    Mock::given(method("POST"))
        .and(path("/me/drive/root/children"))
        .and(body_string_contains("\"folder\""))
        .respond_with(ResponseTemplate::new(201))
        .mount(&s)
        .await;

    // delete("/notes.txt")
    Mock::given(method("DELETE"))
        .and(path("/me/drive/root:/notes.txt:"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&s)
        .await;

    // share_link("/notes.txt")
    Mock::given(method("POST"))
        .and(path("/me/drive/root:/notes.txt:/createLink"))
        .and(body_string_contains("organization"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "link": {"webUrl": "https://contoso-my.sharepoint.com/share"}
        })))
        .mount(&s)
        .await;

    let b = backend(&s.uri());

    let entries = b.list("/").await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, EntryKind::Dir); // dirs first
    assert_eq!(entries[0].name, "Reports");
    assert_eq!(entries[1].name, "notes.txt");
    assert_eq!(entries[1].size, Some(5));

    let st = b.stat("/notes.txt").await.unwrap();
    assert_eq!(st.kind, EntryKind::File);
    assert_eq!(st.size, Some(5));

    let mut r = b.read("/notes.txt", 0).await.unwrap();
    let mut buf = String::new();
    r.read_to_string(&mut buf).await.unwrap();
    assert_eq!(buf, "hello");

    b.mkdir("/New").await.unwrap();
    b.delete("/notes.txt").await.unwrap();

    let link = b.share_link("/notes.txt", 3600).await.unwrap();
    assert_eq!(link, "https://contoso-my.sharepoint.com/share");
}

#[tokio::test]
async fn rename_412_maps_to_conflict() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me/drive/root:/a.txt:"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "I9", "eTag": "ETAGX", "name": "a.txt", "file": {}
        })))
        .mount(&s)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/me/drive/items/I9"))
        .and(header("if-match", "ETAGX"))
        .respond_with(ResponseTemplate::new(412))
        .mount(&s)
        .await;
    let err = backend(&s.uri())
        .rename("/a.txt", "/b.txt")
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        wonderblob_core::error::StorageError::Conflict { .. }
    ));
}

/// Captures every `Content-Range` header the fragment PUTs send so the test can
/// assert 320 KiB-multiple intermediate fragments and the final remainder.
#[derive(Clone, Default)]
struct RangeRecorder {
    ranges: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Respond for RangeRecorder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let cr = req
            .headers
            .get("content-range")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        self.ranges.lock().unwrap().push(cr.clone());
        // Parse "bytes a-b/total"; 202 unless this is the final byte.
        let (range, total) = cr
            .trim_start_matches("bytes ")
            .split_once('/')
            .unwrap_or(("0-0", "0"));
        let end: u64 = range.split('-').nth(1).unwrap_or("0").parse().unwrap_or(0);
        let total: u64 = total.parse().unwrap_or(0);
        if end + 1 >= total {
            ResponseTemplate::new(201).set_body_json(serde_json::json!({"name": "big.bin"}))
        } else {
            ResponseTemplate::new(202)
                .set_body_json(serde_json::json!({"nextExpectedRanges": [format!("{}-", end + 1)]}))
        }
    }
}

#[tokio::test]
async fn chunked_upload_uses_320kib_multiples_and_correct_content_range() {
    let s = MockServer::start().await;
    let recorder = RangeRecorder::default();
    let ranges = recorder.ranges.clone();

    // createUploadSession (>4 MiB triggers the session path).
    Mock::given(method("POST"))
        .and(path("/me/drive/root:/big.bin:/createUploadSession"))
        .and(body_string_contains("replace"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "uploadUrl": format!("{}/upload/1", s.uri())
        })))
        .mount(&s)
        .await;

    // Fragment PUTs — the recorder validates and answers per Content-Range.
    Mock::given(method("PUT"))
        .and(path("/upload/1"))
        .respond_with(recorder)
        .mount(&s)
        .await;

    // 25 MiB => fragments of 10 MiB, 10 MiB, then 5 MiB remainder.
    const FRAG: u64 = 10 * 1024 * 1024;
    const KIB320: u64 = 327_680;
    let total: u64 = 25 * 1024 * 1024;
    let data = vec![0xABu8; total as usize];

    let mut w = backend(&s.uri()).write("/big.bin").await.unwrap();
    w.write_all(&data).await.unwrap();
    w.shutdown().await.unwrap();

    let captured = ranges.lock().unwrap().clone();
    assert_eq!(captured.len(), 3, "expected three fragments: {captured:?}");
    assert_eq!(captured[0], format!("bytes 0-{}/{}", FRAG - 1, total));
    assert_eq!(
        captured[1],
        format!("bytes {}-{}/{}", FRAG, 2 * FRAG - 1, total)
    );
    // Final fragment ends at total-1/total.
    assert_eq!(
        captured[2],
        format!("bytes {}-{}/{}", 2 * FRAG, total - 1, total)
    );
    // Every non-final fragment length is a 320 KiB multiple.
    for cr in &captured[..2] {
        let (range, _) = cr.trim_start_matches("bytes ").split_once('/').unwrap();
        let (a, bb) = range.split_once('-').unwrap();
        let len: u64 = bb.parse::<u64>().unwrap() - a.parse::<u64>().unwrap() + 1;
        assert_eq!(len % KIB320, 0, "fragment {cr} not a 320 KiB multiple");
    }
}

/// On a 401, a token provider that refreshes can recover. We model the
/// retry-on-401 at the provider layer: the backend asks for a token, the first
/// underlying refresh yields a token the mock rejects (401), and a second token
/// works. Here we assert the simpler contract: a 401 from Graph maps to
/// AuthFailed (which the Tauri layer turns into "sign in again" / silent retry).
#[tokio::test]
async fn graph_401_maps_to_auth_failed() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me/drive/root/children"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&s)
        .await;
    let err = backend(&s.uri()).list("/").await.unwrap_err();
    assert!(matches!(
        err,
        wonderblob_core::error::StorageError::AuthFailed { .. }
    ));
}

/// list() follows @odata.nextLink pagination.
#[tokio::test]
async fn list_follows_next_link_pagination() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me/drive/root/children"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "value": [{"name": "page1.txt", "file": {}, "size": 1}],
            "@odata.nextLink": format!("{}/next-page", s.uri())
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/next-page$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "value": [{"name": "page2.txt", "file": {}, "size": 2}]
        })))
        .mount(&s)
        .await;
    let entries = backend(&s.uri()).list("/").await.unwrap();
    assert_eq!(entries.len(), 2);
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"page1.txt"));
    assert!(names.contains(&"page2.txt"));
}
