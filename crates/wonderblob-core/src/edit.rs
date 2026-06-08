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
        "txt", "md", "markdown", "log", "json", "yaml", "yml", "toml", "ini", "cfg", "conf", "xml",
        "csv", "tsv", "sh", "bash", "zsh", "fish", "py", "rs", "go", "c", "h", "cpp", "hpp", "cc",
        "js", "ts", "tsx", "jsx", "svelte", "html", "css", "scss", "sql", "env", "gitignore",
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
}
