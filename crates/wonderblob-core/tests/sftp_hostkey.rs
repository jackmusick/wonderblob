//! Gated TOFU two-phase host-key E2E against the Docker OpenSSH fixture.
//! Run: ./scripts/test-sftp-up.sh && WONDERBLOB_TEST_SFTP=1 \
//!   cargo test -p wonderblob-core --test sftp_hostkey

mod sftp_support;

use sftp_support::{config, PASS};
use wonderblob_core::hostkey::HostKeyStore;
use wonderblob_core::sftp::{HostKeyDecision, SftpAuth, SftpBackend, SftpConnectOutcome};

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

#[tokio::test]
async fn first_connect_is_unverified_then_remember_then_known() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");

    // Phase 1: Verify against an empty store → Unverified with a SHA256 fingerprint.
    let out = SftpBackend::connect(config(
        SftpAuth::Password(PASS.into()),
        HostKeyDecision::Verify(HostKeyStore::new(path.clone())),
    ))
    .await
    .unwrap();
    let unv = match out {
        SftpConnectOutcome::HostKeyUnverified(u) => u,
        _ => panic!("expected unverified on first connect"),
    };
    assert!(unv.fingerprint.starts_with("SHA256:"));
    assert!(!unv.changed, "first-seen key must not be flagged changed");

    // Phase 2: Trust + remember → Connected, and the key lands in the file.
    let out = SftpBackend::connect(config(
        SftpAuth::Password(PASS.into()),
        HostKeyDecision::Trust {
            key_b64: unv.key_b64.clone(),
            remember: true,
            store: Some(HostKeyStore::new(path.clone())),
        },
    ))
    .await
    .unwrap();
    assert!(matches!(out, SftpConnectOutcome::Connected(_)));
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("localhost"),
        "remembered key should be written to known_hosts"
    );

    // Now a Verify connect is Known (no prompt).
    let out = SftpBackend::connect(config(
        SftpAuth::Password(PASS.into()),
        HostKeyDecision::Verify(HostKeyStore::new(path.clone())),
    ))
    .await
    .unwrap();
    assert!(
        matches!(out, SftpConnectOutcome::Connected(_)),
        "a remembered key should connect silently"
    );
}

#[tokio::test]
async fn second_connect_with_a_different_pinned_key_reports_changed() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");

    // Pre-seed the store with a DIFFERENT key for localhost:2222 (a bogus
    // ed25519 blob) so the fixture's real key looks CHANGED (MITM scenario).
    let bogus = wonderblob_core::hostkey::key_from_base64(
        "AAAAC3NzaC1lZDI1NTE5AAAAINyH6NkSkjcrEIX8B/Ki0CcnoB3xe1m7pv8XrjCd0ild",
    )
    .unwrap();
    let store = HostKeyStore::new(path.clone());
    store.remember("localhost", 2222, &bogus).unwrap();

    let out = SftpBackend::connect(config(
        SftpAuth::Password(PASS.into()),
        HostKeyDecision::Verify(HostKeyStore::new(path.clone())),
    ))
    .await
    .unwrap();
    let unv = match out {
        SftpConnectOutcome::HostKeyUnverified(u) => u,
        _ => panic!("expected unverified (changed) when a different key is pinned"),
    };
    assert!(
        unv.changed,
        "a different pinned key must be reported as changed (MITM)"
    );
    assert!(unv.fingerprint.starts_with("SHA256:"));

    // The store must NOT have been silently updated.
    let contents = std::fs::read_to_string(&path).unwrap();
    let lines = contents.lines().filter(|l| l.contains("localhost")).count();
    assert_eq!(
        lines, 1,
        "a changed key must never auto-append to known_hosts"
    );
}
