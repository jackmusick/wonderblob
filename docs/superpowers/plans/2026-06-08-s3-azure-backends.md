# Wonderblob Plan 2: S3 + Azure Blob Backends

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two cloud object-storage backends to the existing `StorageBackend` VFS — Amazon S3 (and S3-compatible endpoints: MinIO, Wasabi, Cloudflare R2) and Azure Blob Storage — both passing the *same* contract suite the SFTP backend passes, wired end-to-end through bookmarks, the Tauri command layer, and the 1Password-8-style frontend, including a "Share Link" action backed by presigned/SAS URLs.

**Architecture:** Two new modules in the UI-agnostic `wonderblob-core` crate (`s3.rs`, `azblob.rs`), each implementing `StorageBackend` unchanged. Object stores have a flat namespace, so both backends synthesize a directory tree: the root listing (`/`) surfaces buckets/containers, and `/bucket/prefix/...` lists objects under a key prefix using a `/` delimiter (CommonPrefixes/BlobPrefix → dirs, objects → files). Streaming writes use multipart/block uploads via an `AsyncWrite` adapter that completes the upload on `poll_shutdown`. The `src-tauri` crate gains typed connect commands and protocol-specific bookmark fields; the Svelte frontend grows a protocol picker, protocol badges, and a capability-gated Share Link toolbar action. Contract tests run against Dockerized MinIO and Azurite fixtures, gated by env flags so plain `cargo test` stays Docker-free.

**Tech Stack:** Rust (`aws-sdk-s3`, `aws-config`, `aws-smithy-types`, `azure_storage_blob`/`azure_storage_blobs`, tokio, async-trait, bytes), Tauri 2.x, Svelte 5 + Vite, `@tauri-apps/plugin-clipboard-manager`, Docker (MinIO + Azurite fixtures).

**Spec:** `docs/superpowers/specs/2026-06-07-wonderblob-design.md`
**Builds on:** `docs/superpowers/plans/2026-06-07-foundation-sftp-slice.md` (Plan 1 — merged)

**Crate-API caveats (read before coding):**
- **AWS SDK:** `aws-sdk-s3` and `aws-config` move in lockstep and are released frequently. Target the latest `1.x` of each (check `cargo add aws-sdk-s3 --dry-run` for the current minor). The *structure* below — `Client`, `ListObjectsV2`, `HeadObject`, `GetObject` with a `Range` header, `CreateMultipartUpload`/`UploadPart`/`CompleteMultipartUpload`, `presigning::PresigningConfig` — is stable across recent 1.x; only builder ergonomics drift. Map SDK errors with `.into_service_error()` and match on the typed error (e.g. `NoSuchKey`, `NotFound`).
- **Azure SDK:** the Azure SDK for Rust reached **1.0 GA** and the blob client moved into the **`azure_storage_blob`** crate (singular), superseding the older community `azure_storage` + `azure_storage_blobs` (plural) generation. **Before writing any Azure code, run `cargo search azure_storage_blob` and `cargo search azure_storage_blobs` and read the top crate's docs.rs page to determine which generation is current and what the client/auth types are named.** Pin the exact version you find. The plan describes Azure operations API-agnostically (stage block / commit block list, ranged download, SAS generation) with best-effort code against the 1.0 `azure_storage_blob` shape; adapt names to whatever `cargo build` and docs.rs confirm. Do **not** invent method names — verify each against docs.rs for the pinned version.

**Trait constraint (do NOT change in this plan):** `StorageBackend::write(&self, path) -> Result<Box<dyn AsyncWrite + Send + Unpin>>` is fixed. The contract suite drives uploads with `w.write_all(..).await` then `w.shutdown().await`, so the multipart/block upload **must finalize inside `poll_shutdown`**. The concrete writer types (`S3MultipartWriter`, `AzBlockWriter`) may expose an inherent `finish()` for internal use, but the trait surface stays exactly as Plan 1 defined it.

---

### Task 1: Shared object-store path helpers

Object stores have no directories; both backends synthesize a tree from a flat key namespace. Factor the parsing once.

**Files:**
- Create: `crates/wonderblob-core/src/objstore.rs`
- Modify: `crates/wonderblob-core/src/lib.rs` (add `pub mod objstore;`)
- Test: inline `#[cfg(test)]` in `objstore.rs`

- [ ] **Step 1: Write the failing test**

In `crates/wonderblob-core/src/objstore.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_container_and_key() {
        assert_eq!(ObjPath::parse("/"), ObjPath { container: None, key: "".into() });
        assert!(ObjPath::parse("/").is_root());
        assert_eq!(
            ObjPath::parse("/wbtest"),
            ObjPath { container: Some("wbtest".into()), key: "".into() }
        );
        assert!(ObjPath::parse("/wbtest").is_container_root());
        assert!(ObjPath::parse("/wbtest/").is_container_root());
        assert_eq!(
            ObjPath::parse("/wbtest/a/b.txt"),
            ObjPath { container: Some("wbtest".into()), key: "a/b.txt".into() }
        );
    }

    #[test]
    fn basename_strips_dirs_and_trailing_slash() {
        assert_eq!(basename("/a/b.txt"), "b.txt");
        assert_eq!(basename("/a/b/"), "b");
        assert_eq!(basename("solo"), "solo");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wonderblob-core objstore`
Expected: FAIL — module/types not defined.

- [ ] **Step 3: Implement the helpers**

Above the tests in `objstore.rs`:

```rust
//! Shared helpers for object-store backends (S3, Azure Blob) that synthesize a
//! directory tree over a flat key namespace. Buckets/containers surface as the
//! root listing; "/bucket/prefix/..." addresses keys inside.

/// 8 MiB part/block size for multipart (S3) and block-list (Azure) uploads.
/// Above S3's 5 MiB minimum part size for all parts except the last.
pub const PART_SIZE: usize = 8 * 1024 * 1024;

/// A normalized object-store path split into container + key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjPath {
    /// Bucket (S3) or container (Azure). `None` only for the synthetic root "/".
    pub container: Option<String>,
    /// Key/blob name within the container; "" addresses the container root.
    pub key: String,
}

impl ObjPath {
    /// Parse "/", "/bucket", "/bucket/", "/bucket/a/b.txt".
    pub fn parse(path: &str) -> Self {
        let trimmed = path.trim_start_matches('/');
        if trimmed.is_empty() {
            return ObjPath { container: None, key: String::new() };
        }
        match trimmed.split_once('/') {
            None => ObjPath { container: Some(trimmed.to_string()), key: String::new() },
            Some((c, k)) => ObjPath {
                container: Some(c.to_string()),
                key: k.trim_start_matches('/').to_string(),
            },
        }
    }

    pub fn is_root(&self) -> bool {
        self.container.is_none()
    }

    pub fn is_container_root(&self) -> bool {
        self.container.is_some() && self.key.is_empty()
    }
}

/// Final path segment: "/a/b.txt" -> "b.txt", "/a/b/" -> "b".
pub fn basename(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}
```

Add `pub mod objstore;` to `crates/wonderblob-core/src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p wonderblob-core objstore`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(core): shared object-store path helpers (ObjPath, basename, PART_SIZE)"
```

---

### Task 2: S3 contract harness + MinIO fixture (red state)

Mirror Plan 1 Task 4: write the Docker fixture and the contract harness that calls the not-yet-existing backend, plus the bucket-listing tests. This compiles to a red state that Tasks 3–4 turn green.

**Files:**
- Create: `scripts/test-s3-up.sh`, `scripts/test-s3-down.sh`
- Create: `crates/wonderblob-core/tests/s3_contract.rs`
- (Reuses `crates/wonderblob-core/tests/contract/mod.rs` unchanged — see Step 4)

- [ ] **Step 1: Write the MinIO fixture scripts**

`scripts/test-s3-up.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
# Throwaway MinIO (S3-compatible) for contract tests.
# Endpoint http://localhost:9000, creds minioadmin/minioadmin, bucket "wbtest".
docker rm -f wonderblob-test-s3 >/dev/null 2>&1 || true
docker run -d --name wonderblob-test-s3 -p 9000:9000 \
  -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
  minio/minio:latest server /data >/dev/null
echo "waiting for minio..."
for i in $(seq 1 30); do
  if curl -sf http://localhost:9000/minio/health/live >/dev/null 2>&1; then
    echo "ready on http://localhost:9000 (minioadmin/minioadmin)"; exit 0
  fi
  sleep 1
done
echo "minio never came up" >&2; exit 1
```

The bucket `wbtest` is created **by the test itself** (Task 3, Step 6 helper) via `create_bucket` — the simplest reliable approach, no extra `mc` container. `scripts/test-s3-down.sh`:

```bash
#!/usr/bin/env bash
docker rm -f wonderblob-test-s3 >/dev/null 2>&1 || true
```

Run: `chmod +x scripts/test-s3-up.sh scripts/test-s3-down.sh`

- [ ] **Step 2: Write the S3 contract harness (failing — no backend yet)**

`crates/wonderblob-core/tests/s3_contract.rs`:

```rust
mod contract;

use wonderblob_core::s3::{S3Backend, S3Config};
use wonderblob_core::vfs::{EntryKind, StorageBackend};

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_S3").as_deref() == Ok("1")
}

fn test_config() -> S3Config {
    S3Config {
        access_key_id: "minioadmin".into(),
        secret_access_key: "minioadmin".into(),
        region: Some("us-east-1".into()),
        endpoint: Some("http://localhost:9000".into()),
        force_path_style: true, // MinIO requires path-style addressing
    }
}

#[tokio::test]
async fn s3_passes_vfs_contract() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_S3=1 and run scripts/test-s3-up.sh");
        return;
    }
    let backend = S3Backend::connect(test_config()).await.expect("connect");
    backend.ensure_test_bucket("wbtest").await.expect("create bucket");
    // Contract root is a writable dir INSIDE a pre-created bucket.
    contract::run_contract(&backend, "/wbtest").await;
}

#[tokio::test]
async fn s3_root_lists_buckets_as_dirs() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_S3=1");
        return;
    }
    let backend = S3Backend::connect(test_config()).await.expect("connect");
    backend.ensure_test_bucket("wbtest").await.expect("create bucket");
    let roots = backend.list("/").await.expect("list root");
    let bucket = roots.iter().find(|e| e.name == "wbtest").expect("wbtest bucket in root");
    assert_eq!(bucket.kind, EntryKind::Dir);
    assert_eq!(bucket.path, "/wbtest");
}
```

> `ensure_test_bucket` is a **test-only inherent method** on `S3Backend` (not part of the trait) — defined in Task 3, Step 6. It creates the bucket if absent so the suite is self-bootstrapping.

- [ ] **Step 3: Verify it fails to compile**

Run: `cargo test -p wonderblob-core --test s3_contract`
Expected: compile error — `wonderblob_core::s3` doesn't exist. Red state for Task 3.

- [ ] **Step 4: Confirm `contract/mod.rs` needs NO changes — and why**

Re-read `crates/wonderblob-core/tests/contract/mod.rs`. It is backend-agnostic and already compatible with object-store marker semantics. Verify each assumption holds for S3/Azure (no edits required):

- **Idempotent pre-clean** (`let _ = b.delete(...)`): our `delete` on a missing key/prefix returns `NotFound`, which the `let _` discards. ✓
- **`mkdir` then `list(root)` shows the dir:** `mkdir` writes a zero-byte `contract-dir/` marker; `list` reports a `CommonPrefix`/marker as `EntryKind::Dir` with `path == "/wbtest/contract-dir"`. ✓
- **stat size == 16:** `HeadObject`/blob properties `content_length` == 16. ✓
- **rename then `stat(old)` is `NotFound`:** CopyObject+DeleteObject leaves no key at the old path. ✓
- **`delete(&dir)` succeeds when the dir holds only its own marker:** the empty-check (Task 4) excludes the dir's own `contract-dir/` marker object, so deleting the now-childless dir removes the marker and returns `Ok`. ✓
- **`stat(&dir)` after delete is `NotFound`:** no marker and no child keys under `contract-dir/` → `NotFound`. ✓

If any of these fail at runtime, the bug is in the **backend**, not the contract. Do not weaken the contract. Leave `contract/mod.rs` untouched.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(core): S3 VFS contract harness + bucket-listing tests + MinIO fixture"
```

---

### Task 3: S3 backend — client, list, stat, read (writes stubbed)

**Files:**
- Create: `crates/wonderblob-core/src/s3.rs`
- Modify: `crates/wonderblob-core/src/lib.rs` (add `pub mod s3;`)
- Modify: `crates/wonderblob-core/Cargo.toml`

- [ ] **Step 1: Add dependencies**

In `crates/wonderblob-core/Cargo.toml` under `[dependencies]` (check current 1.x minors with `cargo add aws-sdk-s3 --dry-run`):

```toml
aws-config = "1"
aws-sdk-s3 = "1"
aws-smithy-types = { version = "1", features = ["rt-tokio"] }
bytes = "1"
futures = "0.3"
tokio-util = { version = "1", features = ["io"] }
```

(`rt-tokio` on `aws-smithy-types` is what makes `ByteStream::into_async_read()` available; `tokio-util`/`futures` are used by the Azure reader in Task 7 — adding them now keeps one dependency commit.)

- [ ] **Step 2: Implement config, client, error mapping**

`crates/wonderblob-core/src/s3.rs`:

```rust
use crate::error::{Result, StorageError};
use crate::objstore::{basename, ObjPath, PART_SIZE};
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
        Ok(Self { client: Client::from_conf(builder.build()) })
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
    if s.contains("notfound") || s.contains("nosuchkey") || s.contains("nosuchbucket") || s.contains("404") {
        StorageError::NotFound { path: path.into() }
    } else if s.contains("accessdenied") || s.contains("invalidaccesskey")
        || s.contains("signaturedoesnotmatch") || s.contains("403") {
        StorageError::PermissionDenied { path: path.into() }
    } else if s.contains("dispatchfailure") || s.contains("timeout") || s.contains("connect") {
        StorageError::Network { detail: e.to_string() }
    } else {
        StorageError::Other { detail: e.to_string() }
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
```

- [ ] **Step 3: Implement `list` (buckets at root, delimited listing inside)**

Append to `s3.rs`:

```rust
impl S3Backend {
    /// Prefix used to address `key` as a directory: "" => "", "a/b" => "a/b/".
    fn dir_prefix(key: &str) -> String {
        if key.is_empty() { String::new() } else { format!("{}/", key.trim_end_matches('/')) }
    }

    async fn list_buckets(&self) -> Result<Vec<Entry>> {
        let resp = self.client.list_buckets().send().await.map_err(|e| map_s3("/", e))?;
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
```

- [ ] **Step 4: Implement the `StorageBackend` trait (read paths real, write paths stubbed)**

Append to `s3.rs`:

```rust
#[async_trait]
impl StorageBackend for S3Backend {
    fn capabilities(&self) -> Capabilities {
        // can_set_mtime=false: object stores don't expose settable mtime.
        Capabilities { can_presign: true, can_rename: true, can_set_mtime: false }
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
        match self.client.head_object().bucket(&container).key(&p.key).send().await {
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
        let container = p.container.ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        let mut req = self.client.get_object().bucket(&container).key(&p.key);
        if offset > 0 {
            req = req.range(format!("bytes={offset}-"));
        }
        let resp = req.send().await.map_err(|e| map_s3(path, e))?;
        Ok(Box::new(resp.body.into_async_read()))
    }

    async fn write(&self, _path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        Err(StorageError::Unsupported { op: "s3 write (Task 4)".into() })
    }

    async fn delete(&self, _path: &str) -> Result<()> {
        Err(StorageError::Unsupported { op: "s3 delete (Task 4)".into() })
    }

    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Err(StorageError::Unsupported { op: "s3 rename (Task 4)".into() })
    }

    async fn mkdir(&self, _path: &str) -> Result<()> {
        Err(StorageError::Unsupported { op: "s3 mkdir (Task 4)".into() })
    }

    async fn share_link(&self, _path: &str, _expiry: u64) -> Result<String> {
        Err(StorageError::Unsupported { op: "s3 share_link (Task 4)".into() })
    }
}
```

Add `pub mod s3;` to `lib.rs`.

- [ ] **Step 5: Build (fixing AWS SDK drift)**

Run: `cargo build -p wonderblob-core`
If a builder/accessor name differs from the pinned 1.x (e.g. `contents()` vs `contents.unwrap_or_default()`, `into_async_read` location), consult docs.rs for the exact version and adapt — keep the trait surface and behavior identical.

- [ ] **Step 6: Smoke-test list/stat/read against MinIO**

```bash
./scripts/test-s3-up.sh
WONDERBLOB_TEST_S3=1 cargo test -p wonderblob-core --test s3_contract s3_root_lists_buckets_as_dirs -- --nocapture
```

Expected: `s3_root_lists_buckets_as_dirs ... ok` (it only needs connect + ensure_test_bucket + list("/")). `s3_passes_vfs_contract` still fails at `mkdir`/`write` (stubbed) — that's expected; Task 4 finishes it. Leave MinIO running for Task 4.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(core): S3 backend — client, bucket/prefix list, stat, ranged read"
```

---

### Task 4: S3 writes — multipart `AsyncWrite` adapter, delete, rename, mkdir, share_link

This is the spec's known design wrinkle: `write()` returns a boxed `AsyncWrite`, and the contract finalizes it with `shutdown()`. The multipart upload therefore **completes inside `poll_shutdown`**. Because the trait erases the concrete writer type, `poll_shutdown` is the *only* finalization path the caller can reach — so it must do the whole job (flush the tail part, `CompleteMultipartUpload`, abort-on-error). An inherent `finish()` is unnecessary and we do **not** add one or touch the trait.

**Files:**
- Modify: `crates/wonderblob-core/src/s3.rs` (replace the five stubs + add the writer)

- [ ] **Step 1: Add imports + the multipart writer**

At the top of `s3.rs` extend the imports:

```rust
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use bytes::BytesMut;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::AsyncReadExt; // (not strictly needed; remove if unused)
```

Append the writer to `s3.rs`:

```rust
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
    io::Error::new(io::ErrorKind::Other, e.to_string())
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
```

> **Tail-flush subtlety:** after `start_part()` flushes the buffer the loop re-enters `Uploading`, banks the part, returns to `Idle` with `buf` empty and `parts` non-empty → `start_complete()`. The `parts.is_empty() || !buf.is_empty()` guard prevents an infinite loop (a second `Idle` pass finds parts non-empty and buf empty → completes).

- [ ] **Step 2: Replace the five trait stubs**

In the `impl StorageBackend for S3Backend` block, replace the stub bodies for `write`, `delete`, `rename`, `mkdir`, `share_link`:

```rust
    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "cannot write at the bucket-list root".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported { op: "cannot write a bucket".into() });
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
            .ok_or_else(|| StorageError::Other { detail: "no upload id".into() })?
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
        let container = p.container.ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported { op: "refusing to delete a bucket".into() });
        }
        // File?
        match self.client.head_object().bucket(&container).key(&p.key).send().await {
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
        let cf = pf.container.ok_or_else(|| StorageError::NotFound { path: from.into() })?;
        let ct = pt.container.ok_or_else(|| StorageError::NotFound { path: to.into() })?;
        if pf.key.is_empty() || pt.key.is_empty() {
            return Err(StorageError::Unsupported { op: "cannot rename a bucket".into() });
        }
        // File rename = CopyObject + DeleteObject.
        match self.client.head_object().bucket(&cf).key(&pf.key).send().await {
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
            return Err(StorageError::Unsupported { op: "cannot mkdir a bucket".into() });
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
            return Err(StorageError::Unsupported { op: "cannot share a bucket".into() });
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
```

- [ ] **Step 2b: Drop the unused import**

`AsyncReadExt` was listed defensively in Step 1; if `cargo build` warns it's unused, delete that line (clippy `-D warnings` in CI will reject it otherwise).

- [ ] **Step 3: Build**

Run: `cargo build -p wonderblob-core`
Adapt any AWS builder drift (`set_e_tag`, `presigned`, `copy_source` signatures) against docs.rs for the pinned `aws-sdk-s3`.

- [ ] **Step 4: Run the S3 contract suite green**

```bash
./scripts/test-s3-up.sh   # (skip if still running from Task 3)
WONDERBLOB_TEST_S3=1 cargo test -p wonderblob-core --test s3_contract -- --nocapture
./scripts/test-s3-down.sh
```

Expected: `s3_passes_vfs_contract ... ok` and `s3_root_lists_buckets_as_dirs ... ok`. Debug against the live container until both pass.

- [ ] **Step 5: Verify presigned URL shape manually (optional but recommended)**

Add a throwaway `eprintln!` of `share_link("/wbtest/contract-dir/hello.txt", 3600)` inside the contract (or a scratch test) and confirm it's a `http://localhost:9000/wbtest/...X-Amz-Signature=...` URL that `curl` can GET. Remove the scratch code before committing.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(core): S3 writes — multipart AsyncWrite, delete/rename/mkdir, presigned share links"
```

---

### Task 5: Azure contract harness + Azurite fixture (red state)

**Files:**
- Create: `scripts/test-azblob-up.sh`, `scripts/test-azblob-down.sh`
- Create: `crates/wonderblob-core/tests/azblob_contract.rs`

- [ ] **Step 1: Write the Azurite fixture scripts**

`scripts/test-azblob-up.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
# Throwaway Azurite (Azure Storage emulator) blob service on port 10000.
# Well-known dev account: devstoreaccount1 (key is the public Azurite default).
docker rm -f wonderblob-test-azblob >/dev/null 2>&1 || true
docker run -d --name wonderblob-test-azblob -p 10000:10000 \
  mcr.microsoft.com/azure-storage/azurite:latest \
  azurite-blob --blobHost 0.0.0.0 --skipApiVersionCheck >/dev/null
echo "waiting for azurite..."
for i in $(seq 1 30); do
  if curl -s http://127.0.0.1:10000/devstoreaccount1 >/dev/null 2>&1; then
    echo "ready on http://127.0.0.1:10000 (devstoreaccount1)"; exit 0
  fi
  sleep 1
done
echo "azurite never came up" >&2; exit 1
```

`scripts/test-azblob-down.sh`:

```bash
#!/usr/bin/env bash
docker rm -f wonderblob-test-azblob >/dev/null 2>&1 || true
```

Run: `chmod +x scripts/test-azblob-up.sh scripts/test-azblob-down.sh`

- [ ] **Step 2: Write the Azure contract harness (failing — no backend yet)**

`crates/wonderblob-core/tests/azblob_contract.rs`:

```rust
mod contract;

use wonderblob_core::azblob::{AzAuth, AzBlobBackend, AzBlobConfig};
use wonderblob_core::vfs::{EntryKind, StorageBackend};

/// Public, well-known Azurite development account key (not a secret).
const DEV_KEY: &str = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_AZBLOB").as_deref() == Ok("1")
}

fn test_config() -> AzBlobConfig {
    AzBlobConfig {
        account: "devstoreaccount1".into(),
        // Azurite path-style endpoint includes the account name.
        endpoint: Some("http://127.0.0.1:10000/devstoreaccount1".into()),
        auth: AzAuth::AccountKey(DEV_KEY.into()),
    }
}

#[tokio::test]
async fn azblob_passes_vfs_contract() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_AZBLOB=1 and run scripts/test-azblob-up.sh");
        return;
    }
    let backend = AzBlobBackend::connect(test_config()).await.expect("connect");
    backend.ensure_test_container("wbtest").await.expect("create container");
    contract::run_contract(&backend, "/wbtest").await;
}

#[tokio::test]
async fn azblob_root_lists_containers_as_dirs() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_AZBLOB=1");
        return;
    }
    let backend = AzBlobBackend::connect(test_config()).await.expect("connect");
    backend.ensure_test_container("wbtest").await.expect("create container");
    let roots = backend.list("/").await.expect("list root");
    let c = roots.iter().find(|e| e.name == "wbtest").expect("wbtest container in root");
    assert_eq!(c.kind, EntryKind::Dir);
    assert_eq!(c.path, "/wbtest");
}
```

- [ ] **Step 3: Verify it fails to compile**

Run: `cargo test -p wonderblob-core --test azblob_contract`
Expected: compile error — `wonderblob_core::azblob` doesn't exist. Red state for Task 6.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "test(core): Azure Blob contract harness + container-listing tests + Azurite fixture"
```

---

### Task 6: Azure Blob backend — client, list, stat, read (writes stubbed)

The VFS-to-flat-namespace mapping is **identical to S3** (markers for dirs, `/` delimiter, container-as-root). Only the SDK calls differ. The VFS logic below is concrete; the SDK calls are **best-effort against the 1.0 `azure_storage_blob` crate** — verify each method name on docs.rs for the version you pin (see the Crate-API caveat at the top).

**Files:**
- Create: `crates/wonderblob-core/src/azblob.rs`
- Modify: `crates/wonderblob-core/src/lib.rs` (add `pub mod azblob;`)
- Modify: `crates/wonderblob-core/Cargo.toml`

- [ ] **Step 1: Determine and add the Azure dependency**

```bash
cargo search azure_storage_blob
cargo search azure_storage_blobs
```

Open the docs.rs page for whichever is current (expected: `azure_storage_blob` 1.x, with `azure_core`). Pin exact versions in `crates/wonderblob-core/Cargo.toml`, e.g.:

```toml
azure_storage_blob = "=<version-you-found>"
azure_core = "=<matching-version>"
```

`bytes`, `futures`, and `tokio-util` (io) were added in Task 3 and are reused for the blob reader.

- [ ] **Step 2: Implement config, client, error mapping**

`crates/wonderblob-core/src/azblob.rs`. Names marked `// VERIFY` must be checked against docs.rs:

```rust
use crate::error::{Result, StorageError};
use crate::objstore::{basename, ObjPath};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

/// Azure auth modes. The secret slot holds whichever single credential applies.
pub enum AzAuth {
    /// Shared-key: account name + this base64 key.
    AccountKey(String),
    /// Full connection string (contains account + key or SAS).
    ConnectionString(String),
    /// Pre-minted SAS token (read-scoped); cannot mint new share links.
    Sas(String),
}

pub struct AzBlobConfig {
    pub account: String,
    /// Custom endpoint (Azurite path-style includes the account); `None` => real Azure.
    pub endpoint: Option<String>,
    pub auth: AzAuth,
}

pub struct AzBlobBackend {
    // The service client; exact type per the pinned crate. // VERIFY
    pub(crate) service: azure_storage_blob::BlobServiceClient,
    /// Whether we hold a key capable of minting SAS links (AccountKey/ConnString).
    pub(crate) can_sign: bool,
    pub(crate) account: String,
}

impl AzBlobBackend {
    pub async fn connect(cfg: AzBlobConfig) -> Result<Self> {
        // Build the service client from the chosen credential. The constructor
        // names below are the 1.0 shape; VERIFY against docs.rs and adapt.
        let (service, can_sign) = match &cfg.auth {
            AzAuth::AccountKey(key) => {
                let cred = azure_storage_blob::credentials::StorageSharedKeyCredential::new(
                    cfg.account.clone(),
                    key.clone(),
                ); // VERIFY
                let endpoint = cfg
                    .endpoint
                    .clone()
                    .unwrap_or_else(|| format!("https://{}.blob.core.windows.net", cfg.account));
                let svc = azure_storage_blob::BlobServiceClient::new(
                    endpoint,
                    cred.into(), // VERIFY credential wrapping
                    None,
                )
                .map_err(StorageError::other)?;
                (svc, true)
            }
            AzAuth::ConnectionString(cs) => {
                let svc = azure_storage_blob::BlobServiceClient::from_connection_string(cs, None)
                    .map_err(StorageError::other)?; // VERIFY
                (svc, true)
            }
            AzAuth::Sas(token) => {
                let endpoint = cfg
                    .endpoint
                    .clone()
                    .unwrap_or_else(|| format!("https://{}.blob.core.windows.net", cfg.account));
                let svc = azure_storage_blob::BlobServiceClient::with_sas(endpoint, token, None)
                    .map_err(StorageError::other)?; // VERIFY
                (svc, false)
            }
        };
        Ok(Self { service, can_sign, account: cfg.account })
    }

    /// TEST-ONLY: create the contract container if absent.
    pub async fn ensure_test_container(&self, container: &str) -> Result<()> {
        let cc = self.service.blob_container_client(container.to_string()); // VERIFY
        match cc.create_container(None).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let s = format!("{e:?}").to_lowercase();
                if s.contains("containeralreadyexists") || s.contains("conflict") {
                    Ok(())
                } else {
                    Err(map_az(container, e))
                }
            }
        }
    }

    fn dir_prefix(key: &str) -> String {
        if key.is_empty() { String::new() } else { format!("{}/", key.trim_end_matches('/')) }
    }
}

/// Heuristic Azure-error mapping (status/code text) into the taxonomy.
fn map_az<E: std::fmt::Debug + std::fmt::Display>(path: &str, e: E) -> StorageError {
    let s = format!("{e:?}").to_lowercase();
    if s.contains("blobnotfound") || s.contains("containernotfound") || s.contains("404") {
        StorageError::NotFound { path: path.into() }
    } else if s.contains("authenticationfailed") || s.contains("authorization") || s.contains("403") {
        StorageError::PermissionDenied { path: path.into() }
    } else if s.contains("timeout") || s.contains("connect") || s.contains("dns") {
        StorageError::Network { detail: e.to_string() }
    } else {
        StorageError::Other { detail: e.to_string() }
    }
}
```

- [ ] **Step 3: Implement `list`, `stat`, `read` + write stubs**

The VFS logic mirrors S3 exactly. Append the `impl StorageBackend` block. Listing uses a *hierarchical* (delimited) blob enumeration so `BlobPrefix` entries become dirs:

```rust
#[async_trait]
impl StorageBackend for AzBlobBackend {
    fn capabilities(&self) -> Capabilities {
        // can_presign only when we hold a key that can mint a SAS.
        Capabilities { can_presign: self.can_sign, can_rename: true, can_set_mtime: false }
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let p = ObjPath::parse(path);
        let Some(container) = p.container else {
            // Root: list containers as dirs. // VERIFY pager API
            let mut out: Vec<Entry> = Vec::new();
            let mut pager = self.service.list_containers(None); // VERIFY
            while let Some(page) = pager.next().await {
                let page = page.map_err(|e| map_az("/", e))?;
                for c in page.containers() {
                    // VERIFY accessor
                    out.push(Entry {
                        name: c.name().to_string(),
                        path: format!("/{}", c.name()),
                        kind: EntryKind::Dir,
                        size: None,
                        modified_ms: None,
                    });
                }
            }
            out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            return Ok(out);
        };
        let prefix = Self::dir_prefix(&p.key);
        let cc = self.service.blob_container_client(container.clone());
        let mut out: Vec<Entry> = Vec::new();
        // Delimited listing: blobs + virtual-dir prefixes. // VERIFY options builder
        let mut pager = cc.list_blobs(
            Some(/* ListBlobsOptions { prefix, delimiter: "/" } */),
        ); // VERIFY
        while let Some(page) = pager.next().await {
            let page = page.map_err(|e| map_az(path, e))?;
            for vp in page.blob_prefixes() {
                // VERIFY: virtual-dir prefixes (e.g. "a/b/")
                let pfx = vp.name(); // VERIFY accessor
                let trimmed = pfx.trim_end_matches('/');
                out.push(Entry {
                    name: basename(trimmed),
                    path: format!("/{container}/{trimmed}"),
                    kind: EntryKind::Dir,
                    size: None,
                    modified_ms: None,
                });
            }
            for b in page.blobs() {
                let name = b.name(); // VERIFY
                if name == prefix || name.ends_with('/') {
                    continue; // skip the dir's own marker and directory markers
                }
                out.push(Entry {
                    name: basename(name),
                    path: format!("/{container}/{name}"),
                    kind: EntryKind::File,
                    size: b.properties().content_length(),  // VERIFY -> Option<u64>
                    modified_ms: b
                        .properties()
                        .last_modified()
                        .map(|t| t.unix_timestamp() * 1000), // VERIFY time type
                });
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
            return Ok(Entry { name: String::new(), path: "/".into(), kind: EntryKind::Dir, size: None, modified_ms: None });
        };
        let cc = self.service.blob_container_client(container.clone());
        if p.key.is_empty() {
            // Container root: get_properties exists => Dir, else NotFound.
            cc.get_properties(None).await.map_err(|e| map_az(path, e))?; // VERIFY
            return Ok(Entry { name: container.clone(), path: format!("/{container}"), kind: EntryKind::Dir, size: None, modified_ms: None });
        }
        let bc = cc.blob_client(p.key.clone());
        match bc.get_properties(None).await {
            Ok(props) => Ok(Entry {
                name: basename(&p.key),
                path: format!("/{container}/{}", p.key),
                kind: EntryKind::File,
                size: props.content_length(),          // VERIFY
                modified_ms: props.last_modified().map(|t| t.unix_timestamp() * 1000), // VERIFY
            }),
            Err(e) => {
                let mapped = map_az(path, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
                // Directory? marker or any child under "key/".
                let prefix = Self::dir_prefix(&p.key);
                let mut pager = cc.list_blobs(Some(/* prefix, max 1 */)); // VERIFY
                let mut any = false;
                if let Some(page) = pager.next().await {
                    let page = page.map_err(|e| map_az(path, e))?;
                    any = !page.blobs().is_empty() || !page.blob_prefixes().is_empty();
                }
                if any {
                    Ok(Entry { name: basename(&p.key), path: format!("/{container}/{}", p.key.trim_end_matches('/')), kind: EntryKind::Dir, size: None, modified_ms: None })
                } else {
                    Err(StorageError::NotFound { path: path.into() })
                }
            }
        }
    }

    async fn read(&self, path: &str, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        let bc = self.service.blob_container_client(container).blob_client(p.key);
        // Ranged download from `offset` to end. The response body is a byte
        // stream; adapt it to AsyncRead via tokio_util::io::StreamReader. // VERIFY
        let resp = bc
            .download(Some(/* range: offset.. */))
            .await
            .map_err(|e| map_az(path, e))?;
        let stream = resp
            .into_body() // VERIFY -> impl Stream<Item = Result<bytes::Bytes, _>>
            .map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string())));
        let reader = tokio_util::io::StreamReader::new(stream);
        Ok(Box::new(reader))
    }

    async fn write(&self, _path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        Err(StorageError::Unsupported { op: "azblob write (Task 7)".into() })
    }
    async fn delete(&self, _path: &str) -> Result<()> {
        Err(StorageError::Unsupported { op: "azblob delete (Task 7)".into() })
    }
    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Err(StorageError::Unsupported { op: "azblob rename (Task 7)".into() })
    }
    async fn mkdir(&self, _path: &str) -> Result<()> {
        Err(StorageError::Unsupported { op: "azblob mkdir (Task 7)".into() })
    }
    async fn share_link(&self, _path: &str, _expiry: u64) -> Result<String> {
        Err(StorageError::Unsupported { op: "azblob share_link (Task 7)".into() })
    }
}
```

Add `use futures::StreamExt;` (for `.next()`/`.map()` on pagers and the body stream) at the top, and `pub mod azblob;` to `lib.rs`.

- [ ] **Step 4: Build, resolving Azure SDK names**

Run: `cargo build -p wonderblob-core`
Every `// VERIFY` marker is a likely drift point. Fix names against docs.rs for the pinned `azure_storage_blob`/`azure_core`. Keep the VFS behavior (marker dirs, delimiter, container-as-root) exactly as written.

- [ ] **Step 5: Smoke-test container listing**

```bash
./scripts/test-azblob-up.sh
WONDERBLOB_TEST_AZBLOB=1 cargo test -p wonderblob-core --test azblob_contract azblob_root_lists_containers_as_dirs -- --nocapture
```

Expected: `azblob_root_lists_containers_as_dirs ... ok`. Leave Azurite running for Task 7.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(core): Azure Blob backend — client, container/prefix list, stat, ranged read"
```

---

### Task 7: Azure writes — block-list `AsyncWrite` adapter, delete, rename, mkdir, SAS share_link

Same `poll_shutdown`-finalizes wrinkle as S3, but using staged blocks: buffer 8 MiB, `stage_block` each, `commit_block_list` on shutdown. SDK names are best-effort (`// VERIFY`).

**Files:**
- Modify: `crates/wonderblob-core/src/azblob.rs`
- Modify: `crates/wonderblob-core/Cargo.toml` (add `base64 = "0.22"`)

- [ ] **Step 1: Add the block writer**

Extend imports at the top of `azblob.rs`:

```rust
use crate::objstore::PART_SIZE;
use base64::Engine as _;
use bytes::BytesMut;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
```

Append to `azblob.rs`:

```rust
type BoxFut<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;

enum WState {
    Idle,
    Staging(BoxFut<()>),
    Committing(BoxFut<()>),
    Done,
}

/// Fixed-width, equal-length (Azure requirement) base64 block id.
fn block_id(index: u32) -> String {
    base64::engine::general_purpose::STANDARD.encode(format!("wb-block-{index:08}"))
}

fn to_io(e: StorageError) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}

/// Buffers 8 MiB blocks, stages each, commits the block list on `poll_shutdown`.
pub struct AzBlockWriter {
    blob: azure_storage_blob::BlobClient, // VERIFY type
    buf: BytesMut,
    block_index: u32,
    block_ids: Vec<String>,
    state: WState,
}

impl AzBlockWriter {
    fn start_stage(&mut self) {
        let id = block_id(self.block_index);
        self.block_index += 1;
        self.block_ids.push(id.clone());
        let body = self.buf.split().to_vec();
        let blob = self.blob.clone();
        self.state = WState::Staging(Box::pin(async move {
            blob.stage_block(id.clone(), body) // VERIFY: (block_id, RequestContent<bytes>)
                .await
                .map_err(|e| map_az(&id, e))?;
            Ok(())
        }));
    }

    fn start_commit(&mut self) {
        let blob = self.blob.clone();
        let ids = std::mem::take(&mut self.block_ids);
        self.state = WState::Committing(Box::pin(async move {
            // Build a "latest" block list from staged ids. // VERIFY BlockList type
            let list = azure_storage_blob::models::BlockList {
                latest: ids,
                ..Default::default()
            };
            blob.commit_block_list(list, None)
                .await
                .map_err(|e| map_az("commit", e))?;
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
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
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
                    // Stage the tail if any; an all-empty write commits an empty
                    // block list, which Azure accepts as a zero-length blob.
                    if !this.buf.is_empty() {
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
```

- [ ] **Step 2: Replace the five trait stubs**

In `impl StorageBackend for AzBlobBackend`, replace `write`/`delete`/`rename`/`mkdir`/`share_link`:

```rust
    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "cannot write at the container-list root".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported { op: "cannot write a container".into() });
        }
        let blob = self.service.blob_container_client(container).blob_client(p.key);
        Ok(Box::new(AzBlockWriter {
            blob,
            buf: BytesMut::with_capacity(PART_SIZE),
            block_index: 0,
            block_ids: Vec::new(),
            state: WState::Idle,
        }))
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "creating containers is not supported".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported { op: "cannot mkdir a container".into() });
        }
        let marker = Self::dir_prefix(&p.key);
        let blob = self.service.blob_container_client(container).blob_client(marker);
        // Zero-byte marker blob (Azure flat namespace has no real dirs).
        blob.upload(Vec::<u8>::new(), true, 0, None) // VERIFY: (body, overwrite, length, opts)
            .await
            .map(|_| ())
            .map_err(|e| map_az(path, e))
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported { op: "refusing to delete a container".into() });
        }
        let cc = self.service.blob_container_client(container.clone());
        // File?
        let bc = cc.blob_client(p.key.clone());
        match bc.get_properties(None).await {
            Ok(_) => return bc.delete(None).await.map(|_| ()).map_err(|e| map_az(path, e)), // VERIFY delete sig
            Err(e) => {
                let mapped = map_az(path, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
            }
        }
        // Directory: inspect children excluding the marker.
        let prefix = Self::dir_prefix(&p.key);
        let (found_marker, has_children) = self.dir_children(&container, &prefix, path).await?;
        if !found_marker && !has_children {
            return Err(StorageError::NotFound { path: path.into() });
        }
        if has_children {
            return Err(StorageError::Conflict { path: path.into(), detail: "directory not empty".into() });
        }
        cc.blob_client(prefix).delete(None).await.map(|_| ()).map_err(|e| map_az(path, e))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let pf = ObjPath::parse(from);
        let pt = ObjPath::parse(to);
        let cf = pf.container.ok_or_else(|| StorageError::NotFound { path: from.into() })?;
        let ct = pt.container.ok_or_else(|| StorageError::NotFound { path: to.into() })?;
        if pf.key.is_empty() || pt.key.is_empty() {
            return Err(StorageError::Unsupported { op: "cannot rename a container".into() });
        }
        let src_bc = self.service.blob_container_client(cf.clone()).blob_client(pf.key.clone());
        // File rename = server-side copy + delete (no native blob rename).
        match src_bc.get_properties(None).await {
            Ok(_) => {
                let dst_bc = self.service.blob_container_client(ct).blob_client(pt.key);
                let src_url = src_bc.url().map_err(StorageError::other)?; // VERIFY -> blob URL
                dst_bc.copy_from_url(src_url, None).await.map_err(|e| map_az(from, e))?; // VERIFY (sync copy)
                src_bc.delete(None).await.map(|_| ()).map_err(|e| map_az(from, e))
            }
            Err(e) => {
                let mapped = map_az(from, e);
                if !matches!(mapped, StorageError::NotFound { .. }) {
                    return Err(mapped);
                }
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
                // Empty dir: move the marker.
                let new_marker = Self::dir_prefix(&pt.key);
                let dst_bc = self.service.blob_container_client(ct).blob_client(new_marker);
                let src_marker_bc = self.service.blob_container_client(cf.clone()).blob_client(prefix.clone());
                let src_url = src_marker_bc.url().map_err(StorageError::other)?;
                dst_bc.copy_from_url(src_url, None).await.map_err(|e| map_az(from, e))?;
                src_marker_bc.delete(None).await.map(|_| ()).map_err(|e| map_az(from, e))
            }
        }
    }

    async fn share_link(&self, path: &str, expiry_secs: u64) -> Result<String> {
        if !self.can_sign {
            return Err(StorageError::Unsupported {
                op: "SAS-token auth cannot mint new share links".into(),
            });
        }
        let p = ObjPath::parse(path);
        let container = p.container.ok_or_else(|| StorageError::Unsupported {
            op: "cannot share the container-list root".into(),
        })?;
        if p.key.is_empty() {
            return Err(StorageError::Unsupported { op: "cannot share a container".into() });
        }
        let bc = self.service.blob_container_client(container).blob_client(p.key);
        // Account-key SAS, read permission, now + expiry. // VERIFY SAS builder API
        let expiry = time::OffsetDateTime::now_utc() + time::Duration::seconds(expiry_secs as i64);
        let url = bc
            .generate_sas_url(/* permissions=read, expiry */ expiry)
            .map_err(StorageError::other)?;
        Ok(url)
    }
```

Add the shared `dir_children` helper to the `impl AzBlobBackend` block:

```rust
impl AzBlobBackend {
    /// (found_marker, has_children) for the synthesized dir at `prefix`.
    async fn dir_children(&self, container: &str, prefix: &str, path: &str) -> Result<(bool, bool)> {
        let cc = self.service.blob_container_client(container.to_string());
        let mut pager = cc.list_blobs(Some(/* prefix, max 2, no delimiter */)); // VERIFY
        let mut found_marker = false;
        let mut has_children = false;
        if let Some(page) = pager.next().await {
            let page = page.map_err(|e| map_az(path, e))?;
            for b in page.blobs() {
                if b.name() == prefix {
                    found_marker = true;
                } else {
                    has_children = true;
                }
            }
        }
        Ok((found_marker, has_children))
    }
}
```

(`time` is re-exported by `azure_core`; if not, add `time = "0.3"` to `Cargo.toml`. VERIFY.)

- [ ] **Step 3: Build, resolving Azure SDK names**

Run: `cargo build -p wonderblob-core`
Fix every `// VERIFY` against docs.rs. Keep behavior identical to the S3 backend's marker/Conflict semantics.

- [ ] **Step 4: Run the Azure contract suite green**

```bash
./scripts/test-azblob-up.sh   # (skip if still running from Task 6)
WONDERBLOB_TEST_AZBLOB=1 cargo test -p wonderblob-core --test azblob_contract -- --nocapture
./scripts/test-azblob-down.sh
```

Expected: `azblob_passes_vfs_contract ... ok` and `azblob_root_lists_containers_as_dirs ... ok`.

- [ ] **Step 5: Full core suite (no Docker)**

Run: `cargo test -p wonderblob-core`
Expected: all unit tests pass; Docker-gated tests skip.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(core): Azure Blob writes — block-list AsyncWrite, delete/rename/mkdir, SAS share links"
```

---

### Task 8: Connection plumbing — protocols, typed bookmark params, connect commands, capabilities

Extend bookmarks to carry per-protocol config (typed, not a `serde_json::Value` blob), add `connect_s3`/`connect_azblob` mirroring `connect_sftp`, make every connect command return `{ id, capabilities }`, and add a `share_link` command.

**Files:**
- Modify: `src-tauri/src/bookmarks.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Extend the bookmark model**

In `src-tauri/src/bookmarks.rs`, replace the `Protocol` enum and add param types:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    Sftp,
    S3,
    AzBlob,
}

/// S3 connection metadata. The secret (secret access key) lives in the keychain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct S3Params {
    pub access_key_id: String,
    pub region: Option<String>,
    /// Custom endpoint for MinIO/Wasabi/R2; `None` => real AWS.
    pub endpoint: Option<String>,
    #[serde(default)]
    pub force_path_style: bool,
}

/// Which single credential the keychain secret represents for Azure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum AzAuthKind {
    AccountKey,
    ConnectionString,
    Sas,
}

/// Azure Blob connection metadata. The secret (key / connection string / SAS)
/// lives in the keychain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AzBlobParams {
    pub account: String,
    /// Custom endpoint (e.g. Azurite path-style); `None` => real Azure.
    pub endpoint: Option<String>,
    pub auth_kind: AzAuthKind,
}
```

Replace the `Bookmark` struct (host/port/username/auth_method become protocol-specific and optional so cloud bookmarks omit them; existing SFTP files still deserialize because their fields are present):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub id: Uuid,
    pub label: String,
    pub protocol: Protocol,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    /// SFTP only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<AuthMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<S3Params>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azblob: Option<AzBlobParams>,
}
```

Update the inline test to wrap `auth_method` and supply the new `None` fields:

```rust
        let b = Bookmark {
            id: uuid::Uuid::new_v4(),
            label: "prod box".into(),
            protocol: Protocol::Sftp,
            host: "example.com".into(),
            port: 22,
            username: "jack".into(),
            auth_method: Some(AuthMethod::Agent),
            initial_path: Some("/var/www".into()),
            s3: None,
            azblob: None,
        };
```

- [ ] **Step 2: Run the bookmark test**

Run: `cargo test -p wonderblob bookmark_file_roundtrips`
Expected: PASS (and still no "password" substring in the file).

- [ ] **Step 3: Add `ConnectResult`, fix `connect_sftp`, add cloud connect commands**

In `src-tauri/src/commands.rs`, extend imports and add the result type + a registration helper:

```rust
use wonderblob_core::s3::{S3Backend, S3Config};
use wonderblob_core::azblob::{AzAuth, AzBlobBackend, AzBlobConfig};
use wonderblob_core::vfs::Capabilities;
use crate::bookmarks::AzAuthKind;
```

```rust
/// Returned by every connect command so the frontend can gate UI on capabilities.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectResult {
    pub id: ConnectionId,
    pub capabilities: Capabilities,
}

async fn register(
    state: &State<'_, AppState>,
    backend: std::sync::Arc<dyn StorageBackend>,
) -> ConnectResult {
    let capabilities = backend.capabilities();
    let id = state.next_id();
    state.connections.write().await.insert(id, backend);
    ConnectResult { id, capabilities }
}
```

Change `connect_sftp` to return `ConnectResult` (replace the tail after `backend` is built):

```rust
    let backend = tokio::time::timeout(CONNECT_TIMEOUT, SftpBackend::connect(SftpConfig {
        host: args.host,
        port: args.port,
        username: args.username,
        auth,
    }))
    .await
    .map_err(|_| StorageError::Network { detail: "connection timed out".into() })??;
    Ok(register(&state, Arc::new(backend)).await)
```

(Change the function's return type to `Result<ConnectResult, StorageError>`.)

Add the two new connect commands:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3ConnectArgs {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub force_path_style: bool,
}

#[tauri::command]
pub async fn connect_s3(
    state: State<'_, AppState>,
    args: S3ConnectArgs,
) -> Result<ConnectResult, StorageError> {
    let backend = tokio::time::timeout(CONNECT_TIMEOUT, S3Backend::connect(S3Config {
        access_key_id: args.access_key_id,
        secret_access_key: args.secret_access_key,
        region: args.region,
        endpoint: args.endpoint,
        force_path_style: args.force_path_style,
    }))
    .await
    .map_err(|_| StorageError::Network { detail: "connection timed out".into() })??;
    Ok(register(&state, Arc::new(backend)).await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AzBlobConnectArgs {
    pub account: String,
    pub endpoint: Option<String>,
    pub auth_kind: AzAuthKind,
    pub secret: String,
}

fn az_auth(kind: AzAuthKind, secret: String) -> AzAuth {
    match kind {
        AzAuthKind::AccountKey => AzAuth::AccountKey(secret),
        AzAuthKind::ConnectionString => AzAuth::ConnectionString(secret),
        AzAuthKind::Sas => AzAuth::Sas(secret),
    }
}

#[tauri::command]
pub async fn connect_azblob(
    state: State<'_, AppState>,
    args: AzBlobConnectArgs,
) -> Result<ConnectResult, StorageError> {
    let backend = tokio::time::timeout(CONNECT_TIMEOUT, AzBlobBackend::connect(AzBlobConfig {
        account: args.account,
        endpoint: args.endpoint,
        auth: az_auth(args.auth_kind, args.secret),
    }))
    .await
    .map_err(|_| StorageError::Network { detail: "connection timed out".into() })??;
    Ok(register(&state, Arc::new(backend)).await)
}
```

- [ ] **Step 4: Add the `share_link` command**

```rust
#[tauri::command]
pub async fn share_link(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
    expiry_secs: u64,
) -> Result<String, StorageError> {
    state.get(id).await?.share_link(&path, expiry_secs).await
}
```

- [ ] **Step 5: Make `connect_bookmark` protocol-aware**

Replace the body of `connect_bookmark` (it currently assumes SFTP). Return `ConnectResult`:

```rust
#[tauri::command]
pub async fn connect_bookmark(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: uuid::Uuid,
) -> Result<ConnectResult, StorageError> {
    use crate::bookmarks::{AuthMethod, Protocol};
    let b = store(&app)?
        .load_all()?
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| StorageError::Other { detail: "bookmark not found".into() })?;
    let key = b.id.to_string();

    let backend: Arc<dyn StorageBackend> = match b.protocol {
        Protocol::Sftp => {
            let auth = match b.auth_method.clone().ok_or_else(|| StorageError::Other {
                detail: "SFTP bookmark missing auth method".into(),
            })? {
                AuthMethod::Agent => SftpAuth::Agent,
                AuthMethod::KeyFile { path } => {
                    let k = key.clone();
                    SftpAuth::KeyFile { path, passphrase: keychain(move || secrets::get(&k)).await? }
                }
                AuthMethod::Password => {
                    let k = key.clone();
                    SftpAuth::Password(keychain(move || secrets::get(&k)).await?.ok_or(
                        StorageError::AuthFailed { detail: "no saved password".into() },
                    )?)
                }
            };
            let backend = tokio::time::timeout(CONNECT_TIMEOUT, SftpBackend::connect(SftpConfig {
                host: b.host, port: b.port, username: b.username, auth,
            }))
            .await
            .map_err(|_| StorageError::Network { detail: "connection timed out".into() })??;
            Arc::new(backend)
        }
        Protocol::S3 => {
            let p = b.s3.ok_or_else(|| StorageError::Other { detail: "S3 bookmark missing params".into() })?;
            let k = key.clone();
            let secret = keychain(move || secrets::get(&k)).await?.ok_or(
                StorageError::AuthFailed { detail: "no saved secret access key".into() },
            )?;
            let backend = tokio::time::timeout(CONNECT_TIMEOUT, S3Backend::connect(S3Config {
                access_key_id: p.access_key_id,
                secret_access_key: secret,
                region: p.region,
                endpoint: p.endpoint,
                force_path_style: p.force_path_style,
            }))
            .await
            .map_err(|_| StorageError::Network { detail: "connection timed out".into() })??;
            Arc::new(backend)
        }
        Protocol::AzBlob => {
            let p = b.azblob.ok_or_else(|| StorageError::Other { detail: "Azure bookmark missing params".into() })?;
            let k = key.clone();
            let secret = keychain(move || secrets::get(&k)).await?.ok_or(
                StorageError::AuthFailed { detail: "no saved Azure credential".into() },
            )?;
            let backend = tokio::time::timeout(CONNECT_TIMEOUT, AzBlobBackend::connect(AzBlobConfig {
                account: p.account,
                endpoint: p.endpoint,
                auth: az_auth(p.auth_kind, secret),
            }))
            .await
            .map_err(|_| StorageError::Network { detail: "connection timed out".into() })??;
            Arc::new(backend)
        }
    };
    Ok(register(&state, backend).await)
}
```

- [ ] **Step 6: Fix `bookmark_save` for the now-`Option` auth method**

In `bookmark_save`, the keychain-cleanup branch matches `AuthMethod::Agent`; update it for cloud protocols (which have `auth_method: None` but DO use a secret). Replace the `matches!` guard:

```rust
        // Agent (SFTP) uses no secret; cloud protocols always use one. Only
        // wipe a stale secret for Agent or when the SFTP method changed.
        let is_agent = matches!(bookmark.auth_method, Some(AuthMethod::Agent));
        if is_agent || method_changed {
            let k = key.clone();
            keychain(move || secrets::delete(&k)).await?;
        }
```

And update `method_changed` to compare the `Option<AuthMethod>` discriminants only when both sides are SFTP (cloud edits keep their secret):

```rust
        let method_changed = existing.as_ref().is_some_and(|e| {
            std::mem::discriminant(&e.auth_method) != std::mem::discriminant(&bookmark.auth_method)
        });
```

(`auth_method` is now `Option<AuthMethod>`; `discriminant` on the `Option` is the correct comparison — `None` vs `None` for two cloud edits compares equal, so the secret is kept.)

- [ ] **Step 7: Register the new commands**

In `src-tauri/src/lib.rs`, add to `generate_handler![]`:

```rust
            commands::connect_s3,
            commands::connect_azblob,
            commands::share_link,
```

- [ ] **Step 8: Build + test**

Run: `cargo build --workspace && cargo test -p wonderblob`
Expected: clean build; bookmark tests pass.

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "feat(app): S3/Azure protocols, typed bookmark params, connect commands return capabilities, share_link command"
```

---

### Task 9: Frontend API + session capabilities

Teach the typed API layer about protocols, capabilities, and the new commands; thread `capabilities` into the active-connection store.

**Files:**
- Modify: `src/lib/api.ts`
- Modify: `src/lib/stores/session.ts`
- Modify: `src/lib/components/BookmarkList.svelte`

- [ ] **Step 1: Extend `api.ts`**

Replace the protocol/bookmark types and the `api` object in `src/lib/api.ts`:

```ts
export type Protocol = "sftp" | "s3" | "azBlob";

export interface Capabilities {
  canPresign: boolean;
  canRename: boolean;
  canSetMtime: boolean;
}
export interface ConnectResult {
  id: number;
  capabilities: Capabilities;
}

export type AzAuthKind = "accountKey" | "connectionString" | "sas";
export interface S3Params {
  accessKeyId: string;
  region: string | null;
  endpoint: string | null;
  forcePathStyle: boolean;
}
export interface AzBlobParams {
  account: string;
  endpoint: string | null;
  authKind: AzAuthKind;
}

export type AuthMethod =
  | { type: "agent" }
  | { type: "keyFile"; path: string }
  | { type: "password" };

export interface Bookmark {
  id: string;
  label: string;
  protocol: Protocol;
  host?: string;
  port?: number;
  username?: string;
  authMethod?: AuthMethod; // SFTP only
  initialPath?: string | null;
  s3?: S3Params;
  azblob?: AzBlobParams;
}

export const api = {
  connectSftp: (args: { host: string; port: number; username: string; auth: AuthSpec }) =>
    invoke<ConnectResult>("connect_sftp", { args }),
  connectS3: (args: {
    accessKeyId: string;
    secretAccessKey: string;
    region: string | null;
    endpoint: string | null;
    forcePathStyle: boolean;
  }) => invoke<ConnectResult>("connect_s3", { args }),
  connectAzblob: (args: {
    account: string;
    endpoint: string | null;
    authKind: AzAuthKind;
    secret: string;
  }) => invoke<ConnectResult>("connect_azblob", { args }),
  disconnect: (id: number) => invoke<void>("disconnect", { id }),
  listDir: (id: number, path: string) => invoke<Entry[]>("list_dir", { id, path }),
  downloadFile: (id: number, remotePath: string, localPath: string) =>
    invoke<void>("download_file", { id, remotePath, localPath }),
  uploadFile: (id: number, localPath: string, remotePath: string) =>
    invoke<void>("upload_file", { id, localPath, remotePath }),
  deleteEntry: (id: number, path: string) => invoke<void>("delete_entry", { id, path }),
  renameEntry: (id: number, from: string, to: string) =>
    invoke<void>("rename_entry", { id, from, to }),
  makeDir: (id: number, path: string) => invoke<void>("make_dir", { id, path }),
  shareLink: (id: number, path: string, expirySecs: number) =>
    invoke<string>("share_link", { id, path, expirySecs }),
  bookmarksList: () => invoke<Bookmark[]>("bookmarks_list"),
  bookmarkSave: (bookmark: Bookmark, secret?: string) =>
    invoke<void>("bookmark_save", { bookmark, secret }),
  bookmarkDelete: (id: string) => invoke<void>("bookmark_delete", { id }),
  connectBookmark: (id: string) => invoke<ConnectResult>("connect_bookmark", { id }),
};
```

Keep the existing `Entry`, `EntryKind`, `StorageError`, `StorageErrorKind`, and `AuthSpec` declarations above this block unchanged.

- [ ] **Step 2: Thread capabilities through the session store**

`src/lib/stores/session.ts`:

```ts
import { writable } from "svelte/store";
import type { Bookmark, Capabilities } from "../api";

export const activeConnection = writable<{
  id: number;
  bookmark: Bookmark;
  capabilities: Capabilities;
} | null>(null);
export const currentPath = writable<string>("/");
```

- [ ] **Step 3: Update BookmarkList's connect to store capabilities**

In `src/lib/components/BookmarkList.svelte`, the `connect` function currently does
`const id = await api.connectBookmark(b.id); activeConnection.set({ id, bookmark: b });`.
Replace those two lines with:

```ts
      const res = await api.connectBookmark(b.id);
      activeConnection.set({ id: res.id, bookmark: b, capabilities: res.capabilities });
      currentPath.set(b.initialPath ?? "/");
```

- [ ] **Step 4: Typecheck**

Run: `npm run check`
Expected: no type errors. (Any other reader of `activeConnection` still works — it gained a field, didn't lose one.)

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(ui): protocol-aware API types, capabilities in session, share_link binding"
```

---

### Task 10: ConnectionSheet — protocol picker + per-protocol field groups

Grow the connection sheet from SFTP-only to a Protocol select that swaps field groups: SFTP (unchanged), S3 (endpoint optional, region, Access Key ID, secret), Azure Blob (Account name, endpoint optional, auth-kind select, secret).

**Files:**
- Modify: `src/lib/components/ConnectionSheet.svelte`

- [ ] **Step 1: Rewrite the script block**

Replace the `<script lang="ts">…</script>` block of `src/lib/components/ConnectionSheet.svelte` with:

```svelte
<script lang="ts">
  import { untrack } from "svelte";
  import type { AuthMethod, AzAuthKind, Bookmark, Protocol } from "../api";
  import { api } from "../api";

  let {
    bookmark = null,
    onclose,
    onsaved,
  }: {
    bookmark?: Bookmark | null;
    onclose: () => void;
    onsaved: (b: Bookmark) => void;
  } = $props();

  const initial = untrack(() => bookmark);

  let protocol = $state<Protocol>(initial?.protocol ?? "sftp");
  let label = $state(initial?.label ?? "");
  let initialPath = $state(initial?.initialPath ?? "");

  // SFTP
  let host = $state(initial?.host ?? "");
  let port = $state(initial?.port ?? 22);
  let username = $state(initial?.username ?? "");
  let authType = $state<AuthMethod["type"]>(initial?.authMethod?.type ?? "agent");
  let keyPath = $state(initial?.authMethod?.type === "keyFile" ? initial.authMethod.path : "");

  // S3
  let s3AccessKeyId = $state(initial?.s3?.accessKeyId ?? "");
  let s3Region = $state(initial?.s3?.region ?? "");
  let s3Endpoint = $state(initial?.s3?.endpoint ?? "");
  let s3ForcePathStyle = $state(initial?.s3?.forcePathStyle ?? false);

  // Azure Blob
  let azAccount = $state(initial?.azblob?.account ?? "");
  let azEndpoint = $state(initial?.azblob?.endpoint ?? "");
  let azAuthKind = $state<AzAuthKind>(initial?.azblob?.authKind ?? "accountKey");

  // Single secret slot; meaning depends on protocol/auth.
  let secret = $state("");
  let saving = $state(false);
  let error = $state<string | null>(null);
  let firstInput = $state<HTMLInputElement | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  // Editing the same protocol that already stored a secret: blank means keep.
  const editingSameProto = initial != null && initial.protocol === protocol;
  const protoUsesSecret = $derived(
    protocol === "s3" ||
      protocol === "azBlob" ||
      (protocol === "sftp" && authType !== "agent")
  );
  let secretPlaceholder = $derived(
    editingSameProto && protoUsesSecret ? "Leave blank to keep saved secret" : ""
  );

  $effect(() => {
    firstInput?.focus();
  });

  function secretRequired(): boolean {
    if (!protoUsesSecret) return false;
    if (protocol === "sftp" && authType === "keyFile") return false; // passphrase optional
    return !(editingSameProto); // required for new; optional when editing same proto
  }

  function valid(): boolean {
    if (secretRequired() && !secret) return false;
    if (protocol === "sftp") {
      if (!host.trim() || !username.trim()) return false;
      if (authType === "keyFile" && !keyPath.trim()) return false;
      return port >= 1 && port <= 65535;
    }
    if (protocol === "s3") {
      return s3AccessKeyId.trim().length > 0;
    }
    // azBlob
    return azAccount.trim().length > 0;
  }

  function buildBookmark(id: string): Bookmark {
    if (protocol === "sftp") {
      const authMethod: AuthMethod =
        authType === "agent"
          ? { type: "agent" }
          : authType === "keyFile"
            ? { type: "keyFile", path: keyPath.trim() }
            : { type: "password" };
      return {
        id,
        label: label.trim() || host.trim(),
        protocol: "sftp",
        host: host.trim(),
        port,
        username: username.trim(),
        authMethod,
        initialPath: initialPath.trim() || null,
      };
    }
    if (protocol === "s3") {
      return {
        id,
        label: label.trim() || s3Endpoint.trim() || "Amazon S3",
        protocol: "s3",
        s3: {
          accessKeyId: s3AccessKeyId.trim(),
          region: s3Region.trim() || null,
          endpoint: s3Endpoint.trim() || null,
          forcePathStyle: s3ForcePathStyle,
        },
        initialPath: initialPath.trim() || "/",
      };
    }
    return {
      id,
      label: label.trim() || azAccount.trim() || "Azure Blob",
      protocol: "azBlob",
      azblob: {
        account: azAccount.trim(),
        endpoint: azEndpoint.trim() || null,
        authKind: azAuthKind,
      },
      initialPath: initialPath.trim() || "/",
    };
  }

  async function save() {
    if (!valid() || saving) return;
    saving = true;
    error = null;
    const b = buildBookmark(bookmark?.id ?? crypto.randomUUID());
    try {
      await api.bookmarkSave(b, secret || undefined);
      secret = "";
      onsaved(b);
    } catch (e) {
      error = (e as { detail?: string })?.detail ?? "Couldn't save bookmark";
      saving = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    } else if (e.key === "Tab") {
      const focusables = Array.from(
        panelEl?.querySelectorAll<HTMLElement>("input, select, button:not(:disabled)") ?? []
      );
      if (focusables.length === 0) return;
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      const target = e.target as HTMLElement;
      if (e.shiftKey && (target === first || !panelEl?.contains(target))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (target === last || !panelEl?.contains(target))) {
        e.preventDefault();
        first.focus();
      }
    } else if (
      e.key === "Enter" &&
      !(e.target instanceof HTMLButtonElement) &&
      !(e.target instanceof HTMLSelectElement)
    ) {
      e.preventDefault();
      save();
    }
  }

  const secretLabel = $derived(
    protocol === "s3"
      ? "Secret Access Key"
      : protocol === "azBlob"
        ? azAuthKind === "accountKey"
          ? "Account Key"
          : azAuthKind === "connectionString"
            ? "Connection String"
            : "SAS Token"
        : "Password"
  );
</script>
```

- [ ] **Step 2: Replace the form fields in the template**

Keep the outer `.overlay`/`.backdrop`/`.panel`/`.title` and the `.actions` footer exactly as they are. Replace the field region (from the first `Label` field through the `Initial path` field, i.e. everything between `<div class="title">…</div>` and `{#if error}`) with:

```svelte
    <label class="field">
      <span>Protocol</span>
      <select bind:value={protocol}>
        <option value="sftp">SFTP</option>
        <option value="s3">Amazon S3 (and compatible)</option>
        <option value="azBlob">Azure Blob Storage</option>
      </select>
    </label>

    <label class="field">
      <span>Label</span>
      <input bind:this={firstInput} bind:value={label} placeholder="My connection" />
    </label>

    {#if protocol === "sftp"}
      <div class="row">
        <label class="field grow">
          <span>Host</span>
          <input bind:value={host} placeholder="example.com" spellcheck="false" />
        </label>
        <label class="field port">
          <span>Port</span>
          <input type="number" bind:value={port} min="1" max="65535" />
        </label>
      </div>
      <label class="field">
        <span>Username</span>
        <input bind:value={username} spellcheck="false" autocapitalize="off" />
      </label>
      <label class="field">
        <span>Authentication</span>
        <select bind:value={authType}>
          <option value="agent">SSH Agent</option>
          <option value="keyFile">Key file</option>
          <option value="password">Password</option>
        </select>
      </label>
      {#if authType === "keyFile"}
        <label class="field">
          <span>Key file path</span>
          <input bind:value={keyPath} placeholder="~/.ssh/id_ed25519" spellcheck="false" />
        </label>
      {/if}
    {:else if protocol === "s3"}
      <label class="field">
        <span>Endpoint (optional — leave blank for AWS)</span>
        <input bind:value={s3Endpoint} placeholder="https://s3.example.com" spellcheck="false" />
      </label>
      <label class="field">
        <span>Region</span>
        <input bind:value={s3Region} placeholder="us-east-1" spellcheck="false" />
      </label>
      <label class="field">
        <span>Access Key ID</span>
        <input bind:value={s3AccessKeyId} spellcheck="false" autocapitalize="off" />
      </label>
      <label class="checkrow">
        <input type="checkbox" bind:checked={s3ForcePathStyle} />
        <span>Force path-style addressing (MinIO, most S3-compatible servers)</span>
      </label>
    {:else}
      <label class="field">
        <span>Account name</span>
        <input bind:value={azAccount} spellcheck="false" autocapitalize="off" placeholder="mystorageacct" />
      </label>
      <label class="field">
        <span>Endpoint (optional — leave blank for Azure)</span>
        <input bind:value={azEndpoint} placeholder="http://127.0.0.1:10000/devstoreaccount1" spellcheck="false" />
      </label>
      <label class="field">
        <span>Credential type</span>
        <select bind:value={azAuthKind}>
          <option value="accountKey">Account key</option>
          <option value="connectionString">Connection string</option>
          <option value="sas">SAS token</option>
        </select>
      </label>
    {/if}

    {#if protoUsesSecret}
      <label class="field">
        <span>{secretLabel}</span>
        <input type="password" bind:value={secret} autocomplete="off" placeholder={secretPlaceholder} />
      </label>
    {/if}

    <label class="field">
      <span>Initial path (optional)</span>
      <input bind:value={initialPath} placeholder={protocol === "sftp" ? "/var/www" : "/bucket"} spellcheck="false" />
    </label>
```

- [ ] **Step 3: Add checkbox styling**

In the component `<style>`, append:

```css
  .checkrow {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-small);
    color: var(--fg-secondary);
  }
  .checkrow input {
    height: auto;
    width: auto;
  }
```

- [ ] **Step 4: Typecheck + visual smoke**

```bash
npm run check
npm run tauri dev
```

Expected: switching the Protocol select swaps field groups with no console errors; the secret field's label tracks the protocol/credential type; SFTP behaves exactly as before.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(ui): ConnectionSheet protocol picker with S3 and Azure field groups"
```

---

### Task 11: BookmarkList — protocol badge

Show a small protocol badge on each bookmark row so SFTP/S3/Azure are distinguishable at a glance.

**Files:**
- Modify: `src/lib/components/BookmarkList.svelte`

- [ ] **Step 1: Add a protocol-label helper**

In the `<script>` block (after `let bookmarks = …`), add:

```ts
  function protoBadge(p: Bookmark["protocol"]): string {
    return p === "sftp" ? "SFTP" : p === "s3" ? "S3" : "Azure";
  }

  function rowTitle(b: Bookmark): string {
    if (b.protocol === "sftp") return `${b.username ?? ""}@${b.host ?? ""}:${b.port ?? 22}`;
    if (b.protocol === "s3") return b.s3?.endpoint ?? `S3 (${b.s3?.region ?? "aws"})`;
    return b.azblob?.endpoint ?? `Azure (${b.azblob?.account ?? ""})`;
  }
```

- [ ] **Step 2: Render the badge**

In the row markup, replace the `.label` span line:

```svelte
        <span class="label" title="{b.username}@{b.host}:{b.port}">{b.label}</span>
```

with:

```svelte
        <span class="label" title={rowTitle(b)}>{b.label}</span>
        <span class="badge">{protoBadge(b.protocol)}</span>
```

- [ ] **Step 3: Style the badge**

Append to the component `<style>`:

```css
  .badge {
    flex-shrink: 0;
    padding: 0 5px;
    font-size: var(--text-small);
    color: var(--fg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    line-height: 16px;
  }
  /* Hide the badge while the row shows its hover actions, to avoid crowding. */
  .row:hover .badge,
  .row:focus-within .badge {
    display: none;
  }
```

- [ ] **Step 4: Typecheck + visual**

Run: `npm run check`
Expected: clean. In `npm run tauri dev`, each bookmark shows its protocol badge; hovering reveals the edit/delete actions in its place.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(ui): protocol badge on bookmark rows"
```

---

### Task 12: Share Link toolbar action + clipboard wiring

A "Share Link" toolbar button, enabled when `capabilities.canPresign`, mints a 24h presigned/SAS URL for the selected file and copies it to the clipboard with a transient "Link copied" confirmation strip.

**Files:**
- Modify: `package.json`, `src-tauri/Cargo.toml`, `src-tauri/src/lib.rs`, `src-tauri/capabilities/default.json`
- Modify: `src/lib/components/FileList.svelte` (expose selection)
- Modify: `src/routes/+page.svelte` (button + clipboard + strip)

- [ ] **Step 1: Install the clipboard plugin (JS + Rust)**

```bash
npm install @tauri-apps/plugin-clipboard-manager
cargo add tauri-plugin-clipboard-manager --manifest-path src-tauri/Cargo.toml
```

- [ ] **Step 2: Register the Rust plugin**

In `src-tauri/src/lib.rs`, add to the builder chain (next to the other `.plugin(...)` calls):

```rust
        .plugin(tauri_plugin_clipboard_manager::init())
```

- [ ] **Step 3: Grant the clipboard write permission**

In `src-tauri/capabilities/default.json`, add to `"permissions"`:

```json
    "clipboard-manager:allow-write-text"
```

- [ ] **Step 4: Expose the selected entry from FileList**

In `src/lib/components/FileList.svelte`, add an exported accessor next to the existing `export … refresh`/`reload`:

```ts
  export function selected(): Entry | null {
    return selectedIndex >= 0 && selectedIndex < entries.length ? entries[selectedIndex] : null;
  }
```

- [ ] **Step 5: Add the Share Link button + clipboard + confirmation strip**

In `src/routes/+page.svelte`, import the clipboard API at the top:

```ts
  import { writeText } from "@tauri-apps/plugin-clipboard-manager";
```

Add state and a handler in the `<script>`:

```ts
  let copied = $state<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;

  async function shareSelected() {
    const conn = $activeConnection;
    if (!conn) return;
    const entry = fileList?.selected() ?? null;
    if (!entry || entry.kind === "dir") {
      showToast("Select a file to share.");
      return;
    }
    try {
      const url = await api.shareLink(conn.id, entry.path, 24 * 60 * 60);
      await writeText(url);
      copied = "Link copied to clipboard";
      if (copiedTimer) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = null), 2500);
    } catch (e) {
      showToast(opError(e, "Couldn't create share link"));
    }
  }
```

In the toolbar `.actions` group, add the button before `Disconnect` (gated on the capability):

```svelte
          {#if $activeConnection?.capabilities.canPresign}
            <button class="ghost" onclick={shareSelected}>Share Link</button>
          {/if}
```

Add the confirmation strip next to the existing `{#if toast}` block:

```svelte
      {#if copied}
        <div class="copied" role="status">{copied}</div>
      {/if}
```

Add its style in the component `<style>` (neutral/success, distinct from the `--danger` toast):

```css
  .copied {
    flex-shrink: 0;
    padding: 6px 12px;
    font-size: var(--text-small);
    color: var(--fg-primary);
    border-top: 1px solid var(--border);
    background: var(--bg-selected);
  }
```

- [ ] **Step 6: Verify end-to-end against MinIO**

```bash
./scripts/test-s3-up.sh
npm run tauri dev
```

1. New Connection → Amazon S3, endpoint `http://localhost:9000`, region `us-east-1`, Access Key ID `minioadmin`, check "Force path-style", Secret Access Key `minioadmin`, Save.
2. Connect → root lists buckets as folders; create one first via `aws`/MinIO console if empty, or browse `wbtest` if Task 4 left it.
3. Select a file → "Share Link" appears (S3 `canPresign=true`) → click → "Link copied" strip → paste the URL into a browser/`curl` and confirm it downloads.
4. Connect an SFTP bookmark → "Share Link" button is **absent** (`canPresign=false`).
5. Repeat against Azurite with an Account key bookmark (`canPresign=true`); a SAS-token bookmark hides the button.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(ui): capability-gated Share Link action with clipboard copy + confirmation"
```

---

### Task 13: CI — MinIO + Azurite contract test blocks

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add the two fixture steps to the `rust` job**

In `.github/workflows/ci.yml`, after the existing "SFTP agent + key-file auth tests" step in the `rust` job, add:

```yaml
      - name: S3 contract tests (MinIO)
        run: |
          ./scripts/test-s3-up.sh
          WONDERBLOB_TEST_S3=1 cargo test -p wonderblob-core --test s3_contract
          ./scripts/test-s3-down.sh

      - name: Azure Blob contract tests (Azurite)
        run: |
          ./scripts/test-azblob-up.sh
          WONDERBLOB_TEST_AZBLOB=1 cargo test -p wonderblob-core --test azblob_contract
          ./scripts/test-azblob-down.sh
```

(GitHub-hosted `ubuntu-latest` runners have Docker preinstalled, matching the existing SFTP step. Both `cargo test` invocations build the whole crate, so a compile failure in either backend fails CI even if the gated tests are the only ones that exercise it.)

- [ ] **Step 2: Validate the workflow locally**

Run: `cargo build --workspace` and re-run both fixture suites once more end-to-end to confirm the exact commands CI will run:

```bash
./scripts/test-s3-up.sh && WONDERBLOB_TEST_S3=1 cargo test -p wonderblob-core --test s3_contract && ./scripts/test-s3-down.sh
./scripts/test-azblob-up.sh && WONDERBLOB_TEST_AZBLOB=1 cargo test -p wonderblob-core --test azblob_contract && ./scripts/test-azblob-down.sh
```

Expected: both green.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "ci: MinIO (S3) and Azurite (Azure Blob) contract test blocks"
```

---

## Done criteria (Plan 2)

- `cargo test --workspace` + `npm run check` + `npm test` green locally and in CI.
- `s3_contract` passes the full VFS contract against MinIO; `azblob_contract` passes against Azurite — using the **same** `contract/mod.rs` as SFTP (no contract weakening).
- `list("/")` surfaces buckets/containers as directories for both cloud backends (dedicated tests assert this).
- Multipart (S3) and block-list (Azure) uploads complete correctly via `poll_shutdown`, including the empty-file and single-part/single-block edge cases.
- A bookmark can be created in the ConnectionSheet for SFTP, S3, and Azure; the protocol picker swaps field groups; the secret label tracks the protocol/credential type; secrets are stored in the keychain and provably absent from `bookmarks.json`.
- Connecting returns capabilities; the "Share Link" toolbar button is present only when `canPresign` is true, mints a 24h URL, copies it to the clipboard, and shows the "Link copied" strip; the URL actually resolves.
- Bookmark rows show a protocol badge.

## Explicitly deferred

- **S3:** named AWS profile / SSO / `~/.aws/credentials` auth (v1 is explicit access-key only); URL-encoding of `copy_source` for keys with reserved characters; recursive directory copy/rename (non-empty dir rename returns `Conflict`); recursive directory delete (non-empty dir delete returns `Conflict`); requester-pays and KMS options.
- **Azure:** Microsoft Entra / `DefaultAzureCredential` and user-delegation SAS (v1 mints account-key/connection-string SAS only; SAS-token connections cannot mint links → `canPresign=false`); append/page blobs (block blobs only); recursive dir copy/rename/delete (same `Conflict` semantics as S3).
- **Both:** resumable/chunk-state-persisted transfers and progress events (Plan 3 TransferEngine — Task-7 download/upload remain blocking one-shots); server-side cross-bucket/container copy; multi-select share links; share-link expiry picker UI (hard-coded 24h for now); host/endpoint TLS pinning and custom CA bundles.
- **Cross-cutting:** OneDrive backend + native sharing links (Plan 5); EditSession/preview (Plan 4); drag & drop + packaging (Plan 6).

## Self-review (writing-plans checklist)

- **Spec coverage:** S3 backend (endpoint/force_path_style/region/explicit creds, ListObjectsV2+delimiter, HeadObject, ranged GetObject, multipart write, DeleteObject + Conflict-on-non-empty-dir, CopyObject+Delete rename, zero-byte mkdir marker, presigned share) ✓; Azure backend (account-key/connection-string/SAS auth, container-as-root, delimited listing, ranged read, staged-block write, marker mkdir, same Conflict semantics, account-key SAS share, `canPresign` reflecting key availability) ✓; contract tests gated by env against MinIO/Azurite with bucket/container-listing assertions ✓; connection plumbing (typed `Protocol` + params, `connect_s3`/`connect_azblob`, capabilities returned + stored) ✓; frontend (protocol picker, badge, capability-gated Share Link + clipboard) ✓; CI blocks ✓.
- **No placeholders / no "similar to task N":** every task has full code, exact paths, run-commands with expected output, and a commit step. Azure SDK calls carry explicit `// VERIFY` markers (the spec mandated checking the crate generation) rather than guessed-as-final names.
- **Type consistency with existing code (verified against the real files):** `StorageBackend` trait surface unchanged (`read(path, offset)`, `write(path) -> Box<dyn AsyncWrite + Send + Unpin>`, `share_link(path, expiry_secs)`); `Capabilities { can_presign, can_rename, can_set_mtime }`; `StorageError` variants used exactly (`NotFound{path}`, `Conflict{path,detail}`, `Unsupported{op}`, `Network{detail}`, `PermissionDenied{path}`, `Other{detail}`, `StorageError::other`); `Result<T>` alias; `Entry { name, path, kind, size: Option<u64>, modified_ms: Option<i64> }`; `AppState`/`ConnectionId`/`register` align with `state.rs`; `keychain(...)` helper and `secrets`/`store` reused; `contract/mod.rs` confirmed compatible without edits.

