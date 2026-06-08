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
    Sftp,
    S3,
    AzBlob, // OneDrive etc. added in later plans
}

/// How to authenticate — the *method* only; secrets live in the keychain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthMethod {
    Agent,
    KeyFile { path: String }, // passphrase (if any) in keychain
    Password,                 // password in keychain
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
            auth_method: Some(AuthMethod::Agent),
            initial_path: Some("/var/www".into()),
            s3: None,
            azblob: None,
        };
        store.save(&b).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].label, "prod box");
        let raw = std::fs::read_to_string(store.file_path()).unwrap();
        assert!(!raw.to_lowercase().contains("password"));
    }

    #[test]
    fn old_sftp_bookmark_json_still_deserializes() {
        // A bookmarks.json written by Plan 1 (no s3/azblob/params; auth_method
        // present, host/port/username present) must still load.
        let json = r#"[{
            "id": "11111111-1111-1111-1111-111111111111",
            "label": "legacy",
            "protocol": "sftp",
            "host": "old.example.com",
            "port": 2222,
            "username": "jack",
            "authMethod": { "type": "agent" },
            "initialPath": "/srv"
        }]"#;
        let parsed: Vec<Bookmark> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        let b = &parsed[0];
        assert_eq!(b.protocol, Protocol::Sftp);
        assert_eq!(b.host, "old.example.com");
        assert_eq!(b.port, 2222);
        assert_eq!(b.auth_method, Some(AuthMethod::Agent));
        assert!(b.s3.is_none());
        assert!(b.azblob.is_none());
    }

    #[test]
    fn s3_bookmark_roundtrips_without_secret_in_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = BookmarkStore::new(dir.path().to_path_buf());
        let b = Bookmark {
            id: uuid::Uuid::new_v4(),
            label: "minio".into(),
            protocol: Protocol::S3,
            host: String::new(),
            port: 0,
            username: String::new(),
            auth_method: None,
            initial_path: Some("/wbtest".into()),
            s3: Some(S3Params {
                access_key_id: "AKIAEXAMPLE".into(),
                region: Some("us-east-1".into()),
                endpoint: Some("http://localhost:9000".into()),
                force_path_style: true,
            }),
            azblob: None,
        };
        store.save(&b).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].protocol, Protocol::S3);
        let params = loaded[0].s3.as_ref().expect("s3 params");
        assert_eq!(params.access_key_id, "AKIAEXAMPLE");
        assert!(params.force_path_style);
        // No auth_method emitted for cloud bookmarks.
        let raw = std::fs::read_to_string(store.file_path()).unwrap();
        assert!(!raw.contains("authMethod"));
    }
}
