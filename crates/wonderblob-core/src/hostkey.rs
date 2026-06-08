//! App-managed SSH host-key store (spec § Auth: host-key verification is a
//! pre-release requirement). Wraps `russh-keys`' OpenSSH `known_hosts` reader so
//! the on-disk format is power-user-compatible and key-changed (MITM) detection
//! is reused, not reimplemented. We never touch ~/.ssh/known_hosts in v1.

use crate::error::{Result, StorageError};
use russh::keys::key::PublicKey;
use std::path::PathBuf;

/// Where a host's key sits relative to what we've already trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// Matches a recorded key — proceed silently.
    Known,
    /// No record for this host — surface for TOFU approval.
    Unknown,
    /// A record exists but the key DIFFERS — hard stop (possible MITM).
    Changed,
}

/// App-managed `known_hosts` file. Path-agnostic: `src-tauri` supplies
/// `<app-config>/known_hosts`; tests supply a temp path.
#[derive(Clone)]
pub struct HostKeyStore {
    path: PathBuf,
}

impl HostKeyStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Classify a presented server key against the store. Reuses
    /// `check_known_hosts_path`, which returns `Err(KeyChanged)` on mismatch.
    pub fn classify(&self, host: &str, port: u16, key: &PublicKey) -> Result<HostKeyStatus> {
        if !self.path.exists() {
            return Ok(HostKeyStatus::Unknown);
        }
        match russh::keys::check_known_hosts_path(host, port, key, &self.path) {
            Ok(true) => Ok(HostKeyStatus::Known),
            Ok(false) => Ok(HostKeyStatus::Unknown),
            Err(russh::keys::Error::KeyChanged { .. }) => Ok(HostKeyStatus::Changed),
            Err(e) => Err(StorageError::other(e)),
        }
    }

    /// Append this host+key to the store in OpenSSH known_hosts format.
    ///
    /// Defense in depth: refuse to persist when the host already has a
    /// *different* pinned key (`Changed`). A changed key must never silently
    /// (or even via a buggy caller) join the store — the UI already suppresses
    /// the "remember" option for changed keys, but the security invariant is
    /// enforced here too so it doesn't depend on the frontend.
    pub fn remember(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        if matches!(self.classify(host, port, key)?, HostKeyStatus::Changed) {
            return Err(StorageError::Conflict {
                path: format!("{host}:{port}"),
                detail: "refusing to remember a host key that differs from the pinned one".into(),
            });
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(StorageError::other)?;
        }
        russh::keys::known_hosts::learn_known_hosts_path(host, port, key, &self.path)
            .map_err(StorageError::other)
    }
}

/// SHA256 base64 fingerprint with the OpenSSH "SHA256:" display prefix.
/// `PublicKey::fingerprint()` returns BASE64_NOPAD(SHA256(pubkey)) without prefix.
pub fn fingerprint(key: &PublicKey) -> String {
    format!("SHA256:{}", key.fingerprint())
}

/// Base64 of the raw public key bytes — the opaque token the frontend round-trips
/// through the two-phase connect so the retry trusts the *exact* key it approved.
pub fn key_to_base64(key: &PublicKey) -> String {
    use russh::keys::PublicKeyBase64;
    key.public_key_base64()
}

/// Parse a public key from the base64 blob produced by [`key_to_base64`].
pub fn key_from_base64(b64: &str) -> Result<PublicKey> {
    russh::keys::parse_public_key_base64(b64).map_err(StorageError::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two real ed25519 public keys (ssh-keygen -t ed25519), the AAAA… blob field.
    const KEY1: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIBPx8NV4qj7ZZcwDud8b+Hfrdesuz00ufUopAawtUCJH";
    const KEY2: &str = "AAAAC3NzaC1lZDI1NTE5AAAAINyH6NkSkjcrEIX8B/Ki0CcnoB3xe1m7pv8XrjCd0ild";

    fn sample_key() -> PublicKey {
        key_from_base64(KEY1).expect("parse test pubkey 1")
    }

    fn other_key_same_type() -> PublicKey {
        key_from_base64(KEY2).expect("parse test pubkey 2")
    }

    #[test]
    fn unknown_then_remembered_then_known() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostKeyStore::new(dir.path().join("known_hosts"));
        let k = sample_key();
        assert!(matches!(
            store.classify("h.example.com", 22, &k).unwrap(),
            HostKeyStatus::Unknown
        ));
        store.remember("h.example.com", 22, &k).unwrap();
        assert!(matches!(
            store.classify("h.example.com", 22, &k).unwrap(),
            HostKeyStatus::Known
        ));
    }

    #[test]
    fn non_standard_port_is_scoped() {
        // A key remembered on port 2222 is Unknown (not Known) on port 22 —
        // known_hosts host lines are port-scoped via the [host]:port syntax.
        let dir = tempfile::tempdir().unwrap();
        let store = HostKeyStore::new(dir.path().join("known_hosts"));
        let k = sample_key();
        store.remember("h.example.com", 2222, &k).unwrap();
        assert!(matches!(
            store.classify("h.example.com", 2222, &k).unwrap(),
            HostKeyStatus::Known
        ));
        assert!(matches!(
            store.classify("h.example.com", 22, &k).unwrap(),
            HostKeyStatus::Unknown
        ));
    }

    #[test]
    fn changed_key_is_flagged_not_silently_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostKeyStore::new(dir.path().join("known_hosts"));
        let k1 = sample_key();
        store.remember("h.example.com", 22, &k1).unwrap();
        // A *different* key of the same type for the same host → Changed.
        let k2 = other_key_same_type();
        assert!(matches!(
            store.classify("h.example.com", 22, &k2).unwrap(),
            HostKeyStatus::Changed
        ));
    }

    #[test]
    fn remember_refuses_to_persist_a_changed_key() {
        // Defense in depth: even if a caller bypasses the dialog's guard and
        // asks to remember a key that conflicts with the pinned one, the store
        // refuses — so the MITM key can never join known_hosts.
        let dir = tempfile::tempdir().unwrap();
        let store = HostKeyStore::new(dir.path().join("known_hosts"));
        let k1 = sample_key();
        store.remember("h.example.com", 22, &k1).unwrap();
        let k2 = other_key_same_type();
        assert!(matches!(
            store.remember("h.example.com", 22, &k2),
            Err(StorageError::Conflict { .. })
        ));
        // The pinned key is untouched; the conflicting one never persisted.
        assert!(matches!(
            store.classify("h.example.com", 22, &k1).unwrap(),
            HostKeyStatus::Known
        ));
    }

    #[test]
    fn fingerprint_is_sha256_base64() {
        let k = sample_key();
        let fp = fingerprint(&k);
        // russh-keys formats SHA256 fingerprints as BASE64_NOPAD (no "SHA256:" prefix);
        // we add the prefix for display parity with OpenSSH.
        assert!(fp.starts_with("SHA256:"));
        // BASE64_NOPAD of a 32-byte SHA256 → 43 chars, no padding '='.
        let body = fp.strip_prefix("SHA256:").unwrap();
        assert_eq!(body.len(), 43);
        assert!(!body.contains('='));
    }

    #[test]
    fn base64_round_trips_the_exact_key() {
        let k = sample_key();
        let b64 = key_to_base64(&k);
        let k2 = key_from_base64(&b64).unwrap();
        assert_eq!(k, k2);
        // A different key must NOT compare equal after the round-trip.
        assert_ne!(k2, other_key_same_type());
    }
}
