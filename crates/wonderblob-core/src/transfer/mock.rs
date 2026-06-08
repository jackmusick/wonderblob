use crate::error::{Result, StorageError};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Clone)]
pub struct MockBackend {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// >= 0 means "next read errors after this many bytes, then disarms".
    fail_read_after: Arc<AtomicI64>,
    /// Network up/down toggle: when false, read/write open() returns Network err.
    online: Arc<AtomicBool>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(HashMap::new())),
            fail_read_after: Arc::new(AtomicI64::new(-1)),
            online: Arc::new(AtomicBool::new(true)),
        }
    }
    pub async fn put(&self, path: &str, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(path.to_string(), bytes);
    }
    pub async fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned()
    }
    /// Arm a one-shot mid-read failure after `n` bytes (auto-disarms when it fires).
    pub fn fail_read_after(&self, n: u64) {
        self.fail_read_after.store(n as i64, Ordering::SeqCst);
    }
    pub fn set_online(&self, up: bool) {
        self.online.store(up, Ordering::SeqCst);
    }
}

struct MockReader {
    data: Vec<u8>,
    pos: usize,
    /// Remaining bytes before the armed failure fires; -1 disarmed.
    fail_after: Arc<AtomicI64>,
    served: usize,
}

impl AsyncRead for MockReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let threshold = self.fail_after.load(Ordering::SeqCst);
        if threshold >= 0 && self.served as i64 >= threshold {
            self.fail_after.store(-1, Ordering::SeqCst); // disarm: retry succeeds
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "injected",
            )));
        }
        let remaining = self.data.len() - self.pos;
        if remaining == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut n = remaining.min(buf.remaining());
        if threshold >= 0 {
            n = n.min((threshold - self.served as i64).max(0) as usize);
            if n == 0 {
                self.fail_after.store(-1, Ordering::SeqCst);
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "injected",
                )));
            }
        }
        buf.put_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        self.served += n;
        Poll::Ready(Ok(()))
    }
}

struct MockWriter {
    path: String,
    buf: Vec<u8>,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl AsyncWrite for MockWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.buf.extend_from_slice(data);
        Poll::Ready(Ok(data.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.files
            .lock()
            .unwrap()
            .insert(this.path.clone(), std::mem::take(&mut this.buf));
        Poll::Ready(Ok(()))
    }
}

#[async_trait]
impl StorageBackend for MockBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn list(&self, _path: &str) -> Result<Vec<Entry>> {
        Ok(vec![])
    }
    async fn stat(&self, path: &str) -> Result<Entry> {
        let len = self.get(path).await.map(|b| b.len() as u64);
        match len {
            Some(size) => Ok(Entry {
                name: path.rsplit('/').next().unwrap_or(path).into(),
                path: path.into(),
                kind: EntryKind::File,
                size: Some(size),
                modified_ms: None,
            }),
            None => Err(StorageError::NotFound { path: path.into() }),
        }
    }
    async fn read(&self, path: &str, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        if !self.online.load(Ordering::SeqCst) {
            return Err(StorageError::Network {
                detail: "offline".into(),
            });
        }
        let all = self
            .get(path)
            .await
            .ok_or_else(|| StorageError::NotFound { path: path.into() })?;
        let start = (offset as usize).min(all.len());
        Ok(Box::new(MockReader {
            data: all[start..].to_vec(),
            pos: 0,
            fail_after: self.fail_read_after.clone(),
            served: 0,
        }))
    }
    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        if !self.online.load(Ordering::SeqCst) {
            return Err(StorageError::Network {
                detail: "offline".into(),
            });
        }
        Ok(Box::new(MockWriter {
            path: path.into(),
            buf: Vec::new(),
            files: self.files.clone(),
        }))
    }
    async fn delete(&self, path: &str) -> Result<()> {
        self.files.lock().unwrap().remove(path);
        Ok(())
    }
    async fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Ok(())
    }
    async fn mkdir(&self, _path: &str) -> Result<()> {
        Ok(())
    }
    async fn share_link(&self, _path: &str, _expiry_secs: u64) -> Result<String> {
        Err(StorageError::Unsupported {
            op: "share_link".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::StorageBackend;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn read_serves_bytes_and_honors_offset() {
        let b = MockBackend::new();
        b.put("/f.bin", vec![7u8; 100]).await;
        let mut r = b.read("/f.bin", 40).await.unwrap();
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf.len(), 60); // 100 - 40 offset
    }

    #[tokio::test]
    async fn read_fails_after_injected_byte_count() {
        let b = MockBackend::new();
        b.put("/f.bin", vec![1u8; 100]).await;
        b.fail_read_after(30); // first read attempt dies after 30 bytes
        let mut r = b.read("/f.bin", 0).await.unwrap();
        let mut buf = [0u8; 100];
        let mut total = 0;
        let err = loop {
            match r.read(&mut buf[total..]).await {
                Ok(0) => break None,
                Ok(n) => total += n,
                Err(e) => break Some(e),
            }
        };
        assert!(err.is_some());
        assert!(total <= 30);
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let b = MockBackend::new();
        let mut w = b.write("/out.bin").await.unwrap();
        w.write_all(&[9u8; 50]).await.unwrap();
        w.shutdown().await.unwrap();
        assert_eq!(b.get("/out.bin").await.unwrap().len(), 50);
    }
}
