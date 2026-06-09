use crate::error::{Result, StorageError};
use crate::hostkey::{fingerprint, key_from_base64, key_to_base64, HostKeyStatus, HostKeyStore};
use crate::vfs::{Capabilities, Entry, EntryKind, StorageBackend};
use async_trait::async_trait;
use russh::client;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, StatusCode};
use std::sync::{Arc, Mutex as StdMutex};
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

/// What to do about the server's host key on THIS connect attempt.
///
/// The TOFU flow is two-phase (see [`SftpBackend::connect`] /
/// [`SftpConnectOutcome`]): phase 1 verifies against the store and rejects an
/// unknown/changed key (capturing it for the caller to surface); phase 2 trusts
/// the exact key the user approved.
pub enum HostKeyDecision {
    /// Verify against the store; unknown/changed keys are rejected and the
    /// presented key is captured for the caller to surface (TOFU phase 1).
    Verify(HostKeyStore),
    /// Trust exactly this key (its base64), and remember it (persist to the
    /// store) if `remember`. Used by the connect retry after the user approves
    /// (TOFU phase 2), and by gated tests (`remember: false` = accept-once, for
    /// the ephemeral fixture key — never persisted).
    Trust {
        key_b64: String,
        remember: bool,
        store: Option<HostKeyStore>,
    },
}

pub struct SftpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SftpAuth,
    /// How to handle the server's host key on this attempt (TOFU phases).
    pub host_key: HostKeyDecision,
}

/// Connect could not complete because the host key is unverified — NOT an error,
/// a decision-needed state the frontend turns into an approval dialog. The
/// handshake was aborted before any data flowed to the (untrusted) server.
pub struct HostKeyUnverified {
    pub host: String,
    pub port: u16,
    /// `SHA256:…` display fingerprint of the presented key.
    pub fingerprint: String,
    /// Opaque base64 of the presented key; round-tripped into the phase-2 retry.
    pub key_b64: String,
    /// true => a DIFFERENT key is already recorded (MITM warning), false => first-seen.
    pub changed: bool,
}

/// Either a connected backend or a TOFU decision-needed state.
pub enum SftpConnectOutcome {
    Connected(SftpBackend),
    HostKeyUnverified(HostKeyUnverified),
}

/// The verdict the [`Handler`] enforces mid-handshake. Computed before the
/// handshake so `check_server_key` never blocks on a browser dialog.
enum HostKeyVerdict {
    /// Verify the presented key against the store; capture + reject if not Known.
    Verify(HostKeyStore),
    /// Trust iff the presented key's base64 equals this exact approved blob.
    Trust { key_b64: String },
}

struct Handler {
    host: String,
    port: u16,
    verdict: HostKeyVerdict,
    /// Filled with `(fingerprint, key_b64, changed)` on a verify-rejection so
    /// `connect` can report the decision-needed state instead of a raw error.
    captured: Arc<StdMutex<Option<(String, String, bool)>>>,
}

#[async_trait]
impl client::Handler for Handler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &russh::keys::key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        let presented_b64 = key_to_base64(key);
        match &self.verdict {
            // Phase 2: trust ONLY the exact key the user approved.
            HostKeyVerdict::Trust { key_b64 } => Ok(&presented_b64 == key_b64),
            // Phase 1: classify; Known proceeds, anything else captures + rejects
            // so no bytes ever flow to an unverified/MITM host.
            HostKeyVerdict::Verify(store) => match store.classify(&self.host, self.port, key) {
                Ok(HostKeyStatus::Known) => Ok(true),
                Ok(status) => {
                    *self.captured.lock().unwrap() = Some((
                        fingerprint(key),
                        presented_b64,
                        status == HostKeyStatus::Changed,
                    ));
                    Ok(false)
                }
                // A store read error is treated as "do not trust".
                Err(_) => Ok(false),
            },
        }
    }
}

pub struct SftpBackend {
    sftp: SftpSession,
    _session: client::Handle<Handler>, // keep the connection alive
}

impl SftpBackend {
    pub async fn connect(cfg: SftpConfig) -> Result<SftpConnectOutcome> {
        let captured: Arc<StdMutex<Option<(String, String, bool)>>> = Arc::new(StdMutex::new(None));

        // Resolve the verdict + the (key_b64, store) to persist on a successful
        // phase-2 remember. We never write to the store on a failed handshake.
        let (verdict, remember): (HostKeyVerdict, Option<(String, HostKeyStore)>) =
            match cfg.host_key {
                HostKeyDecision::Verify(store) => (HostKeyVerdict::Verify(store), None),
                HostKeyDecision::Trust {
                    key_b64,
                    remember,
                    store,
                } => {
                    let to_remember = if remember {
                        store.map(|s| (key_b64.clone(), s))
                    } else {
                        None
                    };
                    (HostKeyVerdict::Trust { key_b64 }, to_remember)
                }
            };

        let handler = Handler {
            host: cfg.host.clone(),
            port: cfg.port,
            verdict,
            captured: captured.clone(),
        };

        let config = Arc::new(client::Config::default());
        let mut session =
            match client::connect(config, (cfg.host.as_str(), cfg.port), handler).await {
                Ok(s) => s,
                Err(e) => {
                    // A rejected host key surfaces here as a handshake error; if we
                    // captured a key, report the decision-needed state instead.
                    if let Some((fp, key_b64, changed)) = captured.lock().unwrap().take() {
                        return Ok(SftpConnectOutcome::HostKeyUnverified(HostKeyUnverified {
                            host: cfg.host,
                            port: cfg.port,
                            fingerprint: fp,
                            key_b64,
                            changed,
                        }));
                    }
                    return Err(StorageError::Network {
                        detail: e.to_string(),
                    });
                }
            };

        // Phase-2 trust that asked to remember: the handshake passed (so the key
        // matched the approved blob) — persist it now. A CHANGED key is never
        // silently updated: that path only reaches here via an explicit retry.
        if let Some((key_b64, store)) = remember {
            if let Ok(k) = key_from_base64(&key_b64) {
                let _ = store.remember(&cfg.host, cfg.port, &k);
            }
        }

        let authed = match &cfg.auth {
            SftpAuth::Password(pw) => session
                .authenticate_password(&cfg.username, pw)
                .await
                .map_err(|e| StorageError::Network {
                    detail: e.to_string(),
                })?,
            SftpAuth::Agent => {
                authenticate_agent(&mut session, &cfg.username, &cfg.host).await?
            }
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

        Ok(SftpConnectOutcome::Connected(Self {
            sftp,
            _session: session,
        }))
    }
}

/// Authenticate via the SSH agent (1Password, KeePassXC, OpenSSH ssh-agent,
/// ...). The socket is resolved the way `ssh` resolves it — honoring
/// `IdentityAgent` from `~/.ssh/config` for `host`, falling back to
/// `$SSH_AUTH_SOCK`. Tries every identity the agent offers; the first one the
/// server accepts wins. Signing happens inside the agent — private keys never
/// touch this process.
async fn authenticate_agent(
    session: &mut client::Handle<Handler>,
    user: &str,
    host: &str,
) -> Result<bool> {
    use russh_keys::agent::client::AgentClient;
    let mut agent = match crate::ssh_agent::resolve_agent_socket(host) {
        // ssh_config's IdentityAgent wins over the environment, just like ssh.
        Some(path) => AgentClient::connect_uds(&path).await.map_err(|e| {
            StorageError::AuthFailed {
                detail: format!(
                    "cannot reach ssh-agent at {} (from ssh_config IdentityAgent): {e}",
                    path.display()
                ),
            }
        })?,
        None => AgentClient::connect_env()
            .await
            .map_err(|e| StorageError::AuthFailed {
                detail: format!("cannot reach ssh-agent (is SSH_AUTH_SOCK set?): {e}"),
            })?,
    };
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
