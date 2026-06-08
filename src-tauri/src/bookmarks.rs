//! Bookmark persistence (JSON on disk) + OS-keychain secret storage.
//!
//! Bookmarks hold connection *metadata* only.  Secrets (passwords, key
//! passphrases) never touch the bookmarks file — they live in the OS keychain,
//! keyed by the bookmark's UUID.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;
use wonderblob_core::error::StorageError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    Sftp, // S3/AzBlob/OneDrive added in later plans
}

/// How to authenticate — the *method* only; secrets live in the keychain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthMethod {
    Agent,
    KeyFile { path: String }, // passphrase (if any) in keychain
    Password,                 // password in keychain
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub id: Uuid,
    pub label: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    pub initial_path: Option<String>,
}

pub struct BookmarkStore {
    dir: PathBuf,
}

impl BookmarkStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn file_path(&self) -> PathBuf {
        self.dir.join("bookmarks.json")
    }

    pub fn load_all(&self) -> Result<Vec<Bookmark>, StorageError> {
        match std::fs::read_to_string(self.file_path()) {
            Ok(s) => serde_json::from_str(&s).map_err(StorageError::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(StorageError::other(e)),
        }
    }

    pub fn save(&self, b: &Bookmark) -> Result<(), StorageError> {
        let mut all = self.load_all()?;
        all.retain(|x| x.id != b.id);
        all.push(b.clone());
        self.write_all(&all)
    }

    pub fn delete(&self, id: Uuid) -> Result<(), StorageError> {
        let mut all = self.load_all()?;
        all.retain(|x| x.id != id);
        self.write_all(&all)
    }

    fn write_all(&self, all: &[Bookmark]) -> Result<(), StorageError> {
        std::fs::create_dir_all(&self.dir).map_err(StorageError::other)?;
        // Write-then-rename so a crash mid-write can't corrupt the file.
        let tmp = self.file_path().with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(all).map_err(StorageError::other)?,
        )
        .map_err(StorageError::other)?;
        std::fs::rename(&tmp, self.file_path()).map_err(StorageError::other)
    }
}

/// Keychain wrapper. Service is constant; account is the bookmark UUID.
pub mod secrets {
    use wonderblob_core::error::StorageError;

    const SERVICE: &str = "com.wonderblob.app";

    pub fn set(bookmark_id: &str, secret: &str) -> Result<(), StorageError> {
        keyring::Entry::new(SERVICE, bookmark_id)
            .and_then(|e| e.set_password(secret))
            .map_err(StorageError::other)
    }

    pub fn get(bookmark_id: &str) -> Result<Option<String>, StorageError> {
        match keyring::Entry::new(SERVICE, bookmark_id).and_then(|e| e.get_password()) {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(StorageError::other(e)),
        }
    }

    pub fn delete(bookmark_id: &str) -> Result<(), StorageError> {
        match keyring::Entry::new(SERVICE, bookmark_id).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(StorageError::other(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_file_roundtrips_and_never_contains_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let store = BookmarkStore::new(dir.path().to_path_buf());
        let b = Bookmark {
            id: uuid::Uuid::new_v4(),
            label: "prod box".into(),
            protocol: Protocol::Sftp,
            host: "example.com".into(),
            port: 22,
            username: "jack".into(),
            auth_method: AuthMethod::Agent,
            initial_path: Some("/var/www".into()),
        };
        store.save(&b).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].label, "prod box");
        let raw = std::fs::read_to_string(store.file_path()).unwrap();
        assert!(!raw.to_lowercase().contains("password"));
    }
}
