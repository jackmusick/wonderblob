use crate::bookmarks::{secrets, AzAuthKind, Bookmark, BookmarkStore};
use crate::state::{AppState, ConnectionId};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tauri::{Manager, State};
use wonderblob_core::azblob::{AzAuth, AzBlobBackend, AzBlobConfig};
use wonderblob_core::error::StorageError;
use wonderblob_core::s3::{S3Backend, S3Config};
use wonderblob_core::sftp::{SftpAuth, SftpBackend, SftpConfig};
use wonderblob_core::transfer::engine::TransferEngine;
use wonderblob_core::transfer::model::{Direction, Transfer, TransferId};
use wonderblob_core::transfer::store::NewTransfer;
use wonderblob_core::vfs::{Capabilities, Entry, StorageBackend};

/// Generous ceiling so an unanswered agent prompt (e.g. 1Password approval
/// dialog) can't hang a frontend invoke forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthSpec {
    Agent,
    KeyFile {
        path: String,
        passphrase: Option<String>,
    },
    Password {
        password: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpConnectArgs {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthSpec,
}

/// Returned by every connect command so the frontend can gate UI on capabilities.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectResult {
    pub id: ConnectionId,
    pub capabilities: Capabilities,
}

/// Register a freshly-built backend in the connection map and capture its
/// capabilities for the connect result.
async fn register(state: &State<'_, AppState>, backend: Arc<dyn StorageBackend>) -> ConnectResult {
    let capabilities = backend.capabilities();
    let id = state.next_id();
    state.connections.write().await.insert(id, backend);
    ConnectResult { id, capabilities }
}

#[tauri::command]
pub async fn connect_sftp(
    state: State<'_, AppState>,
    args: SftpConnectArgs,
) -> Result<ConnectResult, StorageError> {
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
    .map_err(|_| StorageError::Network {
        detail: "connection timed out".into(),
    })??;
    Ok(register(&state, Arc::new(backend)).await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3ConnectArgs {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub force_path_style: bool,
}

#[tauri::command]
pub async fn connect_s3(
    state: State<'_, AppState>,
    args: S3ConnectArgs,
) -> Result<ConnectResult, StorageError> {
    let backend = tokio::time::timeout(
        CONNECT_TIMEOUT,
        S3Backend::connect(S3Config {
            access_key_id: args.access_key_id,
            secret_access_key: args.secret_access_key,
            region: args.region,
            endpoint: args.endpoint,
            force_path_style: args.force_path_style,
        }),
    )
    .await
    .map_err(|_| StorageError::Network {
        detail: "connection timed out".into(),
    })??;
    Ok(register(&state, Arc::new(backend)).await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AzBlobConnectArgs {
    pub account: String,
    pub endpoint: Option<String>,
    pub auth_kind: AzAuthKind,
    pub secret: String,
}

/// Build the Azure auth credential from a discriminant + the keychain/arg secret.
fn az_auth(kind: AzAuthKind, secret: String) -> AzAuth {
    match kind {
        AzAuthKind::AccountKey => AzAuth::AccountKey(secret),
        AzAuthKind::ConnectionString => AzAuth::ConnectionString(secret),
        AzAuthKind::Sas => AzAuth::Sas(secret),
    }
}

#[tauri::command]
pub async fn connect_azblob(
    state: State<'_, AppState>,
    args: AzBlobConnectArgs,
) -> Result<ConnectResult, StorageError> {
    let backend = tokio::time::timeout(
        CONNECT_TIMEOUT,
        AzBlobBackend::connect(AzBlobConfig {
            account: args.account,
            endpoint: args.endpoint,
            auth: az_auth(args.auth_kind, args.secret),
        }),
    )
    .await
    .map_err(|_| StorageError::Network {
        detail: "connection timed out".into(),
    })??;
    Ok(register(&state, Arc::new(backend)).await)
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

#[tauri::command]
pub async fn share_link(
    state: State<'_, AppState>,
    id: ConnectionId,
    path: String,
    expiry_secs: u64,
) -> Result<String, StorageError> {
    state.get(id).await?.share_link(&path, expiry_secs).await
}

// ---------------------------------------------------------------------------
// Transfers (queued, persistent, resumable — see wonderblob_core::transfer)
// ---------------------------------------------------------------------------

/// Last path component, tolerant of trailing slashes and either separator.
fn basename_of(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

#[tauri::command]
pub async fn enqueue_download(
    state: State<'_, AppState>,
    engine: State<'_, Arc<TransferEngine>>,
    id: ConnectionId,
    remote_path: String,
    local_path: String,
    total_bytes: Option<u64>,
) -> Result<TransferId, StorageError> {
    // Use the caller-supplied size, else stat the remote for the progress bar,
    // else leave it indeterminate.
    let total = match total_bytes {
        Some(b) => Some(b),
        None => match state.get(id).await {
            Ok(b) => b.stat(&remote_path).await.ok().and_then(|e| e.size),
            Err(_) => None,
        },
    };
    engine
        .enqueue(NewTransfer {
            connection_id: id,
            direction: Direction::Down,
            name: basename_of(&remote_path),
            remote_path,
            local_path,
            total_bytes: total,
        })
        .await
}

#[tauri::command]
pub async fn enqueue_upload(
    engine: State<'_, Arc<TransferEngine>>,
    id: ConnectionId,
    local_path: String,
    remote_path: String,
) -> Result<TransferId, StorageError> {
    let total = tokio::fs::metadata(&local_path).await.ok().map(|m| m.len());
    engine
        .enqueue(NewTransfer {
            connection_id: id,
            direction: Direction::Up,
            name: basename_of(&local_path),
            remote_path,
            local_path,
            total_bytes: total,
        })
        .await
}

#[tauri::command]
pub async fn pause_transfer(
    engine: State<'_, Arc<TransferEngine>>,
    transfer_id: TransferId,
) -> Result<(), StorageError> {
    engine.pause(transfer_id).await
}

#[tauri::command]
pub async fn resume_transfer(
    engine: State<'_, Arc<TransferEngine>>,
    transfer_id: TransferId,
    connection_id: Option<u64>,
) -> Result<(), StorageError> {
    engine.resume_with(transfer_id, connection_id).await
}

#[tauri::command]
pub async fn cancel_transfer(
    engine: State<'_, Arc<TransferEngine>>,
    transfer_id: TransferId,
) -> Result<(), StorageError> {
    engine.cancel(transfer_id).await
}

#[tauri::command]
pub async fn list_transfers(
    engine: State<'_, Arc<TransferEngine>>,
) -> Result<Vec<Transfer>, StorageError> {
    engine.list()
}

#[tauri::command]
pub async fn clear_completed(
    engine: State<'_, Arc<TransferEngine>>,
) -> Result<usize, StorageError> {
    engine.clear_completed()
}

// ---------------------------------------------------------------------------
// Bookmarks
// ---------------------------------------------------------------------------

fn store(app: &tauri::AppHandle) -> Result<BookmarkStore, StorageError> {
    let dir = app.path().app_config_dir().map_err(StorageError::other)?;
    Ok(BookmarkStore::new(dir))
}

/// Run a blocking keychain call off the async runtime — KWallet/secret-service
/// can block indefinitely waiting for the user to unlock the wallet.
async fn keychain<T, F>(f: F) -> Result<T, StorageError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StorageError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(StorageError::other)?
}

#[tauri::command]
pub async fn bookmarks_list(app: tauri::AppHandle) -> Result<Vec<Bookmark>, StorageError> {
    store(&app)?.load_all()
}

#[tauri::command]
pub async fn bookmark_save(
    app: tauri::AppHandle,
    bookmark: Bookmark,
    secret: Option<String>,
) -> Result<(), StorageError> {
    use crate::bookmarks::AuthMethod;
    let st = store(&app)?;
    let existing = st.load_all()?.into_iter().find(|b| b.id == bookmark.id);
    let is_new = existing.is_none();
    let key = bookmark.id.to_string();
    let mut created_secret = false;
    if let Some(s) = secret {
        let k = key.clone();
        keychain(move || secrets::set(&k, &s)).await?;
        created_secret = true;
    } else {
        // No new secret supplied: drop any stale keychain entry when the new
        // method doesn't use one (Agent) or the method changed (e.g. an old
        // password must not be reused as a key passphrase).  Only when the
        // method is unchanged and still secret-using do we keep the saved one.
        // `auth_method` is now `Option<AuthMethod>`. Compare the discriminant of
        // the INNER variant, not the Option: two cloud edits (both `None`) stay
        // equal so the saved secret is kept, while an SFTP Password↔KeyFile switch
        // (both `Some`, different variants) correctly reads as changed so the old
        // password isn't silently reused as a key passphrase.
        let method_changed = existing.as_ref().is_some_and(|e| {
            e.auth_method.as_ref().map(std::mem::discriminant)
                != bookmark.auth_method.as_ref().map(std::mem::discriminant)
        });
        // Agent (SFTP) uses no secret; cloud protocols always use one. Only wipe
        // a stale secret for Agent or when the SFTP method changed.
        let is_agent = matches!(bookmark.auth_method, Some(AuthMethod::Agent));
        if is_agent || method_changed {
            let k = key.clone();
            keychain(move || secrets::delete(&k)).await?;
        }
    }
    let result = st.save(&bookmark);
    if result.is_err() && created_secret && is_new {
        // Best-effort orphan cleanup: this save created the secret for a brand
        // new bookmark id, so nothing else references it.  (For edits, keeping
        // the previous secret is correct.)
        let _ = keychain(move || secrets::delete(&key)).await;
    }
    result
}

#[tauri::command]
pub async fn bookmark_delete(app: tauri::AppHandle, id: uuid::Uuid) -> Result<(), StorageError> {
    let key = id.to_string();
    keychain(move || secrets::delete(&key)).await?;
    store(&app)?.delete(id)
}

/// Connect using a saved bookmark: resolves the secret from the keychain.
#[tauri::command]
pub async fn connect_bookmark(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: uuid::Uuid,
) -> Result<ConnectResult, StorageError> {
    use crate::bookmarks::{AuthMethod, Protocol};
    let b = store(&app)?
        .load_all()?
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| StorageError::Other {
            detail: "bookmark not found".into(),
        })?;
    let key = b.id.to_string();

    let backend: Arc<dyn StorageBackend> = match b.protocol {
        Protocol::Sftp => {
            let auth = match b.auth_method.clone().ok_or_else(|| StorageError::Other {
                detail: "SFTP bookmark missing auth method".into(),
            })? {
                AuthMethod::Agent => SftpAuth::Agent,
                AuthMethod::KeyFile { path } => {
                    let k = key.clone();
                    SftpAuth::KeyFile {
                        path,
                        passphrase: keychain(move || secrets::get(&k)).await?,
                    }
                }
                AuthMethod::Password => {
                    let k = key.clone();
                    SftpAuth::Password(keychain(move || secrets::get(&k)).await?.ok_or(
                        StorageError::AuthFailed {
                            detail: "no saved password".into(),
                        },
                    )?)
                }
            };
            let backend = tokio::time::timeout(
                CONNECT_TIMEOUT,
                SftpBackend::connect(SftpConfig {
                    host: b.host,
                    port: b.port,
                    username: b.username,
                    auth,
                }),
            )
            .await
            .map_err(|_| StorageError::Network {
                detail: "connection timed out".into(),
            })??;
            Arc::new(backend)
        }
        Protocol::S3 => {
            let p = b.s3.ok_or_else(|| StorageError::Other {
                detail: "S3 bookmark missing params".into(),
            })?;
            let k = key.clone();
            let secret =
                keychain(move || secrets::get(&k))
                    .await?
                    .ok_or(StorageError::AuthFailed {
                        detail: "no saved secret access key".into(),
                    })?;
            let backend = tokio::time::timeout(
                CONNECT_TIMEOUT,
                S3Backend::connect(S3Config {
                    access_key_id: p.access_key_id,
                    secret_access_key: secret,
                    region: p.region,
                    endpoint: p.endpoint,
                    force_path_style: p.force_path_style,
                }),
            )
            .await
            .map_err(|_| StorageError::Network {
                detail: "connection timed out".into(),
            })??;
            Arc::new(backend)
        }
        Protocol::AzBlob => {
            let p = b.azblob.ok_or_else(|| StorageError::Other {
                detail: "Azure bookmark missing params".into(),
            })?;
            let k = key.clone();
            let secret =
                keychain(move || secrets::get(&k))
                    .await?
                    .ok_or(StorageError::AuthFailed {
                        detail: "no saved Azure credential".into(),
                    })?;
            let backend = tokio::time::timeout(
                CONNECT_TIMEOUT,
                AzBlobBackend::connect(AzBlobConfig {
                    account: p.account,
                    endpoint: p.endpoint,
                    auth: az_auth(p.auth_kind, secret),
                }),
            )
            .await
            .map_err(|_| StorageError::Network {
                detail: "connection timed out".into(),
            })??;
            Arc::new(backend)
        }
    };
    Ok(register(&state, backend).await)
}
