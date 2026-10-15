use anyhow::{Result, anyhow};
use directories::ProjectDirs;
use oo7::Keyring;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

fn secret_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("Failed to initialize Tokio runtime for SecretStore")
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredSecrets {
    pub db_password: String,
    pub ssh_secret: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FallbackSecretsFile {
    #[serde(default)]
    entries: HashMap<String, StoredSecrets>,
}

pub struct SecretStore;

impl SecretStore {
    fn fallback_path() -> PathBuf {
        if let Some(proj_dirs) = ProjectDirs::from("io", "tsndr", "dbased") {
            proj_dirs.config_dir().join(".credentials.toml")
        } else {
            PathBuf::from(".dbased_credentials.toml")
        }
    }

    pub async fn save_secrets(bookmark_id: &str, db_password: &str, ssh_secret: &str) -> Result<()> {
        let bm_id = bookmark_id.to_string();
        let db_pass = db_password.to_string();
        let ssh_sec = ssh_secret.to_string();

        secret_runtime()
            .spawn(async move {
                // Attempt OS keyring first via oo7
                let keyring_ok = match Keyring::new().await {
                    Ok(keyring) => {
                        let db_attrs = HashMap::from([
                            ("application", "io.github.tsndr.dbased"),
                            ("bookmark_id", bm_id.as_str()),
                            ("secret_type", "db_password"),
                        ]);
                        let ssh_attrs = HashMap::from([
                            ("application", "io.github.tsndr.dbased"),
                            ("bookmark_id", bm_id.as_str()),
                            ("secret_type", "ssh_secret"),
                        ]);
                        let r1 = keyring
                            .create_item(
                                &format!("DBased DB Password ({})", bm_id),
                                &db_attrs,
                                db_pass.as_bytes(),
                                true,
                            )
                            .await;
                        let r2 = keyring
                            .create_item(
                                &format!("DBased SSH Secret ({})", bm_id),
                                &ssh_attrs,
                                ssh_sec.as_bytes(),
                                true,
                            )
                            .await;
                        r1.is_ok() && r2.is_ok()
                    }
                    Err(_) => false,
                };

                if !keyring_ok {
                    Self::save_fallback(&bm_id, &db_pass, &ssh_sec)?;
                } else {
                    let _ = Self::save_fallback(&bm_id, &db_pass, &ssh_sec);
                }
                Ok(())
            })
            .await
            .map_err(|e| anyhow!("SecretStore task failed: {}", e))?
    }

    pub async fn load_secrets(bookmark_id: &str) -> StoredSecrets {
        let bm_id = bookmark_id.to_string();
        secret_runtime()
            .spawn(async move {
                if let Ok(keyring) = Keyring::new().await {
                    let db_attrs = HashMap::from([
                        ("application", "io.github.tsndr.dbased"),
                        ("bookmark_id", bm_id.as_str()),
                        ("secret_type", "db_password"),
                    ]);
                    let ssh_attrs = HashMap::from([
                        ("application", "io.github.tsndr.dbased"),
                        ("bookmark_id", bm_id.as_str()),
                        ("secret_type", "ssh_secret"),
                    ]);

                    let db_pass = match keyring.search_items(&db_attrs).await {
                        Ok(items) => {
                            if let Some(item) = items.first() {
                                if let Ok(secret) = item.secret().await {
                                    String::from_utf8(secret.to_vec()).ok()
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                        Err(_) => None,
                    };

                    let ssh_sec = match keyring.search_items(&ssh_attrs).await {
                        Ok(items) => {
                            if let Some(item) = items.first() {
                                if let Ok(secret) = item.secret().await {
                                    String::from_utf8(secret.to_vec()).ok()
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                        Err(_) => None,
                    };

                    if db_pass.is_some() || ssh_sec.is_some() {
                        return StoredSecrets {
                            db_password: db_pass.unwrap_or_default(),
                            ssh_secret: ssh_sec.unwrap_or_default(),
                        };
                    }
                }

                Self::load_fallback(&bm_id).unwrap_or_default()
            })
            .await
            .unwrap_or_default()
    }

    pub async fn delete_secrets(bookmark_id: &str) {
        let bm_id = bookmark_id.to_string();
        let _ = secret_runtime()
            .spawn(async move {
                if let Ok(keyring) = Keyring::new().await {
                    let attrs = HashMap::from([
                        ("application", "io.github.tsndr.dbased"),
                        ("bookmark_id", bm_id.as_str()),
                    ]);
                    let _ = keyring.delete(&attrs).await;
                }
                let _ = Self::delete_fallback(&bm_id);
            })
            .await;
    }

    fn save_fallback(bookmark_id: &str, db_password: &str, ssh_secret: &str) -> Result<()> {
        let path = Self::fallback_path();
        Self::save_fallback_at(&path, bookmark_id, db_password, ssh_secret)
    }

    fn save_fallback_at(
        path: &std::path::Path,
        bookmark_id: &str,
        db_password: &str,
        ssh_secret: &str,
    ) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file: FallbackSecretsFile = if path.exists() {
            let raw = fs::read_to_string(path).unwrap_or_default();
            toml::from_str(&raw).unwrap_or_default()
        } else {
            FallbackSecretsFile::default()
        };

        file.entries.insert(
            bookmark_id.to_string(),
            StoredSecrets {
                db_password: db_password.to_string(),
                ssh_secret: ssh_secret.to_string(),
            },
        );
        let serialized = toml::to_string_pretty(&file)?;
        fs::write(path, serialized)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn load_fallback(bookmark_id: &str) -> Option<StoredSecrets> {
        let path = Self::fallback_path();
        Self::load_fallback_at(&path, bookmark_id)
    }

    fn load_fallback_at(path: &std::path::Path, bookmark_id: &str) -> Option<StoredSecrets> {
        if !path.exists() {
            return None;
        }
        let raw = fs::read_to_string(path).ok()?;
        let mut file: FallbackSecretsFile = toml::from_str(&raw).ok()?;
        file.entries.remove(bookmark_id)
    }

    fn delete_fallback(bookmark_id: &str) -> Result<()> {
        let path = Self::fallback_path();
        Self::delete_fallback_at(&path, bookmark_id)
    }

    fn delete_fallback_at(path: &std::path::Path, bookmark_id: &str) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let raw = fs::read_to_string(path)?;
        let mut file: FallbackSecretsFile = toml::from_str(&raw).unwrap_or_default();
        file.entries.remove(bookmark_id);
        let serialized = toml::to_string_pretty(&file)?;
        fs::write(path, serialized)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_store_from_glib_context_without_tokio_reactor() {
        let ctx = gtk4::glib::MainContext::new();
        let test_id = format!("test-bm-{}", uuid::Uuid::new_v4());
        ctx.block_on(async {
            SecretStore::save_secrets(&test_id, "s3cr3t_db", "s3cr3t_ssh")
                .await
                .unwrap();
            let loaded = SecretStore::load_secrets(&test_id).await;
            assert_eq!(loaded.db_password, "s3cr3t_db");
            assert_eq!(loaded.ssh_secret, "s3cr3t_ssh");
            SecretStore::delete_secrets(&test_id).await;
        });
    }

    #[test]
    fn test_fallback_file_isolation_permissions_and_corrupt_toml_recovery() {
        let tmp_path = std::env::temp_dir().join(format!(
            "dbased_secrets_test_{}/secrets.toml",
            uuid::Uuid::new_v4()
        ));

        // 1. Save two bookmarks and verify isolation
        SecretStore::save_fallback_at(&tmp_path, "bm-1", "db_p1", "ssh_s1").unwrap();
        SecretStore::save_fallback_at(&tmp_path, "bm-2", "db_p2", "ssh_s2").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&tmp_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let s1 = SecretStore::load_fallback_at(&tmp_path, "bm-1").unwrap();
        assert_eq!(s1.db_password, "db_p1");
        assert_eq!(s1.ssh_secret, "ssh_s1");

        // 2. Delete bm-1 and verify bm-2 remains intact
        SecretStore::delete_fallback_at(&tmp_path, "bm-1").unwrap();
        assert!(SecretStore::load_fallback_at(&tmp_path, "bm-1").is_none());
        let s2 = SecretStore::load_fallback_at(&tmp_path, "bm-2").unwrap();
        assert_eq!(s2.db_password, "db_p2");

        // 3. Corrupt TOML file recovery on save
        fs::write(&tmp_path, "not valid [[[ toml").unwrap();
        assert!(SecretStore::load_fallback_at(&tmp_path, "bm-2").is_none());
        SecretStore::save_fallback_at(&tmp_path, "bm-3", "recovered_pw", "").unwrap();
        let s3 = SecretStore::load_fallback_at(&tmp_path, "bm-3").unwrap();
        assert_eq!(s3.db_password, "recovered_pw");

        if let Some(parent) = tmp_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

