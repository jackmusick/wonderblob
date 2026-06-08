use crate::error::Result;
use async_trait::async_trait;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub modified_ms: Option<i64>,
}

/// What this backend can do; UI greys out the rest (spec: capability flags).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub can_presign: bool,
    pub can_rename: bool,
    pub can_set_mtime: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            can_presign: false,
            can_rename: true,
            can_set_mtime: false,
        }
    }
}

/// One implementation per protocol. Object-safe; held as `Arc<dyn StorageBackend>`.
#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    async fn list(&self, path: &str) -> Result<Vec<Entry>>;
    async fn stat(&self, path: &str) -> Result<Entry>;
    /// Reader over file contents starting at `offset` (ranged reads for preview/resume).
    async fn read(&self, path: &str, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>>;
    /// Writer that creates/replaces the file at `path`.
    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>>;
    async fn delete(&self, path: &str) -> Result<()>;
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    async fn mkdir(&self, path: &str) -> Result<()>;
    /// Time-limited share link; backends without support return Unsupported.
    async fn share_link(&self, path: &str, expiry_secs: u64) -> Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_serializes_camel_case() {
        let e = Entry {
            name: "report.pdf".into(),
            path: "/docs/report.pdf".into(),
            kind: EntryKind::File,
            size: Some(1024),
            modified_ms: Some(1_700_000_000_000),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "file");
        assert_eq!(v["modifiedMs"], 1_700_000_000_000i64);
    }

    #[test]
    fn default_capabilities_are_conservative() {
        let c = Capabilities::default();
        assert!(!c.can_presign);
        assert!(c.can_rename); // most backends rename; opt out, not in
    }
}
