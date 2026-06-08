use crate::state::{AppState, ConnectionId};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;
use tokio::io::AsyncWriteExt;
use wonderblob_core::error::StorageError;
use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};
use wonderblob_core::vfs::{Entry, StorageBackend};

/// Generous ceiling so an unanswered agent prompt (e.g. 1Password approval
/// dialog) can't hang a frontend invoke forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthSpec {
    Agent,
    KeyFile { path: String, passphrase: Option<String> },
    Password { password: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpConnectArgs {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthSpec,
}

#[tauri::command]
pub async fn connect_sftp(
    state: State<'_, AppState>,
    args: SftpConnectArgs,
) -> Result<ConnectionId, StorageError> {
    let auth = match args.auth {
        AuthSpec::Agent => SftpAuth::Agent,
        AuthSpec::KeyFile { path, passphrase } => SftpAuth::KeyFile { path, passphrase },
        AuthSpec::Password { password } => SftpAuth::Password(password),
    };
    let backend = tokio::time::timeout(
        CONNECT_TIMEOUT,
        SftpBackend::connect(SftpConfig {
            host: args.host,
            port: args.port,
            username: args.username,
            auth,
        }),
    )
    .await
    .map_err(|_| StorageError::Network { detail: "connection timed out".into() })??;
    let id = state.next_id();
    state.connections.write().await.insert(id, Arc::new(backend));
    Ok(id)
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, id: ConnectionId) -> Result<(), StorageError> {
    state.remove(id).await;
    Ok(())
}

#[tauri::command]
pub async fn list_dir(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
) -> Result<Vec<Entry>, StorageError> {
    state.get(id).await?.list(&path).await
}

#[tauri::command]
pub async fn download_file(
    state: State<'_, AppState>,
    id: ConnectionId,
    remote_path: String,
    local_path: String,
) -> Result<(), StorageError> {
    let b: Arc<dyn StorageBackend> = state.get(id).await?;
    let mut r = b.read(&remote_path, 0).await?;
    let mut f =
        tokio::fs::File::create(&local_path).await.map_err(StorageError::other)?;
    tokio::io::copy(&mut r, &mut f).await.map_err(StorageError::other)?;
    f.flush().await.map_err(StorageError::other)?;
    Ok(())
}

#[tauri::command]
pub async fn upload_file(
    state: State<'_, AppState>,
    id: ConnectionId,
    local_path: String,
    remote_path: String,
) -> Result<(), StorageError> {
    let b: Arc<dyn StorageBackend> = state.get(id).await?;
    let mut f =
        tokio::fs::File::open(&local_path).await.map_err(StorageError::other)?;
    let mut w = b.write(&remote_path).await?;
    tokio::io::copy(&mut f, &mut w).await.map_err(StorageError::other)?;
    w.shutdown().await.map_err(StorageError::other)?;
    Ok(())
}

#[tauri::command]
pub async fn delete_entry(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
) -> Result<(), StorageError> {
    state.get(id).await?.delete(&path).await
}

#[tauri::command]
pub async fn rename_entry(
    state: State<'_, AppState>,
    id: ConnectionId,
    from: String,
    to: String,
) -> Result<(), StorageError> {
    state.get(id).await?.rename(&from, &to).await
}

#[tauri::command]
pub async fn make_dir(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
) -> Result<(), StorageError> {
    state.get(id).await?.mkdir(&path).await
}
