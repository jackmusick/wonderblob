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
//! requests. Share links reuse the same signer to mint a short-lived,
//! read-only, object-scoped account SAS.
//!
//! ## Listing
//! The GA SDK's `list_blobs` exposes no `delimiter` parameter, so there are no
//! server-side `BlobPrefix` entries. We list flat under the prefix and
//! synthesize immediate child directories client-side from the key segments.

use crate::error::{Result, StorageError};
use crate::objstore::{basename, ObjPath};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use azure_core::http::Url;
use azure_storage_blob::models::{
    BlobClientDownloadOptions, BlobClientGetPropertiesResultHeaders,
    BlobContainerClientListBlobsOptions, HttpRange,
};
use azure_storage_blob::BlobServiceClient;
use base64::Engine as _;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io;
use std::sync::Arc;
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
    /// Compute the `sig` value for an account SAS over the given fields
    /// (string-to-sign per "Create an account SAS", version >= 2020-12-06).
    fn sign(&self, perms: &str, srt: &str, expiry: &str) -> Result<String> {
        let string_to_sign = format!(
            "{account}\n{perms}\nb\n{srt}\n\n{expiry}\n\nhttps,http\n{sv}\n\n",
            account = self.account,
            perms = perms,
            srt = srt,
            expiry = expiry,
            sv = SAS_VERSION,
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(StorageError::other)?;
        mac.update(string_to_sign.as_bytes());
        Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
    }

    /// Append an account-SAS query string (authorizing the given scope) to `url`.
    fn apply_sas(&self, url: &mut Url, perms: &str, srt: &str, expiry: &str) -> Result<()> {
        let sig = self.sign(perms, srt, expiry)?;
        url.query_pairs_mut()
            .clear()
            .append_pair("sv", SAS_VERSION)
            .append_pair("ss", "b")
            .append_pair("srt", srt)
            .append_pair("sp", perms)
            .append_pair("se", expiry)
            .append_pair("spr", "https,http")
            .append_pair("sig", &sig);
        Ok(())
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
            AzAuth::AccountKey(k) => (cfg.account.clone(), Some(k), cfg.endpoint.clone(), true, None),
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
                signer.apply_sas(&mut service_url, "rwdlacup", "sco", &expiry_in(7 * 86_400))?;
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
    async fn dir_children(&self, container: &str, prefix: &str, path: &str) -> Result<(bool, bool)> {
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
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(out)
    }
}

/// Heuristic Azure-error mapping (status/code text) into the taxonomy. The
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
            detail: e.to_string(),
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

    async fn write(&self, _path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        Err(StorageError::Unsupported {
            op: "azblob write (Task 7)".into(),
        })
    }
    async fn delete(&self, _path: &str) -> Result<()> {
        Err(StorageError::Unsupported {
            op: "azblob delete (Task 7)".into(),
        })
    }
    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Err(StorageError::Unsupported {
            op: "azblob rename (Task 7)".into(),
        })
    }
    async fn mkdir(&self, _path: &str) -> Result<()> {
        Err(StorageError::Unsupported {
            op: "azblob mkdir (Task 7)".into(),
        })
    }
    async fn share_link(&self, _path: &str, _expiry: u64) -> Result<String> {
        Err(StorageError::Unsupported {
            op: "azblob share_link (Task 7)".into(),
        })
    }
}
