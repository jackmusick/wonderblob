//! Azure Blob Storage backend.
//!
//! Mirrors the S3 backend's VFS-over-flat-namespace mapping: containers surface
//! as root directories, `/container/prefix/...` addresses blobs under a key
//! prefix, and zero-byte `dir/` marker blobs synthesize directories.
//!
//! ## Auth (the big SDK constraint)
//! The GA `azure_storage_blob` 1.0 client only accepts an Entra ID
//! `TokenCredential` (or no credential for pre-signed URLs) — it has **no**
//! shared-key credential and rejects shared-key entirely. To support the
//! account-key path (and Azurite over HTTP), we mint an **account SAS** from the
//! account key ourselves (HMAC-SHA256) and bake it into the service endpoint
//! URL, constructing every client with `credential = None`. The SDK preserves
//! the endpoint's query string across operations, so the SAS authorizes all
//! requests. Share links use the same signer to mint a short-lived, read-only
//! **service** SAS scoped to the single target blob (the blob path is part of
//! the signature, so the link can't be edited to reach another blob).
//! `https`-only off-emulator; `https,http` only for the Azurite emulator.
//!
//! ## Listing
//! The GA SDK's `list_blobs` exposes no `delimiter` parameter, so there are no
//! server-side `BlobPrefix` entries. We list flat under the prefix and
//! synthesize immediate child directories client-side from the key segments.

use crate::error::{Result, StorageError};
use crate::objstore::{basename, ObjPath, PART_SIZE};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use azure_core::http::{RequestContent, Url, XmlFormat};
use azure_storage_blob::models::{
    BlobClientDownloadOptions, BlobClientGetPropertiesResultHeaders,
    BlobContainerClientListBlobsOptions, BlockLookupList, HttpRange,
};
use azure_storage_blob::{BlobClient, BlobServiceClient, BlockBlobClient};
use base64::Engine as _;
use bytes::BytesMut;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

/// SAS service version. Azurite's newest supported API version; the SDK's own
/// request header version (2026-04-06) is independent and tolerated via
/// `--skipApiVersionCheck`.
const SAS_VERSION: &str = "2025-11-05";

/// Azure auth modes. The secret slot holds whichever single credential applies.
pub enum AzAuth {
    /// Shared-key: account name + this base64 key.
    AccountKey(String),
    /// Full connection string (contains account name + key).
    ConnectionString(String),
    /// Pre-minted SAS token (read-scoped); cannot mint new share links.
    Sas(String),
}

pub struct AzBlobConfig {
    pub account: String,
    /// Custom endpoint (e.g. Azurite path-style includes the account); `None` => real Azure.
    pub endpoint: Option<String>,
    pub auth: AzAuth,
}

/// Holds the decoded account key so we can mint fresh (e.g. read-only share)
/// SAS tokens on demand.
struct SasSigner {
    account: String,
    key: Vec<u8>,
}

impl SasSigner {
    fn hmac(&self, string_to_sign: &str) -> Result<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(StorageError::other)?;
        mac.update(string_to_sign.as_bytes());
        Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
    }

    /// Append an **account** SAS (string-to-sign per "Create an account SAS",
    /// version >= 2020-12-06) authorizing `srt`-scoped operations to `url`.
    /// Used for the broad connection-level credential (needs account scope to
    /// list containers); never for share links.
    fn apply_account_sas(
        &self,
        url: &mut Url,
        perms: &str,
        srt: &str,
        expiry: &str,
        protocol: &str,
    ) -> Result<()> {
        let string_to_sign = format!(
            "{account}\n{perms}\nb\n{srt}\n\n{expiry}\n\n{protocol}\n{sv}\n\n",
            account = self.account,
            sv = SAS_VERSION,
        );
        let sig = self.hmac(&string_to_sign)?;
        url.query_pairs_mut()
            .clear()
            .append_pair("sv", SAS_VERSION)
            .append_pair("ss", "b")
            .append_pair("srt", srt)
            .append_pair("sp", perms)
            .append_pair("se", expiry)
            .append_pair("spr", protocol)
            .append_pair("sig", &sig);
        Ok(())
    }

    /// Append a **service** SAS scoped to a single blob (signedResource `b`,
    /// string-to-sign per "Create a service SAS", version >= 2020-12-06) to
    /// `url`. The blob-scoped `canonicalizedResource` is part of the signature,
    /// so the link cannot be edited to read a different blob. Used for share
    /// links.
    fn apply_blob_service_sas(
        &self,
        url: &mut Url,
        container: &str,
        blob: &str,
        perms: &str,
        expiry: &str,
        protocol: &str,
    ) -> Result<()> {
        let canonicalized_resource = format!("/blob/{}/{container}/{blob}", self.account);
        // Field order (each on its own line; empty fields kept):
        // sp, st, se, canonicalizedResource, signedIdentifier, sip, spr, sv,
        // sr(=b), snapshotTime, signedEncryptionScope, rscc, rscd, rsce, rscl,
        // rsct (no trailing newline on the last field).
        let string_to_sign = format!(
            "{perms}\n\n{expiry}\n{res}\n\n\n{protocol}\n{sv}\nb\n\n\n\n\n\n\n",
            res = canonicalized_resource,
            sv = SAS_VERSION,
        );
        let sig = self.hmac(&string_to_sign)?;
        url.query_pairs_mut()
            .clear()
            .append_pair("sv", SAS_VERSION)
            .append_pair("sr", "b")
            .append_pair("sp", perms)
            .append_pair("se", expiry)
            .append_pair("spr", protocol)
            .append_pair("sig", &sig);
        Ok(())
    }
}

/// SAS protocol restriction. Real Azure is HTTPS-only; the Azurite emulator is
/// reached over plain HTTP, so it needs `https,http`.
fn sas_protocol(endpoint_base: &str) -> &'static str {
    let is_emulator = Url::parse(endpoint_base).ok().is_some_and(|u| {
        u.scheme() == "http"
            || matches!(
                u.host_str(),
                Some("127.0.0.1") | Some("localhost") | Some("[::1]")
            )
    });
    if is_emulator {
        "https,http"
    } else {
        "https"
    }
}

/// ISO-8601 UTC `YYYY-MM-DDThh:mm:ssZ`, `secs_from_now` in the future.
fn expiry_in(secs_from_now: i64) -> String {
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

pub struct AzBlobBackend {
    pub(crate) service: BlobServiceClient,
    /// Whether we hold a key capable of minting SAS links (AccountKey/ConnString).
    pub(crate) can_sign: bool,
    /// Endpoint base (no query), e.g. `http://127.0.0.1:10000/devstoreaccount1`.
    endpoint_base: String,
    signer: Option<Arc<SasSigner>>,
    /// SAS `spr` value: `https` for real Azure, `https,http` for the emulator.
    sas_protocol: &'static str,
}

/// Parse the AccountName/AccountKey/BlobEndpoint out of a connection string.
fn parse_connection_string(cs: &str) -> Result<(String, String, Option<String>)> {
    let mut account = None;
    let mut key = None;
    let mut endpoint = None;
    for part in cs.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            match k.trim() {
                "AccountName" => account = Some(v.trim().to_string()),
                "AccountKey" => key = Some(v.trim().to_string()),
                "BlobEndpoint" => endpoint = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    match (account, key) {
        (Some(a), Some(k)) => Ok((a, k, endpoint)),
        _ => Err(StorageError::AuthFailed {
            detail: "connection string missing AccountName/AccountKey".into(),
        }),
    }
}

impl AzBlobBackend {
    pub async fn connect(cfg: AzBlobConfig) -> Result<Self> {
        let (account, key_b64, endpoint, can_sign, sas_token) = match cfg.auth {
            AzAuth::AccountKey(k) => (
                cfg.account.clone(),
                Some(k),
                cfg.endpoint.clone(),
                true,
                None,
            ),
            AzAuth::ConnectionString(cs) => {
                let (a, k, ep) = parse_connection_string(&cs)?;
                let endpoint = cfg.endpoint.clone().or(ep);
                (a, Some(k), endpoint, true, None)
            }
            AzAuth::Sas(token) => (
                cfg.account.clone(),
                None,
                cfg.endpoint.clone(),
                false,
                Some(token),
            ),
        };

        let endpoint_base = endpoint
            .unwrap_or_else(|| format!("https://{account}.blob.core.windows.net"))
            .trim_end_matches('/')
            .to_string();

        let protocol = sas_protocol(&endpoint_base);

        // Build the service URL with whatever auth query string applies, then
        // construct the client with no credential (the SAS in the URL authorizes).
        let mut service_url = Url::parse(&endpoint_base).map_err(StorageError::other)?;
        let signer = match (&key_b64, &sas_token) {
            (Some(k), _) => {
                let key = base64::engine::general_purpose::STANDARD
                    .decode(k.trim())
                    .map_err(|e| StorageError::AuthFailed {
                        detail: format!("account key is not valid base64: {e}"),
                    })?;
                let signer = SasSigner {
                    account: account.clone(),
                    key,
                };
                // Connection-level account SAS: full blob permissions, all
                // resource types, valid for 7 days.
                signer.apply_account_sas(
                    &mut service_url,
                    "rwdlacup",
                    "sco",
                    &expiry_in(7 * 86_400),
                    protocol,
                )?;
                Some(Arc::new(signer))
            }
            (None, Some(token)) => {
                let token = token.trim_start_matches('?');
                service_url.set_query(Some(token));
                None
            }
            (None, None) => None,
        };

        let service =
            BlobServiceClient::new(service_url, None, None).map_err(StorageError::other)?;
        Ok(Self {
            service,
            can_sign,
            endpoint_base,
            signer,
            sas_protocol: protocol,
        })
    }

    /// TEST-ONLY: create the contract container if absent.
    pub async fn ensure_test_container(&self, container: &str) -> Result<()> {
        let cc = self.service.blob_container_client(container);
        match cc.create(None).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let s = format!("{e:?}").to_lowercase();
                if s.contains("containeralreadyexists")
                    || s.contains("alreadyexists")
                    || s.contains("conflict")
                    || s.contains("409")
                {
                    Ok(())
                } else {
                    Err(map_az(container, e))
                }
            }
        }
    }

    /// TEST-ONLY: put an APPEND blob at `path` (a non-block type), so overwrite /
    /// save-back can be exercised against the type that triggers `InvalidBlobType`.
    pub async fn put_append_blob_for_test(&self, path: &str, data: &[u8]) -> Result<()> {
        let p = ObjPath::parse(path);
        let container = p
            .container
            .ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        let ab = self
            .service
            .blob_container_client(&container)
            .blob_client(&p.key)
            .append_blob_client();
        ab.create(None).await.map_err(|e| map_az(path, e))?;
        if !data.is_empty() {
            ab.append_block(RequestContent::from(data.to_vec()), data.len() as u64, None)
                .await
                .map_err(|e| map_az(path, e))?;
        }
        Ok(())
    }

    /// Prefix used to address `key` as a directory: "" => "", "a/b" => "a/b/".
    fn dir_prefix(key: &str) -> String {
        if key.is_empty() {
            String::new()
        } else {
            format!("{}/", key.trim_end_matches('/'))
        }
    }

    /// (found_marker, has_children) for the synthesized dir at `prefix`.
    /// `found_marker` is the dir's own `prefix` blob; `has_children` is any blob
    /// strictly underneath it.
    async fn dir_children(
        &self,
        container: &str,
        prefix: &str,
        path: &str,
    ) -> Result<(bool, bool)> {
        let cc = self.service.blob_container_client(container);
        let opts = BlobContainerClientListBlobsOptions {
            prefix: Some(prefix.to_string()),
            ..Default::default()
        };
        let mut pager = cc.list_blobs(Some(opts)).map_err(|e| map_az(path, e))?;
        let mut found_marker = false;
        let mut has_children = false;
        while let Some(item) = pager.next().await {
            let item = item.map_err(|e| map_az(path, e))?;
            let name = item.name.unwrap_or_default();
            if name == prefix {
                found_marker = true;
            } else if name.starts_with(prefix) {
                has_children = true;
            }
            if found_marker && has_children {
                break;
            }
        }
        Ok((found_marker, has_children))
    }

    /// Server has no copy API in the GA SDK: download the source blob fully and
    /// re-upload it to the destination. Used by `rename` (files + dir markers).
    async fn copy_blob(
        &self,
        src_container: &str,
        src_key: &str,
        dst_container: &str,
        dst_key: &str,
        errctx: &str,
    ) -> Result<()> {
        let src = self
            .service
            .blob_container_client(src_container)
            .blob_client(src_key);
        let resp = src.download(None).await.map_err(|e| map_az(errctx, e))?;
        let data = resp.body.collect().await.map_err(|e| map_az(errctx, e))?;
        self.service
            .blob_container_client(dst_container)
            .blob_client(dst_key)
            .upload(RequestContent::from(data.to_vec()), None)
            .await
            .map(|_| ())
            .map_err(|e| map_az(errctx, e))
    }

    async fn list_containers(&self) -> Result<Vec<Entry>> {
        let mut out: Vec<Entry> = Vec::new();
        let mut pager = self
            .service
            .list_containers(None)
            .map_err(|e| map_az("/", e))?;
        while let Some(item) = pager.next().await {
            let item = item.map_err(|e| map_az("/", e))?;
            if let Some(name) = item.name {
                out.push(Entry {
                    name: name.clone(),
                    path: format!("/{name}"),
                    kind: EntryKind::Dir,
                    size: None,
                    modified_ms: None,
                });
            }
        }
        out.sort_by_key(|e| e.name.to_lowercase());
        Ok(out)
    }
}

/// Heuristic Azure-error mapping (status/code text) into the taxonomy. The
/// Inner text of the first `<tag>…</tag>` in `s`, trimmed. Used to pull the
/// `Code`/`Message` out of an Azure XML error body.
fn xml_tag<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let start = s.find(&format!("<{tag}>"))? + tag.len() + 2;
    let rest = &s[start..];
    let end = rest.find(&format!("</{tag}>"))?;
    Some(rest[..end].trim())
}

/// Condense an Azure error into a readable one-liner. Azure returns failures as
/// an XML body — `<Error><Code>X</Code><Message>Y</Message></Error>` — and the
/// SDK surfaces that raw (including a `RequestId:`/`Time:` trailer on `Message`),
/// which is what produced the wall-of-XML toast. We keep just `Code: Message`
/// (Message cut at its first line); with no XML body, the trimmed original.
fn az_error_summary(raw: &str) -> String {
    let code = xml_tag(raw, "Code");
    let msg = xml_tag(raw, "Message").map(|m| m.lines().next().unwrap_or(m).trim());
    match (code, msg) {
        (Some(c), Some(m)) => format!("{c}: {m}"),
        (Some(c), None) => c.to_string(),
        (None, Some(m)) => m.to_string(),
        (None, None) => raw.trim().to_string(),
    }
}

/// `Debug` form of `azure_core::Error` carries the HTTP status + storage error
/// code (e.g. `BlobNotFound`), which is enough for the taxonomy's coarse buckets.
fn map_az(path: &str, e: azure_core::Error) -> StorageError {
    let s = format!("{e:?}").to_lowercase();
    if s.contains("blobnotfound")
        || s.contains("containernotfound")
        || s.contains("the specified blob does not exist")
        || s.contains("404")
    {
        StorageError::NotFound { path: path.into() }
    } else if s.contains("authenticationfailed")
        || s.contains("authorization")
        || s.contains("forbidden")
        || s.contains("403")
    {
        StorageError::PermissionDenied { path: path.into() }
    } else if s.contains("timeout")
        || s.contains("connect")
        || s.contains("dns")
        || s.contains("transport")
    {
        StorageError::Network {
            detail: e.to_string(),
        }
    } else {
        StorageError::Other {
            detail: az_error_summary(&e.to_string()),
        }
    }
}

#[async_trait]
impl StorageBackend for AzBlobBackend {
    fn capabilities(&self) -> Capabilities {
        // can_presign only when we hold a key that can mint a SAS.
        Capabilities {
            can_presign: self.can_sign,
            can_rename: true,
            can_set_mtime: false,
        }
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let p = ObjPath::parse(path);
        let Some(container) = p.container else {
            return self.list_containers().await;
        };
        let prefix = Self::dir_prefix(&p.key);
        let cc = self.service.blob_container_client(&container);
        let opts = BlobContainerClientListBlobsOptions {
            prefix: Some(prefix.clone()),
            ..Default::default()
        };
        let mut pager = cc.list_blobs(Some(opts)).map_err(|e| map_az(path, e))?;

        // Synthesize immediate-child directories client-side (no server delimiter).
        let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut files: Vec<Entry> = Vec::new();
        while let Some(item) = pager.next().await {
            let item = item.map_err(|e| map_az(path, e))?;
            let name = item.name.unwrap_or_default();
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() {
                continue; // the dir's own marker blob
            }
            match rest.find('/') {
                Some(idx) => {
                    dirs.insert(rest[..idx].to_string());
                }
                None => {
                    let props = item.properties;
                    files.push(Entry {
                        name: basename(&name),
                        path: format!("/{container}/{name}"),
                        kind: EntryKind::File,
                        size: props.as_ref().and_then(|p| p.content_length),
                        modified_ms: props
                            .as_ref()
                            .and_then(|p| p.last_modified)
                            .map(|t| t.unix_timestamp() * 1000),
                    });
                }
            }
        }

        let mut out: Vec<Entry> = dirs
            .into_iter()
            .map(|d| Entry {
                name: d.clone(),
                path: format!("/{container}/{prefix}{d}"),
                kind: EntryKind::Dir,
                size: None,
                modified_ms: None,
            })
            .collect();
        out.extend(files);
        out.sort_by(|a, b| {
            (b.kind == EntryKind::Dir)
                .cmp(&(a.kind == EntryKind::Dir))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<Entry> {
        let p = ObjPath::parse(path);
        let Some(container) = p.container else {
            return Ok(Entry {
                name: String::new(),
                path: "/".into(),
                kind: EntryKind::Dir,
                size: None,
                modified_ms: None,
            });
        };
        let cc = self.service.blob_container_client(&container);
        if p.key.is_empty() {
            // Container root: get_properties exists => Dir, else NotFound.
            cc.get_properties(None).await.map_err(|e| map_az(path, e))?;
            return Ok(Entry {
                name: container.clone(),
                path: format!("/{container}"),
                kind: EntryKind::Dir,
                size: None,
                modified_ms: None,
            });
        }
        let bc = cc.blob_client(&p.key);
        match bc.get_properties(None).await {
            Ok(props) => Ok(Entry {
                name: basename(&p.key),
                path: format!("/{container}/{}", p.key),
                kind: EntryKind::File,
                size: props.content_length().ok().flatten(),
                modified_ms: props
                    .last_modified()
                    .ok()
                    .flatten()
                    .map(|t| t.unix_timestamp() * 1000),
            }),
            Err(e) => {
                let mapped = map_az(path, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
                // Not a blob — is it a synthesized directory? (marker or children)
                let prefix = Self::dir_prefix(&p.key);
                let (found_marker, has_children) =
                    self.dir_children(&container, &prefix, path).await?;
                if found_marker || has_children {
                    Ok(Entry {
                        name: basename(&p.key),
                        path: format!("/{container}/{}", p.key.trim_end_matches('/')),
                        kind: EntryKind::Dir,
                        size: None,
                        modified_ms: None,
                    })
                } else {
                    Err(StorageError::NotFound { path: path.into() })
                }
            }
        }
    }

    async fn read(&self, path: &str, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        let p = ObjPath::parse(path);
        let container = p
            .container
            .ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        let bc = self
            .service
            .blob_container_client(&container)
            .blob_client(&p.key);
        let opts = if offset > 0 {
            Some(BlobClientDownloadOptions {
                range: Some(HttpRange::from_offset(offset)),
                ..Default::default()
            })
        } else {
            None
        };
        let resp = bc.download(opts).await.map_err(|e| map_az(path, e))?;
        let stream = resp.body.map(|r| r.map_err(io::Error::other));
        Ok(Box::new(tokio_util::io::StreamReader::new(stream)))
    }

    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "cannot write at the container-list root".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported {
                op: "cannot write a container".into(),
            });
        }
        // Keep the generic BlobClient alongside the block client: if the target
        // name already holds a non-block blob (append/page), Put Block is rejected
        // with InvalidBlobType, and the writer recovers by deleting it through the
        // generic client and recreating as a block blob (see AzBlockWriter).
        let blob_client = self
            .service
            .blob_container_client(&container)
            .blob_client(&p.key);
        let blob = blob_client.block_blob_client();
        Ok(Box::new(AzBlockWriter {
            blob: Arc::new(blob),
            blob_client: Arc::new(blob_client),
            buf: BytesMut::with_capacity(PART_SIZE),
            block_index: 0,
            block_ids: Vec::new(),
            state: WState::Idle,
        }))
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let p = ObjPath::parse(path);
        let container = p
            .container
            .ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported {
                op: "refusing to delete a container".into(),
            });
        }
        let cc = self.service.blob_container_client(&container);
        // File?
        let bc = cc.blob_client(&p.key);
        match bc.get_properties(None).await {
            Ok(_) => {
                return bc
                    .delete(None)
                    .await
                    .map(|_| ())
                    .map_err(|e| map_az(path, e));
            }
            Err(e) => {
                let mapped = map_az(path, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
            }
        }
        // Directory: inspect children, excluding the dir's own marker.
        let prefix = Self::dir_prefix(&p.key);
        let (found_marker, has_children) = self.dir_children(&container, &prefix, path).await?;
        if !found_marker && !has_children {
            return Err(StorageError::NotFound { path: path.into() });
        }
        if has_children {
            return Err(StorageError::Conflict {
                path: path.into(),
                detail: "directory not empty".into(),
            });
        }
        // Only the marker remains → delete it.
        cc.blob_client(&prefix)
            .delete(None)
            .await
            .map(|_| ())
            .map_err(|e| map_az(path, e))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let pf = ObjPath::parse(from);
        let pt = ObjPath::parse(to);
        let cf = pf
            .container
            .ok_or_else(|| StorageError::NotFound { path: from.into() })?;
        let ct = pt
            .container
            .ok_or_else(|| StorageError::NotFound { path: to.into() })?;
        if pf.key.is_empty() || pt.key.is_empty() {
            return Err(StorageError::Unsupported {
                op: "cannot rename a container".into(),
            });
        }
        let src_bc = self.service.blob_container_client(&cf).blob_client(&pf.key);
        // No native blob rename and no copy API in the GA SDK: read the source
        // and re-upload to the destination, then delete the source.
        match src_bc.get_properties(None).await {
            Ok(_) => {
                self.copy_blob(&cf, &pf.key, &ct, &pt.key, from).await?;
                src_bc
                    .delete(None)
                    .await
                    .map(|_| ())
                    .map_err(|e| map_az(from, e))
            }
            Err(e) => {
                let mapped = map_az(from, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
                // Directory rename: refuse non-empty (v1); move marker if empty.
                let prefix = Self::dir_prefix(&pf.key);
                let (found_marker, has_children) = self.dir_children(&cf, &prefix, from).await?;
                if !found_marker && !has_children {
                    return Err(StorageError::NotFound { path: from.into() });
                }
                if has_children {
                    return Err(StorageError::Conflict {
                        path: from.into(),
                        detail: "renaming a non-empty directory is not supported (v1)".into(),
                    });
                }
                let new_marker = Self::dir_prefix(&pt.key);
                self.copy_blob(&cf, &prefix, &ct, &new_marker, from).await?;
                self.service
                    .blob_container_client(&cf)
                    .blob_client(&prefix)
                    .delete(None)
                    .await
                    .map(|_| ())
                    .map_err(|e| map_az(from, e))
            }
        }
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "creating containers is not supported".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported {
                op: "cannot mkdir a container".into(),
            });
        }
        let marker = Self::dir_prefix(&p.key);
        // Zero-byte marker blob (Azure flat namespace has no real dirs).
        self.service
            .blob_container_client(&container)
            .blob_client(&marker)
            .upload(RequestContent::from(Vec::new()), None)
            .await
            .map(|_| ())
            .map_err(|e| map_az(path, e))
    }

    async fn share_link(&self, path: &str, expiry_secs: u64) -> Result<String> {
        let Some(signer) = self.signer.as_ref() else {
            return Err(StorageError::Unsupported {
                op: "SAS-token auth cannot mint new share links".into(),
            });
        };
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "cannot share the container-list root".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported {
                op: "cannot share a container".into(),
            });
        }
        // Read-only, single-blob-scoped service SAS (the blob path is part of
        // the signature, so the link can't be edited to read another blob).
        let mut url = Url::parse(&format!("{}/{}/{}", self.endpoint_base, container, p.key))
            .map_err(StorageError::other)?;
        signer.apply_blob_service_sas(
            &mut url,
            &container,
            &p.key,
            "r",
            &expiry_in(expiry_secs as i64),
            self.sas_protocol,
        )?;
        Ok(url.to_string())
    }
}

type BoxFut = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

enum WState {
    Idle,
    Staging(BoxFut),
    Committing(BoxFut),
    Done,
}

/// Fixed-width raw block id (Azure requires every staged block id to share the
/// same pre-base64 length). The SDK base64-encodes these for `stage_block` and
/// the committed block list, so we hand it the raw bytes in both places.
fn block_id(index: u32) -> Vec<u8> {
    format!("wb-block-{index:016}").into_bytes()
}

fn to_io(e: StorageError) -> io::Error {
    io::Error::other(e.to_string())
}

/// Does this Azure error carry the `InvalidBlobType` code? Returned (409) when an
/// operation is wrong for the existing blob's type — here, a `Put Block` against a
/// name already holding an append or page blob. The code lives in the Debug form
/// (the XML error body), matched case-insensitively.
fn is_invalid_blob_type(e: &azure_core::Error) -> bool {
    format!("{e:?}").to_lowercase().contains("invalidblobtype")
}

/// Buffers 8 MiB blocks, stages each, and commits the block list on
/// `poll_shutdown`. `Unpin` because every field is `Unpin` (the in-flight
/// future is boxed). Uncommitted blocks are garbage-collected by Azure
/// automatically (~7 days), so an abandoned writer needs no explicit cleanup.
pub struct AzBlockWriter {
    blob: Arc<BlockBlobClient>,
    /// Generic client for the same blob, used only to delete-and-recreate when the
    /// existing blob turns out to be a non-block (append/page) type (see below).
    blob_client: Arc<BlobClient>,
    buf: BytesMut,
    block_index: u32,
    block_ids: Vec<Vec<u8>>,
    state: WState,
}

impl AzBlockWriter {
    /// Drain the current buffer into a new stage-block future.
    ///
    /// Only the FIRST block can meet a pre-existing blob of the wrong type: if the
    /// name already holds an append or page blob, `Put Block` fails with
    /// `InvalidBlobType`. Azure has no in-place type conversion, so we delete the
    /// existing blob and retry the stage — the deleted name then accepts a fresh
    /// block blob. Subsequent blocks build on that block blob and need no recovery,
    /// so only the first block clones its body for a possible retry.
    fn start_stage(&mut self) {
        let id = block_id(self.block_index);
        let recover = self.block_index == 0;
        self.block_index += 1;
        self.block_ids.push(id.clone());
        let body = self.buf.split().to_vec();
        let len = body.len() as u64;
        let blob = self.blob.clone();
        if recover {
            let del = self.blob_client.clone();
            let retry = body.clone();
            self.state = WState::Staging(Box::pin(async move {
                match blob
                    .stage_block(&id, len, RequestContent::from(body), None)
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(e) if is_invalid_blob_type(&e) => {
                        del.delete(None).await.map_err(|e| map_az("delete", e))?;
                        blob.stage_block(&id, len, RequestContent::from(retry), None)
                            .await
                            .map_err(|e| map_az("stage_block", e))?;
                        Ok(())
                    }
                    Err(e) => Err(map_az("stage_block", e)),
                }
            }));
        } else {
            self.state = WState::Staging(Box::pin(async move {
                blob.stage_block(&id, len, RequestContent::from(body), None)
                    .await
                    .map_err(|e| map_az("stage_block", e))?;
                Ok(())
            }));
        }
    }

    /// Kick off Put Block List with the staged ids (in stage order).
    fn start_commit(&mut self) {
        let blob = self.blob.clone();
        let ids = std::mem::take(&mut self.block_ids);
        self.state = WState::Committing(Box::pin(async move {
            let list = BlockLookupList {
                latest: Some(ids),
                ..Default::default()
            };
            let content: RequestContent<BlockLookupList, XmlFormat> =
                list.try_into().map_err(StorageError::other)?;
            blob.commit_block_list(content, None)
                .await
                .map_err(|e| map_az("commit_block_list", e))?;
            Ok(())
        }));
    }

    fn drive_staging(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = match &mut self.state {
            WState::Staging(fut) => fut.as_mut().poll(cx),
            _ => return Poll::Ready(Ok(())),
        };
        match poll {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => {
                self.state = WState::Idle;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => {
                self.state = WState::Done;
                Poll::Ready(Err(to_io(e)))
            }
        }
    }

    fn drive_committing(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = match &mut self.state {
            WState::Committing(fut) => fut.as_mut().poll(cx),
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
}

impl AsyncWrite for AzBlockWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        // Never accept new bytes while a block stage is in flight.
        if let WState::Staging(_) = this.state {
            match this.drive_staging(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        this.buf.extend_from_slice(data);
        if this.buf.len() >= PART_SIZE {
            this.start_stage();
        }
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Only drain an in-flight stage; don't force-cut a sub-block-size block.
        self.get_mut().drive_staging(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            match this.state {
                WState::Staging(_) => match this.drive_staging(cx) {
                    Poll::Ready(Ok(())) => continue,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                },
                WState::Idle => {
                    // Stage the tail; an all-empty write stages one empty block
                    // so the committed list has >= 1 block (a 0-byte blob).
                    if this.block_ids.is_empty() || !this.buf.is_empty() {
                        this.start_stage();
                    } else {
                        this.start_commit();
                    }
                }
                WState::Committing(_) => match this.drive_committing(cx) {
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
    use super::az_error_summary;

    #[test]
    fn summarizes_azure_xml_error_to_code_and_message() {
        // The real shape from a failed save: status prefix + XML body, with a
        // RequestId/Time trailer on the Message line.
        let raw = "409: <?xml version=\"1.0\" encoding=\"utf-8\"?><Error><Code>InvalidBlobType</Code><Message>The blob type is invalid for this operation.\nRequestId:a2c31cd8-601e-00f9-38ab-f716d8000000\nTime:2026-06-09T01:01:44.7581809Z</Message></Error>";
        assert_eq!(
            az_error_summary(raw),
            "InvalidBlobType: The blob type is invalid for this operation."
        );
    }

    #[test]
    fn passes_through_plain_text_without_xml() {
        assert_eq!(az_error_summary("  connection reset  "), "connection reset");
    }
}
