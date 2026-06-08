//! OneDrive for Business backend over the Microsoft Graph REST API.
//!
//! Unlike s3.rs/azblob.rs this drive has REAL folders, so we use Graph's
//! path-addressed DriveItem URLs (`/me/drive/root:/path:` …) rather than the
//! flat-namespace ObjPath synthesis. Only the tiny `basename` helper is shared.
//! (<https://learn.microsoft.com/graph/api/resources/onedrive>)
//!
//! ## Auth (injected)
//! The backend depends only on an injected [`TokenProvider`] (`Arc<dyn …>`) and a
//! configurable `base_url`, so the whole backend is testable against a `wiremock`
//! mock Graph server with zero real auth. The OAuth code-exchange/refresh token
//! calls live here too (as pure async fns over `reqwest`, parameterized by
//! `auth_base`) so they are mock-tested without real Entra calls. The interactive
//! browser/deep-link half lives in `src-tauri/src/onedrive_auth.rs`.

use crate::error::{Result, StorageError};
use crate::objstore::basename;
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use futures::StreamExt;
use std::future::Future;
use std::io;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Path addressing (Task 1)
// ---------------------------------------------------------------------------

/// Percent-encode each path segment but keep the `/` separators, per Graph
/// path-addressing. Root ("" / "/") addresses `/me/drive/root` directly.
fn encode_drive_path(path: &str) -> String {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(urlencode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Minimal RFC 3986 path-segment encoder. Keeps the unreserved set
/// (`A-Z a-z 0-9 - . _ ~`) literal and percent-encodes every other byte —
/// notably space, `#`, `?`, `%`, `:`, and non-ASCII (UTF-8 bytes). This is what
/// Graph expects for the colon-delimited path-addressing scheme.
fn urlencode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `GET`-able item URL: "/" -> root, "/a/b" -> root:/a/b:
pub(crate) fn item_url(base: &str, path: &str) -> String {
    let p = encode_drive_path(path);
    if p.is_empty() {
        format!("{base}/me/drive/root")
    } else {
        format!("{base}/me/drive/root:/{p}:")
    }
}

/// Children-listing URL: "/" -> root/children, "/a/b" -> root:/a/b:/children
pub(crate) fn children_url(base: &str, path: &str) -> String {
    let p = encode_drive_path(path);
    if p.is_empty() {
        format!("{base}/me/drive/root/children")
    } else {
        format!("{base}/me/drive/root:/{p}:/children")
    }
}

/// Parent directory of an absolute path: "/a/b.txt" -> "/", "/a/b/c" -> "/a/b".
fn parent_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

// ---------------------------------------------------------------------------
// TokenProvider (Tasks 2 + 4)
// ---------------------------------------------------------------------------

/// Source of a currently-valid Graph bearer token. Injected so core stays
/// testable (mock returns a fixed token; the real impl silently refreshes).
#[async_trait]
pub trait TokenProvider: Send + Sync {
    async fn access_token(&self) -> Result<String>;
}

/// Test/double: a fixed token (used by the wiremock suite).
pub struct StaticToken(String);

impl StaticToken {
    pub fn new(t: impl Into<String>) -> Self {
        Self(t.into())
    }
}

#[async_trait]
impl TokenProvider for StaticToken {
    async fn access_token(&self) -> Result<String> {
        Ok(self.0.clone())
    }
}

/// Holds the long-lived refresh token + a cached short-lived access token. On
/// refresh, if Graph rotates the refresh token, `on_rotate` persists it (the
/// keychain writer, in src-tauri).
pub struct RefreshingTokenProvider {
    client: reqwest::Client,
    auth_base: String,
    client_id: String,
    inner: Mutex<RtState>,
    on_rotate: Arc<dyn Fn(String) + Send + Sync>,
}

struct RtState {
    refresh_token: String,
    access: Option<(String, Instant)>,
}

impl RefreshingTokenProvider {
    pub fn new(
        client: reqwest::Client,
        auth_base: String,
        client_id: String,
        refresh_token: String,
        on_rotate: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Self {
        Self {
            client,
            auth_base,
            client_id,
            inner: Mutex::new(RtState {
                refresh_token,
                access: None,
            }),
            on_rotate,
        }
    }
}

#[async_trait]
impl TokenProvider for RefreshingTokenProvider {
    async fn access_token(&self) -> Result<String> {
        let mut st = self.inner.lock().await;
        if let Some((tok, exp)) = &st.access {
            if Instant::now() < *exp {
                return Ok(tok.clone());
            }
        }
        let r = refresh_tokens(
            &self.client,
            &self.auth_base,
            &self.client_id,
            &st.refresh_token,
        )
        .await?;
        if let Some(new_rt) = r.refresh_token {
            if new_rt != st.refresh_token {
                (self.on_rotate)(new_rt.clone());
                st.refresh_token = new_rt;
            }
        }
        // Refresh 60s early to avoid edge-of-expiry 401s.
        let ttl = Duration::from_secs((r.expires_in.max(60) as u64).saturating_sub(60));
        st.access = Some((r.access_token.clone(), Instant::now() + ttl));
        Ok(r.access_token)
    }
}

// ---------------------------------------------------------------------------
// Token-endpoint calls (Task 3)
// ---------------------------------------------------------------------------

/// Delegated scopes (work/school). `offline_access` is required to receive a
/// refresh token; `openid profile` yield the id_token for the account label.
/// (<https://learn.microsoft.com/graph/permissions-reference>)
pub const SCOPES: &str = "Files.ReadWrite.All offline_access User.Read openid profile";

#[derive(Debug, serde::Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    /// Rotated refresh token; may be absent if not rotated.
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub id_token: Option<String>,
}

/// PKCE authorization-code redemption (public client: NO client_secret).
/// (<https://learn.microsoft.com/entra/identity-platform/v2-oauth2-auth-code-flow#redeem-a-code-for-an-access-token>)
pub async fn exchange_code(
    client: &reqwest::Client,
    auth_base: &str,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    let form = [
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", code_verifier),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
    ];
    post_token(client, auth_base, &form).await
}

/// Refresh grant (public client). May rotate the refresh token.
/// (<https://learn.microsoft.com/entra/identity-platform/v2-oauth2-auth-code-flow#refresh-the-access-token>)
pub async fn refresh_tokens(
    client: &reqwest::Client,
    auth_base: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse> {
    let form = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("scope", SCOPES),
    ];
    post_token(client, auth_base, &form).await
}

async fn post_token(
    client: &reqwest::Client,
    auth_base: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse> {
    let resp = client
        .post(format!("{auth_base}/token"))
        .form(form)
        .send()
        .await
        .map_err(net)?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        // invalid_grant => refresh token expired/revoked => re-auth required.
        return Err(
            if body.contains("invalid_grant") || status.as_u16() == 400 {
                StorageError::AuthFailed {
                    detail: format!("token endpoint {status}: {body}"),
                }
            } else {
                StorageError::Network {
                    detail: format!("token endpoint {status}"),
                }
            },
        );
    }
    resp.json::<TokenResponse>()
        .await
        .map_err(StorageError::other)
}

fn net(e: reqwest::Error) -> StorageError {
    StorageError::Network {
        detail: e.to_string(),
    }
}

// ---------------------------------------------------------------------------
// DriveItem model + error mapping
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct DriveItem {
    name: Option<String>,
    size: Option<i64>,
    #[serde(rename = "lastModifiedDateTime")]
    last_modified: Option<String>,
    #[serde(rename = "eTag")]
    etag: Option<String>,
    /// facet present => directory.
    folder: Option<serde_json::Value>,
    #[allow(dead_code)]
    file: Option<serde_json::Value>,
    #[serde(rename = "@microsoft.graph.downloadUrl")]
    download_url: Option<String>,
    id: Option<String>,
}

#[derive(serde::Deserialize)]
struct Children {
    value: Vec<DriveItem>,
    #[serde(rename = "@odata.nextLink")]
    next: Option<String>,
}

#[derive(serde::Deserialize)]
struct CreateLinkResponse {
    link: Option<SharingLink>,
}

#[derive(serde::Deserialize)]
struct SharingLink {
    #[serde(rename = "webUrl")]
    web_url: Option<String>,
}

/// Map a Graph HTTP status (+ optional error body) into the taxonomy. Mirrors
/// `map_s3`/`map_az`'s coarse buckets.
fn map_graph(path: &str, status: reqwest::StatusCode, body: &str) -> StorageError {
    match status.as_u16() {
        401 => StorageError::AuthFailed {
            detail: format!("graph 401: {body}"),
        },
        403 => StorageError::PermissionDenied { path: path.into() },
        404 => StorageError::NotFound { path: path.into() },
        409 | 412 => StorageError::Conflict {
            path: path.into(),
            detail: format!("graph {status}: {body}"),
        },
        429 => StorageError::Network {
            detail: format!("graph throttled (429): {body}"),
        },
        s if s >= 500 => StorageError::Network {
            detail: format!("graph {status}: {body}"),
        },
        _ => StorageError::Other {
            detail: format!("graph {status}: {body}"),
        },
    }
}

/// Parse RFC3339 `lastModifiedDateTime` into unix milliseconds.
fn parse_modified_ms(s: &Option<String>) -> Option<i64> {
    let s = s.as_ref()?;
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
}

fn item_to_entry(it: &DriveItem, path: &str) -> Entry {
    let kind = if it.folder.is_some() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    Entry {
        name: it.name.clone().unwrap_or_else(|| basename(path)),
        path: path.to_string(),
        kind,
        size: it.size.and_then(|s| u64::try_from(s).ok()),
        modified_ms: parse_modified_ms(&it.last_modified),
    }
}

/// ISO-8601 UTC `secs` in the future, for `expirationDateTime` on share links.
fn iso8601_in(secs_from_now: i64) -> String {
    let t = time::OffsetDateTime::now_utc() + time::Duration::seconds(secs_from_now);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
    )
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

pub struct OneDriveConfig {
    /// Graph base, e.g. `https://graph.microsoft.com/v1.0` (or a wiremock URI).
    pub base_url: String,
    pub token: Arc<dyn TokenProvider>,
}

pub struct OneDriveBackend {
    client: reqwest::Client,
    base_url: String,
    token: Arc<dyn TokenProvider>,
}

impl OneDriveBackend {
    pub fn new(cfg: OneDriveConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: cfg.base_url,
            token: cfg.token,
        }
    }

    async fn bearer(&self) -> Result<String> {
        Ok(format!("Bearer {}", self.token.access_token().await?))
    }

    /// GET a single DriveItem (used by stat + rename id/eTag capture).
    async fn stat_item(&self, path: &str) -> Result<DriveItem> {
        let resp = self
            .client
            .get(item_url(&self.base_url, path))
            .header("Authorization", self.bearer().await?)
            .send()
            .await
            .map_err(net)?;
        ok_item(path, resp).await
    }
}

/// Parse a successful (2xx) DriveItem response or map the error.
async fn ok_item(path: &str, resp: reqwest::Response) -> Result<DriveItem> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(map_graph(path, status, &body));
    }
    resp.json::<DriveItem>().await.map_err(StorageError::other)
}

#[async_trait]
impl StorageBackend for OneDriveBackend {
    fn capabilities(&self) -> Capabilities {
        // OneDrive has real folders + native sharing links + eTag conflict.
        Capabilities {
            can_presign: true,
            can_rename: true,
            can_set_mtime: false,
        }
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let mut url = children_url(&self.base_url, path);
        let mut entries: Vec<Entry> = Vec::new();
        loop {
            let resp = self
                .client
                .get(&url)
                .header("Authorization", self.bearer().await?)
                .send()
                .await
                .map_err(net)?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(map_graph(path, status, &body));
            }
            let page: Children = resp.json().await.map_err(StorageError::other)?;
            let parent = if path == "/" || path.is_empty() {
                String::new()
            } else {
                path.trim_end_matches('/').to_string()
            };
            for it in &page.value {
                let name = it.name.clone().unwrap_or_default();
                let child_path = format!("{parent}/{name}");
                entries.push(item_to_entry(it, &child_path));
            }
            match page.next {
                Some(n) => url = n,
                None => break,
            }
        }
        // Dirs first, then case-insensitive name (mirrors s3.rs/azblob.rs).
        entries.sort_by(|a, b| {
            (b.kind == EntryKind::Dir)
                .cmp(&(a.kind == EntryKind::Dir))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(entries)
    }

    async fn stat(&self, path: &str) -> Result<Entry> {
        let it = self.stat_item(path).await?;
        // Root has no name; present it as the root dir.
        let p = if path.is_empty() { "/" } else { path };
        Ok(item_to_entry(&it, p))
    }

    async fn read(&self, path: &str, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        // Ranged reads must apply `Range` to the pre-authed downloadUrl, NOT to
        // `/content`. (driveitem-get-content#partial-range-downloads)
        let resp = if offset > 0 {
            let it = self.stat_item(path).await?;
            let dl = it.download_url.ok_or_else(|| StorageError::Other {
                detail: "no downloadUrl for ranged read".into(),
            })?;
            self.client
                .get(&dl)
                .header("Range", format!("bytes={offset}-"))
                .send()
                .await
                .map_err(net)?
        } else {
            // reqwest follows the 302 from /content to the pre-authed downloadUrl.
            self.client
                .get(format!("{}/content", item_url(&self.base_url, path)))
                .header("Authorization", self.bearer().await?)
                .send()
                .await
                .map_err(net)?
        };
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_graph(path, status, &body));
        }
        let stream = resp.bytes_stream().map(|r| r.map_err(io::Error::other));
        Ok(Box::new(tokio_util::io::StreamReader::new(stream)))
    }

    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        let spill = tempfile::NamedTempFile::new().map_err(StorageError::other)?;
        Ok(Box::new(GraphUploadWriter {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            token: self.token.clone(),
            path: path.to_string(),
            spill: Some(spill),
            total: 0,
            state: WState::Idle,
        }))
    }

    async fn delete(&self, path: &str) -> Result<()> {
        // Graph deletes folders recursively to the recycle bin — unlike
        // s3.rs/azblob.rs's non-empty-dir Conflict; accepted for v1.
        let resp = self
            .client
            .delete(item_url(&self.base_url, path))
            .header("Authorization", self.bearer().await?)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(map_graph(path, status, &body))
        }
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        // PATCH the (id-addressed) item with new name and/or parentReference,
        // guarded by `if-match: {eTag}` → 412 Conflict on mismatch.
        // (driveitem-move / driveitem-update)
        let src = self.stat_item(from).await?;
        let id = src.id.ok_or_else(|| StorageError::Other {
            detail: "source item missing id".into(),
        })?;
        let etag = src.etag.unwrap_or_default();

        let from_parent = parent_path(from);
        let to_parent = parent_path(to);
        let new_name = basename(to);

        let mut body = serde_json::json!({ "name": new_name });
        if from_parent != to_parent {
            // Moving across folders: resolve the destination parent's id.
            let parent_item = self.stat_item(&to_parent).await?;
            let parent_id = parent_item.id.ok_or_else(|| StorageError::Other {
                detail: "destination parent missing id".into(),
            })?;
            body["parentReference"] = serde_json::json!({ "id": parent_id });
        }

        let mut req = self
            .client
            .patch(format!("{}/me/drive/items/{}", self.base_url, id))
            .header("Authorization", self.bearer().await?)
            .json(&body);
        if !etag.is_empty() {
            req = req.header("if-match", etag);
        }
        let resp = req.send().await.map_err(net)?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let txt = resp.text().await.unwrap_or_default();
            Err(map_graph(from, status, &txt))
        }
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        let parent = parent_path(path);
        let name = basename(path);
        let body = serde_json::json!({
            "name": name,
            "folder": {},
            "@microsoft.graph.conflictBehavior": "fail",
        });
        let resp = self
            .client
            .post(children_url(&self.base_url, &parent))
            .header("Authorization", self.bearer().await?)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let txt = resp.text().await.unwrap_or_default();
            Err(map_graph(path, status, &txt))
        }
    }

    async fn share_link(&self, path: &str, expiry_secs: u64) -> Result<String> {
        // type=view + scope=organization: org-scoped by default for OneDrive for
        // Business; `anonymous` may be admin-disabled. (driveitem-createlink)
        let body = serde_json::json!({
            "type": "view",
            "scope": "organization",
            "expirationDateTime": iso8601_in(expiry_secs as i64),
        });
        let resp = self
            .client
            .post(format!("{}/createLink", item_url(&self.base_url, path)))
            .header("Authorization", self.bearer().await?)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if !status.is_success() {
            let txt = resp.text().await.unwrap_or_default();
            return Err(map_graph(path, status, &txt));
        }
        // 201 new / 200 existing both OK.
        let parsed: CreateLinkResponse = resp.json().await.map_err(StorageError::other)?;
        parsed
            .link
            .and_then(|l| l.web_url)
            .ok_or_else(|| StorageError::Other {
                detail: "createLink response had no link.webUrl".into(),
            })
    }
}

// ---------------------------------------------------------------------------
// Chunked upload writer (Task 7)
// ---------------------------------------------------------------------------

const SMALL_MAX: u64 = 4 * 1024 * 1024; // ≤4 MiB => simple PUT
                                        // Graph requires each non-final upload fragment to be a multiple of 320 KiB
                                        // (327680 B); the final fragment may be any size. 10 MiB == 32 * 320 KiB.
                                        // (driveitem-createuploadsession#best-practices)
const FRAGMENT: u64 = 10 * 1024 * 1024;

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

enum WState {
    /// Accumulating bytes into the spill file.
    Idle,
    /// A finalize future (small PUT, create-session, or the full fragment loop)
    /// is in flight.
    Finalizing(BoxFut<Result<()>>),
    Done,
}

fn to_io(e: StorageError) -> io::Error {
    io::Error::other(e.to_string())
}

/// Spills incoming bytes to a tempfile while counting, then on `poll_shutdown`
/// uploads them: a single `PUT …/content` for ≤4 MiB, else a `createUploadSession`
/// streamed in 320 KiB-multiple fragments with correct `Content-Range` — total
/// known because we spilled. Mirrors `S3MultipartWriter`'s boxed-future state
/// machine so the writer is `Unpin`. The size problem (Graph needs the total up
/// front) is why we spill instead of streaming like S3/Azure.
pub struct GraphUploadWriter {
    client: reqwest::Client,
    base_url: String,
    token: Arc<dyn TokenProvider>,
    path: String,
    spill: Option<tempfile::NamedTempFile>,
    total: u64,
    state: WState,
}

/// Read the whole spill file and `PUT …/content` it (small-upload path).
async fn simple_put(
    client: reqwest::Client,
    base_url: String,
    token: Arc<dyn TokenProvider>,
    path: String,
    mut file: std::fs::File,
) -> Result<()> {
    file.seek(SeekFrom::Start(0)).map_err(StorageError::other)?;
    let mut body = Vec::new();
    file.read_to_end(&mut body).map_err(StorageError::other)?;
    let bearer = format!("Bearer {}", token.access_token().await?);
    let resp = client
        .put(format!("{}/content", item_url(&base_url, &path)))
        .header("Authorization", bearer)
        .body(body)
        .send()
        .await
        .map_err(net)?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let txt = resp.text().await.unwrap_or_default();
        Err(map_graph(&path, status, &txt))
    }
}

/// Create an upload session and stream the spill file in 320 KiB-multiple
/// fragments (last fragment any size) with `Content-Range: bytes a-b/total`.
async fn session_upload(
    client: reqwest::Client,
    base_url: String,
    token: Arc<dyn TokenProvider>,
    path: String,
    mut file: std::fs::File,
    total: u64,
) -> Result<()> {
    let bearer = format!("Bearer {}", token.access_token().await?);
    let body = serde_json::json!({
        "item": {
            "@microsoft.graph.conflictBehavior": "replace",
            "name": basename(&path),
        }
    });
    let resp = client
        .post(format!(
            "{}/createUploadSession",
            item_url(&base_url, &path)
        ))
        .header("Authorization", &bearer)
        .json(&body)
        .send()
        .await
        .map_err(net)?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(map_graph(&path, status, &txt));
    }
    #[derive(serde::Deserialize)]
    struct Session {
        #[serde(rename = "uploadUrl")]
        upload_url: String,
    }
    let session: Session = resp.json().await.map_err(StorageError::other)?;
    let upload_url = session.upload_url;

    file.seek(SeekFrom::Start(0)).map_err(StorageError::other)?;
    let mut offset: u64 = 0;
    while offset < total {
        let len = FRAGMENT.min(total - offset);
        let mut chunk = vec![0u8; len as usize];
        file.read_exact(&mut chunk).map_err(StorageError::other)?;
        let end = offset + len - 1;
        // NOTE: do NOT send Authorization on the upload PUT — the uploadUrl is
        // pre-authed and an Authorization header can yield a 401.
        let resp = client
            .put(&upload_url)
            .header("Content-Length", len.to_string())
            .header("Content-Range", format!("bytes {offset}-{end}/{total}"))
            .body(chunk)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        // 202 Accepted => more fragments; 200/201 => final commit done.
        if status.as_u16() == 202 {
            offset += len;
            continue;
        }
        if status.is_success() {
            return Ok(());
        }
        // 404 => session gone (restart needed); surface as error for v1.
        let txt = resp.text().await.unwrap_or_default();
        let _ = client.delete(&upload_url).send().await; // best-effort cancel
        return Err(map_graph(&path, status, &txt));
    }
    Ok(())
}

impl GraphUploadWriter {
    /// Drive the in-flight finalize future.
    fn drive_finalize(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = match &mut self.state {
            WState::Finalizing(fut) => fut.as_mut().poll(cx),
            _ => return Poll::Ready(Ok(())),
        };
        match poll {
            Poll::Pending => Poll::Pending,
            Poll::Ready(r) => {
                self.state = WState::Done;
                Poll::Ready(r.map_err(to_io))
            }
        }
    }

    /// Build the finalize future once all bytes are spilled and total is known.
    fn start_finalize(&mut self) -> io::Result<()> {
        // Reopen the spill file independently so the future owns a handle.
        let tmp = self
            .spill
            .take()
            .ok_or_else(|| io::Error::other("upload writer finalized twice".to_string()))?;
        let file = tmp.reopen().map_err(io::Error::other)?;
        // Keep the NamedTempFile alive until the future resolves by leaking it
        // into the future via a guard-less reopen: we hold `file` (an fd) which
        // stays valid even after the path is unlinked on drop. Drop `tmp` now;
        // the open fd keeps the data accessible.
        drop(tmp);
        let client = self.client.clone();
        let base_url = self.base_url.clone();
        let token = self.token.clone();
        let path = self.path.clone();
        let total = self.total;
        let fut: BoxFut<Result<()>> = if total <= SMALL_MAX {
            Box::pin(simple_put(client, base_url, token, path, file))
        } else {
            Box::pin(session_upload(client, base_url, token, path, file, total))
        };
        self.state = WState::Finalizing(fut);
        Ok(())
    }
}

impl AsyncWrite for GraphUploadWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match &mut this.spill {
            Some(f) => {
                f.write_all(data).map_err(io::Error::other)?;
                this.total += data.len() as u64;
                Poll::Ready(Ok(data.len()))
            }
            None => Poll::Ready(Err(io::Error::other("write after shutdown"))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(f) = &mut this.spill {
            f.flush().map_err(io::Error::other)?;
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            match this.state {
                WState::Idle => {
                    if let Some(f) = &mut this.spill {
                        f.flush().map_err(io::Error::other)?;
                    }
                    this.start_finalize()?;
                }
                WState::Finalizing(_) => match this.drive_finalize(cx) {
                    Poll::Ready(Ok(())) => continue,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                },
                WState::Done => return Poll::Ready(Ok(())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_and_children_urls_are_path_addressed() {
        let b = "https://graph.microsoft.com/v1.0";
        assert_eq!(children_url(b, "/"), format!("{b}/me/drive/root/children"));
        assert_eq!(item_url(b, "/"), format!("{b}/me/drive/root"));
        assert_eq!(
            children_url(b, "/Documents/Reports"),
            format!("{b}/me/drive/root:/Documents/Reports:/children")
        );
        assert_eq!(
            item_url(b, "/Documents/a b.txt"),
            format!("{b}/me/drive/root:/Documents/a%20b.txt:")
        );
    }

    #[test]
    fn encode_keeps_unreserved_and_escapes_specials() {
        assert_eq!(urlencode_segment("a b"), "a%20b");
        assert_eq!(urlencode_segment("a#b?c%d"), "a%23b%3Fc%25d");
        assert_eq!(
            urlencode_segment("report-2024_final.txt"),
            "report-2024_final.txt"
        );
    }

    #[test]
    fn parent_path_walks_up() {
        assert_eq!(parent_path("/a/b.txt"), "/a");
        assert_eq!(parent_path("/a.txt"), "/");
        assert_eq!(parent_path("/a/b/c"), "/a/b");
        assert_eq!(parent_path("/a/b/"), "/a");
    }

    #[test]
    fn parse_modified_ms_parses_rfc3339() {
        let ms = parse_modified_ms(&Some("2024-01-02T03:04:05Z".into())).unwrap();
        // 2024-01-02T03:04:05Z = 1704164645 s
        assert_eq!(ms, 1_704_164_645_000);
    }

    #[tokio::test]
    async fn static_token_provider_returns_token() {
        let p = StaticToken::new("abc");
        assert_eq!(p.access_token().await.unwrap(), "abc");
    }

    use wiremock::matchers::{body_string_contains, header, method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn refresh_posts_grant_and_parses_tokens() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("client_id=CID"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT", "refresh_token": "RT2", "expires_in": 3600
            })))
            .mount(&s)
            .await;
        let r = refresh_tokens(&reqwest::Client::new(), &s.uri(), "CID", "RT1")
            .await
            .unwrap();
        assert_eq!(r.access_token, "AT");
        assert_eq!(r.refresh_token.as_deref(), Some("RT2"));
        assert!(r.expires_in >= 3600);
    }

    #[tokio::test]
    async fn exchange_code_posts_authorization_code() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier=VER"))
            .and(body_string_contains("code=CODE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT", "refresh_token": "RT", "expires_in": 3600,
                "id_token": "header.payload.sig"
            })))
            .mount(&s)
            .await;
        let r = exchange_code(
            &reqwest::Client::new(),
            &s.uri(),
            "CID",
            "CODE",
            "VER",
            "wonderblob://auth",
        )
        .await
        .unwrap();
        assert_eq!(r.access_token, "AT");
        assert_eq!(r.refresh_token.as_deref(), Some("RT"));
        assert_eq!(r.id_token.as_deref(), Some("header.payload.sig"));
    }

    #[tokio::test]
    async fn refresh_invalid_grant_maps_to_auth_failed() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant", "error_description": "token expired"
            })))
            .mount(&s)
            .await;
        let err = refresh_tokens(&reqwest::Client::new(), &s.uri(), "CID", "RT1")
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::AuthFailed { .. }));
    }

    #[tokio::test]
    async fn refreshing_provider_caches_and_rotates() {
        // First call hits /token once; a second call within expiry reuses the
        // cached access token (mock expects exactly 1 call). Rotated RT2 is
        // delivered to the on_rotate callback.
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT1", "refresh_token": "RT2", "expires_in": 3600
            })))
            .expect(1)
            .mount(&s)
            .await;
        let rotated = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let r2 = rotated.clone();
        let provider = RefreshingTokenProvider::new(
            reqwest::Client::new(),
            s.uri(),
            "CID".into(),
            "RT1".into(),
            Arc::new(move |rt: String| {
                r2.lock().unwrap().push(rt);
            }),
        );
        assert_eq!(provider.access_token().await.unwrap(), "AT1");
        assert_eq!(provider.access_token().await.unwrap(), "AT1"); // cached
        s.verify().await;
        assert_eq!(rotated.lock().unwrap().as_slice(), ["RT2"]);
    }

    fn backend(uri: &str) -> OneDriveBackend {
        OneDriveBackend::new(OneDriveConfig {
            base_url: uri.to_string(),
            token: Arc::new(StaticToken::new("T")),
        })
    }

    #[tokio::test]
    async fn list_root_parses_children_dirs_first() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/me/drive/root/children"))
            .and(header("authorization", "Bearer T"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {"name": "a.txt", "file": {}, "size": 5,
                     "lastModifiedDateTime": "2024-01-02T03:04:05Z", "eTag": "\"E1\""},
                    {"name": "Documents", "folder": {}}
                ]
            })))
            .mount(&s)
            .await;
        let entries = backend(&s.uri()).list("/").await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, EntryKind::Dir);
        assert_eq!(entries[0].name, "Documents");
        assert_eq!(entries[0].path, "/Documents");
        assert_eq!(entries[1].kind, EntryKind::File);
        assert_eq!(entries[1].path, "/a.txt");
        assert_eq!(entries[1].size, Some(5));
        assert_eq!(entries[1].modified_ms, Some(1_704_164_645_000));
    }

    #[tokio::test]
    async fn stat_file_and_root_and_notfound() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/me/drive/root:/a.txt:"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "a.txt", "file": {}, "size": 9,
                "lastModifiedDateTime": "2024-01-02T03:04:05Z", "eTag": "\"E1\""
            })))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(wm_path("/me/drive/root"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"folder": {}})),
            )
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(wm_path("/me/drive/root:/missing.txt:"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&s)
            .await;
        let b = backend(&s.uri());
        let f = b.stat("/a.txt").await.unwrap();
        assert_eq!(f.kind, EntryKind::File);
        assert_eq!(f.size, Some(9));
        let root = b.stat("/").await.unwrap();
        assert_eq!(root.kind, EntryKind::Dir);
        let err = b.stat("/missing.txt").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound { .. }));
    }

    #[tokio::test]
    async fn read_full_and_ranged() {
        use tokio::io::AsyncReadExt;
        let s = MockServer::start().await;
        // Full read: /content returns 200 body directly (reqwest would follow a
        // 302, but a direct 200 also works for the mock).
        Mock::given(method("GET"))
            .and(wm_path("/me/drive/root:/a.txt:/content"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&s)
            .await;
        // Ranged read: stat for the downloadUrl, then GET it with Range.
        Mock::given(method("GET"))
            .and(wm_path("/me/drive/root:/a.txt:"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "a.txt", "file": {},
                "@microsoft.graph.downloadUrl": format!("{}/dl/a", s.uri())
            })))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(wm_path("/dl/a"))
            .and(header("range", "bytes=2-"))
            .respond_with(ResponseTemplate::new(206).set_body_string("llo"))
            .mount(&s)
            .await;
        let b = backend(&s.uri());
        let mut r = b.read("/a.txt", 0).await.unwrap();
        let mut buf = String::new();
        r.read_to_string(&mut buf).await.unwrap();
        assert_eq!(buf, "hello");
        let mut r2 = b.read("/a.txt", 2).await.unwrap();
        let mut buf2 = String::new();
        r2.read_to_string(&mut buf2).await.unwrap();
        assert_eq!(buf2, "llo");
    }

    #[tokio::test]
    async fn write_small_uses_put_content() {
        use tokio::io::AsyncWriteExt;
        let s = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(wm_path("/me/drive/root:/a.txt:/content"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({"name": "a.txt"})),
            )
            .expect(1)
            .mount(&s)
            .await;
        let mut w = backend(&s.uri()).write("/a.txt").await.unwrap();
        w.write_all(b"hi").await.unwrap();
        w.shutdown().await.unwrap();
        s.verify().await;
    }

    #[tokio::test]
    async fn write_empty_uses_put_content() {
        use tokio::io::AsyncWriteExt;
        let s = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(wm_path("/me/drive/root:/empty.txt:/content"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&s)
            .await;
        let mut w = backend(&s.uri()).write("/empty.txt").await.unwrap();
        w.write_all(b"").await.unwrap();
        w.shutdown().await.unwrap();
        s.verify().await;
    }

    #[tokio::test]
    async fn delete_and_mkdir() {
        let s = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(wm_path("/me/drive/root:/a.txt:"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(wm_path("/me/drive/root/children"))
            .and(body_string_contains("\"folder\""))
            .and(body_string_contains("\"fail\""))
            .respond_with(ResponseTemplate::new(201))
            .mount(&s)
            .await;
        let b = backend(&s.uri());
        b.delete("/a.txt").await.unwrap();
        b.mkdir("/New").await.unwrap();
    }

    #[tokio::test]
    async fn rename_sends_if_match_and_412_conflicts() {
        let s = MockServer::start().await;
        // Same-parent rename: GET source for id+eTag, PATCH with if-match.
        Mock::given(method("GET"))
            .and(wm_path("/me/drive/root:/a.txt:"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "I1", "eTag": "E1", "name": "a.txt", "file": {}
            })))
            .mount(&s)
            .await;
        Mock::given(method("PATCH"))
            .and(wm_path("/me/drive/items/I1"))
            .and(header("if-match", "E1"))
            .and(body_string_contains("\"b.txt\""))
            .respond_with(ResponseTemplate::new(200))
            .mount(&s)
            .await;
        backend(&s.uri()).rename("/a.txt", "/b.txt").await.unwrap();

        // 412 → Conflict.
        let s2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wm_path("/me/drive/root:/a.txt:"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "I1", "eTag": "E1", "name": "a.txt", "file": {}
            })))
            .mount(&s2)
            .await;
        Mock::given(method("PATCH"))
            .and(wm_path("/me/drive/items/I1"))
            .respond_with(ResponseTemplate::new(412))
            .mount(&s2)
            .await;
        let err = backend(&s2.uri())
            .rename("/a.txt", "/b.txt")
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::Conflict { .. }));
    }

    #[tokio::test]
    async fn share_link_returns_weburl() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/me/drive/root:/a.txt:/createLink"))
            .and(body_string_contains("\"view\""))
            .and(body_string_contains("\"organization\""))
            .and(body_string_contains("expirationDateTime"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "link": {"webUrl": "https://contoso-my.sharepoint.com/x"}
            })))
            .mount(&s)
            .await;
        let url = backend(&s.uri())
            .share_link("/a.txt", 86_400)
            .await
            .unwrap();
        assert_eq!(url, "https://contoso-my.sharepoint.com/x");
    }

    #[tokio::test]
    async fn error_mapping_covers_taxonomy() {
        let s = MockServer::start().await;
        for (code, p) in [(403u16, "/f403"), (404, "/f404"), (500, "/f500")] {
            Mock::given(method("GET"))
                .and(wm_path(format!("/me/drive/root:{p}:")))
                .respond_with(ResponseTemplate::new(code))
                .mount(&s)
                .await;
        }
        let b = backend(&s.uri());
        assert!(matches!(
            b.stat("/f403").await.unwrap_err(),
            StorageError::PermissionDenied { .. }
        ));
        assert!(matches!(
            b.stat("/f404").await.unwrap_err(),
            StorageError::NotFound { .. }
        ));
        assert!(matches!(
            b.stat("/f500").await.unwrap_err(),
            StorageError::Network { .. }
        ));
    }
}
