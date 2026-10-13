use crate::config::bookmarks::{SshAuthMethod, SshConfig};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use russh::client::{self, Config, Handle, Handler, KeyboardInteractiveAuthResponse};
use russh_keys::key::PrivateKeyWithHashAlg;
use russh_keys::{HashAlg, PublicKey, load_secret_key};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

struct SshClientHandler;

#[async_trait]
impl Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct SshTunnel {
    local_port: u16,
    shutdown_tx: watch::Sender<bool>,
    listener_task: JoinHandle<()>,
}

impl SshTunnel {
    pub async fn establish(
        ssh: &SshConfig,
        secret: &str,
        remote_db_host: &str,
        remote_db_port: u16,
    ) -> Result<Self> {
        let config = Arc::new(Config {
            keepalive_interval: Some(Duration::from_secs(30)),
            inactivity_timeout: Some(Duration::from_secs(3600)),
            ..Default::default()
        });

        let ssh_addr = format!("{}:{}", ssh.host.trim(), ssh.port);
        let mut session: Handle<SshClientHandler> = tokio::time::timeout(
            Duration::from_secs(15),
            client::connect(config, ssh_addr.as_str(), SshClientHandler),
        )
        .await
        .context("Timed out connecting to SSH server")?
        .with_context(|| format!("Failed to connect to SSH server at {}", ssh_addr))?;

        let authenticated = match &ssh.auth_method {
            SshAuthMethod::Password => {
                let pass_ok = session
                    .authenticate_password(&ssh.username, secret)
                    .await
                    .context("SSH password authentication error")?;
                if pass_ok {
                    true
                } else {
                    // Fallback to keyboard-interactive for servers configured with PAM/ChallengeResponse
                    match session
                        .authenticate_keyboard_interactive_start(&ssh.username, None)
                        .await
                    {
                        Ok(KeyboardInteractiveAuthResponse::Success) => true,
                        Ok(KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. }) => {
                            let responses = vec![secret.to_string(); prompts.len()];
                            matches!(
                                session
                                    .authenticate_keyboard_interactive_respond(responses)
                                    .await,
                                Ok(KeyboardInteractiveAuthResponse::Success)
                            )
                        }
                        _ => false,
                    }
                }
            }
            SshAuthMethod::PrivateKey { key_path } => {
                let expanded_path = expand_tilde(key_path);
                let pass_opt = if secret.is_empty() {
                    None
                } else {
                    Some(secret)
                };
                let priv_key = load_secret_key(&expanded_path, pass_opt)
                    .with_context(|| format!("Failed to load SSH private key from {:?}", expanded_path))?;
                let is_rsa = priv_key.algorithm().is_rsa();
                let key_arc = Arc::new(priv_key);
                let hash_alg = if is_rsa { Some(HashAlg::Sha256) } else { None };
                let key_with_alg = PrivateKeyWithHashAlg::new(key_arc.clone(), hash_alg)
                    .context("Failed to prepare SSH key for signing")?;
                let ok = session
                    .authenticate_publickey(&ssh.username, key_with_alg)
                    .await
                    .context("SSH public key authentication failed")?;
                if !ok && is_rsa {
                    // Fallback to legacy RSA SHA-1 if server requires it
                    let fallback_key = PrivateKeyWithHashAlg::new(key_arc, None)?;
                    session
                        .authenticate_publickey(&ssh.username, fallback_key)
                        .await
                        .context("SSH RSA public key authentication failed")?
                } else {
                    ok
                }
            }
        };

        if !authenticated {
            return Err(anyhow!(
                "SSH authentication rejected for user '{}' on {}",
                ssh.username,
                ssh_addr
            ));
        }

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind local port for SSH tunnel")?;
        let local_port = listener.local_addr()?.port();

        let session = Arc::new(session);
        let remote_host = remote_db_host.to_string();
        let remote_port = remote_db_port as u32;
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let listener_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((mut local_stream, peer_addr)) => {
                                let session_clone = Arc::clone(&session);
                                let target_host = remote_host.clone();
                                tokio::spawn(async move {
                                    match session_clone
                                        .channel_open_direct_tcpip(
                                            target_host,
                                            remote_port,
                                            peer_addr.ip().to_string(),
                                            peer_addr.port() as u32,
                                        )
                                        .await
                                    {
                                        Ok(channel) => {
                                            let mut channel_stream = channel.into_stream();
                                            let _ = copy_bidirectional(&mut local_stream, &mut channel_stream).await;
                                        }
                                        Err(err) => {
                                            tracing::warn!("SSH direct-tcpip channel failed: {}", err);
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                tracing::warn!("Local SSH tunnel listener error: {}", err);
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            local_port,
            shutdown_tx,
            listener_task,
        })
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        self.listener_task.abort();
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    let trimmed = path.trim();
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(user_dirs) = directories::UserDirs::new() {
            return user_dirs.home_dir().join(rest);
        }
    }
    PathBuf::from(trimmed)
}
