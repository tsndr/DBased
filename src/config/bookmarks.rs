use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbEngine {
    MySql,
    Postgres,
}

impl DbEngine {
    pub fn default_port(&self) -> u16 {
        match self {
            DbEngine::MySql => 3306,
            DbEngine::Postgres => 5432,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            DbEngine::MySql => "MySQL",
            DbEngine::Postgres => "PostgreSQL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SshAuthMethod {
    Password,
    PrivateKey { key_path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 22,
            username: String::new(),
            auth_method: SshAuthMethod::Password,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionBookmark {
    pub id: String,
    pub name: String,
    pub engine: DbEngine,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub default_database: String,
    pub ssh: SshConfig,
}

impl Default for ConnectionBookmark {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: "New Connection".to_string(),
            engine: DbEngine::Postgres,
            host: "127.0.0.1".to_string(),
            port: 5432,
            username: "postgres".to_string(),
            default_database: "postgres".to_string(),
            ssh: SshConfig::default(),
        }
    }
}

/// Complete connection profile including runtime credentials (fetched from keyring or entered for Quick Connect).
#[derive(Debug, Clone)]
pub struct ConnectionProfile {
    pub bookmark: ConnectionBookmark,
    pub db_password: String,
    pub ssh_password_or_passphrase: String,
}

impl ConnectionProfile {
    /// Parse a connection URL (`mysql://user:pass@host:port/db` or `postgres://user:pass@host:port/db`)
    /// for Quick Connect.
    pub fn from_url(raw_url: &str) -> Result<Self> {
        let parsed = Url::parse(raw_url.trim()).context("Invalid connection URL format")?;
        let engine = match parsed.scheme() {
            "mysql" | "mariadb" => DbEngine::MySql,
            "postgres" | "postgresql" => DbEngine::Postgres,
            other => return Err(anyhow!("Unsupported URL scheme '{}'. Use mysql:// or postgres://", other)),
        };

        let host = parsed
            .host_str()
            .unwrap_or("127.0.0.1")
            .to_string();
        let port = parsed.port().unwrap_or_else(|| engine.default_port());
        let username = if parsed.username().is_empty() {
            match engine {
                DbEngine::MySql => "root".to_string(),
                DbEngine::Postgres => "postgres".to_string(),
            }
        } else {
            urlencoding::decode(parsed.username()).unwrap_or_else(|_| parsed.username().into()).to_string()
        };
        let db_password = parsed
            .password()
            .map(|p| urlencoding::decode(p).unwrap_or_else(|_| p.to_string()))
            .unwrap_or_default();
        let raw_db = parsed.path().trim_start_matches('/');
        let default_database = urlencoding::decode(raw_db).unwrap_or_else(|_| raw_db.to_string());

        let bookmark = ConnectionBookmark {
            id: Uuid::new_v4().to_string(),
            name: format!("{} ({}@{})", engine.display_name(), username, host),
            engine,
            host,
            port,
            username,
            default_database,
            ssh: SshConfig::default(),
        };

        Ok(Self {
            bookmark,
            db_password,
            ssh_password_or_passphrase: String::new(),
        })
    }
}

mod urlencoding {
    pub fn decode(input: &str) -> Result<String, ()> {
        let mut out = Vec::with_capacity(input.len());
        let bytes = input.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                    out.push((h << 4) | l);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).map_err(|_| ())
    }

    fn from_hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BookmarkFile {
    #[serde(default)]
    bookmarks: Vec<ConnectionBookmark>,
}

pub struct BookmarkStore {
    path: PathBuf,
}

impl BookmarkStore {
    pub fn new() -> Self {
        let path = if let Some(proj_dirs) = ProjectDirs::from("dev", "tsndr", "dbased") {
            proj_dirs.config_dir().join("bookmarks.toml")
        } else {
            PathBuf::from("bookmarks.toml")
        };
        Self { path }
    }

    #[cfg(test)]
    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load_all(&self) -> Result<Vec<ConnectionBookmark>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to read bookmarks from {:?}", self.path))?;
        let file: BookmarkFile = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML in {:?}", self.path))?;
        Ok(file.bookmarks)
    }

    pub fn save_all(&self, bookmarks: &[ConnectionBookmark]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory {:?}", parent))?;
        }
        let file = BookmarkFile {
            bookmarks: bookmarks.to_vec(),
        };
        let serialized = toml::to_string_pretty(&file)
            .context("Failed to serialize bookmarks to TOML")?;
        fs::write(&self.path, serialized)
            .with_context(|| format!("Failed to write bookmarks to {:?}", self.path))?;
        Ok(())
    }

    pub fn upsert(&self, bookmark: ConnectionBookmark) -> Result<Vec<ConnectionBookmark>> {
        let mut all = self.load_all().unwrap_or_default();
        if let Some(existing) = all.iter_mut().find(|b| b.id == bookmark.id) {
            *existing = bookmark;
        } else {
            all.push(bookmark);
        }
        self.save_all(&all)?;
        Ok(all)
    }

    pub fn delete(&self, id: &str) -> Result<Vec<ConnectionBookmark>> {
        let mut all = self.load_all().unwrap_or_default();
        all.retain(|b| b.id != id);
        self.save_all(&all)?;
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_parse_quick_connect_urls() {
        let pg = ConnectionProfile::from_url("postgres://alice:s3cret@db.example.com:5433/analytics").unwrap();
        assert_eq!(pg.bookmark.engine, DbEngine::Postgres);
        assert_eq!(pg.bookmark.username, "alice");
        assert_eq!(pg.db_password, "s3cret");
        assert_eq!(pg.bookmark.host, "db.example.com");
        assert_eq!(pg.bookmark.port, 5433);
        assert_eq!(pg.bookmark.default_database, "analytics");

        let my = ConnectionProfile::from_url("mysql://root:pass@127.0.0.1/shop").unwrap();
        assert_eq!(my.bookmark.engine, DbEngine::MySql);
        assert_eq!(my.bookmark.port, 3306);
        assert_eq!(my.bookmark.default_database, "shop");

        // Percent-encoded credentials and database name, plus scheme aliases and default username
        let encoded = ConnectionProfile::from_url(
            "postgresql://user%40domain:p%40ss%3A1%2F2@db.internal/my%20db",
        )
        .unwrap();
        assert_eq!(encoded.bookmark.engine, DbEngine::Postgres);
        assert_eq!(encoded.bookmark.username, "user@domain");
        assert_eq!(encoded.db_password, "p@ss:1/2");
        assert_eq!(encoded.bookmark.default_database, "my db");

        let maria_default_user = ConnectionProfile::from_url("mariadb://localhost").unwrap();
        assert_eq!(maria_default_user.bookmark.engine, DbEngine::MySql);
        assert_eq!(maria_default_user.bookmark.username, "root");
        assert_eq!(maria_default_user.bookmark.port, 3306);
        assert_eq!(maria_default_user.bookmark.default_database, "");

        assert!(ConnectionProfile::from_url("sqlite:///tmp/test.db").is_err());
    }

    #[test]
    fn test_bookmark_store_roundtrip() {
        let temp_path = env::temp_dir().join(format!("dbased_test_{}.toml", Uuid::new_v4()));
        let store = BookmarkStore::with_path(temp_path.clone());

        let mut bm = ConnectionBookmark::default();
        bm.name = "Production Postgres".to_string();
        bm.ssh.enabled = true;
        bm.ssh.host = "bastion.internal".to_string();

        let list = store.upsert(bm.clone()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Production Postgres");
        assert!(list[0].ssh.enabled);

        let after_del = store.delete(&bm.id).unwrap();
        assert!(after_del.is_empty());
        let _ = fs::remove_file(temp_path);
    }
}
