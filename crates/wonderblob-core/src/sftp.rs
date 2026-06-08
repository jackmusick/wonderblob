use crate::error::{Result, StorageError};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use russh::client;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, StatusCode};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncSeekExt, AsyncWrite};

pub enum SftpAuth {
    /// Try every identity in the SSH agent (SSH_AUTH_SOCK) — 1Password et al.
    Agent,
    KeyFile {
        path: String,
        passphrase: Option<String>,
    },
    Password(String),
}

pub struct SftpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SftpAuth,
}

struct Handler;

#[async_trait]
impl client::Handler for Handler {
    type Error = russh::Error;
    // v1: accept any host key; host-key verification is a tracked follow-up
    // before any public release.
    async fn check_server_key(
        &mut self,
        _key: &russh::keys::key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct SftpBackend {
    sftp: SftpSession,
    _session: client::Handle<Handler>, // keep the connection alive
}

impl SftpBackend {
    pub async fn connect(cfg: SftpConfig) -> Result<Self> {
        let config = Arc::new(client::Config::default());
        let mut session = client::connect(config, (cfg.host.as_str(), cfg.port), Handler)
            .await
            .map_err(|e| StorageError::Network {
                detail: e.to_string(),
            })?;

        let authed = match &cfg.auth {
            SftpAuth::Password(pw) => session
                .authenticate_password(&cfg.username, pw)
                .await
                .map_err(|e| StorageError::Network {
                    detail: e.to_string(),
                })?,
            SftpAuth::Agent => authenticate_agent(&mut session, &cfg.username).await?,
            SftpAuth::KeyFile { path, passphrase } => {
                authenticate_keyfile(&mut session, &cfg.username, path, passphrase.as_deref())
                    .await?
            }
        };
        if !authed {
            return Err(StorageError::AuthFailed {
                detail: format!("all auth methods rejected for {}", cfg.username),
            });
        }

        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| StorageError::Network {
                detail: e.to_string(),
            })?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| StorageError::Network {
                detail: e.to_string(),
            })?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(StorageError::other)?;

        Ok(Self {
            sftp,
            _session: session,
        })
    }
}

/// Authenticate via the SSH agent at SSH_AUTH_SOCK (1Password, KeePassXC,
/// OpenSSH ssh-agent, ...). Tries every identity the agent offers; the first
/// one the server accepts wins. Signing happens inside the agent — private
/// keys never touch this process.
async fn authenticate_agent(session: &mut client::Handle<Handler>, user: &str) -> Result<bool> {
    let mut agent = russh_keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|e| StorageError::AuthFailed {
            detail: format!("cannot reach ssh-agent (is SSH_AUTH_SOCK set?): {e}"),
        })?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| StorageError::AuthFailed {
            detail: format!("ssh-agent refused to list identities: {e}"),
        })?;
    if identities.is_empty() {
        return Err(StorageError::AuthFailed {
            detail: "ssh-agent has no identities loaded".into(),
        });
    }
    for key in identities {
        let (returned_agent, result) = session.authenticate_future(user, key, agent).await;
        agent = returned_agent;
        match result {
            Ok(true) => return Ok(true),
            // Server rejected this identity — try the next one.
            Ok(false) => continue,
            // Session channel is gone: the connection itself died.
            Err(russh::AgentAuthError::Send(e)) => {
                return Err(StorageError::Network {
                    detail: e.to_string(),
                })
            }
            // Agent refused/failed to sign with this key — try the next one.
            Err(russh::AgentAuthError::Key(_)) => continue,
        }
    }
    Ok(false)
}

/// Authenticate with an on-disk private key (OpenSSH/PKCS#8 formats),
/// optionally passphrase-protected.
async fn authenticate_keyfile(
    session: &mut client::Handle<Handler>,
    user: &str,
    path: &str,
    passphrase: Option<&str>,
) -> Result<bool> {
    let key =
        russh_keys::load_secret_key(path, passphrase).map_err(|e| StorageError::AuthFailed {
            detail: format!("failed to load key file {path}: {e}"),
        })?;
    session
        .authenticate_publickey(user, Arc::new(key))
        .await
        .map_err(|e| StorageError::Network {
            detail: e.to_string(),
        })
}

/// Map russh-sftp errors into the taxonomy using the typed SFTP status code.
fn map_sftp_err(path: &str, e: russh_sftp::client::error::Error) -> StorageError {
    if let russh_sftp::client::error::Error::Status(status) = &e {
        match status.status_code {
            StatusCode::NoSuchFile => return StorageError::NotFound { path: path.into() },
            StatusCode::PermissionDenied => {
                return StorageError::PermissionDenied { path: path.into() }
            }
            _ => {}
        }
    }
    StorageError::Other {
        detail: e.to_string(),
    }
}

fn entry_from(
    path_prefix: &str,
    name: &str,
    attrs: &russh_sftp::protocol::FileAttributes,
) -> Entry {
    let kind = match attrs.file_type() {
        FileType::Dir => EntryKind::Dir,
        FileType::Symlink => EntryKind::Symlink,
        _ => EntryKind::File,
    };
    Entry {
        name: name.to_string(),
        path: format!("{}/{}", path_prefix.trim_end_matches('/'), name),
        kind,
        size: attrs.size,
        modified_ms: attrs.mtime.map(|t| (t as i64) * 1000),
    }
}

#[async_trait]
impl StorageBackend for SftpBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_presign: false,
            can_rename: true,
            can_set_mtime: true,
        }
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let dir = self
            .sftp
            .read_dir(path)
            .await
            .map_err(|e| map_sftp_err(path, e))?;
        let mut out: Vec<Entry> = dir
            .filter(|f| f.file_name() != "." && f.file_name() != "..")
            .map(|f| entry_from(path, &f.file_name(), &f.metadata()))
            .collect();
        out.sort_by(|a, b| {
            (b.kind == EntryKind::Dir)
                .cmp(&(a.kind == EntryKind::Dir))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<Entry> {
        let attrs = self
            .sftp
            .metadata(path)
            .await
            .map_err(|e| map_sftp_err(path, e))?;
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        Ok(entry_from(parent, &name, &attrs))
    }

    async fn read(&self, path: &str, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        let mut f = self
            .sftp
            .open(path)
            .await
            .map_err(|e| map_sftp_err(path, e))?;
        if offset > 0 {
            f.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(StorageError::other)?;
        }
        Ok(Box::new(f))
    }

    async fn write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        let f = self
            .sftp
            .create(path)
            .await
            .map_err(|e| map_sftp_err(path, e))?;
        Ok(Box::new(f))
    }

    async fn delete(&self, path: &str) -> Result<()> {
        match self.stat(path).await?.kind {
            EntryKind::Dir => self.sftp.remove_dir(path).await,
            _ => self.sftp.remove_file(path).await,
        }
        .map_err(|e| map_sftp_err(path, e))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.sftp
            .rename(from, to)
            .await
            .map_err(|e| map_sftp_err(from, e))
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        self.sftp
            .create_dir(path)
            .await
            .map_err(|e| map_sftp_err(path, e))
    }

    async fn share_link(&self, _path: &str, _expiry_secs: u64) -> Result<String> {
        Err(StorageError::Unsupported {
            op: "share_link".into(),
        })
    }
}
