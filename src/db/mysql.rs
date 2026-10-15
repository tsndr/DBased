use crate::db::driver::{
    DatabaseDriver, escape_sql_literal, is_row_returning_query, quote_mysql_ident,
};
use crate::db::models::*;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::{Column, Row, ValueRef};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

pub struct MySqlDriver {
    host: String,
    port: u16,
    username: String,
    password: String,
    active_pool: RwLock<(String, MySqlPool)>,
}

impl MySqlDriver {
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        default_database: &str,
    ) -> Result<Self> {
        let initial_db = default_database.trim();
        let pool = Self::create_pool(host, port, username, password, initial_db).await?;

        Ok(Self {
            host: host.to_string(),
            port,
            username: username.to_string(),
            password: password.to_string(),
            active_pool: RwLock::new((initial_db.to_string(), pool)),
        })
    }

    async fn create_pool(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        database: &str,
    ) -> Result<MySqlPool> {
        let mut opts = MySqlConnectOptions::new()
            .host(host)
            .port(port)
            .username(username)
            .password(password);

        if !database.trim().is_empty() {
            opts = opts.database(database.trim());
        }

        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(opts)
            .await
            .with_context(|| format!("Failed to connect to MySQL at {}:{}", host, port))?;
        Ok(pool)
    }

    async fn pool_for_db(&self, db_name: &str) -> Result<MySqlPool> {
        let target = db_name.trim();
        {
            let guard = self.active_pool.read().await;
            if target.is_empty() || guard.0 == target {
                return Ok(guard.1.clone());
            }
        }
        let mut write_guard = self.active_pool.write().await;
        if !target.is_empty() && write_guard.0 != target {
            let new_pool =
                Self::create_pool(&self.host, self.port, &self.username, &self.password, target)
                    .await?;
            *write_guard = (target.to_string(), new_pool);
        }
        Ok(write_guard.1.clone())
    }

    fn qualified_table(db_name: &str, table: &str) -> String {
        if db_name.is_empty() {
            quote_mysql_ident(table)
        } else {
            format!("{}.{}", quote_mysql_ident(db_name), quote_mysql_ident(table))
        }
    }

    fn build_pk_where_clause(pk: &PrimaryKeyValue) -> Result<String> {
        if pk.columns.is_empty() {
            return Err(anyhow!("Table has no primary key to uniquely identify the row"));
        }
        let parts: Vec<String> = pk
            .columns
            .iter()
            .map(|(col, val)| {
                format!(
                    "{} = {}",
                    quote_mysql_ident(col),
                    escape_sql_literal(&DbValue::Text(val.clone()))
                )
            })
            .collect();
        Ok(parts.join(" AND "))
    }

    fn format_column_spec(spec: &ColumnSpec, include_pk: bool) -> String {
        let mut parts = vec![quote_mysql_ident(&spec.name), spec.data_type.trim().to_string()];
        let cleaned_extra: Vec<&str> = spec
            .extra
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .filter(|tok| {
                !tok.eq_ignore_ascii_case("DEFAULT_GENERATED")
                    && !tok.eq_ignore_ascii_case("None")
            })
            .collect();
        let is_auto_increment = cleaned_extra
            .iter()
            .any(|tok| tok.eq_ignore_ascii_case("AUTO_INCREMENT"));
        if spec.is_nullable && !is_auto_increment {
            parts.push("NULL".to_string());
        } else {
            parts.push("NOT NULL".to_string());
        }
        if let Some(def) = &spec.default_value {
            let trimmed = def.trim();
            if !trimmed.is_empty() && !is_auto_increment {
                if trimmed.eq_ignore_ascii_case("NULL")
                    || trimmed.eq_ignore_ascii_case("CURRENT_TIMESTAMP")
                    || trimmed.eq_ignore_ascii_case("CURRENT_TIMESTAMP()")
                    || trimmed.eq_ignore_ascii_case("NOW()")
                    || trimmed.ends_with("()")
                    || (trimmed.starts_with('(') && trimmed.ends_with(')'))
                    || trimmed.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-')
                {
                    parts.push(format!("DEFAULT {}", trimmed));
                } else {
                    parts.push(format!(
                        "DEFAULT {}",
                        escape_sql_literal(&DbValue::Text(trimmed.to_string()))
                    ));
                }
            }
        }
        if !cleaned_extra.is_empty() {
            parts.push(cleaned_extra.join(" "));
        }
        if include_pk && spec.is_primary_key {
            parts.push("PRIMARY KEY".to_string());
        }
        parts.join(" ")
    }

    fn format_column_info_for_reorder(col: &ColumnInfo, after_column: Option<&str>) -> String {
        let mut parts = vec![quote_mysql_ident(&col.name), col.data_type.trim().to_string()];
        if col.is_nullable {
            parts.push("NULL".to_string());
        } else {
            parts.push("NOT NULL".to_string());
        }
        if let Some(def) = &col.default_value {
            let trimmed = def.trim();
            if !trimmed.is_empty() {
                let is_generated_def = col
                    .extra
                    .to_ascii_uppercase()
                    .contains("DEFAULT_GENERATED");
                if is_generated_def
                    || trimmed.eq_ignore_ascii_case("NULL")
                    || trimmed.eq_ignore_ascii_case("CURRENT_TIMESTAMP")
                    || trimmed.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-')
                {
                    parts.push(format!("DEFAULT {}", trimmed));
                } else {
                    parts.push(format!(
                        "DEFAULT {}",
                        escape_sql_literal(&DbValue::Text(trimmed.to_string()))
                    ));
                }
            }
        }
        let cleaned_extra: Vec<&str> = col
            .extra
            .split_whitespace()
            .filter(|tok| !tok.eq_ignore_ascii_case("DEFAULT_GENERATED"))
            .collect();
        if !cleaned_extra.is_empty() {
            parts.push(cleaned_extra.join(" "));
        }
        match after_column {
            Some(prev) => parts.push(format!("AFTER {}", quote_mysql_ident(prev))),
            None => parts.push("FIRST".to_string()),
        }
        parts.join(" ")
    }

    fn format_add_index_clause(spec: &IndexSpec) -> Result<String> {
        if spec.columns.is_empty() {
            return Err(anyhow!("At least one column must be selected for an index"));
        }
        let cols = spec
            .columns
            .iter()
            .map(|c| quote_mysql_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        let using_clause = match spec
            .method
            .as_deref()
            .map(|s| s.trim().to_ascii_uppercase())
        {
            Some(ref m) if m == "BTREE" || m == "HASH" => format!(" USING {}", m),
            _ => String::new(),
        };
        let idx_name = spec.name.trim();
        let name_part = if idx_name.is_empty() || idx_name.eq_ignore_ascii_case("PRIMARY") {
            String::new()
        } else {
            format!(" {}", quote_mysql_ident(idx_name))
        };

        let clause = match spec.index_type.trim().to_ascii_uppercase().as_str() {
            "PRIMARY" => format!("ADD PRIMARY KEY{} ({})", using_clause, cols),
            "UNIQUE" => format!("ADD UNIQUE INDEX{}{} ({})", name_part, using_clause, cols),
            "FULLTEXT" => format!("ADD FULLTEXT INDEX{} ({})", name_part, cols),
            "SPATIAL" => format!("ADD SPATIAL INDEX{} ({})", name_part, cols),
            _ => format!("ADD INDEX{}{} ({})", name_part, using_clause, cols),
        };
        Ok(clause)
    }

    fn format_drop_index_clause(name: &str, is_primary: bool) -> String {
        if is_primary || name.trim().eq_ignore_ascii_case("PRIMARY") {
            "DROP PRIMARY KEY".to_string()
        } else {
            format!("DROP INDEX {}", quote_mysql_ident(name.trim()))
        }
    }

    fn format_create_database_stmt(spec: &CreateDatabaseSpec) -> Result<String> {
        let name = spec.name.trim();
        if name.is_empty() {
            return Err(anyhow!("Database name cannot be empty"));
        }
        let mut parts = vec![format!("CREATE DATABASE {}", quote_mysql_ident(name))];
        if let Some(charset) = &spec.charset_or_encoding {
            let cs = charset.trim();
            if !cs.is_empty() && !cs.eq_ignore_ascii_case("Default") {
                if !cs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(anyhow!("Invalid character set '{}'", cs));
                }
                parts.push(format!("DEFAULT CHARACTER SET {}", cs));
            }
        }
        if let Some(collation) = &spec.collation {
            let col = collation.trim();
            if !col.is_empty() && !col.eq_ignore_ascii_case("Default") {
                if !col.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(anyhow!("Invalid collation '{}'", col));
                }
                parts.push(format!("DEFAULT COLLATE {}", col));
            }
        }
        Ok(parts.join(" "))
    }

    fn format_account(username: &str, host: Option<&str>) -> Result<String> {
        let user = username.trim();
        if user.is_empty() {
            return Err(anyhow!("Username cannot be empty"));
        }
        let h = host
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("%");
        Ok(format!(
            "{}@{}",
            escape_sql_literal(&DbValue::Text(user.to_string())),
            escape_sql_literal(&DbValue::Text(h.to_string()))
        ))
    }

    fn sanitize_privileges(privs: &[String], allowed: &[&str]) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for p in privs {
            let up = p.trim().to_ascii_uppercase();
            if up.is_empty() || up == "USAGE" {
                continue;
            }
            if up == "ALL PRIVILEGES" || up == "ALL" {
                return Ok(allowed.iter().map(|s| (*s).to_string()).collect());
            }
            if allowed.iter().any(|&a| a == up) {
                if !out.contains(&up) {
                    out.push(up);
                }
            } else {
                return Err(anyhow!("Unsupported MySQL privilege '{}'", p));
            }
        }
        Ok(out)
    }

    fn format_privilege_clause(privs: &[String], allowed: &[&str]) -> String {
        if privs.len() >= allowed.len()
            && allowed
                .iter()
                .all(|&a| privs.iter().any(|p| p.eq_ignore_ascii_case(a)))
        {
            "ALL PRIVILEGES".to_string()
        } else {
            privs.join(", ")
        }
    }

    fn format_create_user_stmts(spec: &CreateUserSpec) -> Result<Vec<String>> {
        let account = Self::format_account(&spec.username, spec.host.as_deref())?;
        let privs = Self::sanitize_privileges(&spec.global_privileges, MYSQL_GLOBAL_PRIVILEGES)?;
        let mut stmts = vec![format!(
            "CREATE USER {} IDENTIFIED BY {}",
            account,
            escape_sql_literal(&DbValue::Text(spec.password.clone()))
        )];

        if !privs.is_empty() {
            let priv_clause = Self::format_privilege_clause(&privs, MYSQL_GLOBAL_PRIVILEGES);
            let grant_opt = if spec.with_grant_option {
                " WITH GRANT OPTION"
            } else {
                ""
            };
            stmts.push(format!(
                "GRANT {} ON *.* TO {}{}",
                priv_clause, account, grant_opt
            ));
        }

        Ok(stmts)
    }

    fn format_update_user_stmts(spec: &UpdateUserSpec) -> Result<Vec<String>> {
        let old_account = Self::format_account(&spec.old_username, spec.old_host.as_deref())?;
        let new_account = Self::format_account(&spec.new_username, spec.new_host.as_deref())?;
        let privs = Self::sanitize_privileges(&spec.global_privileges, MYSQL_GLOBAL_PRIVILEGES)?;

        let mut stmts = Vec::new();
        if old_account != new_account {
            stmts.push(format!("RENAME USER {} TO {}", old_account, new_account));
        }
        if let Some(pwd) = &spec.new_password {
            if !pwd.is_empty() {
                stmts.push(format!(
                    "ALTER USER {} IDENTIFIED BY {}",
                    new_account,
                    escape_sql_literal(&DbValue::Text(pwd.clone()))
                ));
            }
        }

        stmts.push(format!(
            "REVOKE ALL PRIVILEGES ON *.* FROM {}",
            new_account
        ));
        stmts.push(format!("REVOKE GRANT OPTION ON *.* FROM {}", new_account));

        if !privs.is_empty() {
            let priv_clause = Self::format_privilege_clause(&privs, MYSQL_GLOBAL_PRIVILEGES);
            let grant_opt = if spec.with_grant_option {
                " WITH GRANT OPTION"
            } else {
                ""
            };
            stmts.push(format!(
                "GRANT {} ON *.* TO {}{}",
                priv_clause, new_account, grant_opt
            ));
        }

        Ok(stmts)
    }

    fn format_drop_user_stmt(username: &str, host: Option<&str>) -> Result<String> {
        let account = Self::format_account(username, host)?;
        Ok(format!("DROP USER {}", account))
    }

    fn format_set_db_permissions_stmts(spec: &DbPermissionSpec) -> Result<Vec<String>> {
        let account = Self::format_account(&spec.username, spec.host.as_deref())?;
        let db = spec.database.trim();
        if db.is_empty() {
            return Err(anyhow!("Database name cannot be empty"));
        }
        let privs = Self::sanitize_privileges(&spec.privileges, MYSQL_DB_PRIVILEGES)?;
        let quoted_db = quote_mysql_ident(db);

        let mut stmts = vec![
            format!("REVOKE ALL PRIVILEGES ON {}.* FROM {}", quoted_db, account),
            format!("REVOKE GRANT OPTION ON {}.* FROM {}", quoted_db, account),
        ];

        if !privs.is_empty() {
            let priv_clause = Self::format_privilege_clause(&privs, MYSQL_DB_PRIVILEGES);
            let grant_opt = if spec.with_grant_option {
                " WITH GRANT OPTION"
            } else {
                ""
            };
            stmts.push(format!(
                "GRANT {} ON {}.* TO {}{}",
                priv_clause, quoted_db, account, grant_opt
            ));
        }

        Ok(stmts)
    }

    fn format_revoke_db_permissions_stmts(
        username: &str,
        host: Option<&str>,
        database: &str,
    ) -> Result<Vec<String>> {
        let account = Self::format_account(username, host)?;
        let db = database.trim();
        if db.is_empty() {
            return Err(anyhow!("Database name cannot be empty"));
        }
        let quoted_db = quote_mysql_ident(db);
        Ok(vec![
            format!("REVOKE ALL PRIVILEGES ON {}.* FROM {}", quoted_db, account),
            format!("REVOKE GRANT OPTION ON {}.* FROM {}", quoted_db, account),
        ])
    }

    fn parse_mysql_grants(
        username: &str,
        host: &str,
        grant_lines: &[String],
    ) -> (Vec<String>, bool, Vec<DatabasePermission>) {
        use std::collections::BTreeMap;

        let mut global_privs = Vec::<String>::new();
        let mut global_grant_opt = false;
        let mut db_map: BTreeMap<String, (Vec<String>, bool)> = BTreeMap::new();

        for line in grant_lines {
            let trimmed = line.trim();
            let upper = trimmed.to_ascii_uppercase();
            if !upper.starts_with("GRANT ") {
                continue;
            }
            let Some(on_idx) = upper.find(" ON ") else {
                continue;
            };
            let privs_part = &trimmed[6..on_idx];
            let after_on = &trimmed[on_idx + 4..];
            let after_on_upper = after_on.to_ascii_uppercase();
            let Some(to_idx) = after_on_upper.find(" TO ") else {
                continue;
            };
            let target_part = after_on[..to_idx].trim();
            let tail_upper = &after_on_upper[to_idx + 4..];
            let has_grant_option = tail_upper.contains("WITH GRANT OPTION");

            let raw_tokens: Vec<String> = privs_part
                .split(',')
                .map(|t| t.trim().to_ascii_uppercase())
                .filter(|t| !t.is_empty() && t != "USAGE")
                .collect();

            if target_part == "*.*" {
                if has_grant_option {
                    global_grant_opt = true;
                }
                for tok in raw_tokens {
                    if tok == "ALL PRIVILEGES" || tok == "ALL" {
                        for &p in MYSQL_GLOBAL_PRIVILEGES {
                            let s = p.to_string();
                            if !global_privs.contains(&s) {
                                global_privs.push(s);
                            }
                        }
                    } else if MYSQL_GLOBAL_PRIVILEGES.contains(&tok.as_str())
                        && !global_privs.contains(&tok)
                    {
                        global_privs.push(tok);
                    }
                }
            } else if let Some(raw_db) = target_part.strip_suffix(".*") {
                let unquoted_db = raw_db
                    .trim()
                    .trim_matches(|c| c == '`' || c == '\'' || c == '"')
                    .replace("``", "`")
                    .replace("\\_", "_")
                    .replace("\\%", "%");
                if unquoted_db.is_empty() || unquoted_db == "*" {
                    continue;
                }
                let entry = db_map
                    .entry(unquoted_db)
                    .or_insert_with(|| (Vec::new(), false));
                if has_grant_option {
                    entry.1 = true;
                }
                for tok in raw_tokens {
                    if tok == "ALL PRIVILEGES" || tok == "ALL" {
                        for &p in MYSQL_DB_PRIVILEGES {
                            let s = p.to_string();
                            if !entry.0.contains(&s) {
                                entry.0.push(s);
                            }
                        }
                    } else if MYSQL_DB_PRIVILEGES.contains(&tok.as_str())
                        && !entry.0.contains(&tok)
                    {
                        entry.0.push(tok);
                    }
                }
            }
        }

        let db_permissions = db_map
            .into_iter()
            .filter(|(_, (privs, _))| !privs.is_empty())
            .map(|(database, (privileges, has_grant_option))| DatabasePermission {
                username: username.to_string(),
                host: Some(host.to_string()),
                database,
                privileges,
                has_grant_option,
            })
            .collect();

        (global_privs, global_grant_opt, db_permissions)
    }

    async fn fetch_grants_for_account(&self, pool: &MySqlPool, account: &str) -> Vec<String> {
        let sql = format!("SHOW GRANTS FOR {}", account);
        match sqlx::raw_sql(&sql).fetch_all(pool).await {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|r| match decode_mysql_cell(&r, 0) {
                    DbValue::Text(s) => Some(s),
                    DbValue::Null => None,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

fn decode_mysql_cell(row: &MySqlRow, idx: usize) -> DbValue {
    if let Ok(raw) = row.try_get_raw(idx) {
        if raw.is_null() {
            return DbValue::Null;
        }
    }
    if let Ok(opt_s) = row.try_get_unchecked::<Option<String>, _>(idx) {
        return match opt_s {
            Some(s) => DbValue::Text(s),
            None => DbValue::Null,
        };
    }
    if let Ok(Some(v)) = row.try_get::<Option<i64>, _>(idx) {
        return DbValue::Text(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<u64>, _>(idx) {
        return DbValue::Text(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<f64>, _>(idx) {
        return DbValue::Text(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<rust_decimal::Decimal>, _>(idx) {
        return DbValue::Text(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<bool>, _>(idx) {
        return DbValue::Text(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<chrono::NaiveDateTime>, _>(idx) {
        return DbValue::Text(v.format("%Y-%m-%d %H:%M:%S").to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx) {
        return DbValue::Text(v.to_rfc3339());
    }
    if let Ok(Some(v)) = row.try_get::<Option<chrono::NaiveDate>, _>(idx) {
        return DbValue::Text(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<chrono::NaiveTime>, _>(idx) {
        return DbValue::Text(v.to_string());
    }
    if let Ok(Some(bytes)) = row.try_get_unchecked::<Option<Vec<u8>>, _>(idx) {
        return match String::from_utf8(bytes) {
            Ok(s) => DbValue::Text(s),
            Err(e) => {
                let b = e.into_bytes();
                let hex: String = b.iter().take(64).map(|byte| format!("{:02x}", byte)).collect();
                DbValue::Text(format!("0x{}", hex))
            }
        };
    }
    DbValue::Text("<unprintable>".to_string())
}

#[async_trait]
impl DatabaseDriver for MySqlDriver {
    async fn list_databases(&self) -> Result<Vec<String>> {
        let pool = self.pool_for_db("").await?;
        let rows = match sqlx::raw_sql("SHOW DATABASES").fetch_all(&pool).await {
            Ok(rows) => rows,
            Err(_) => {
                let fallback_pool = Self::create_pool(
                    &self.host,
                    self.port,
                    &self.username,
                    &self.password,
                    "",
                )
                .await?;
                let rows = sqlx::raw_sql("SHOW DATABASES")
                    .fetch_all(&fallback_pool)
                    .await
                    .context("Failed to list MySQL databases")?;
                *self.active_pool.write().await = (String::new(), fallback_pool);
                rows
            }
        };

        let mut dbs = Vec::with_capacity(rows.len());
        for row in rows {
            if let DbValue::Text(name) = decode_mysql_cell(&row, 0) {
                dbs.push(name);
            }
        }
        Ok(dbs)
    }

    async fn create_database(&self, spec: &CreateDatabaseSpec) -> Result<()> {
        let sql = Self::format_create_database_stmt(spec)?;
        let pool = self.pool_for_db("").await?;
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to create MySQL database '{}'", spec.name))?;
        Ok(())
    }

    async fn use_database(&mut self, db_name: &str) -> Result<()> {
        let _ = self.pool_for_db(db_name).await?;
        Ok(())
    }

    async fn list_tables(&self, db_name: &str) -> Result<Vec<TableInfo>> {
        let pool = self.pool_for_db(db_name).await?;
        let sql = format!(
            "SELECT TABLE_NAME, TABLE_TYPE \
             FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = {} \
             ORDER BY TABLE_NAME",
            escape_sql_literal(&DbValue::Text(db_name.to_string()))
        );
        let rows = sqlx::raw_sql(&sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to list tables in database '{}'", db_name))?;

        let mut tables = Vec::with_capacity(rows.len());
        for row in rows {
            let name = decode_mysql_cell(&row, 0).display_text().to_string();
            let table_type = decode_mysql_cell(&row, 1).display_text().to_string();
            tables.push(TableInfo {
                name,
                is_view: table_type.contains("VIEW"),
            });
        }
        Ok(tables)
    }

    async fn get_table_structure(&self, db_name: &str, table: &str) -> Result<Vec<ColumnInfo>> {
        let pool = self.pool_for_db(db_name).await?;
        let sql = format!(
            "SELECT ORDINAL_POSITION, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, EXTRA \
             FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             ORDER BY ORDINAL_POSITION",
            escape_sql_literal(&DbValue::Text(db_name.to_string())),
            escape_sql_literal(&DbValue::Text(table.to_string()))
        );
        let rows = sqlx::raw_sql(&sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to inspect structure of '{}.{}'", db_name, table))?;

        let mut cols = Vec::with_capacity(rows.len());
        for row in rows {
            let ordinal = decode_mysql_cell(&row, 0)
                .display_text()
                .parse::<u32>()
                .unwrap_or(0);
            let name = decode_mysql_cell(&row, 1).display_text().to_string();
            let data_type = decode_mysql_cell(&row, 2).display_text().to_string();
            let is_nullable = decode_mysql_cell(&row, 3)
                .display_text()
                .eq_ignore_ascii_case("YES");
            let col_key = decode_mysql_cell(&row, 4).display_text().to_string();
            let default_value = match decode_mysql_cell(&row, 5) {
                DbValue::Null => None,
                DbValue::Text(s) => Some(s),
            };
            let extra = match decode_mysql_cell(&row, 6) {
                DbValue::Null => String::new(),
                DbValue::Text(s) => s,
            };

            cols.push(ColumnInfo {
                ordinal,
                name,
                data_type,
                is_nullable,
                is_primary_key: col_key.eq_ignore_ascii_case("PRI"),
                default_value,
                extra,
            });
        }
        Ok(cols)
    }

    async fn alter_column(&self, db_name: &str, table: &str, op: ColumnAlterOp) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(db_name, table);
        let sql = match op {
            ColumnAlterOp::Add(spec) => {
                format!(
                    "ALTER TABLE {} ADD COLUMN {}",
                    fq_table,
                    Self::format_column_spec(&spec, true)
                )
            }
            ColumnAlterOp::Modify { old_name, spec } => {
                format!(
                    "ALTER TABLE {} CHANGE COLUMN {} {}",
                    fq_table,
                    quote_mysql_ident(&old_name),
                    Self::format_column_spec(&spec, false)
                )
            }
            ColumnAlterOp::Drop { name } => {
                format!(
                    "ALTER TABLE {} DROP COLUMN {}",
                    fq_table,
                    quote_mysql_ident(&name)
                )
            }
            ColumnAlterOp::Reorder {
                column,
                after_column,
                ..
            } => {
                format!(
                    "ALTER TABLE {} MODIFY COLUMN {}",
                    fq_table,
                    Self::format_column_info_for_reorder(&column, after_column.as_deref())
                )
            }
        };

        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to execute: {}", sql))?;
        Ok(())
    }

    async fn get_table_indexes(&self, db_name: &str, table: &str) -> Result<Vec<IndexInfo>> {
        let pool = self.pool_for_db(db_name).await?;
        let sql = format!(
            "SELECT INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME, INDEX_TYPE \
             FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             ORDER BY CASE WHEN INDEX_NAME = 'PRIMARY' THEN 0 ELSE 1 END, INDEX_NAME, SEQ_IN_INDEX",
            escape_sql_literal(&DbValue::Text(db_name.to_string())),
            escape_sql_literal(&DbValue::Text(table.to_string()))
        );
        let rows = sqlx::raw_sql(&sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to fetch indexes for '{}.{}'", db_name, table))?;

        let mut indexes: Vec<IndexInfo> = Vec::new();
        for row in rows {
            let idx_name = decode_mysql_cell(&row, 0).display_text().to_string();
            let non_unique = decode_mysql_cell(&row, 1)
                .display_text()
                .parse::<i32>()
                .unwrap_or(1);
            let col_name = match decode_mysql_cell(&row, 3) {
                DbValue::Null => String::new(),
                DbValue::Text(s) => s,
            };
            let raw_method = decode_mysql_cell(&row, 4)
                .display_text()
                .to_ascii_uppercase();

            if let Some(existing) = indexes.last_mut().filter(|i| i.name == idx_name) {
                if !col_name.is_empty() {
                    existing.columns.push(col_name);
                }
            } else {
                let is_primary = idx_name.eq_ignore_ascii_case("PRIMARY");
                let is_unique = is_primary || non_unique == 0;
                let index_type = if is_primary {
                    "PRIMARY".to_string()
                } else if raw_method == "FULLTEXT" {
                    "FULLTEXT".to_string()
                } else if raw_method == "SPATIAL" {
                    "SPATIAL".to_string()
                } else if is_unique {
                    "UNIQUE".to_string()
                } else {
                    "INDEX".to_string()
                };
                let mut columns = Vec::new();
                if !col_name.is_empty() {
                    columns.push(col_name);
                }
                indexes.push(IndexInfo {
                    name: idx_name,
                    columns,
                    is_unique,
                    is_primary,
                    index_type,
                    method: raw_method,
                });
            }
        }
        Ok(indexes)
    }

    async fn alter_index(&self, db_name: &str, table: &str, op: IndexAlterOp) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(db_name, table);
        let sql = match op {
            IndexAlterOp::Add(spec) => {
                format!(
                    "ALTER TABLE {} {}",
                    fq_table,
                    Self::format_add_index_clause(&spec)?
                )
            }
            IndexAlterOp::Modify {
                old_name,
                old_is_primary,
                spec,
            } => {
                format!(
                    "ALTER TABLE {} {}, {}",
                    fq_table,
                    Self::format_drop_index_clause(&old_name, old_is_primary),
                    Self::format_add_index_clause(&spec)?
                )
            }
            IndexAlterOp::Drop { name, is_primary } => {
                format!(
                    "ALTER TABLE {} {}",
                    fq_table,
                    Self::format_drop_index_clause(&name, is_primary)
                )
            }
        };

        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to execute: {}", sql))?;
        Ok(())
    }

    async fn get_table_content(
        &self,
        db_name: &str,
        table: &str,
        query: &ContentQuery,
    ) -> Result<TablePage> {
        let pool = self.pool_for_db(db_name).await?;
        let columns = self.get_table_structure(db_name, table).await?;
        let fq_table = Self::qualified_table(db_name, table);

        let where_clause = if query.search_text.trim().is_empty() {
            String::new()
        } else {
            let trimmed = query.search_text.trim();
            if let Some(custom_where) = trimmed
                .strip_prefix("WHERE ")
                .or_else(|| trimmed.strip_prefix("where "))
            {
                format!(" WHERE {}", custom_where)
            } else if columns.is_empty() {
                String::new()
            } else {
                let pattern = format!("%{}%", trimmed.replace('\'', "''"));
                let predicates: Vec<String> = columns
                    .iter()
                    .map(|c| {
                        format!(
                            "CAST({} AS CHAR) LIKE '{}'",
                            quote_mysql_ident(&c.name),
                            pattern
                        )
                    })
                    .collect();
                format!(" WHERE ({})", predicates.join(" OR "))
            }
        };

        let order_clause = if let Some(sort_col) = &query.sort_column {
            if columns.iter().any(|c| &c.name == sort_col) {
                format!(
                    " ORDER BY {} {}",
                    quote_mysql_ident(sort_col),
                    query.sort_direction.as_sql()
                )
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let count_sql = format!("SELECT COUNT(*) FROM {}{}", fq_table, where_clause);
        let count_rows = sqlx::raw_sql(&count_sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to count rows in {}", fq_table))?;
        let total_rows = count_rows
            .first()
            .and_then(|r| decode_mysql_cell(r, 0).display_text().parse::<u64>().ok())
            .unwrap_or(0);

        let data_sql = format!(
            "SELECT * FROM {}{}{} LIMIT {} OFFSET {}",
            fq_table, where_clause, order_clause, query.limit, query.offset
        );
        let raw_rows = sqlx::raw_sql(&data_sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to fetch content from {}", fq_table))?;

        let pk_indices: Vec<(usize, String)> = columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_primary_key)
            .map(|(idx, c)| (idx, c.name.clone()))
            .collect();

        let mut rows = Vec::with_capacity(raw_rows.len());
        for raw_row in raw_rows {
            let mut values = Vec::with_capacity(columns.len());
            for idx in 0..columns.len() {
                values.push(decode_mysql_cell(&raw_row, idx));
            }
            let pk = if pk_indices.is_empty() {
                None
            } else {
                let mut pk_cols = Vec::with_capacity(pk_indices.len());
                for (idx, col_name) in &pk_indices {
                    if let Some(val) = values.get(*idx) {
                        pk_cols.push((col_name.clone(), val.display_text().to_string()));
                    }
                }
                Some(PrimaryKeyValue { columns: pk_cols })
            };
            rows.push(TableRowData { pk, values });
        }

        Ok(TablePage {
            database: db_name.to_string(),
            table: table.to_string(),
            columns,
            rows,
            total_rows,
            page_limit: query.limit,
            page_offset: query.offset,
        })
    }

    async fn commit_cell_updates(
        &self,
        db_name: &str,
        table: &str,
        updates: &[RowUpdate],
    ) -> Result<u64> {
        if updates.is_empty() {
            return Ok(0);
        }
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(db_name, table);
        let mut total_affected = 0u64;

        for upd in updates {
            if upd.changes.is_empty() {
                continue;
            }
            let set_parts: Vec<String> = upd
                .changes
                .iter()
                .map(|(col, val)| format!("{} = {}", quote_mysql_ident(col), escape_sql_literal(val)))
                .collect();
            let where_part = Self::build_pk_where_clause(&upd.pk)?;
            let sql = format!(
                "UPDATE {} SET {} WHERE {}",
                fq_table,
                set_parts.join(", "),
                where_part
            );
            let res = sqlx::raw_sql(&sql)
                .execute(&pool)
                .await
                .with_context(|| format!("Failed to update row: {}", sql))?;
            total_affected += res.rows_affected();
        }

        Ok(total_affected)
    }

    async fn insert_row(
        &self,
        db_name: &str,
        table: &str,
        values: &[(String, DbValue)],
    ) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(db_name, table);
        let sql = if values.is_empty() {
            format!("INSERT INTO {} () VALUES ()", fq_table)
        } else {
            let cols: Vec<String> = values.iter().map(|(c, _)| quote_mysql_ident(c)).collect();
            let vals: Vec<String> = values.iter().map(|(_, v)| escape_sql_literal(v)).collect();
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                fq_table,
                cols.join(", "),
                vals.join(", ")
            )
        };
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to insert row: {}", sql))?;
        Ok(())
    }

    async fn delete_rows(
        &self,
        db_name: &str,
        table: &str,
        pks: &[PrimaryKeyValue],
    ) -> Result<u64> {
        if pks.is_empty() {
            return Ok(0);
        }
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(db_name, table);
        let mut deleted = 0u64;

        for pk in pks {
            let where_part = Self::build_pk_where_clause(pk)?;
            let sql = format!("DELETE FROM {} WHERE {}", fq_table, where_part);
            let res = sqlx::raw_sql(&sql)
                .execute(&pool)
                .await
                .with_context(|| format!("Failed to delete row: {}", sql))?;
            deleted += res.rows_affected();
        }

        Ok(deleted)
    }

    async fn get_relations(&self, db_name: &str, table: &str) -> Result<Vec<ForeignKeyInfo>> {
        let pool = self.pool_for_db(db_name).await?;
        let sql = format!(
            "SELECT \
                kcu.CONSTRAINT_NAME, \
                kcu.COLUMN_NAME, \
                kcu.REFERENCED_TABLE_NAME, \
                kcu.REFERENCED_COLUMN_NAME, \
                rc.UPDATE_RULE, \
                rc.DELETE_RULE \
             FROM information_schema.KEY_COLUMN_USAGE kcu \
             JOIN information_schema.REFERENTIAL_CONSTRAINTS rc \
               ON kcu.CONSTRAINT_SCHEMA = rc.CONSTRAINT_SCHEMA \
              AND kcu.CONSTRAINT_NAME = rc.CONSTRAINT_NAME \
             WHERE kcu.TABLE_SCHEMA = {} \
               AND kcu.TABLE_NAME = {} \
               AND kcu.REFERENCED_TABLE_NAME IS NOT NULL \
             ORDER BY kcu.CONSTRAINT_NAME, kcu.ORDINAL_POSITION",
            escape_sql_literal(&DbValue::Text(db_name.to_string())),
            escape_sql_literal(&DbValue::Text(table.to_string()))
        );

        let rows = sqlx::raw_sql(&sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to fetch foreign keys for '{}.{}'", db_name, table))?;

        let mut relations = Vec::with_capacity(rows.len());
        for row in rows {
            relations.push(ForeignKeyInfo {
                constraint_name: decode_mysql_cell(&row, 0).display_text().to_string(),
                column_name: decode_mysql_cell(&row, 1).display_text().to_string(),
                referenced_table: decode_mysql_cell(&row, 2).display_text().to_string(),
                referenced_column: decode_mysql_cell(&row, 3).display_text().to_string(),
                update_rule: decode_mysql_cell(&row, 4).display_text().to_string(),
                delete_rule: decode_mysql_cell(&row, 5).display_text().to_string(),
            });
        }
        Ok(relations)
    }

    async fn add_relation(&self, db_name: &str, table: &str, fk: &NewForeignKey) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(db_name, table);
        let fq_ref_table = Self::qualified_table(db_name, &fk.referenced_table);
        let constraint_clause = if fk.constraint_name.trim().is_empty() {
            String::new()
        } else {
            format!("CONSTRAINT {} ", quote_mysql_ident(fk.constraint_name.trim()))
        };
        let sql = format!(
            "ALTER TABLE {} ADD {}FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {} ON UPDATE {}",
            fq_table,
            constraint_clause,
            quote_mysql_ident(&fk.column_name),
            fq_ref_table,
            quote_mysql_ident(&fk.referenced_column),
            fk.on_delete,
            fk.on_update
        );
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to add relation: {}", sql))?;
        Ok(())
    }

    async fn drop_relation(
        &self,
        db_name: &str,
        table: &str,
        constraint_name: &str,
    ) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(db_name, table);
        let sql = format!(
            "ALTER TABLE {} DROP FOREIGN KEY {}",
            fq_table,
            quote_mysql_ident(constraint_name)
        );
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to drop relation: {}", sql))?;
        Ok(())
    }

    async fn execute_raw_query(&self, db_name: &str, sql: &str) -> Result<RawQueryOutcome> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("Query is empty"));
        }

        let pool = self.pool_for_db(db_name).await?;
        let start = Instant::now();

        if is_row_returning_query(trimmed) {
            let raw_rows = sqlx::raw_sql(trimmed).fetch_all(&pool).await?;
            let elapsed = start.elapsed().as_millis();

            let columns: Vec<String> = raw_rows
                .first()
                .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
                .unwrap_or_default();

            let mut rows = Vec::with_capacity(raw_rows.len());
            for raw_row in raw_rows {
                let col_count = raw_row.columns().len();
                let mut vals = Vec::with_capacity(col_count);
                for idx in 0..col_count {
                    vals.push(decode_mysql_cell(&raw_row, idx));
                }
                rows.push(vals);
            }

            Ok(RawQueryOutcome::Rows(RawQueryResultSet {
                columns,
                rows,
                execution_time_ms: elapsed,
            }))
        } else {
            let res = sqlx::raw_sql(trimmed).execute(&pool).await?;
            let elapsed = start.elapsed().as_millis();
            let affected = res.rows_affected();
            Ok(RawQueryOutcome::Executed {
                rows_affected: affected,
                execution_time_ms: elapsed,
                message: format!(
                    "Query executed successfully. Rows affected: {} ({} ms)",
                    affected, elapsed
                ),
            })
        }
    }

    async fn list_users(&self) -> Result<Vec<DbUser>> {
        let pool = self.pool_for_db("").await?;
        let user_rows = match sqlx::raw_sql(
            "SELECT CAST(User AS CHAR), CAST(Host AS CHAR) FROM mysql.user ORDER BY User, Host",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => {
                sqlx::raw_sql(
                    "SELECT SUBSTRING_INDEX(CURRENT_USER(), '@', 1), SUBSTRING_INDEX(CURRENT_USER(), '@', -1)",
                )
                .fetch_all(&pool)
                .await
                .context("Failed to list MySQL users")?
            }
        };

        let mut users = Vec::with_capacity(user_rows.len());
        for row in user_rows {
            let username = decode_mysql_cell(&row, 0).display_text().trim().to_string();
            let host = decode_mysql_cell(&row, 1).display_text().trim().to_string();
            if username.is_empty() {
                continue;
            }
            let account = Self::format_account(&username, Some(&host))?;
            let grant_lines = self.fetch_grants_for_account(&pool, &account).await;
            let (global_privileges, has_grant_option, _) =
                Self::parse_mysql_grants(&username, &host, &grant_lines);
            users.push(DbUser {
                username,
                host: Some(host),
                global_privileges,
                has_grant_option,
            });
        }
        Ok(users)
    }

    async fn create_user(&self, spec: &CreateUserSpec) -> Result<()> {
        let pool = self.pool_for_db("").await?;
        let stmts = Self::format_create_user_stmts(spec)?;
        for sql in stmts {
            sqlx::raw_sql(&sql)
                .execute(&pool)
                .await
                .with_context(|| format!("Failed to execute: {}", sql))?;
        }
        let _ = sqlx::raw_sql("FLUSH PRIVILEGES").execute(&pool).await;
        Ok(())
    }

    async fn update_user(&self, spec: &UpdateUserSpec) -> Result<()> {
        let pool = self.pool_for_db("").await?;
        let stmts = Self::format_update_user_stmts(spec)?;
        for sql in stmts {
            if let Err(e) = sqlx::raw_sql(&sql).execute(&pool).await {
                let err_str = format!("{:#}", e).to_ascii_lowercase();
                if sql.starts_with("REVOKE ") && err_str.contains("no such grant") {
                    continue;
                }
                return Err(e).with_context(|| format!("Failed to execute: {}", sql));
            }
        }
        let _ = sqlx::raw_sql("FLUSH PRIVILEGES").execute(&pool).await;
        Ok(())
    }

    async fn drop_user(&self, username: &str, host: Option<&str>) -> Result<()> {
        let pool = self.pool_for_db("").await?;
        let sql = Self::format_drop_user_stmt(username, host)?;
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to drop user: {}", sql))?;
        let _ = sqlx::raw_sql("FLUSH PRIVILEGES").execute(&pool).await;
        Ok(())
    }

    async fn get_database_permissions(
        &self,
        username: &str,
        host: Option<&str>,
    ) -> Result<Vec<DatabasePermission>> {
        let pool = self.pool_for_db("").await?;
        let h = host
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("%");
        let account = Self::format_account(username, Some(h))?;
        let grant_lines = self.fetch_grants_for_account(&pool, &account).await;
        let (_, _, db_permissions) = Self::parse_mysql_grants(username.trim(), h, &grant_lines);
        Ok(db_permissions)
    }

    async fn set_database_permissions(&self, spec: &DbPermissionSpec) -> Result<()> {
        let pool = self.pool_for_db("").await?;
        let stmts = Self::format_set_db_permissions_stmts(spec)?;
        for sql in stmts {
            if let Err(e) = sqlx::raw_sql(&sql).execute(&pool).await {
                let err_str = format!("{:#}", e).to_ascii_lowercase();
                if sql.starts_with("REVOKE ") && err_str.contains("no such grant") {
                    continue;
                }
                return Err(e).with_context(|| format!("Failed to execute: {}", sql));
            }
        }
        let _ = sqlx::raw_sql("FLUSH PRIVILEGES").execute(&pool).await;
        Ok(())
    }

    async fn revoke_database_permissions(
        &self,
        username: &str,
        host: Option<&str>,
        database: &str,
    ) -> Result<()> {
        let pool = self.pool_for_db("").await?;
        let stmts = Self::format_revoke_db_permissions_stmts(username, host, database)?;
        for sql in stmts {
            if let Err(e) = sqlx::raw_sql(&sql).execute(&pool).await {
                let err_str = format!("{:#}", e).to_ascii_lowercase();
                if err_str.contains("no such grant") {
                    continue;
                }
                return Err(e).with_context(|| format!("Failed to execute: {}", sql));
            }
        }
        let _ = sqlx::raw_sql("FLUSH PRIVILEGES").execute(&pool).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mysql_reorder_column_sql_formatting() {
        let col_first = ColumnInfo {
            ordinal: 1,
            name: "id".to_string(),
            data_type: "int unsigned".to_string(),
            is_nullable: false,
            is_primary_key: true,
            default_value: None,
            extra: "auto_increment".to_string(),
        };
        assert_eq!(
            MySqlDriver::format_column_info_for_reorder(&col_first, None),
            "`id` int unsigned NOT NULL auto_increment FIRST"
        );

        let col_after = ColumnInfo {
            ordinal: 2,
            name: "updated_at".to_string(),
            data_type: "timestamp".to_string(),
            is_nullable: true,
            is_primary_key: false,
            default_value: Some("CURRENT_TIMESTAMP".to_string()),
            extra: "DEFAULT_GENERATED on update CURRENT_TIMESTAMP".to_string(),
        };
        assert_eq!(
            MySqlDriver::format_column_info_for_reorder(&col_after, Some("id")),
            "`updated_at` timestamp NULL DEFAULT CURRENT_TIMESTAMP on update CURRENT_TIMESTAMP AFTER `id`"
        );
    }

    #[test]
    fn test_mysql_index_sql_formatting() {
        let idx = IndexSpec {
            name: "idx_users_email_status".to_string(),
            columns: vec!["email".to_string(), "status".to_string()],
            index_type: "UNIQUE".to_string(),
            method: Some("BTREE".to_string()),
        };
        assert_eq!(
            MySqlDriver::format_add_index_clause(&idx).unwrap(),
            "ADD UNIQUE INDEX `idx_users_email_status` USING BTREE (`email`, `status`)"
        );

        let pk = IndexSpec {
            name: "PRIMARY".to_string(),
            columns: vec!["id".to_string()],
            index_type: "PRIMARY".to_string(),
            method: None,
        };
        assert_eq!(
            MySqlDriver::format_add_index_clause(&pk).unwrap(),
            "ADD PRIMARY KEY (`id`)"
        );

        assert_eq!(
            MySqlDriver::format_drop_index_clause("PRIMARY", true),
            "DROP PRIMARY KEY"
        );
        assert_eq!(
            MySqlDriver::format_drop_index_clause("idx_users_email", false),
            "DROP INDEX `idx_users_email`"
        );
    }

    #[test]
    fn test_mysql_format_column_spec() {
        let auto_inc_pk = ColumnSpec {
            name: "id".to_string(),
            data_type: "INT".to_string(),
            is_nullable: false,
            default_value: None,
            extra: Some("AUTO_INCREMENT".to_string()),
            is_primary_key: true,
        };
        assert_eq!(
            MySqlDriver::format_column_spec(&auto_inc_pk, true),
            "`id` INT NOT NULL AUTO_INCREMENT PRIMARY KEY"
        );

        let on_update_ts = ColumnSpec {
            name: "updated_at".to_string(),
            data_type: "TIMESTAMP".to_string(),
            is_nullable: false,
            default_value: Some("CURRENT_TIMESTAMP".to_string()),
            extra: Some("ON UPDATE CURRENT_TIMESTAMP".to_string()),
            is_primary_key: false,
        };
        assert_eq!(
            MySqlDriver::format_column_spec(&on_update_ts, false),
            "`updated_at` TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP"
        );
    }

    #[test]
    fn test_mysql_create_database_sql_formatting() {
        let basic = CreateDatabaseSpec {
            name: "analytics_db".to_string(),
            charset_or_encoding: None,
            collation: None,
            template: None,
        };
        assert_eq!(
            MySqlDriver::format_create_database_stmt(&basic).unwrap(),
            "CREATE DATABASE `analytics_db`"
        );

        let with_charset_and_collation = CreateDatabaseSpec {
            name: "app`db".to_string(),
            charset_or_encoding: Some("utf8mb4".to_string()),
            collation: Some("utf8mb4_unicode_ci".to_string()),
            template: None,
        };
        assert_eq!(
            MySqlDriver::format_create_database_stmt(&with_charset_and_collation).unwrap(),
            "CREATE DATABASE `app``db` DEFAULT CHARACTER SET utf8mb4 DEFAULT COLLATE utf8mb4_unicode_ci"
        );

        let empty = CreateDatabaseSpec {
            name: "   ".to_string(),
            charset_or_encoding: None,
            collation: None,
            template: None,
        };
        assert!(MySqlDriver::format_create_database_stmt(&empty).is_err());
    }

    #[test]
    fn test_mysql_user_and_permission_sql_formatting() {
        let create_spec = CreateUserSpec {
            username: "app_reader".to_string(),
            host: Some("%".to_string()),
            password: "s3cr'et".to_string(),
            global_privileges: vec!["SELECT".to_string(), "SHOW VIEW".to_string()],
            with_grant_option: false,
        };
        assert_eq!(
            MySqlDriver::format_create_user_stmts(&create_spec).unwrap(),
            vec![
                "CREATE USER 'app_reader'@'%' IDENTIFIED BY 's3cr''et'",
                "GRANT SELECT, SHOW VIEW ON *.* TO 'app_reader'@'%'"
            ]
        );

        let update_spec = UpdateUserSpec {
            old_username: "app_reader".to_string(),
            old_host: Some("%".to_string()),
            new_username: "app_writer".to_string(),
            new_host: Some("localhost".to_string()),
            new_password: Some("newpass".to_string()),
            global_privileges: vec!["SELECT".to_string(), "INSERT".to_string()],
            with_grant_option: true,
        };
        assert_eq!(
            MySqlDriver::format_update_user_stmts(&update_spec).unwrap(),
            vec![
                "RENAME USER 'app_reader'@'%' TO 'app_writer'@'localhost'",
                "ALTER USER 'app_writer'@'localhost' IDENTIFIED BY 'newpass'",
                "REVOKE ALL PRIVILEGES ON *.* FROM 'app_writer'@'localhost'",
                "REVOKE GRANT OPTION ON *.* FROM 'app_writer'@'localhost'",
                "GRANT SELECT, INSERT ON *.* TO 'app_writer'@'localhost' WITH GRANT OPTION"
            ]
        );

        let db_perm = DbPermissionSpec {
            username: "app_writer".to_string(),
            host: Some("localhost".to_string()),
            database: "commerce_prod".to_string(),
            privileges: vec![
                "SELECT".to_string(),
                "INSERT".to_string(),
                "UPDATE".to_string(),
            ],
            with_grant_option: false,
        };
        assert_eq!(
            MySqlDriver::format_set_db_permissions_stmts(&db_perm).unwrap(),
            vec![
                "REVOKE ALL PRIVILEGES ON `commerce_prod`.* FROM 'app_writer'@'localhost'",
                "REVOKE GRANT OPTION ON `commerce_prod`.* FROM 'app_writer'@'localhost'",
                "GRANT SELECT, INSERT, UPDATE ON `commerce_prod`.* TO 'app_writer'@'localhost'"
            ]
        );
    }

    #[test]
    fn test_parse_mysql_grants() {
        let lines = vec![
            "GRANT SELECT, PROCESS ON *.* TO `reporter`@`%`".to_string(),
            "GRANT SELECT, INSERT, UPDATE ON `shop\\_db`.* TO `reporter`@`%` WITH GRANT OPTION"
                .to_string(),
            "GRANT ALL PRIVILEGES ON `analytics`.* TO `reporter`@`%`".to_string(),
        ];
        let (global_privs, global_grant_opt, db_perms) =
            MySqlDriver::parse_mysql_grants("reporter", "%", &lines);
        assert_eq!(global_privs, vec!["SELECT", "PROCESS"]);
        assert!(!global_grant_opt);
        assert_eq!(db_perms.len(), 2);
        assert_eq!(db_perms[0].database, "analytics");
        assert_eq!(db_perms[0].privileges.len(), MYSQL_DB_PRIVILEGES.len());
        assert!(!db_perms[0].has_grant_option);
        assert_eq!(db_perms[1].database, "shop_db");
        assert_eq!(db_perms[1].privileges, vec!["SELECT", "INSERT", "UPDATE"]);
        assert!(db_perms[1].has_grant_option);
    }
}
