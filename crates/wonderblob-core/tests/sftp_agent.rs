//! Agent + keyfile auth integration tests against the Docker fixture.
//! Gated by WONDERBLOB_TEST_SFTP=1 (same gate as the contract test).
//!
//! Full ritual (or just run `scripts/test-sftp-auth.sh`, which does all of it):
//!
//! 1. `./scripts/test-sftp-up.sh` — throwaway sshd on localhost:2222 (user wb).
//! 2. Generate throwaway keys:
//!    `ssh-keygen -t ed25519 -N '' -f /tmp/wbtest_key` and
//!    `ssh-keygen -t ed25519 -N 'testpass' -f /tmp/wbtest_key_pp`
//! 3. Authorize both pubkeys inside the container (the linuxserver image runs
//!    the SSH user as uid 911): pipe them into
//!    `docker exec -i wonderblob-test-sftp sh -c 'mkdir -p /config/.ssh &&
//!    cat >> /config/.ssh/authorized_keys && chown -R 911:911 /config/.ssh &&
//!    chmod 700 /config/.ssh && chmod 600 /config/.ssh/authorized_keys'`
//! 4. Start a PRIVATE agent (never the developer's real one) and load the key:
//!    `eval $(ssh-agent) && ssh-add /tmp/wbtest_key`
//! 5. Run with `WONDERBLOB_TEST_SFTP=1 WONDERBLOB_TEST_KEYFILE=/tmp/wbtest_key
//!    WONDERBLOB_TEST_KEYFILE_PP=/tmp/wbtest_key_pp cargo test -p
//!    wonderblob-core --test sftp_agent`
//! 6. Kill the agent (`ssh-agent -k`) and `./scripts/test-sftp-down.sh`.
//!
//! Tests skip (early-return with eprintln) when their env vars are unset, so a
//! bare `cargo test` stays green without Docker or an agent.

mod sftp_support;

use sftp_support::{capture_host_key, config, connect_accept_once, PASS};
use wonderblob_core::error::StorageError;
use wonderblob_core::sftp::{HostKeyDecision, SftpAuth, SftpBackend, SftpConnectOutcome};
use wonderblob_core::vfs::StorageBackend;

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

/// Connect with the given auth, accepting the fixture's ephemeral host key once.
/// Returns the connect Result so auth-failure tests can assert on the error.
/// The host-key handshake always passes (we capture the key via password first),
/// so any error reflects the AUTH method under test, not host-key verification.
async fn connect_auth(auth: SftpAuth) -> Result<SftpConnectOutcome, StorageError> {
    let key_b64 = capture_host_key(SftpAuth::Password(PASS.into())).await;
    SftpBackend::connect(config(
        auth,
        HostKeyDecision::Trust {
            key_b64,
            remember: false,
            store: None,
        },
    ))
    .await
}

async fn assert_lists_home(backend: &wonderblob_core::sftp::SftpBackend) {
    // Listing the home dir proves the SFTP channel works post-auth.
    backend.list("/config").await.expect("list home dir");
}

#[tokio::test]
async fn agent_auth_connects_and_lists() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 (see module docs)");
        return;
    }
    if std::env::var("SSH_AUTH_SOCK").is_err() {
        eprintln!("skipped: no SSH_AUTH_SOCK (start the test agent — see module docs)");
        return;
    }
    let backend = connect_accept_once(SftpAuth::Agent).await;
    assert_lists_home(&backend).await;
}

#[tokio::test]
async fn keyfile_auth_connects_and_lists() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 (see module docs)");
        return;
    }
    let Ok(path) = std::env::var("WONDERBLOB_TEST_KEYFILE") else {
        eprintln!("skipped: set WONDERBLOB_TEST_KEYFILE (see module docs)");
        return;
    };
    let backend = connect_accept_once(SftpAuth::KeyFile {
        path,
        passphrase: None,
    })
    .await;
    assert_lists_home(&backend).await;
}

#[tokio::test]
async fn keyfile_auth_with_passphrase_connects_and_lists() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 (see module docs)");
        return;
    }
    let Ok(path) = std::env::var("WONDERBLOB_TEST_KEYFILE_PP") else {
        eprintln!("skipped: set WONDERBLOB_TEST_KEYFILE_PP (see module docs)");
        return;
    };
    let backend = connect_accept_once(SftpAuth::KeyFile {
        path,
        passphrase: Some("testpass".into()),
    })
    .await;
    assert_lists_home(&backend).await;
}

#[tokio::test]
async fn keyfile_auth_fails_cleanly_on_missing_file() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 (see module docs)");
        return;
    }
    let err = match connect_auth(SftpAuth::KeyFile {
        path: "/nonexistent/wonderblob_no_such_key".into(),
        passphrase: None,
    })
    .await
    {
        Ok(_) => panic!("missing key file must fail"),
        Err(e) => e,
    };
    match err {
        StorageError::AuthFailed { detail } => {
            assert!(
                detail.contains("/nonexistent/wonderblob_no_such_key"),
                "detail should name the key path, got: {detail}"
            );
        }
        other => panic!("expected AuthFailed, got: {other:?}"),
    }
}

#[tokio::test]
async fn agent_auth_fails_cleanly_without_agent() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 (see module docs)");
        return;
    }
    // connect() dials TCP before auth, so this stays docker-gated. Point the
    // child process at a socket that cannot exist; env vars are process-global
    // so this runs in a subprocess rather than poisoning parallel tests.
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent_auth_no_agent_inner",
            "--ignored",
            "--nocapture",
        ])
        .env("SSH_AUTH_SOCK", "/nonexistent/wonderblob_no_agent.sock")
        // Isolate from the developer's real ~/.ssh/config: an `IdentityAgent`
        // there (e.g. 1Password) would otherwise be resolved ahead of the
        // bogus SSH_AUTH_SOCK and connect successfully, defeating this test.
        .env("WONDERBLOB_SSH_CONFIG", "/dev/null")
        .output()
        .expect("spawn inner test");
    assert!(
        out.status.success(),
        "inner no-agent test failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Inner body for `agent_auth_fails_cleanly_without_agent`; run only via the
/// subprocess above (ignored otherwise) with SSH_AUTH_SOCK pointing nowhere.
#[tokio::test]
#[ignore]
async fn agent_auth_no_agent_inner() {
    let err = match connect_auth(SftpAuth::Agent).await {
        Ok(_) => panic!("agent auth without an agent must fail"),
        Err(e) => e,
    };
    match err {
        StorageError::AuthFailed { detail } => {
            assert!(
                detail.contains("SSH_AUTH_SOCK") || detail.to_lowercase().contains("agent"),
                "detail should mention the agent/SSH_AUTH_SOCK, got: {detail}"
            );
        }
        other => panic!("expected AuthFailed, got: {other:?}"),
    }
}
