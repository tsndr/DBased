use crate::config::bookmarks::{SshAuthMethod, SshConfig};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use directories::ProjectDirs;
use russh::client::{self, Config, Handle, Handler, KeyboardInteractiveAuthResponse};
use russh_keys::key::PrivateKeyWithHashAlg;
use russh_keys::{HashAlg, PublicKey, load_secret_key};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Debug, Default, Serialize, Deserialize)]
struct KnownHostsFile {
    #[serde(default)]
    hosts: BTreeMap<String, String>,
}

struct KnownHostsStore;

impl KnownHostsStore {
    fn default_path() -> PathBuf {
        if let Some(proj_dirs) = ProjectDirs::from("dev", "tsndr", "dbased") {
            proj_dirs.config_dir().join("known_hosts.toml")
        } else {
            PathBuf::from(".dbased_known_hosts.toml")
        }
    }

    fn write_private_file_atomically(path: &Path, content: &str) -> Result<()> {
        use std::io::Write;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
        {
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp_path)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn verify_or_trust_at(
        path: &Path,
        host_key_id: &str,
        server_public_key: &PublicKey,
    ) -> Result<()> {
        let presented_key = server_public_key
            .to_openssh()
            .map_err(|e| anyhow!("Failed to encode SSH server public key: {}", e))?;

        let mut file: KnownHostsFile = if path.exists() {
            let raw = fs::read_to_string(path).with_context(|| {
                format!("Failed to read SSH known_hosts file '{}'", path.display())
            })?;
            toml::from_str(&raw).with_context(|| {
                format!("Failed to parse SSH known_hosts file '{}'", path.display())
            })?
        } else {
            KnownHostsFile::default()
        };

        if let Some(stored_key) = file.hosts.get(host_key_id) {
            if stored_key.trim() == presented_key.trim() {
                return Ok(());
            }
            let expected_fp = PublicKey::from_openssh(stored_key.trim())
                .map(|k| k.fingerprint(HashAlg::Sha256).to_string())
                .unwrap_or_else(|_| stored_key.trim().to_string());
            let presented_fp = server_public_key.fingerprint(HashAlg::Sha256).to_string();
            return Err(anyhow!(
                "SSH host key mismatch for '{}'! Expected {}, got {} (in {})",
                host_key_id,
                expected_fp,
                presented_fp,
                path.display()
            ));
        }

        file.hosts.insert(host_key_id.to_string(), presented_key);
        let serialized = toml::to_string_pretty(&file)?;
        Self::write_private_file_atomically(path, &serialized)?;
        Ok(())
    }
}

struct SshClientHandler {
    host_key_id: String,
    known_hosts_path: PathBuf,
    verification_error: Arc<std::sync::Mutex<Option<String>>>,
}

impl SshClientHandler {
    fn new(host_key_id: String) -> Self {
        Self::new_with_path(host_key_id, KnownHostsStore::default_path())
    }

    fn new_with_path(host_key_id: String, known_hosts_path: PathBuf) -> Self {
        Self {
            host_key_id,
            known_hosts_path,
            verification_error: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[cfg(test)]
    fn take_verification_error(&self) -> Option<String> {
        self.verification_error.lock().ok().and_then(|mut g| g.take())
    }
}

#[async_trait]
impl Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        match KnownHostsStore::verify_or_trust_at(
            &self.known_hosts_path,
            &self.host_key_id,
            server_public_key,
        ) {
            Ok(()) => Ok(true),
            Err(err) => {
                if let Ok(mut guard) = self.verification_error.lock() {
                    *guard = Some(err.to_string());
                }
                Ok(false)
            }
        }
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
        let handler = SshClientHandler::new(ssh_addr.clone());
        let verification_error = Arc::clone(&handler.verification_error);
        let mut session: Handle<SshClientHandler> = match tokio::time::timeout(
            Duration::from_secs(15),
            client::connect(config, ssh_addr.as_str(), handler),
        )
        .await
        .context("Timed out connecting to SSH server")?
        {
            Ok(session) => session,
            Err(err) => {
                if let Some(host_key_err) =
                    verification_error.lock().ok().and_then(|mut g| g.take())
                {
                    return Err(anyhow!(host_key_err));
                }
                return Err(err)
                    .with_context(|| format!("Failed to connect to SSH server at {}", ssh_addr));
            }
        };

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
    if trimmed == "~" {
        if let Some(user_dirs) = directories::UserDirs::new() {
            return user_dirs.home_dir().to_path_buf();
        }
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(user_dirs) = directories::UserDirs::new() {
            return user_dirs.home_dir().join(rest);
        }
    }
    PathBuf::from(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY_1: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti";
    const TEST_KEY_2: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG4r/3v8/1+2+3+4+5+6+7+8+9+0+a+b+c+d+e+f+g+h";

    #[test]
    fn test_expand_tilde_paths() {
        let abs = expand_tilde("  /etc/ssh/ssh_host_ed25519_key  ");
        assert_eq!(abs, PathBuf::from("/etc/ssh/ssh_host_ed25519_key"));

        if let Some(user_dirs) = directories::UserDirs::new() {
            let home = user_dirs.home_dir();
            assert_eq!(
                expand_tilde("~/.ssh/id_ed25519"),
                home.join(".ssh/id_ed25519")
            );
            assert_eq!(expand_tilde("~"), home.to_path_buf());
        }
    }

    #[tokio::test]
    async fn test_ssh_tunnel_drop_signals_shutdown_and_aborts_listener() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let listener_task = tokio::spawn(async move {
            let _ = shutdown_rx.changed().await;
        });
        let abort_check = listener_task.abort_handle();
        let tunnel = SshTunnel {
            local_port: 45678,
            shutdown_tx,
            listener_task,
        };
        assert_eq!(tunnel.local_port(), 45678);
        drop(tunnel);
        tokio::task::yield_now().await;
        assert!(abort_check.is_finished());
    }

    #[tokio::test]
    async fn test_ssh_known_hosts_tofu_persists_first_key_and_rejects_mismatch() {
        let tmp_dir = std::env::temp_dir().join(format!("dbased_ssh_test_{}", uuid::Uuid::new_v4()));
        let known_hosts_path = tmp_dir.join("known_hosts.toml");

        let key1 = PublicKey::from_openssh(TEST_KEY_1).expect("valid test key 1");
        let key2 = PublicKey::from_openssh(TEST_KEY_2).expect("valid test key 2");

        // 1. First connection trusts and persists key1
        let mut handler1 = SshClientHandler::new_with_path(
            "bastion.example.com:22".to_string(),
            known_hosts_path.clone(),
        );
        assert!(handler1.check_server_key(&key1).await.unwrap());
        assert!(known_hosts_path.exists(), "known_hosts.toml should be created on first connect");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&known_hosts_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "known_hosts.toml should be mode 0600");
        }

        // 2. Subsequent connection with matching key1 succeeds
        let mut handler2 = SshClientHandler::new_with_path(
            "bastion.example.com:22".to_string(),
            known_hosts_path.clone(),
        );
        assert!(handler2.check_server_key(&key1).await.unwrap());
        assert!(handler2.take_verification_error().is_none());

        // 3. Subsequent connection with different key2 is rejected
        let mut handler3 = SshClientHandler::new_with_path(
            "bastion.example.com:22".to_string(),
            known_hosts_path.clone(),
        );
        assert!(
            !handler3.check_server_key(&key2).await.unwrap(),
            "Mismatched SSH server public key must be rejected"
        );
        let err_msg = handler3
            .take_verification_error()
            .expect("Verification error should be recorded on key mismatch");
        assert!(
            err_msg.contains("SSH host key mismatch for 'bastion.example.com:22'"),
            "Unexpected error message: {}",
            err_msg
        );

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[tokio::test]
    async fn test_ssh_known_hosts_rejects_corrupt_file_instead_of_overwriting() {
        let tmp_dir = std::env::temp_dir().join(format!("dbased_ssh_corrupt_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let known_hosts_path = tmp_dir.join("known_hosts.toml");
        std::fs::write(&known_hosts_path, "not valid toml = [[[").unwrap();

        let key1 = PublicKey::from_openssh(TEST_KEY_1).expect("valid test key 1");
        let mut handler = SshClientHandler::new_with_path(
            "bastion.example.com:22".to_string(),
            known_hosts_path.clone(),
        );
        assert!(
            !handler.check_server_key(&key1).await.unwrap(),
            "Corrupt known_hosts.toml must not silently trust keys"
        );
        assert!(handler.take_verification_error().is_some());
        assert_eq!(
            std::fs::read_to_string(&known_hosts_path).unwrap(),
            "not valid toml = [[["
        );

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}

