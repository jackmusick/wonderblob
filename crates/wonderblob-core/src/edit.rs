//! EditSession core (spec: "EditSession — open / edit / save-back"). Protocol-
//! agnostic, Tauri-free: the app layer (`src-tauri/src/edit.rs`) owns the
//! watcher, the opener, the session registry, and the temp-dir layout, and calls
//! these functions for the actual download / conflict-check / upload.
//!
//! Conflict baseline = `(size, modified_ms)` from `stat`. There is **no etag** in
//! `StorageBackend`; size mismatch always counts as a conflict, mtime only when
//! both sides report it (mtime-less backends degrade to size-only — best effort).

use crate::error::{Result, StorageError};
use crate::vfs::{Entry, StorageBackend};
use std::path::Path;
use std::time::SystemTime;
use tokio::io::AsyncWriteExt;

/// Snapshot used to detect out-of-band remote changes between open and save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStat {
    pub size: Option<u64>,
    pub modified_ms: Option<i64>,
}

impl RemoteStat {
    pub fn from_entry(e: &Entry) -> Self {
        Self {
            size: e.size,
            modified_ms: e.modified_ms,
        }
    }

    /// True when the remote looks changed vs `baseline`. Size mismatch always
    /// counts; mtime only when BOTH report it.
    pub fn differs_from(&self, baseline: &RemoteStat) -> bool {
        if self.size != baseline.size {
            return true;
        }
        match (self.modified_ms, baseline.modified_ms) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        }
    }
}

/// Download remote→`temp_path`, returning the baseline captured *after* the read.
pub async fn download_to_temp(
    backend: &dyn StorageBackend,
    remote_path: &str,
    temp_path: &Path,
) -> Result<RemoteStat> {
    if let Some(parent) = temp_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(StorageError::other)?;
    }
    let mut reader = backend.read(remote_path, 0).await?;
    let mut file = tokio::fs::File::create(temp_path)
        .await
        .map_err(StorageError::other)?;
    tokio::io::copy(&mut reader, &mut file)
        .await
        .map_err(StorageError::other)?;
    file.flush().await.map_err(StorageError::other)?;
    let entry = backend.stat(remote_path).await?;
    Ok(RemoteStat::from_entry(&entry))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "result")]
pub enum ConflictCheck {
    Clear,
    Conflict { current: RemoteStat },
}

/// Re-stat the remote and compare to `baseline`. A vanished remote is a conflict.
pub async fn check_conflict(
    backend: &dyn StorageBackend,
    remote_path: &str,
    baseline: &RemoteStat,
) -> Result<ConflictCheck> {
    let current = match backend.stat(remote_path).await {
        Ok(e) => RemoteStat::from_entry(&e),
        Err(StorageError::NotFound { .. }) => {
            return Ok(ConflictCheck::Conflict {
                current: RemoteStat {
                    size: None,
                    modified_ms: None,
                },
            });
        }
        Err(e) => return Err(e),
    };
    Ok(if current.differs_from(baseline) {
        ConflictCheck::Conflict { current }
    } else {
        ConflictCheck::Clear
    })
}

/// Upload `temp_path`→remote (create/replace), returning the fresh baseline.
pub async fn save_back(
    backend: &dyn StorageBackend,
    temp_path: &Path,
    remote_path: &str,
) -> Result<RemoteStat> {
    let mut file = tokio::fs::File::open(temp_path)
        .await
        .map_err(StorageError::other)?;
    let mut writer = backend.write(remote_path).await?;
    tokio::io::copy(&mut file, &mut writer)
        .await
        .map_err(StorageError::other)?;
    writer.shutdown().await.map_err(StorageError::other)?;
    let entry = backend.stat(remote_path).await?;
    Ok(RemoteStat::from_entry(&entry))
}

/// The temp file's last-modified time, if it exists. The app layer captures this
/// right after a download or a successful save to mark the "last synced" point;
/// a later mtime means the user has unsaved local edits (see `temp_is_pending`).
pub fn temp_mtime(temp_path: &Path) -> Option<SystemTime> {
    std::fs::metadata(temp_path).and_then(|m| m.modified()).ok()
}

/// Are there local edits in `temp_path` not yet pushed to the remote? True when
/// the temp file's mtime is newer than `last_synced` (the mtime captured at the
/// last download/save). A missing temp file means nothing to flush (`false`); a
/// `None` baseline means "never synced" so anything present counts as pending.
///
/// This is what makes a re-baseline (Discard) a no-op: after re-downloading the
/// remote into the temp file the app records the fresh mtime as `last_synced`,
/// so the watcher event that the re-download itself triggers sees no pending
/// change and does **not** re-upload identical bytes.
pub fn temp_is_pending(temp_path: &Path, last_synced: Option<SystemTime>) -> bool {
    match (temp_mtime(temp_path), last_synced) {
        (Some(mtime), Some(synced)) => mtime > synced,
        (Some(_), None) => true, // present but never synced → assume pending
        (None, _) => false,      // temp gone → nothing to flush
    }
}

/// Outcome of a conflict-aware flush of pending local edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlushResult {
    /// No local edits since the last sync — nothing was written to the remote.
    NothingPending,
    /// Local edits were uploaded; `stat` is the fresh baseline and `synced_at`
    /// is the temp mtime of the bytes that were uploaded (record as last-synced).
    Saved {
        stat: RemoteStat,
        synced_at: Option<SystemTime>,
    },
    /// The remote changed out-of-band; nothing was written (caller must resolve).
    Conflict { current: RemoteStat },
}

/// Conflict-aware "save the temp file back if (and only if) it has pending edits".
/// Pending is decided by `temp_is_pending`; when pending and the remote is
/// unchanged vs `baseline`, the temp file is uploaded; when the remote changed,
/// **nothing is written** and a `Conflict` is returned so the caller never
/// silently overwrites — and never deletes a temp holding unflushed work.
pub async fn flush_if_pending(
    backend: &dyn StorageBackend,
    remote_path: &str,
    temp_path: &Path,
    last_synced: Option<SystemTime>,
    baseline: &RemoteStat,
) -> Result<FlushResult> {
    if !temp_is_pending(temp_path, last_synced) {
        return Ok(FlushResult::NothingPending);
    }
    match check_conflict(backend, remote_path, baseline).await? {
        ConflictCheck::Conflict { current } => Ok(FlushResult::Conflict { current }),
        ConflictCheck::Clear => {
            // Capture the mtime of the bytes we're about to upload BEFORE the
            // write, so a concurrent newer edit stays "pending" rather than being
            // masked by a last_synced that ran ahead of the bytes we actually sent.
            let synced_at = temp_mtime(temp_path);
            let fresh = save_back(backend, temp_path, remote_path).await?;
            Ok(FlushResult::Saved {
                stat: fresh,
                synced_at,
            })
        }
    }
}

/// Default ceiling for in-app preview; over this we offer "open in editor".
pub const PREVIEW_CAP_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PreviewPlan {
    Text,
    Image,
    Pdf,
    TooLarge { size: u64, cap: u64 },
    Unsupported { ext: String },
}

fn ext_of(name: &str) -> Option<String> {
    name.rfind('.')
        .filter(|&i| i > 0 && i + 1 < name.len())
        .map(|i| name[i + 1..].to_ascii_lowercase())
}

/// Decide how `name` (with optional known `size`) should preview, capped at `cap`.
pub fn preview_plan(name: &str, size: Option<u64>, cap: u64) -> PreviewPlan {
    if let Some(sz) = size {
        if sz > cap {
            return PreviewPlan::TooLarge { size: sz, cap };
        }
    }
    let ext = match ext_of(name) {
        None => return PreviewPlan::Text, // extensionless (Makefile, README) → text
        Some(e) => e,
    };
    const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"];
    const TEXT: &[&str] = &[
        "txt",
        "md",
        "markdown",
        "log",
        "json",
        "yaml",
        "yml",
        "toml",
        "ini",
        "cfg",
        "conf",
        "xml",
        "csv",
        "tsv",
        "sh",
        "bash",
        "zsh",
        "fish",
        "py",
        "rs",
        "go",
        "c",
        "h",
        "cpp",
        "hpp",
        "cc",
        "js",
        "ts",
        "tsx",
        "jsx",
        "svelte",
        "html",
        "css",
        "scss",
        "sql",
        "env",
        "gitignore",
        "dockerfile",
    ];
    if IMAGE.contains(&ext.as_str()) {
        PreviewPlan::Image
    } else if ext == "pdf" {
        PreviewPlan::Pdf
    } else if TEXT.contains(&ext.as_str()) {
        PreviewPlan::Text
    } else {
        PreviewPlan::Unsupported { ext }
    }
}

/// MIME for an image extension (drives the preview `data:` URL).
pub fn image_mime(name: &str) -> &'static str {
    match ext_of(name).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::mock::MockBackend;

    #[test]
    fn remote_stat_size_mismatch_is_a_conflict() {
        let base = RemoteStat {
            size: Some(100),
            modified_ms: Some(10),
        };
        let now = RemoteStat {
            size: Some(120),
            modified_ms: Some(10),
        };
        assert!(now.differs_from(&base));
    }

    #[test]
    fn remote_stat_mtime_only_compared_when_both_present() {
        let base = RemoteStat {
            size: Some(100),
            modified_ms: Some(10),
        };
        // both present, mtime moved → conflict
        assert!(RemoteStat {
            size: Some(100),
            modified_ms: Some(20)
        }
        .differs_from(&base));
        // current lacks mtime → fall back to size-only → no conflict
        assert!(!RemoteStat {
            size: Some(100),
            modified_ms: None
        }
        .differs_from(&base));
    }

    #[tokio::test]
    async fn download_records_baseline_and_writes_temp() {
        let b = MockBackend::new();
        b.put("/r.txt", b"hello world".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        let base = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        assert_eq!(std::fs::read(&temp).unwrap(), b"hello world");
        assert_eq!(base.size, Some(11));
    }

    #[tokio::test]
    async fn save_back_overwrites_remote_and_returns_fresh_baseline() {
        let b = MockBackend::new();
        b.put("/r.txt", b"old".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        std::fs::write(&temp, b"new contents").unwrap();
        let base = save_back(&b, &temp, "/r.txt").await.unwrap();
        assert_eq!(b.get("/r.txt").await.unwrap(), b"new contents");
        assert_eq!(base.size, Some(12));
    }

    #[tokio::test]
    async fn check_conflict_detects_out_of_band_change() {
        let b = MockBackend::new();
        b.put("/r.txt", b"hello".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        let base = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        // someone else changes the remote out-of-band (different size)
        b.put("/r.txt", b"hello, world!".to_vec()).await;
        match check_conflict(&b, "/r.txt", &base).await.unwrap() {
            ConflictCheck::Conflict { .. } => {}
            ConflictCheck::Clear => panic!("expected a conflict"),
        }
    }

    #[tokio::test]
    async fn check_conflict_clear_when_unchanged() {
        let b = MockBackend::new();
        b.put("/r.txt", b"hello".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        let base = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        assert_eq!(
            check_conflict(&b, "/r.txt", &base).await.unwrap(),
            ConflictCheck::Clear
        );
    }

    #[test]
    fn preview_plan_classifies_by_extension() {
        assert_eq!(
            preview_plan("notes.txt", Some(10), PREVIEW_CAP_BYTES),
            PreviewPlan::Text
        );
        assert_eq!(
            preview_plan("Makefile", Some(10), PREVIEW_CAP_BYTES),
            PreviewPlan::Text
        ); // no ext → text
        assert_eq!(
            preview_plan("logo.PNG", Some(10), PREVIEW_CAP_BYTES),
            PreviewPlan::Image
        ); // case-insensitive
        assert_eq!(
            preview_plan("report.pdf", Some(10), PREVIEW_CAP_BYTES),
            PreviewPlan::Pdf
        );
        assert_eq!(
            preview_plan("archive.zip", Some(10), PREVIEW_CAP_BYTES),
            PreviewPlan::Unsupported { ext: "zip".into() }
        );
    }

    #[test]
    fn preview_plan_size_guard_wins_over_type() {
        let cap = 1000;
        assert_eq!(
            preview_plan("big.txt", Some(5000), cap),
            PreviewPlan::TooLarge { size: 5000, cap }
        );
        // unknown size → allowed (the command still caps the actual read)
        assert_eq!(preview_plan("x.txt", None, cap), PreviewPlan::Text);
    }

    // Force a strictly-newer mtime than `marker` regardless of fs timestamp
    // granularity, so "pending" is deterministic on coarse-resolution systems.
    fn write_newer(path: &Path, bytes: &[u8], marker: Option<SystemTime>) {
        loop {
            std::fs::write(path, bytes).unwrap();
            let now = temp_mtime(path);
            match (now, marker) {
                (Some(n), Some(m)) if n > m => break,
                (Some(_), None) => break,
                _ => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
    }

    #[tokio::test]
    async fn flush_uploads_when_temp_has_pending_edit() {
        let b = MockBackend::new();
        b.put("/r.txt", b"original".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        let baseline = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        let synced = temp_mtime(&temp);

        // user edits locally → temp now newer than the synced marker
        write_newer(&temp, b"edited locally!", synced);

        let res = flush_if_pending(&b, "/r.txt", &temp, synced, &baseline)
            .await
            .unwrap();
        match res {
            FlushResult::Saved { .. } => {}
            other => panic!("expected Saved, got {other:?}"),
        }
        assert_eq!(b.get("/r.txt").await.unwrap(), b"edited locally!");
    }

    #[tokio::test]
    async fn flush_conflict_does_not_overwrite_and_keeps_local_bytes() {
        let b = MockBackend::new();
        b.put("/r.txt", b"v1".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        let baseline = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        let synced = temp_mtime(&temp);

        // someone else changes the remote out-of-band (different size)…
        b.put("/r.txt", b"v2 changed elsewhere".to_vec()).await;
        // …and the user has unsaved local edits.
        write_newer(&temp, b"my local edit", synced);

        let res = flush_if_pending(&b, "/r.txt", &temp, synced, &baseline)
            .await
            .unwrap();
        assert!(
            matches!(res, FlushResult::Conflict { .. }),
            "expected Conflict, got {res:?}"
        );
        // remote untouched (no silent overwrite) and the local edit is preserved.
        assert_eq!(b.get("/r.txt").await.unwrap(), b"v2 changed elsewhere");
        assert_eq!(std::fs::read(&temp).unwrap(), b"my local edit");
    }

    #[tokio::test]
    async fn discard_rebaseline_then_flush_is_a_noop_with_zero_remote_writes() {
        // Reproduces I2: after a Discard re-download, the watcher-triggered flush
        // must NOT re-upload identical bytes. NothingPending ⇒ save_back never ran.
        let b = MockBackend::new();
        b.put("/r.txt", b"remote v1".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("r.txt");
        download_to_temp(&b, "/r.txt", &temp).await.unwrap();

        // user edits locally…
        write_newer(&temp, b"local junk", temp_mtime(&temp));
        // …then Discards: re-download the remote and re-record last_synced.
        let rebaselined = download_to_temp(&b, "/r.txt", &temp).await.unwrap();
        let synced_after_discard = temp_mtime(&temp);

        let res = flush_if_pending(&b, "/r.txt", &temp, synced_after_discard, &rebaselined)
            .await
            .unwrap();
        assert_eq!(res, FlushResult::NothingPending);
        assert_eq!(b.get("/r.txt").await.unwrap(), b"remote v1"); // unchanged
    }
}
