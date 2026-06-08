use crate::error::{Result, StorageError};
use crate::objstore::{basename, ObjPath};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::Client;
use tokio::io::{AsyncRead, AsyncWrite};

/// Explicit-credential config. Profile/SSO support is a tracked stretch item.
pub struct S3Config {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: Option<String>,
    /// Custom endpoint for MinIO/Wasabi/R2; `None` => real AWS.
    pub endpoint: Option<String>,
    /// Path-style addressing (required by MinIO and most S3-compatible servers).
    pub force_path_style: bool,
}

pub struct S3Backend {
    pub(crate) client: Client,
}

impl S3Backend {
    pub async fn connect(cfg: S3Config) -> Result<Self> {
        let creds = Credentials::new(
            cfg.access_key_id,
            cfg.secret_access_key,
            None,
            None,
            "wonderblob",
        );
        let region = Region::new(cfg.region.unwrap_or_else(|| "us-east-1".to_string()));
        let mut builder = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(creds)
            .region(region)
            .force_path_style(cfg.force_path_style);
        if let Some(ep) = cfg.endpoint {
            builder = builder.endpoint_url(ep);
        }
        Ok(Self {
            client: Client::from_conf(builder.build()),
        })
    }

    /// TEST-ONLY: create the contract bucket if it doesn't already exist.
    /// Not part of the trait; the contract harness calls it to self-bootstrap.
    pub async fn ensure_test_bucket(&self, bucket: &str) -> Result<()> {
        match self.client.create_bucket().bucket(bucket).send().await {
            Ok(_) => Ok(()),
            Err(e) => {
                let s = format!("{e:?}").to_lowercase();
                if s.contains("alreadyownedbyyou") || s.contains("alreadyexists") {
                    Ok(())
                } else {
                    Err(map_s3(bucket, e))
                }
            }
        }
    }
}

/// Heuristic SDK-error mapping into the taxonomy. The typed error variants
/// (`NoSuchKey`, `NotFound`, …) vary by operation, so we inspect the debug/
/// display text — robust enough for the taxonomy's coarse buckets.
fn map_s3<E: std::fmt::Debug + std::fmt::Display>(
    path: &str,
    e: aws_sdk_s3::error::SdkError<E>,
) -> StorageError {
    let s = format!("{e:?}").to_lowercase();
    if s.contains("notfound")
        || s.contains("nosuchkey")
        || s.contains("nosuchbucket")
        || s.contains("404")
    {
        StorageError::NotFound { path: path.into() }
    } else if s.contains("accessdenied")
        || s.contains("invalidaccesskey")
        || s.contains("signaturedoesnotmatch")
        || s.contains("403")
    {
        StorageError::PermissionDenied { path: path.into() }
    } else if s.contains("dispatchfailure") || s.contains("timeout") || s.contains("connect") {
        StorageError::Network {
            detail: e.to_string(),
        }
    } else {
        StorageError::Other {
            detail: e.to_string(),
        }
    }
}

fn dir_entry(container: &str, prefix: &str) -> Entry {
    let trimmed = prefix.trim_end_matches('/');
    Entry {
        name: basename(trimmed),
        path: format!("/{container}/{trimmed}"),
        kind: EntryKind::Dir,
        size: None,
        modified_ms: None,
    }
}

impl S3Backend {
    /// Prefix used to address `key` as a directory: "" => "", "a/b" => "a/b/".
    fn dir_prefix(key: &str) -> String {
        if key.is_empty() {
            String::new()
        } else {
            format!("{}/", key.trim_end_matches('/'))
        }
    }

    async fn list_buckets(&self) -> Result<Vec<Entry>> {
        let resp = self
            .client
            .list_buckets()
            .send()
            .await
            .map_err(|e| map_s3("/", e))?;
        let mut out: Vec<Entry> = resp
            .buckets()
            .iter()
            .filter_map(|b| b.name())
            .map(|name| Entry {
                name: name.to_string(),
                path: format!("/{name}"),
                kind: EntryKind::Dir,
                size: None,
                modified_ms: None,
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(out)
    }
}

#[async_trait]
impl StorageBackend for S3Backend {
    fn capabilities(&self) -> Capabilities {
        // can_set_mtime=false: object stores don't expose settable mtime.
        Capabilities {
            can_presign: true,
            can_rename: true,
            can_set_mtime: false,
        }
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let p = ObjPath::parse(path);
        let Some(container) = p.container else {
            return self.list_buckets().await;
        };
        let prefix = Self::dir_prefix(&p.key);
        let mut out: Vec<Entry> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&container)
                .delimiter("/")
                .prefix(&prefix);
            if let Some(t) = &token {
                req = req.continuation_token(t);
            }
            let resp = req.send().await.map_err(|e| map_s3(path, e))?;
            for cp in resp.common_prefixes() {
                if let Some(pfx) = cp.prefix() {
                    out.push(dir_entry(&container, pfx));
                }
            }
            for obj in resp.contents() {
                let key = obj.key().unwrap_or_default();
                // Skip the dir's own marker object and any directory markers.
                if key == prefix || key.ends_with('/') {
                    continue;
                }
                out.push(Entry {
                    name: basename(key),
                    path: format!("/{container}/{key}"),
                    kind: EntryKind::File,
                    size: obj.size().map(|s| s as u64),
                    modified_ms: obj.last_modified().and_then(|d| d.to_millis().ok()),
                });
            }
            if resp.is_truncated().unwrap_or(false) {
                token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }
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
        if p.key.is_empty() {
            // Container root: exists iff HeadBucket succeeds.
            self.client
                .head_bucket()
                .bucket(&container)
                .send()
                .await
                .map_err(|e| map_s3(path, e))?;
            return Ok(Entry {
                name: container.clone(),
                path: format!("/{container}"),
                kind: EntryKind::Dir,
                size: None,
                modified_ms: None,
            });
        }
        // Try the object itself.
        match self
            .client
            .head_object()
            .bucket(&container)
            .key(&p.key)
            .send()
            .await
        {
            Ok(h) => Ok(Entry {
                name: basename(&p.key),
                path: format!("/{container}/{}", p.key),
                kind: EntryKind::File,
                size: h.content_length().map(|s| s as u64),
                modified_ms: h.last_modified().and_then(|d| d.to_millis().ok()),
            }),
            Err(e) => {
                let mapped = map_s3(path, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
                // Not an object — is it a synthesized directory? (marker or children)
                let prefix = Self::dir_prefix(&p.key);
                let resp = self
                    .client
                    .list_objects_v2()
                    .bucket(&container)
                    .prefix(&prefix)
                    .max_keys(1)
                    .send()
                    .await
                    .map_err(|e| map_s3(path, e))?;
                if resp.key_count().unwrap_or(0) > 0 || !resp.contents().is_empty() {
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
        let mut req = self.client.get_object().bucket(&container).key(&p.key);
        if offset > 0 {
            req = req.range(format!("bytes={offset}-"));
        }
        let resp = req.send().await.map_err(|e| map_s3(path, e))?;
        Ok(Box::new(resp.body.into_async_read()))
    }

    async fn write(&self, _path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        Err(StorageError::Unsupported {
            op: "s3 write (Task 4)".into(),
        })
    }

    async fn delete(&self, _path: &str) -> Result<()> {
        Err(StorageError::Unsupported {
            op: "s3 delete (Task 4)".into(),
        })
    }

    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Err(StorageError::Unsupported {
            op: "s3 rename (Task 4)".into(),
        })
    }

    async fn mkdir(&self, _path: &str) -> Result<()> {
        Err(StorageError::Unsupported {
            op: "s3 mkdir (Task 4)".into(),
        })
    }

    async fn share_link(&self, _path: &str, _expiry: u64) -> Result<String> {
        Err(StorageError::Unsupported {
            op: "s3 share_link (Task 4)".into(),
        })
    }
}
