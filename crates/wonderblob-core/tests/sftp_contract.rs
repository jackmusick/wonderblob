mod contract;
mod sftp_support;

use sftp_support::{connect_accept_once, PASS};
use wonderblob_core::sftp::SftpAuth;

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

#[tokio::test]
async fn sftp_passes_vfs_contract() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    // Accept-once against the ephemeral fixture key (see sftp_support).
    let backend = connect_accept_once(SftpAuth::Password(PASS.into())).await;
    contract::run_contract(&backend, "/config").await;
}
