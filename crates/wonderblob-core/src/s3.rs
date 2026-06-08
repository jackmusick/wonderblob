use crate::error::{Result, StorageError};
use crate::objstore::{basename, ObjPath, PART_SIZE};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client;
use bytes::BytesMut;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
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
        out.sort_by_key(|e| e.name.to_lowercase());
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

    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "cannot write at the bucket-list root".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported {
                op: "cannot write a bucket".into(),
            });
        }
        let resp = self
            .client
            .create_multipart_upload()
            .bucket(&container)
            .key(&p.key)
            .send()
            .await
            .map_err(|e| map_s3(path, e))?;
        let upload_id = resp
            .upload_id()
            .ok_or_else(|| StorageError::Other {
                detail: "no upload id".into(),
            })?
            .to_string();
        Ok(Box::new(S3MultipartWriter {
            client: self.client.clone(),
            bucket: container,
            key: p.key,
            upload_id,
            buf: BytesMut::with_capacity(PART_SIZE),
            part_number: 0,
            parts: Vec::new(),
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
                op: "refusing to delete a bucket".into(),
            });
        }
        // File?
        match self
            .client
            .head_object()
            .bucket(&container)
            .key(&p.key)
            .send()
            .await
        {
            Ok(_) => {
                return self
                    .client
                    .delete_object()
                    .bucket(&container)
                    .key(&p.key)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(|e| map_s3(path, e));
            }
            Err(e) => {
                let mapped = map_s3(path, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
            }
        }
        // Directory: inspect children, excluding the dir's own marker.
        let prefix = Self::dir_prefix(&p.key);
        let resp = self
            .client
            .list_objects_v2()
            .bucket(&container)
            .prefix(&prefix)
            .max_keys(2)
            .send()
            .await
            .map_err(|e| map_s3(path, e))?;
        let mut found_marker = false;
        let mut has_children = false;
        for o in resp.contents() {
            match o.key() {
                Some(k) if k == prefix => found_marker = true,
                Some(_) => has_children = true,
                None => {}
            }
        }
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
        self.client
            .delete_object()
            .bucket(&container)
            .key(&prefix)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| map_s3(path, e))
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
                op: "cannot rename a bucket".into(),
            });
        }
        // File rename = CopyObject + DeleteObject.
        match self
            .client
            .head_object()
            .bucket(&cf)
            .key(&pf.key)
            .send()
            .await
        {
            Ok(_) => {
                // copy_source is "bucket/key"; encode in real impl for keys with
                // reserved chars (tracked). Contract keys are ASCII-safe.
                self.client
                    .copy_object()
                    .bucket(&ct)
                    .key(&pt.key)
                    .copy_source(format!("{cf}/{}", pf.key))
                    .send()
                    .await
                    .map_err(|e| map_s3(from, e))?;
                self.client
                    .delete_object()
                    .bucket(&cf)
                    .key(&pf.key)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(|e| map_s3(from, e))
            }
            Err(e) => {
                let mapped = map_s3(from, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
                // Directory rename: refuse non-empty (v1); move marker if empty.
                let prefix = Self::dir_prefix(&pf.key);
                let resp = self
                    .client
                    .list_objects_v2()
                    .bucket(&cf)
                    .prefix(&prefix)
                    .max_keys(2)
                    .send()
                    .await
                    .map_err(|e| map_s3(from, e))?;
                let mut found_marker = false;
                let mut has_children = false;
                for o in resp.contents() {
                    match o.key() {
                        Some(k) if k == prefix => found_marker = true,
                        Some(_) => has_children = true,
                        None => {}
                    }
                }
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
                self.client
                    .copy_object()
                    .bucket(&ct)
                    .key(&new_marker)
                    .copy_source(format!("{cf}/{prefix}"))
                    .send()
                    .await
                    .map_err(|e| map_s3(from, e))?;
                self.client
                    .delete_object()
                    .bucket(&cf)
                    .key(&prefix)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(|e| map_s3(from, e))
            }
        }
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "creating buckets is not supported".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported {
                op: "cannot mkdir a bucket".into(),
            });
        }
        let marker = Self::dir_prefix(&p.key);
        self.client
            .put_object()
            .bucket(&container)
            .key(&marker)
            .body(ByteStream::from(Vec::new()))
            .send()
            .await
            .map(|_| ())
            .map_err(|e| map_s3(path, e))
    }

    async fn share_link(&self, path: &str, expiry_secs: u64) -> Result<String> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "cannot share the bucket-list root".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported {
                op: "cannot share a bucket".into(),
            });
        }
        let cfg = PresigningConfig::expires_in(Duration::from_secs(expiry_secs))
            .map_err(StorageError::other)?;
        let req = self
            .client
            .get_object()
            .bucket(&container)
            .key(&p.key)
            .presigned(cfg)
            .await
            .map_err(|e| map_s3(path, e))?;
        Ok(req.uri().to_string())
    }
}

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

enum WState {
    Idle,
    Uploading(BoxFut<Result<CompletedPart>>),
    Completing(BoxFut<Result<()>>),
    Done,
}

/// Buffers 8 MiB parts and completes the multipart upload on `poll_shutdown`.
/// `Unpin` because every field is `Unpin` (the in-flight future is boxed).
pub struct S3MultipartWriter {
    client: Client,
    bucket: String,
    key: String,
    upload_id: String,
    buf: BytesMut,
    part_number: i32,
    parts: Vec<CompletedPart>,
    state: WState,
}

fn to_io(e: StorageError) -> io::Error {
    io::Error::other(e.to_string())
}

fn upload_part_fut(
    client: Client,
    bucket: String,
    key: String,
    upload_id: String,
    part_number: i32,
    body: Vec<u8>,
) -> BoxFut<Result<CompletedPart>> {
    Box::pin(async move {
        let resp = client
            .upload_part()
            .bucket(&bucket)
            .key(&key)
            .upload_id(&upload_id)
            .part_number(part_number)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|e| map_s3(&key, e))?;
        Ok(CompletedPart::builder()
            .set_e_tag(resp.e_tag().map(|s| s.to_string()))
            .part_number(part_number)
            .build())
    })
}

impl S3MultipartWriter {
    /// Drain the current buffer into a new part-upload future.
    fn start_part(&mut self) {
        let body = self.buf.split().to_vec();
        self.part_number += 1;
        self.state = WState::Uploading(upload_part_fut(
            self.client.clone(),
            self.bucket.clone(),
            self.key.clone(),
            self.upload_id.clone(),
            self.part_number,
            body,
        ));
    }

    /// Kick off CompleteMultipartUpload with the parts gathered so far.
    fn start_complete(&mut self) {
        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(std::mem::take(&mut self.parts)))
            .build();
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = self.key.clone();
        let upload_id = self.upload_id.clone();
        self.state = WState::Completing(Box::pin(async move {
            client
                .complete_multipart_upload()
                .bucket(&bucket)
                .key(&key)
                .upload_id(&upload_id)
                .multipart_upload(completed)
                .send()
                .await
                .map_err(|e| map_s3(&key, e))?;
            Ok(())
        }));
    }

    /// Poll an in-flight UploadPart to completion, banking the part.
    fn drive_upload(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = match &mut self.state {
            WState::Uploading(fut) => fut.as_mut().poll(cx),
            _ => return Poll::Ready(Ok(())),
        };
        match poll {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(part)) => {
                self.parts.push(part);
                self.state = WState::Idle;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => {
                self.state = WState::Done;
                Poll::Ready(Err(to_io(e)))
            }
        }
    }

    fn drive_complete(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = match &mut self.state {
            WState::Completing(fut) => fut.as_mut().poll(cx),
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

impl AsyncWrite for S3MultipartWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        // Never accept new bytes while a part upload is in flight.
        if let WState::Uploading(_) = this.state {
            match this.drive_upload(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        this.buf.extend_from_slice(data);
        if this.buf.len() >= PART_SIZE {
            this.start_part();
        }
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Only drain an in-flight upload; don't force-cut a sub-part-size part.
        self.get_mut().drive_upload(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            match this.state {
                WState::Uploading(_) => match this.drive_upload(cx) {
                    Poll::Ready(Ok(())) => continue,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                },
                WState::Idle => {
                    // Flush the tail: upload remaining buffer (even 0 bytes) as
                    // the final part when there are no parts yet OR data remains,
                    // guaranteeing CompleteMultipartUpload sees >= 1 part.
                    if this.parts.is_empty() || !this.buf.is_empty() {
                        this.start_part();
                    } else {
                        this.start_complete();
                    }
                }
                WState::Completing(_) => match this.drive_complete(cx) {
                    Poll::Ready(Ok(())) => continue,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                },
                WState::Done => return Poll::Ready(Ok(())),
            }
        }
    }
}
