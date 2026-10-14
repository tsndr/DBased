use crate::db::driver::{
    DatabaseDriver, escape_sql_literal, is_row_returning_query, quote_pg_ident,
};
use crate::db::models::*;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Column, Row, ValueRef};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

pub struct PostgresDriver {
    host: String,
    port: u16,
    username: String,
    password: String,
    active_pool: RwLock<(String, PgPool)>,
}

impl PostgresDriver {
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        default_database: &str,
    ) -> Result<Self> {
        let initial_db = if default_database.trim().is_empty() {
            "postgres"
        } else {
            default_database.trim()
        };

        let pool = match Self::create_pool(host, port, username, password, initial_db).await {
            Ok(p) => (initial_db.to_string(), p),
            Err(first_err) => {
                if default_database.trim().is_empty() && username != "postgres" {
                    // Fallback to database named after username
                    match Self::create_pool(host, port, username, password, username).await {
                        Ok(p) => (username.to_string(), p),
                        Err(_) => return Err(first_err),
                    }
                } else {
                    return Err(first_err);
                }
            }
        };

        Ok(Self {
            host: host.to_string(),
            port,
            username: username.to_string(),
            password: password.to_string(),
            active_pool: RwLock::new(pool),
        })
    }

    async fn create_pool(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        database: &str,
    ) -> Result<PgPool> {
        let opts = PgConnectOptions::new()
            .host(host)
            .port(port)
            .username(username)
            .password(password)
            .database(database);

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(opts)
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to PostgreSQL at {}:{}/{}",
                    host, port, database
                )
            })?;
        Ok(pool)
    }

    async fn pool_for_db(&self, db_name: &str) -> Result<PgPool> {
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

    fn qualified_table(table: &str) -> String {
        format!("\"public\".{}", quote_pg_ident(table))
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
                    "{}::text = {}",
                    quote_pg_ident(col),
                    escape_sql_literal(&DbValue::Text(val.clone()))
                )
            })
            .collect();
        Ok(parts.join(" AND "))
    }

    fn format_default_expr(def: &str) -> String {
        let trimmed = def.trim();
        if trimmed.eq_ignore_ascii_case("NULL")
            || trimmed.eq_ignore_ascii_case("CURRENT_TIMESTAMP")
            || trimmed.eq_ignore_ascii_case("NOW()")
            || trimmed.eq_ignore_ascii_case("true")
            || trimmed.eq_ignore_ascii_case("false")
            || trimmed.ends_with("()")
            || trimmed.contains("::")
            || trimmed.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-')
        {
            trimmed.to_string()
        } else {
            escape_sql_literal(&DbValue::Text(trimmed.to_string()))
        }
    }

    async fn reorder_columns(
        &self,
        db_name: &str,
        table: &str,
        new_order: &[String],
    ) -> Result<()> {
        let current_cols = self.get_table_structure(db_name, table).await?;
        if new_order.len() != current_cols.len() {
            return Err(anyhow!("Column count mismatch during reorder"));
        }

        let mut new_cols = Vec::with_capacity(new_order.len());
        for col_name in new_order {
            let col = current_cols
                .iter()
                .find(|c| &c.name == col_name)
                .cloned()
                .ok_or_else(|| anyhow!("Column '{}' not found in table '{}'", col_name, table))?;
            new_cols.push(col);
        }

        let Some(start_idx) = current_cols
            .iter()
            .zip(new_cols.iter())
            .position(|(old_c, new_c)| old_c.name != new_c.name)
        else {
            return Ok(());
        };

        let affected_cols = &new_cols[start_idx..];
        let relations = self.get_relations(db_name, table).await?;
        let existing_indexes = self.get_table_indexes(db_name, table).await.unwrap_or_default();
        let fq_table = Self::qualified_table(table);
        let pool = self.pool_for_db(db_name).await?;

        // Check if any affected column is a primary key; if so, find the PK constraint name
        let pk_affected = affected_cols.iter().any(|c| c.is_primary_key);
        let pk_constraint_name = if pk_affected {
            let escaped_table = escape_sql_literal(&DbValue::Text(table.to_string()));
            let pk_sql = format!(
                "SELECT tc.constraint_name \
                 FROM information_schema.table_constraints tc \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = 'public' \
                   AND tc.table_name = {}",
                escaped_table
            );
            let rows = sqlx::raw_sql(&pk_sql).fetch_all(&pool).await?;
            rows.first()
                .map(|r| decode_pg_cell(r, 0).display_text().to_string())
        } else {
            None
        };

        let mut stmts = vec!["BEGIN".to_string()];

        // 1. Rename affected columns to temporary names
        for (idx, col) in affected_cols.iter().enumerate() {
            let tmp_name = format!("__dbased_mv_{}_{}", idx, col.name);
            stmts.push(format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                fq_table,
                quote_pg_ident(&col.name),
                quote_pg_ident(&tmp_name)
            ));
        }

        // 2. Re-add columns in the new order and copy data
        for (idx, col) in affected_cols.iter().enumerate() {
            let tmp_name = format!("__dbased_mv_{}_{}", idx, col.name);
            let col_ident = quote_pg_ident(&col.name);
            let tmp_ident = quote_pg_ident(&tmp_name);

            stmts.push(format!(
                "ALTER TABLE {} ADD COLUMN {} {}",
                fq_table,
                col_ident,
                col.data_type.trim()
            ));

            stmts.push(format!(
                "UPDATE {} SET {} = {}",
                fq_table, col_ident, tmp_ident
            ));

            if !col.is_nullable {
                stmts.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} SET NOT NULL",
                    fq_table, col_ident
                ));
            }

            if let Some(def) = &col.default_value {
                if !def.trim().is_empty() {
                    stmts.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {}",
                        fq_table,
                        col_ident,
                        Self::format_default_expr(def)
                    ));

                    // Reassign sequence ownership if default uses nextval('...')
                    if let Some(rest) = def.trim().strip_prefix("nextval('") {
                        if let Some(end_quote) = rest.find('\'') {
                            let seq_name = &rest[..end_quote];
                            stmts.push(format!(
                                "ALTER SEQUENCE {} OWNED BY {}.{}",
                                seq_name, fq_table, col_ident
                            ));
                        }
                    }
                }
            } else if !col.extra.trim().is_empty() {
                let gen_type = col.extra.trim();
                if gen_type.eq_ignore_ascii_case("ALWAYS")
                    || gen_type.eq_ignore_ascii_case("BY DEFAULT")
                {
                    stmts.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} ADD GENERATED {} AS IDENTITY",
                        fq_table, col_ident, gen_type
                    ));
                }
            }
        }

        // 3. Recreate foreign keys on affected columns
        for fk in &relations {
            if affected_cols.iter().any(|c| c.name == fk.column_name) {
                stmts.push(format!(
                    "ALTER TABLE {} DROP CONSTRAINT {}",
                    fq_table,
                    quote_pg_ident(&fk.constraint_name)
                ));

                let fq_ref_table = Self::qualified_table(&fk.referenced_table);
                stmts.push(format!(
                    "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {} ON UPDATE {}",
                    fq_table,
                    quote_pg_ident(&fk.constraint_name),
                    quote_pg_ident(&fk.column_name),
                    fq_ref_table,
                    quote_pg_ident(&fk.referenced_column),
                    fk.delete_rule,
                    fk.update_rule
                ));
            }
        }

        // 4. Recreate primary key if any affected column was part of the PK
        if let Some(pk_cname) = pk_constraint_name {
            stmts.push(format!(
                "ALTER TABLE {} DROP CONSTRAINT {}",
                fq_table,
                quote_pg_ident(&pk_cname)
            ));

            let pk_cols: Vec<String> = new_cols
                .iter()
                .filter(|c| c.is_primary_key)
                .map(|c| quote_pg_ident(&c.name))
                .collect();
            if !pk_cols.is_empty() {
                stmts.push(format!(
                    "ALTER TABLE {} ADD CONSTRAINT {} PRIMARY KEY ({})",
                    fq_table,
                    quote_pg_ident(&pk_cname),
                    pk_cols.join(", ")
                ));
            }
        }

        // 5. Drop temporary columns
        for (idx, col) in affected_cols.iter().enumerate() {
            let tmp_name = format!("__dbased_mv_{}_{}", idx, col.name);
            stmts.push(format!(
                "ALTER TABLE {} DROP COLUMN {}",
                fq_table,
                quote_pg_ident(&tmp_name)
            ));
        }

        // 6. Recreate any secondary indexes that referenced affected columns
        for idx in &existing_indexes {
            if !idx.is_primary
                && idx
                    .columns
                    .iter()
                    .any(|ic| affected_cols.iter().any(|ac| &ac.name == ic))
            {
                let spec = IndexSpec {
                    name: idx.name.clone(),
                    columns: idx.columns.clone(),
                    index_type: idx.index_type.clone(),
                    method: Some(idx.method.clone()),
                };
                if let Ok(create_stmt) = Self::format_create_index_stmt(table, &spec) {
                    stmts.push(create_stmt);
                }
            }
        }

        stmts.push("COMMIT".to_string());
        let batch_sql = stmts.join(";\n");

        sqlx::raw_sql(&batch_sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to reorder columns in '{}'", table))?;

        Ok(())
    }

    fn format_create_index_stmt(table: &str, spec: &IndexSpec) -> Result<String> {
        if spec.columns.is_empty() {
            return Err(anyhow!("At least one column must be selected for an index"));
        }
        let fq_table = Self::qualified_table(table);
        let cols = spec
            .columns
            .iter()
            .map(|c| quote_pg_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        let idx_type = spec.index_type.trim().to_ascii_uppercase();
        if idx_type == "PRIMARY" {
            let trimmed_name = spec.name.trim();
            if trimmed_name.is_empty() || trimmed_name.eq_ignore_ascii_case("PRIMARY") {
                return Ok(format!("ALTER TABLE {} ADD PRIMARY KEY ({})", fq_table, cols));
            } else {
                return Ok(format!(
                    "ALTER TABLE {} ADD CONSTRAINT {} PRIMARY KEY ({})",
                    fq_table,
                    quote_pg_ident(trimmed_name),
                    cols
                ));
            }
        }

        let unique_kw = if idx_type == "UNIQUE" { "UNIQUE " } else { "" };
        let default_name = format!(
            "{}_{}_{}",
            if idx_type == "UNIQUE" { "uniq" } else { "idx" },
            table,
            spec.columns.join("_")
        );
        let idx_name = if spec.name.trim().is_empty()
            || spec.name.trim().eq_ignore_ascii_case("PRIMARY")
        {
            default_name.as_str()
        } else {
            spec.name.trim()
        };

        let using_clause = match spec
            .method
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
        {
            Some(ref m) if matches!(m.as_str(), "btree" | "hash" | "gin" | "gist" | "brin") => {
                format!(" USING {}", m)
            }
            _ if idx_type == "SPATIAL" => " USING gist".to_string(),
            _ => String::new(),
        };

        Ok(format!(
            "CREATE {}INDEX {} ON {}{} ({})",
            unique_kw,
            quote_pg_ident(idx_name),
            fq_table,
            using_clause,
            cols
        ))
    }

    async fn resolve_drop_index_stmt(
        &self,
        pool: &PgPool,
        table: &str,
        index_name: &str,
        is_primary: bool,
    ) -> Result<String> {
        let fq_table = Self::qualified_table(table);
        let escaped_table = escape_sql_literal(&DbValue::Text(table.to_string()));
        let escaped_idx = escape_sql_literal(&DbValue::Text(index_name.trim().to_string()));

        // Check if this index backs a constraint in pg_constraint (e.g. PRIMARY KEY or UNIQUE constraint)
        let con_sql = format!(
            "SELECT con.conname \
             FROM pg_constraint con \
             JOIN pg_class idx ON idx.oid = con.conindid \
             JOIN pg_class tbl ON tbl.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = tbl.relnamespace \
             WHERE n.nspname = 'public' \
               AND tbl.relname = {} \
               AND idx.relname = {}",
            escaped_table, escaped_idx
        );
        let rows = sqlx::raw_sql(&con_sql).fetch_all(pool).await?;
        if let Some(first) = rows.first() {
            let con_name = decode_pg_cell(first, 0).display_text().to_string();
            if !con_name.is_empty() {
                return Ok(format!(
                    "ALTER TABLE {} DROP CONSTRAINT {}",
                    fq_table,
                    quote_pg_ident(&con_name)
                ));
            }
        }

        if is_primary {
            let pk_sql = format!(
                "SELECT tc.constraint_name \
                 FROM information_schema.table_constraints tc \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = 'public' \
                   AND tc.table_name = {}",
                escaped_table
            );
            let pk_rows = sqlx::raw_sql(&pk_sql).fetch_all(pool).await?;
            if let Some(first) = pk_rows.first() {
                let pk_name = decode_pg_cell(first, 0).display_text().to_string();
                if !pk_name.is_empty() {
                    return Ok(format!(
                        "ALTER TABLE {} DROP CONSTRAINT {}",
                        fq_table,
                        quote_pg_ident(&pk_name)
                    ));
                }
            }
        }

        Ok(format!(
            "DROP INDEX \"public\".{}",
            quote_pg_ident(index_name.trim())
        ))
    }
}

fn decode_pg_cell(row: &PgRow, idx: usize) -> DbValue {
    if let Ok(raw) = row.try_get_raw(idx) {
        if raw.is_null() {
            return DbValue::Null;
        }
        if let Ok(s) = raw.as_str() {
            return DbValue::Text(s.to_string());
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
    if let Ok(Some(v)) = row.try_get::<Option<i32>, _>(idx) {
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
    if let Ok(Some(v)) = row.try_get::<Option<uuid::Uuid>, _>(idx) {
        return DbValue::Text(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<chrono::NaiveDateTime>, _>(idx) {
        return DbValue::Text(v.format("%Y-%m-%d %H:%M:%S").to_string());
    }
    if let Ok(Some(v)) = row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx) {
        return DbValue::Text(v.to_rfc3339());
    }
    if let Ok( raw ) = row.try_get_raw(idx) {
        if let Ok(bytes) = raw.as_bytes() {
            let hex: String = bytes.iter().take(64).map(|b| format!("{:02x}", b)).collect();
            return DbValue::Text(format!("\\x{}", hex));
        }
    }
    DbValue::Text("<unprintable>".to_string())
}

#[async_trait]
impl DatabaseDriver for PostgresDriver {
    async fn list_databases(&self) -> Result<Vec<String>> {
        let pool = self.pool_for_db("").await?;
        let rows = sqlx::raw_sql(
            "SELECT datname FROM pg_database WHERE datistemplate = false ORDER BY datname",
        )
        .fetch_all(&pool)
        .await
        .context("Failed to list PostgreSQL databases")?;

        let mut dbs = Vec::with_capacity(rows.len());
        for row in rows {
            if let DbValue::Text(name) = decode_pg_cell(&row, 0) {
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
        let sql = "SELECT table_name, table_type \
                   FROM information_schema.tables \
                   WHERE table_schema = 'public' \
                   ORDER BY table_name";
        let rows = sqlx::raw_sql(sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to list tables in database '{}'", db_name))?;

        let mut tables = Vec::with_capacity(rows.len());
        for row in rows {
            let name = decode_pg_cell(&row, 0).display_text().to_string();
            let table_type = decode_pg_cell(&row, 1).display_text().to_string();
            tables.push(TableInfo {
                name,
                is_view: table_type.contains("VIEW"),
            });
        }
        Ok(tables)
    }

    async fn get_table_structure(&self, db_name: &str, table: &str) -> Result<Vec<ColumnInfo>> {
        let pool = self.pool_for_db(db_name).await?;
        let escaped_table = escape_sql_literal(&DbValue::Text(table.to_string()));
        let sql = format!(
            "SELECT \
                c.ordinal_position, \
                c.column_name, \
                CASE \
                    WHEN c.character_maximum_length IS NOT NULL THEN c.udt_name || '(' || c.character_maximum_length || ')' \
                    ELSE c.udt_name \
                END AS data_type, \
                c.is_nullable, \
                CASE WHEN pk.column_name IS NOT NULL THEN 'PRI' ELSE '' END AS column_key, \
                c.column_default, \
                COALESCE( \
                    c.identity_generation, \
                    ( \
                        SELECT 'enum(' || string_agg(quote_literal(e.enumlabel), ',' ORDER BY e.enumsortorder) || ')' \
                        FROM pg_type t \
                        JOIN pg_namespace n ON n.oid = t.typnamespace \
                        JOIN pg_enum e ON e.enumtypid = t.oid \
                        WHERE n.nspname = c.udt_schema AND t.typname = c.udt_name \
                    ), \
                    '' \
                ) AS extra \
             FROM information_schema.columns c \
             LEFT JOIN ( \
                 SELECT kcu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                  AND tc.table_schema = kcu.table_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = 'public' \
                   AND tc.table_name = {tbl} \
             ) pk ON c.column_name = pk.column_name \
             WHERE c.table_schema = 'public' AND c.table_name = {tbl} \
             ORDER BY c.ordinal_position",
            tbl = escaped_table
        );

        let rows = sqlx::raw_sql(&sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to inspect structure of '{}'", table))?;

        let mut cols = Vec::with_capacity(rows.len());
        for row in rows {
            let ordinal = decode_pg_cell(&row, 0)
                .display_text()
                .parse::<u32>()
                .unwrap_or(0);
            let name = decode_pg_cell(&row, 1).display_text().to_string();
            let data_type = decode_pg_cell(&row, 2).display_text().to_string();
            let is_nullable = decode_pg_cell(&row, 3)
                .display_text()
                .eq_ignore_ascii_case("YES");
            let col_key = decode_pg_cell(&row, 4).display_text().to_string();
            let default_value = match decode_pg_cell(&row, 5) {
                DbValue::Null => None,
                DbValue::Text(s) => Some(s),
            };
            let extra = match decode_pg_cell(&row, 6) {
                DbValue::Null => String::new(),
                DbValue::Text(s) => s,
            };

            cols.push(ColumnInfo {
                ordinal,
                name,
                data_type,
                is_nullable,
                is_primary_key: col_key == "PRI",
                default_value,
                extra,
            });
        }
        Ok(cols)
    }

    async fn alter_column(&self, db_name: &str, table: &str, op: ColumnAlterOp) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(table);

        let sql = match op {
            ColumnAlterOp::Add(spec) => {
                let mut parts = vec![
                    format!("ALTER TABLE {} ADD COLUMN", fq_table),
                    quote_pg_ident(&spec.name),
                    spec.data_type.trim().to_string(),
                ];
                if !spec.is_nullable {
                    parts.push("NOT NULL".to_string());
                }
                if let Some(def) = &spec.default_value {
                    if !def.trim().is_empty() {
                        parts.push(format!("DEFAULT {}", Self::format_default_expr(def)));
                    }
                }
                if spec.is_primary_key {
                    parts.push("PRIMARY KEY".to_string());
                }
                parts.join(" ")
            }
            ColumnAlterOp::Modify { old_name, spec } => {
                let mut stmts = Vec::new();
                if old_name != spec.name {
                    stmts.push(format!(
                        "ALTER TABLE {} RENAME COLUMN {} TO {}",
                        fq_table,
                        quote_pg_ident(&old_name),
                        quote_pg_ident(&spec.name)
                    ));
                }
                let col_ident = quote_pg_ident(&spec.name);
                let dt = spec.data_type.trim();
                stmts.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} TYPE {} USING {}::{}",
                    fq_table, col_ident, dt, col_ident, dt
                ));
                if spec.is_nullable {
                    stmts.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} DROP NOT NULL",
                        fq_table, col_ident
                    ));
                } else {
                    stmts.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET NOT NULL",
                        fq_table, col_ident
                    ));
                }
                match &spec.default_value {
                    Some(def) if !def.trim().is_empty() => {
                        stmts.push(format!(
                            "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {}",
                            fq_table,
                            col_ident,
                            Self::format_default_expr(def)
                        ));
                    }
                    _ => {
                        stmts.push(format!(
                            "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT",
                            fq_table, col_ident
                        ));
                    }
                }
                stmts.join(";\n")
            }
            ColumnAlterOp::Drop { name } => {
                format!(
                    "ALTER TABLE {} DROP COLUMN {}",
                    fq_table,
                    quote_pg_ident(&name)
                )
            }
            ColumnAlterOp::Reorder { new_order, .. } => {
                return self.reorder_columns(db_name, table, &new_order).await;
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
        let escaped_table = escape_sql_literal(&DbValue::Text(table.to_string()));
        let sql = format!(
            "SELECT \
                i.relname AS index_name, \
                ix.indisunique AS is_unique, \
                ix.indisprimary AS is_primary, \
                UPPER(am.amname) AS index_method, \
                k.ord AS seq_in_index, \
                COALESCE(a.attname, pg_get_indexdef(ix.indexrelid, k.ord::int, true)) AS column_name \
             FROM pg_index ix \
             JOIN pg_class t ON t.oid = ix.indrelid \
             JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_am am ON am.oid = i.relam \
             CROSS JOIN LATERAL unnest(ix.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord) \
             LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum AND k.attnum > 0 \
             WHERE n.nspname = 'public' \
               AND t.relname = {tbl} \
               AND k.ord <= ix.indnkeyatts \
             ORDER BY ix.indisprimary DESC, i.relname ASC, k.ord ASC",
            tbl = escaped_table
        );

        let rows = sqlx::raw_sql(&sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to fetch indexes for '{}'", table))?;

        let mut indexes: Vec<IndexInfo> = Vec::new();
        for row in rows {
            let idx_name = decode_pg_cell(&row, 0).display_text().to_string();
            let unique_str = decode_pg_cell(&row, 1).display_text().to_string();
            let is_unique = unique_str.eq_ignore_ascii_case("true") || unique_str == "t";
            let primary_str = decode_pg_cell(&row, 2).display_text().to_string();
            let is_primary = primary_str.eq_ignore_ascii_case("true") || primary_str == "t";
            let method = decode_pg_cell(&row, 3).display_text().to_ascii_uppercase();
            let col_name = match decode_pg_cell(&row, 5) {
                DbValue::Null => String::new(),
                DbValue::Text(s) => s,
            };

            if let Some(existing) = indexes.last_mut().filter(|i| i.name == idx_name) {
                if !col_name.is_empty() {
                    existing.columns.push(col_name);
                }
            } else {
                let index_type = if is_primary {
                    "PRIMARY".to_string()
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
                    method,
                });
            }
        }
        Ok(indexes)
    }

    async fn alter_index(&self, db_name: &str, table: &str, op: IndexAlterOp) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let sql = match op {
            IndexAlterOp::Add(spec) => Self::format_create_index_stmt(table, &spec)?,
            IndexAlterOp::Modify {
                old_name,
                old_is_primary,
                spec,
            } => {
                let drop_stmt = self
                    .resolve_drop_index_stmt(&pool, table, &old_name, old_is_primary)
                    .await?;
                let create_stmt = Self::format_create_index_stmt(table, &spec)?;
                format!("BEGIN;\n{};\n{};\nCOMMIT", drop_stmt, create_stmt)
            }
            IndexAlterOp::Drop { name, is_primary } => {
                self.resolve_drop_index_stmt(&pool, table, &name, is_primary)
                    .await?
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
        let fq_table = Self::qualified_table(table);

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
                    .map(|c| format!("{}::text ILIKE '{}'", quote_pg_ident(&c.name), pattern))
                    .collect();
                format!(" WHERE ({})", predicates.join(" OR "))
            }
        };

        let order_clause = if let Some(sort_col) = &query.sort_column {
            if columns.iter().any(|c| &c.name == sort_col) {
                format!(
                    " ORDER BY {} {}",
                    quote_pg_ident(sort_col),
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
            .and_then(|r| decode_pg_cell(r, 0).display_text().parse::<u64>().ok())
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
                values.push(decode_pg_cell(&raw_row, idx));
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
        let fq_table = Self::qualified_table(table);
        let mut total_affected = 0u64;

        for upd in updates {
            if upd.changes.is_empty() {
                continue;
            }
            let set_parts: Vec<String> = upd
                .changes
                .iter()
                .map(|(col, val)| format!("{} = {}", quote_pg_ident(col), escape_sql_literal(val)))
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
        let fq_table = Self::qualified_table(table);
        let sql = if values.is_empty() {
            format!("INSERT INTO {} DEFAULT VALUES", fq_table)
        } else {
            let cols: Vec<String> = values.iter().map(|(c, _)| quote_pg_ident(c)).collect();
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
        let fq_table = Self::qualified_table(table);
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
        let escaped_table = escape_sql_literal(&DbValue::Text(table.to_string()));
        let sql = format!(
            "SELECT \
                tc.constraint_name, \
                kcu.column_name, \
                ccu.table_name AS referenced_table_name, \
                ccu.column_name AS referenced_column_name, \
                rc.update_rule, \
                rc.delete_rule \
             FROM information_schema.table_constraints AS tc \
             JOIN information_schema.key_column_usage AS kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema = kcu.table_schema \
             JOIN information_schema.constraint_column_usage AS ccu \
               ON ccu.constraint_name = tc.constraint_name \
              AND ccu.table_schema = tc.table_schema \
             JOIN information_schema.referential_constraints AS rc \
               ON rc.constraint_name = tc.constraint_name \
              AND rc.constraint_schema = tc.table_schema \
             WHERE tc.constraint_type = 'FOREIGN KEY' \
               AND tc.table_schema = 'public' \
               AND tc.table_name = {} \
             ORDER BY tc.constraint_name, kcu.ordinal_position",
            escaped_table
        );

        let rows = sqlx::raw_sql(&sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to fetch foreign keys for '{}'", table))?;

        let mut relations = Vec::with_capacity(rows.len());
        for row in rows {
            relations.push(ForeignKeyInfo {
                constraint_name: decode_pg_cell(&row, 0).display_text().to_string(),
                column_name: decode_pg_cell(&row, 1).display_text().to_string(),
                referenced_table: decode_pg_cell(&row, 2).display_text().to_string(),
                referenced_column: decode_pg_cell(&row, 3).display_text().to_string(),
                update_rule: decode_pg_cell(&row, 4).display_text().to_string(),
                delete_rule: decode_pg_cell(&row, 5).display_text().to_string(),
            });
        }
        Ok(relations)
    }

    async fn add_relation(&self, db_name: &str, table: &str, fk: &NewForeignKey) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(table);
        let fq_ref_table = Self::qualified_table(&fk.referenced_table);
        let constraint_clause = if fk.constraint_name.trim().is_empty() {
            String::new()
        } else {
            format!("CONSTRAINT {} ", quote_pg_ident(fk.constraint_name.trim()))
        };
        let sql = format!(
            "ALTER TABLE {} ADD {}FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {} ON UPDATE {}",
            fq_table,
            constraint_clause,
            quote_pg_ident(&fk.column_name),
            fq_ref_table,
            quote_pg_ident(&fk.referenced_column),
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
        let fq_table = Self::qualified_table(table);
        let sql = format!(
            "ALTER TABLE {} DROP CONSTRAINT {}",
            fq_table,
            quote_pg_ident(constraint_name)
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
                    vals.push(decode_pg_cell(&raw_row, idx));
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
    fn test_pg_index_sql_formatting() {
        let idx = IndexSpec {
            name: "idx_orders_user_created".to_string(),
            columns: vec!["user_id".to_string(), "created_at".to_string()],
            index_type: "INDEX".to_string(),
            method: Some("BTREE".to_string()),
        };
        assert_eq!(
            PostgresDriver::format_create_index_stmt("orders", &idx).unwrap(),
            "CREATE INDEX \"idx_orders_user_created\" ON \"public\".\"orders\" USING btree (\"user_id\", \"created_at\")"
        );

        let uniq = IndexSpec {
            name: String::new(),
            columns: vec!["email".to_string()],
            index_type: "UNIQUE".to_string(),
            method: None,
        };
        assert_eq!(
            PostgresDriver::format_create_index_stmt("users", &uniq).unwrap(),
            "CREATE UNIQUE INDEX \"uniq_users_email\" ON \"public\".\"users\" (\"email\")"
        );

        let pk = IndexSpec {
            name: "PRIMARY".to_string(),
            columns: vec!["id".to_string()],
            index_type: "PRIMARY".to_string(),
            method: None,
        };
        assert_eq!(
            PostgresDriver::format_create_index_stmt("users", &pk).unwrap(),
            "ALTER TABLE \"public\".\"users\" ADD PRIMARY KEY (\"id\")"
        );
    }
}
