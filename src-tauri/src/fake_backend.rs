/// Minimal in-memory `StorageBackend` used only in unit tests.
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use wonderblob_core::error::{Result, StorageError};
use wonderblob_core::vfs::{Capabilities, Entry, StorageBackend};

pub struct FakeBackend;

#[async_trait]
impl StorageBackend for FakeBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn list(&self, _path: &str) -> Result<Vec<Entry>> {
        Ok(vec![])
    }
    async fn stat(&self, path: &str) -> Result<Entry> {
        Err(StorageError::NotFound { path: path.into() })
    }
    async fn read(
        &self,
        path: &str,
        _offset: u64,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        Err(StorageError::NotFound { path: path.into() })
    }
    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        Err(StorageError::NotFound { path: path.into() })
    }
    async fn delete(&self, path: &str) -> Result<()> {
        Err(StorageError::NotFound { path: path.into() })
    }
    async fn rename(&self, from: &str, _to: &str) -> Result<()> {
        Err(StorageError::NotFound { path: from.into() })
    }
    async fn mkdir(&self, _path: &str) -> Result<()> {
        Ok(())
    }
    async fn share_link(&self, _path: &str, _expiry_secs: u64) -> Result<String> {
        Err(StorageError::Unsupported { op: "share_link".into() })
    }
}
