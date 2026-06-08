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
}
