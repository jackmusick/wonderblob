use crate::bookmarks::{secrets, Bookmark, BookmarkStore};
use crate::state::{AppState, ConnectionId};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tauri::{Manager, State};
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
    let result = async {
        tokio::io::copy(&mut r, &mut f).await.map_err(StorageError::other)?;
        f.flush().await.map_err(StorageError::other)?;
        Ok(())
    }
    .await;
    if result.is_err() {
        // Best-effort: don't leave a truncated partial file behind.
        drop(f); // close the handle first (required on Windows)
        let _ = tokio::fs::remove_file(&local_path).await;
    }
    result
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
    // FIXME(v1): remote partial file is left behind; TransferEngine (Plan 3)
    // replaces this whole path with resumable chunked uploads + cleanup.
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
        let method_changed = existing.as_ref().is_some_and(|e| {
            std::mem::discriminant(&e.auth_method)
                != std::mem::discriminant(&bookmark.auth_method)
        });
        if matches!(bookmark.auth_method, AuthMethod::Agent) || method_changed {
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
) -> Result<ConnectionId, StorageError> {
    use crate::bookmarks::AuthMethod;
    let b = store(&app)?
        .load_all()?
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| StorageError::Other { detail: "bookmark not found".into() })?;
    let auth = match b.auth_method {
        AuthMethod::Agent => SftpAuth::Agent,
        AuthMethod::KeyFile { path } => {
            let key = b.id.to_string();
            SftpAuth::KeyFile { path, passphrase: keychain(move || secrets::get(&key)).await? }
        }
        AuthMethod::Password => {
            let key = b.id.to_string();
            SftpAuth::Password(keychain(move || secrets::get(&key)).await?.ok_or(
                StorageError::AuthFailed { detail: "no saved password".into() },
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
    .map_err(|_| StorageError::Network { detail: "connection timed out".into() })??;
    let cid = state.next_id();
    state.connections.write().await.insert(cid, Arc::new(backend));
    Ok(cid)
}
