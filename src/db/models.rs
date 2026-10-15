use crate::config::bookmarks::{ConnectionProfile, DbEngine};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    pub is_view: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub ordinal: u32,
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub is_primary_key: bool,
    pub default_value: Option<String>,
    pub extra: String,
}

impl ColumnInfo {
    pub fn is_long_text(&self) -> bool {
        is_long_text_data_type(&self.data_type)
    }

    pub fn enum_values(&self) -> Option<Vec<String>> {
        parse_enum_data_type(&self.data_type).or_else(|| parse_enum_data_type(&self.extra))
    }
}

/// If `data_type` represents an `ENUM(...)` column definition, returns the list of
/// allowed enum values in declaration order.
pub fn parse_enum_data_type(data_type: &str) -> Option<Vec<String>> {
    let trimmed = data_type.trim();
    let (prefix, rest) = trimmed.split_once('(')?;
    if !prefix.trim().eq_ignore_ascii_case("enum") {
        return None;
    }
    let (inner, _) = rest.rsplit_once(')')?;
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }

    let mut values = Vec::new();
    let mut chars = inner.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() || ch == ',' {
            chars.next();
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            chars.next();
            let mut val = String::new();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(escaped) = chars.next() {
                        val.push(escaped);
                    }
                } else if c == quote {
                    if chars.peek() == Some(&quote) {
                        chars.next();
                        val.push(quote);
                    } else {
                        break;
                    }
                } else {
                    val.push(c);
                }
            }
            values.push(val);
            while let Some(&next_ch) = chars.peek() {
                if next_ch == ',' {
                    chars.next();
                    break;
                } else if next_ch.is_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }
        } else {
            let mut val = String::new();
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    chars.next();
                    break;
                }
                val.push(c);
                chars.next();
            }
            let token = val.trim();
            if !token.is_empty() {
                values.push(token.to_string());
            }
        }
    }

    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// Returns true if the SQL column data type represents a long or multiline text type
/// (e.g. `TEXT`, `TINYTEXT`, `MEDIUMTEXT`, `LONGTEXT`, `CITEXT`, `NTEXT`, `CLOB`,
/// `JSON`, `JSONB`, `XML`, unbounded `VARCHAR`, or `VARCHAR(N)` with `N > 255`).
pub fn is_long_text_data_type(data_type: &str) -> bool {
    let dt = data_type.trim().to_ascii_lowercase();
    if dt.is_empty() || dt.starts_with("enum(") || dt.starts_with("set(") {
        return false;
    }
    if dt.contains("text")
        || dt.contains("clob")
        || dt == "json"
        || dt == "jsonb"
        || dt == "xml"
        || dt == "long varchar"
    {
        return true;
    }
    if dt == "varchar" || dt == "nvarchar" || dt == "character varying" {
        return true;
    }
    for prefix in [
        "varchar(",
        "nvarchar(",
        "character varying(",
        "char(",
        "bpchar(",
    ] {
        if let Some(rest) = dt.strip_prefix(prefix) {
            let inner = rest
                .split(')')
                .next()
                .unwrap_or("")
                .trim();
            if inner.eq_ignore_ascii_case("max") {
                return true;
            }
            if let Ok(len) = inner.parse::<usize>() {
                if len > 255 {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub default_value: Option<String>,
    pub extra: Option<String>,
    pub is_primary_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnAlterOp {
    Add(ColumnSpec),
    Modify {
        old_name: String,
        spec: ColumnSpec,
    },
    Drop {
        name: String,
    },
    Reorder {
        column: ColumnInfo,
        after_column: Option<String>,
        new_order: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
    pub is_primary: bool,
    pub index_type: String,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSpec {
    pub name: String,
    pub columns: Vec<String>,
    pub index_type: String,
    pub method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexAlterOp {
    Add(IndexSpec),
    Modify {
        old_name: String,
        old_is_primary: bool,
        spec: IndexSpec,
    },
    Drop {
        name: String,
        is_primary: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn as_sql(&self) -> &'static str {
        match self {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContentQuery {
    pub search_text: String,
    pub sort_column: Option<String>,
    pub sort_direction: SortDirection,
    pub limit: u32,
    pub offset: u32,
}

impl Default for ContentQuery {
    fn default() -> Self {
        Self {
            search_text: String::new(),
            sort_column: None,
            sort_direction: SortDirection::Asc,
            limit: 100,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbValue {
    Null,
    Text(String),
}

impl DbValue {
    pub fn display_text(&self) -> &str {
        match self {
            DbValue::Null => "NULL",
            DbValue::Text(s) => s.as_str(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, DbValue::Null)
    }

    pub fn from_editor_str(s: &str) -> Self {
        if s.eq_ignore_ascii_case("NULL") {
            DbValue::Null
        } else {
            DbValue::Text(s.to_string())
        }
    }
}

/// Identifies a row uniquely by its primary key column(s) and value(s).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrimaryKeyValue {
    pub columns: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableRowData {
    pub pk: Option<PrimaryKeyValue>,
    pub values: Vec<DbValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TablePage {
    pub database: String,
    pub table: String,
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<TableRowData>,
    pub total_rows: u64,
    pub page_limit: u32,
    pub page_offset: u32,
}

#[derive(Debug, Clone)]
pub struct RowUpdate {
    pub pk: PrimaryKeyValue,
    pub changes: Vec<(String, DbValue)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeyInfo {
    pub constraint_name: String,
    pub column_name: String,
    pub referenced_table: String,
    pub referenced_column: String,
    pub update_rule: String,
    pub delete_rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewForeignKey {
    pub constraint_name: String,
    pub column_name: String,
    pub referenced_table: String,
    pub referenced_column: String,
    pub on_update: String,
    pub on_delete: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawQueryResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<DbValue>>,
    pub execution_time_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RawQueryOutcome {
    Rows(RawQueryResultSet),
    Executed {
        rows_affected: u64,
        execution_time_ms: u128,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateDatabaseSpec {
    pub name: String,
    /// MySQL: CHARACTER SET (e.g. "utf8mb4"); PostgreSQL: ENCODING (e.g. "UTF8")
    pub charset_or_encoding: Option<String>,
    /// MySQL: COLLATE (e.g. "utf8mb4_unicode_ci")
    pub collation: Option<String>,
    /// PostgreSQL: TEMPLATE (e.g. "template0", "template1")
    pub template: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbUser {
    pub username: String,
    pub host: Option<String>,
    pub global_privileges: Vec<String>,
    pub has_grant_option: bool,
}

impl DbUser {
    pub fn display_identity(&self) -> String {
        match &self.host {
            Some(h) if !h.is_empty() => format!("'{}'@'{}'", self.username, h),
            _ => self.username.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabasePermission {
    pub username: String,
    pub host: Option<String>,
    pub database: String,
    pub privileges: Vec<String>,
    pub has_grant_option: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateUserSpec {
    pub username: String,
    pub host: Option<String>,
    pub password: String,
    pub global_privileges: Vec<String>,
    pub with_grant_option: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateUserSpec {
    pub old_username: String,
    pub old_host: Option<String>,
    pub new_username: String,
    pub new_host: Option<String>,
    pub new_password: Option<String>,
    pub global_privileges: Vec<String>,
    pub with_grant_option: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbPermissionSpec {
    pub username: String,
    pub host: Option<String>,
    pub database: String,
    pub privileges: Vec<String>,
    pub with_grant_option: bool,
}

pub const MYSQL_GLOBAL_PRIVILEGES: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "CREATE",
    "ALTER",
    "DROP",
    "INDEX",
    "REFERENCES",
    "CREATE VIEW",
    "SHOW VIEW",
    "CREATE ROUTINE",
    "ALTER ROUTINE",
    "EXECUTE",
    "TRIGGER",
    "EVENT",
    "LOCK TABLES",
    "CREATE USER",
    "PROCESS",
    "RELOAD",
    "SHOW DATABASES",
    "SUPER",
];

pub const MYSQL_DB_PRIVILEGES: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "CREATE",
    "ALTER",
    "DROP",
    "INDEX",
    "REFERENCES",
    "CREATE VIEW",
    "SHOW VIEW",
    "CREATE ROUTINE",
    "ALTER ROUTINE",
    "EXECUTE",
    "TRIGGER",
    "EVENT",
    "LOCK TABLES",
];

pub const PG_GLOBAL_PRIVILEGES: &[&str] = &[
    "LOGIN",
    "SUPERUSER",
    "CREATEDB",
    "CREATEROLE",
    "REPLICATION",
    "BYPASSRLS",
];

pub const PG_DB_OBJECT_PRIVILEGES: &[&str] = &["CONNECT", "CREATE", "TEMPORARY"];

pub const PG_TABLE_PRIVILEGES: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "TRUNCATE",
    "REFERENCES",
    "TRIGGER",
];

pub const PG_DB_PRIVILEGES: &[&str] = &[
    "CONNECT",
    "CREATE",
    "TEMPORARY",
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "TRUNCATE",
    "REFERENCES",
    "TRIGGER",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionPreset {
    None,
    ReadOnly,
    ReadWrite,
    FullAdmin,
    Custom,
}

impl PermissionPreset {
    pub const ALL_GLOBAL: &[PermissionPreset] = &[
        PermissionPreset::None,
        PermissionPreset::ReadOnly,
        PermissionPreset::ReadWrite,
        PermissionPreset::FullAdmin,
        PermissionPreset::Custom,
    ];

    pub const ALL_DB: &[PermissionPreset] = &[
        PermissionPreset::ReadOnly,
        PermissionPreset::ReadWrite,
        PermissionPreset::FullAdmin,
        PermissionPreset::Custom,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            PermissionPreset::None => "None",
            PermissionPreset::ReadOnly => "Read-Only",
            PermissionPreset::ReadWrite => "Read/Write",
            PermissionPreset::FullAdmin => "Full Admin",
            PermissionPreset::Custom => "Custom",
        }
    }

    pub fn global_privileges_for_engine(engine: DbEngine) -> &'static [&'static str] {
        match engine {
            DbEngine::MySql => MYSQL_GLOBAL_PRIVILEGES,
            DbEngine::Postgres => PG_GLOBAL_PRIVILEGES,
        }
    }

    pub fn db_privileges_for_engine(engine: DbEngine) -> &'static [&'static str] {
        match engine {
            DbEngine::MySql => MYSQL_DB_PRIVILEGES,
            DbEngine::Postgres => PG_DB_PRIVILEGES,
        }
    }

    pub fn preset_global_privileges(engine: DbEngine, preset: PermissionPreset) -> Vec<String> {
        let list: &[&str] = match (engine, preset) {
            (DbEngine::MySql, PermissionPreset::None) => &[],
            (DbEngine::MySql, PermissionPreset::ReadOnly) => &["SELECT", "SHOW VIEW", "PROCESS"],
            (DbEngine::MySql, PermissionPreset::ReadWrite) => &[
                "SELECT",
                "INSERT",
                "UPDATE",
                "DELETE",
                "CREATE",
                "ALTER",
                "INDEX",
                "CREATE VIEW",
                "SHOW VIEW",
                "EXECUTE",
            ],
            (DbEngine::MySql, PermissionPreset::FullAdmin) => MYSQL_GLOBAL_PRIVILEGES,
            (DbEngine::MySql, PermissionPreset::Custom) => &[],
            (DbEngine::Postgres, PermissionPreset::None) => &["LOGIN"],
            (DbEngine::Postgres, PermissionPreset::ReadOnly) => &["LOGIN"],
            (DbEngine::Postgres, PermissionPreset::ReadWrite) => &["LOGIN", "CREATEDB"],
            (DbEngine::Postgres, PermissionPreset::FullAdmin) => PG_GLOBAL_PRIVILEGES,
            (DbEngine::Postgres, PermissionPreset::Custom) => &[],
        };
        list.iter().map(|s| (*s).to_string()).collect()
    }

    pub fn preset_db_privileges(engine: DbEngine, preset: PermissionPreset) -> Vec<String> {
        let list: &[&str] = match (engine, preset) {
            (DbEngine::MySql, PermissionPreset::None) => &[],
            (DbEngine::MySql, PermissionPreset::ReadOnly) => &["SELECT", "SHOW VIEW"],
            (DbEngine::MySql, PermissionPreset::ReadWrite) => &[
                "SELECT",
                "INSERT",
                "UPDATE",
                "DELETE",
                "CREATE",
                "ALTER",
                "INDEX",
                "REFERENCES",
                "CREATE VIEW",
                "SHOW VIEW",
                "EXECUTE",
                "TRIGGER",
                "LOCK TABLES",
            ],
            (DbEngine::MySql, PermissionPreset::FullAdmin) => MYSQL_DB_PRIVILEGES,
            (DbEngine::MySql, PermissionPreset::Custom) => &[],
            (DbEngine::Postgres, PermissionPreset::None) => &[],
            (DbEngine::Postgres, PermissionPreset::ReadOnly) => &["CONNECT", "SELECT"],
            (DbEngine::Postgres, PermissionPreset::ReadWrite) => &[
                "CONNECT",
                "TEMPORARY",
                "SELECT",
                "INSERT",
                "UPDATE",
                "DELETE",
                "TRUNCATE",
                "REFERENCES",
                "TRIGGER",
            ],
            (DbEngine::Postgres, PermissionPreset::FullAdmin) => PG_DB_PRIVILEGES,
            (DbEngine::Postgres, PermissionPreset::Custom) => &[],
        };
        list.iter().map(|s| (*s).to_string()).collect()
    }

    pub fn infer_global_preset(engine: DbEngine, active_privs: &[String]) -> PermissionPreset {
        let norm_active = normalize_priv_set(active_privs);
        if norm_active.is_empty() {
            return PermissionPreset::None;
        }
        if norm_active.iter().any(|p| p == "ALL PRIVILEGES" || p == "ALL") {
            return PermissionPreset::FullAdmin;
        }
        if engine == DbEngine::Postgres && norm_active.iter().any(|p| p == "SUPERUSER") {
            let full = normalize_priv_set(&Self::preset_global_privileges(
                engine,
                PermissionPreset::FullAdmin,
            ));
            if norm_active == full
                || norm_active
                    == normalize_priv_set(&[
                        "LOGIN".to_string(),
                        "SUPERUSER".to_string(),
                        "CREATEDB".to_string(),
                        "CREATEROLE".to_string(),
                    ])
                || norm_active == normalize_priv_set(&["LOGIN".to_string(), "SUPERUSER".to_string()])
            {
                return PermissionPreset::FullAdmin;
            }
        }
        for preset in [
            PermissionPreset::FullAdmin,
            PermissionPreset::ReadWrite,
            PermissionPreset::ReadOnly,
            PermissionPreset::None,
        ] {
            let expected =
                normalize_priv_set(&Self::preset_global_privileges(engine, preset));
            if norm_active == expected {
                return preset;
            }
        }
        PermissionPreset::Custom
    }

    pub fn infer_db_preset(engine: DbEngine, active_privs: &[String]) -> PermissionPreset {
        let norm_active = normalize_priv_set(active_privs);
        if norm_active.is_empty() {
            return PermissionPreset::None;
        }
        if norm_active.iter().any(|p| p == "ALL PRIVILEGES" || p == "ALL") {
            return PermissionPreset::FullAdmin;
        }
        for preset in [
            PermissionPreset::FullAdmin,
            PermissionPreset::ReadWrite,
            PermissionPreset::ReadOnly,
        ] {
            let expected = normalize_priv_set(&Self::preset_db_privileges(engine, preset));
            if norm_active == expected {
                return preset;
            }
        }
        PermissionPreset::Custom
    }
}

fn normalize_priv_set(privs: &[String]) -> Vec<String> {
    let mut v: Vec<String> = privs
        .iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty() && s != "USAGE")
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Commands sent from the GTK4 UI thread to the background Tokio database worker.
#[derive(Debug, Clone)]
pub enum BackendCommand {
    TestConnection(ConnectionProfile),
    Connect(ConnectionProfile),
    Disconnect,
    Refresh {
        current_database: Option<String>,
    },
    CreateDatabase(CreateDatabaseSpec),
    SelectDatabase(String),
    SelectTable {
        database: String,
        table: String,
        content_query: ContentQuery,
    },
    RefreshStructure {
        database: String,
        table: String,
    },
    AlterColumn {
        database: String,
        table: String,
        op: ColumnAlterOp,
    },
    AlterIndex {
        database: String,
        table: String,
        op: IndexAlterOp,
    },
    FetchTableContent {
        database: String,
        table: String,
        query: ContentQuery,
    },
    CommitContentChanges {
        database: String,
        table: String,
        updates: Vec<RowUpdate>,
        inserts: Vec<Vec<(String, DbValue)>>,
        query_for_refresh: ContentQuery,
    },
    DeleteRows {
        database: String,
        table: String,
        pks: Vec<PrimaryKeyValue>,
        query_for_refresh: ContentQuery,
    },
    RefreshRelations {
        database: String,
        table: String,
    },
    AddRelation {
        database: String,
        table: String,
        fk: NewForeignKey,
    },
    DropRelation {
        database: String,
        table: String,
        constraint_name: String,
    },
    FetchTargetTableColumns {
        database: String,
        table: String,
    },
    ExecuteRawQuery {
        database: String,
        sql: String,
    },
    RefreshUsers {
        selected_user: Option<(String, Option<String>)>,
    },
    SelectUserForPermissions {
        username: String,
        host: Option<String>,
    },
    CreateUser(CreateUserSpec),
    UpdateUser(UpdateUserSpec),
    DropUser {
        username: String,
        host: Option<String>,
    },
    SetDatabasePermissions(DbPermissionSpec),
    RevokeDatabasePermissions {
        username: String,
        host: Option<String>,
        database: String,
    },
}

/// Events sent from the background Tokio database worker back to the GTK4 UI thread.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    ConnectionTestResult(Result<String, String>),
    Connected {
        connection_name: String,
        engine: DbEngine,
        engine_label: String,
        databases: Vec<String>,
        selected_database: Option<String>,
        tables: Vec<TableInfo>,
    },
    Disconnected,
    Refreshed {
        databases: Vec<String>,
        selected_database: Option<String>,
        tables: Vec<TableInfo>,
    },
    DatabaseCreated {
        databases: Vec<String>,
        selected_database: String,
        tables: Vec<TableInfo>,
        toast: String,
    },
    DatabaseSelected {
        database: String,
        tables: Vec<TableInfo>,
    },
    TableLoaded {
        database: String,
        table: String,
        all_tables: Vec<String>,
        structure: Vec<ColumnInfo>,
        indexes: Vec<IndexInfo>,
        content: TablePage,
        relations: Vec<ForeignKeyInfo>,
        content_query: ContentQuery,
    },
    StructureUpdated {
        database: String,
        table: String,
        structure: Vec<ColumnInfo>,
        indexes: Vec<IndexInfo>,
        toast: Option<String>,
    },
    ContentLoaded {
        page: TablePage,
        toast: Option<String>,
    },
    RelationsUpdated {
        database: String,
        table: String,
        relations: Vec<ForeignKeyInfo>,
        toast: Option<String>,
    },
    TargetTableColumnsLoaded {
        table: String,
        columns: Vec<String>,
    },
    RawQueryFinished(Result<RawQueryOutcome, String>),
    UsersLoaded {
        users: Vec<DbUser>,
        selected_user: Option<(String, Option<String>)>,
        db_permissions: Vec<DatabasePermission>,
        toast: Option<String>,
    },
    UserDbPermissionsLoaded {
        username: String,
        host: Option<String>,
        db_permissions: Vec<DatabasePermission>,
        toast: Option<String>,
    },
    Error(String),
    InfoToast(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_preset_roundtrip_and_inference() {
        for engine in [DbEngine::MySql, DbEngine::Postgres] {
            for preset in [
                PermissionPreset::ReadOnly,
                PermissionPreset::ReadWrite,
                PermissionPreset::FullAdmin,
            ] {
                let db_privs = PermissionPreset::preset_db_privileges(engine, preset);
                assert_eq!(PermissionPreset::infer_db_preset(engine, &db_privs), preset);
            }
        }

        let mysql_rw_global =
            PermissionPreset::preset_global_privileges(DbEngine::MySql, PermissionPreset::ReadWrite);
        assert_eq!(
            PermissionPreset::infer_global_preset(DbEngine::MySql, &mysql_rw_global),
            PermissionPreset::ReadWrite
        );

        let pg_admin_global = vec!["LOGIN".to_string(), "SUPERUSER".to_string()];
        assert_eq!(
            PermissionPreset::infer_global_preset(DbEngine::Postgres, &pg_admin_global),
            PermissionPreset::FullAdmin
        );

        let custom_privs = vec!["SELECT".to_string(), "DELETE".to_string()];
        assert_eq!(
            PermissionPreset::infer_db_preset(DbEngine::MySql, &custom_privs),
            PermissionPreset::Custom
        );
    }
}
