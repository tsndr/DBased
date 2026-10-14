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
        if spec.is_nullable {
            parts.push("NULL".to_string());
        } else {
            parts.push("NOT NULL".to_string());
        }
        if let Some(def) = &spec.default_value {
            let trimmed = def.trim();
            if !trimmed.is_empty() {
                if trimmed.eq_ignore_ascii_case("NULL")
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
        let rows = sqlx::raw_sql("SHOW DATABASES")
            .fetch_all(&pool)
            .await
            .context("Failed to list MySQL databases")?;

        let mut dbs = Vec::with_capacity(rows.len());
        for row in rows {
            if let DbValue::Text(name) = decode_mysql_cell(&row, 0) {
                dbs.push(name);
            }
        }
        Ok(dbs)
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
}
