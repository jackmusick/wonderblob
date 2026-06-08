//! Shared SFTP test helpers for the gated suites.
//!
//! The Dockerized OpenSSH fixture regenerates its host key on every container
//! start, so no committed `known_hosts` can pin it. The gated suites therefore
//! connect with an **accept-once** decision: a phase-1 `Verify` against a fresh
//! temp store captures the presented key, then a phase-2 `Trust { remember:
//! false }` trusts exactly that key for this session WITHOUT persisting it.
//!
//! This is an explicit accept-once decision, not a verification bypass: the
//! production code path is unchanged; tests just supply the same decision a user
//! would by clicking "Connect Once".

#![allow(dead_code)] // not every suite uses every helper

use wonderblob_core::hostkey::HostKeyStore;
use wonderblob_core::sftp::{
    HostKeyDecision, SftpAuth, SftpBackend, SftpConfig, SftpConnectOutcome,
};

/// The fixture's address (see scripts/test-sftp-up.sh).
pub const HOST: &str = "localhost";
pub const PORT: u16 = 2222;
pub const USER: &str = "wb";
pub const PASS: &str = "wbpass";

/// Build an `SftpConfig` for the fixture with the given auth and host-key decision.
pub fn config(auth: SftpAuth, host_key: HostKeyDecision) -> SftpConfig {
    SftpConfig {
        host: HOST.into(),
        port: PORT,
        username: USER.into(),
        auth,
        host_key,
    }
}

/// Phase 1: capture the fixture's ephemeral host key (its base64 blob) by doing
/// a `Verify` connect against an empty in-memory store. Panics if the connect
/// unexpectedly succeeds (it must reject + capture) or errors.
pub async fn capture_host_key(auth_for_phase1: SftpAuth) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = HostKeyStore::new(dir.path().join("known_hosts"));
    let out = SftpBackend::connect(config(auth_for_phase1, HostKeyDecision::Verify(store)))
        .await
        .expect("phase-1 verify connect");
    match out {
        SftpConnectOutcome::HostKeyUnverified(u) => {
            assert!(
                u.fingerprint.starts_with("SHA256:"),
                "fingerprint should be SHA256-prefixed"
            );
            assert!(!u.changed, "first-seen fixture key should not be 'changed'");
            u.key_b64
        }
        SftpConnectOutcome::Connected(_) => {
            panic!("phase-1 verify against an empty store must NOT connect")
        }
    }
}

/// Connect to the fixture with the given auth, accepting its host key **once**
/// (no persistence). This is the drop-in replacement for the old accept-any
/// `SftpBackend::connect` the gated suites used.
///
/// Phase 1 uses password auth to learn the key (cheap + always available on the
/// fixture); phase 2 uses the caller's real `auth`.
pub async fn connect_accept_once(auth: SftpAuth) -> SftpBackend {
    // Phase 1: learn the key via password (the fixture always accepts wb/wbpass).
    let key_b64 = capture_host_key(SftpAuth::Password(PASS.into())).await;
    // Phase 2: trust exactly that key for this session, with the real auth.
    let out = SftpBackend::connect(config(
        auth,
        HostKeyDecision::Trust {
            key_b64,
            remember: false,
            store: None,
        },
    ))
    .await
    .expect("phase-2 accept-once connect");
    match out {
        SftpConnectOutcome::Connected(b) => b,
        SftpConnectOutcome::HostKeyUnverified(_) => {
            panic!("accept-once trust connect should not report unverified")
        }
    }
}
