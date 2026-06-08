mod contract;

use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};

fn enabled() -> bool {
    std::env::var("WONDERBLOB_TEST_SFTP").as_deref() == Ok("1")
}

#[tokio::test]
async fn sftp_passes_vfs_contract() {
    if !enabled() {
        eprintln!("skipped: set WONDERBLOB_TEST_SFTP=1 and run scripts/test-sftp-up.sh");
        return;
    }
    let backend = SftpBackend::connect(SftpConfig {
        host: "localhost".into(),
        port: 2222,
        username: "wb".into(),
        auth: SftpAuth::Password("wbpass".into()),
    })
    .await
    .expect("connect");
    contract::run_contract(&backend, "/config").await;
}
