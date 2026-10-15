use crate::config::bookmarks::DbEngine;
use crate::db::driver::{
    DatabaseDriver, escape_sql_literal, escape_sql_literal_for_engine, is_row_returning_query,
    quote_pg_ident, truncate_sql_preview,
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

    fn parse_identity_generation(extra: Option<&str>) -> Option<&'static str> {
        let trimmed = extra?.trim();
        if trimmed.eq_ignore_ascii_case("ALWAYS")
            || trimmed.eq_ignore_ascii_case("GENERATED ALWAYS AS IDENTITY")
        {
            Some("ALWAYS")
        } else if trimmed.eq_ignore_ascii_case("BY DEFAULT")
            || trimmed.eq_ignore_ascii_case("GENERATED BY DEFAULT AS IDENTITY")
        {
            Some("BY DEFAULT")
        } else {
            None
        }
    }

    fn pg_enum_type_name(table: &str, col_name: &str) -> String {
        format!("{}_{}_enum", table.trim(), col_name.trim())
    }

    fn format_add_column_stmt(table: &str, spec: &ColumnSpec) -> String {
        let fq_table = Self::qualified_table(table);
        let enum_vals = parse_enum_data_type(&spec.data_type)
            .and_then(|vals| format_sql_value_list(&vals));
        let col_data_type = if enum_vals.is_some() {
            quote_pg_ident(&Self::pg_enum_type_name(table, &spec.name))
        } else {
            spec.data_type.trim().to_string()
        };

        let mut parts = vec![
            format!("ALTER TABLE {} ADD COLUMN", fq_table),
            quote_pg_ident(&spec.name),
            col_data_type,
        ];
        let identity_gen = Self::parse_identity_generation(spec.extra.as_deref());
        if !spec.is_nullable || identity_gen.is_some() {
            parts.push("NOT NULL".to_string());
        }
        if let Some(def) = &spec.default_value {
            if !def.trim().is_empty() && identity_gen.is_none() {
                parts.push(format!("DEFAULT {}", Self::format_default_expr(def)));
            }
        }
        if let Some(gen_kind) = identity_gen {
            parts.push(format!("GENERATED {} AS IDENTITY", gen_kind));
        } else if let Some(extra) = &spec.extra {
            let trimmed = extra.trim();
            if !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("None")
                && !trimmed.to_ascii_lowercase().starts_with("enum(")
            {
                parts.push(trimmed.to_string());
            }
        }
        if spec.is_primary_key {
            parts.push("PRIMARY KEY".to_string());
        }
        let add_col_sql = parts.join(" ");
        if let Some(vals_sql) = enum_vals {
            let enum_type_ident = quote_pg_ident(&Self::pg_enum_type_name(table, &spec.name));
            format!(
                "DROP TYPE IF EXISTS {};\nCREATE TYPE {} AS ENUM ({});\n{}",
                enum_type_ident, enum_type_ident, vals_sql, add_col_sql
            )
        } else {
            add_col_sql
        }
    }

    fn format_modify_column_stmt(
        table: &str,
        old_name: &str,
        existing_identity: Option<&str>,
        spec: &ColumnSpec,
    ) -> String {
        let fq_table = Self::qualified_table(table);
        let mut stmts = Vec::new();
        if old_name != spec.name {
            stmts.push(format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                fq_table,
                quote_pg_ident(old_name),
                quote_pg_ident(&spec.name)
            ));
        }
        let col_ident = quote_pg_ident(&spec.name);
        let enum_vals = parse_enum_data_type(&spec.data_type)
            .and_then(|vals| format_sql_value_list(&vals));
        if let Some(vals_sql) = enum_vals {
            let target_type_name = Self::pg_enum_type_name(table, &spec.name);
            let target_type_ident = quote_pg_ident(&target_type_name);
            let tmp_type_ident = quote_pg_ident(&format!("{}_new", target_type_name));
            stmts.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT",
                fq_table, col_ident
            ));
            stmts.push(format!("DROP TYPE IF EXISTS {}", tmp_type_ident));
            stmts.push(format!(
                "CREATE TYPE {} AS ENUM ({})",
                tmp_type_ident, vals_sql
            ));
            stmts.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} TYPE {} USING {}::text::{}",
                fq_table, col_ident, tmp_type_ident, col_ident, tmp_type_ident
            ));
            if old_name != spec.name {
                let old_type_ident = quote_pg_ident(&Self::pg_enum_type_name(table, old_name));
                stmts.push(format!("DROP TYPE IF EXISTS {}", old_type_ident));
            }
            stmts.push(format!("DROP TYPE IF EXISTS {}", target_type_ident));
            stmts.push(format!(
                "ALTER TYPE {} RENAME TO {}",
                tmp_type_ident, target_type_ident
            ));
        } else {
            let dt = spec.data_type.trim();
            stmts.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} TYPE {} USING {}::{}",
                fq_table, col_ident, dt, col_ident, dt
            ));
        }
        let old_id_gen = Self::parse_identity_generation(existing_identity);
        let new_id_gen = Self::parse_identity_generation(spec.extra.as_deref());
        if spec.is_nullable && new_id_gen.is_none() {
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
        match (old_id_gen, new_id_gen) {
            (Some(old_gen), Some(new_gen)) => {
                if !old_gen.eq_ignore_ascii_case(new_gen) {
                    stmts.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET GENERATED {}",
                        fq_table, col_ident, new_gen
                    ));
                }
            }
            (Some(_), None) => {
                stmts.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} DROP IDENTITY IF EXISTS",
                    fq_table, col_ident
                ));
                if let Some(def) = &spec.default_value {
                    if !def.trim().is_empty() {
                        stmts.push(format!(
                            "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {}",
                            fq_table,
                            col_ident,
                            Self::format_default_expr(def)
                        ));
                    }
                }
            }
            (None, Some(new_gen)) => {
                stmts.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT",
                    fq_table, col_ident
                ));
                stmts.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} ADD GENERATED {} AS IDENTITY",
                    fq_table, col_ident, new_gen
                ));
            }
            (None, None) => match &spec.default_value {
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
            },
        }
        stmts.join(";\n")
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
            } else if let Some(gen_type) = Self::parse_identity_generation(Some(&col.extra)) {
                stmts.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} ADD GENERATED {} AS IDENTITY",
                    fq_table, col_ident, gen_type
                ));
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

    fn format_create_database_stmt(spec: &CreateDatabaseSpec) -> Result<String> {
        let name = spec.name.trim();
        if name.is_empty() {
            return Err(anyhow!("Database name cannot be empty"));
        }
        let mut opts = Vec::new();
        if let Some(enc) = &spec.charset_or_encoding {
            let e = enc.trim();
            if !e.is_empty() && !e.eq_ignore_ascii_case("Default") {
                if !e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(anyhow!("Invalid encoding '{}'", e));
                }
                opts.push(format!(
                    "ENCODING = {}",
                    escape_sql_literal(&DbValue::Text(e.to_string()))
                ));
            }
        }
        if let Some(tpl) = &spec.template {
            let t = tpl.trim();
            if !t.is_empty() && !t.eq_ignore_ascii_case("Default") {
                if !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(anyhow!("Invalid template '{}'", t));
                }
                opts.push(format!("TEMPLATE = {}", quote_pg_ident(t)));
            }
        }
        if opts.is_empty() {
            Ok(format!("CREATE DATABASE {}", quote_pg_ident(name)))
        } else {
            Ok(format!(
                "CREATE DATABASE {} WITH {}",
                quote_pg_ident(name),
                opts.join(" ")
            ))
        }
    }

    fn format_role_attributes(global_privileges: &[String], with_grant_option: bool) -> String {
        let norm: Vec<String> = global_privileges
            .iter()
            .map(|s| s.trim().to_ascii_uppercase())
            .collect();
        let has = |kw: &str| norm.iter().any(|p| p == kw);

        let login = if has("LOGIN") { "LOGIN" } else { "NOLOGIN" };
        let superuser = if has("SUPERUSER") {
            "SUPERUSER"
        } else {
            "NOSUPERUSER"
        };
        let createdb = if has("CREATEDB") {
            "CREATEDB"
        } else {
            "NOCREATEDB"
        };
        let createrole = if has("CREATEROLE") || with_grant_option {
            "CREATEROLE"
        } else {
            "NOCREATEROLE"
        };
        let replication = if has("REPLICATION") {
            "REPLICATION"
        } else {
            "NOREPLICATION"
        };
        let bypassrls = if has("BYPASSRLS") {
            "BYPASSRLS"
        } else {
            "NOBYPASSRLS"
        };

        format!(
            "{} {} {} {} {} {}",
            login, superuser, createdb, createrole, replication, bypassrls
        )
    }

    fn format_create_user_stmt(spec: &CreateUserSpec) -> Result<String> {
        let user = spec.username.trim();
        if user.is_empty() {
            return Err(anyhow!("Username cannot be empty"));
        }
        let attrs = Self::format_role_attributes(&spec.global_privileges, spec.with_grant_option);
        if spec.password.is_empty() {
            Ok(format!("CREATE ROLE {} WITH {}", quote_pg_ident(user), attrs))
        } else {
            Ok(format!(
                "CREATE ROLE {} WITH {} PASSWORD {}",
                quote_pg_ident(user),
                attrs,
                escape_sql_literal(&DbValue::Text(spec.password.clone()))
            ))
        }
    }

    fn format_update_user_stmts(spec: &UpdateUserSpec) -> Result<Vec<String>> {
        let old_user = spec.old_username.trim();
        let new_user = spec.new_username.trim();
        if old_user.is_empty() || new_user.is_empty() {
            return Err(anyhow!("Username cannot be empty"));
        }
        let mut stmts = Vec::new();
        if old_user != new_user {
            stmts.push(format!(
                "ALTER ROLE {} RENAME TO {}",
                quote_pg_ident(old_user),
                quote_pg_ident(new_user)
            ));
        }
        let attrs = Self::format_role_attributes(&spec.global_privileges, spec.with_grant_option);
        if let Some(pwd) = &spec.new_password {
            if !pwd.is_empty() {
                stmts.push(format!(
                    "ALTER ROLE {} WITH {} PASSWORD {}",
                    quote_pg_ident(new_user),
                    attrs,
                    escape_sql_literal(&DbValue::Text(pwd.clone()))
                ));
                return Ok(stmts);
            }
        }
        stmts.push(format!(
            "ALTER ROLE {} WITH {}",
            quote_pg_ident(new_user),
            attrs
        ));
        Ok(stmts)
    }

    fn format_drop_user_stmt(username: &str) -> Result<String> {
        let user = username.trim();
        if user.is_empty() {
            return Err(anyhow!("Username cannot be empty"));
        }
        Ok(format!("DROP ROLE {}", quote_pg_ident(user)))
    }

    fn format_set_db_permissions_stmts(
        spec: &DbPermissionSpec,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let user = spec.username.trim();
        let db = spec.database.trim();
        if user.is_empty() {
            return Err(anyhow!("Username cannot be empty"));
        }
        if db.is_empty() {
            return Err(anyhow!("Database name cannot be empty"));
        }

        let quoted_user = quote_pg_ident(user);
        let quoted_db = quote_pg_ident(db);
        let grant_opt = if spec.with_grant_option {
            " WITH GRANT OPTION"
        } else {
            ""
        };

        let mut db_obj_privs = Vec::new();
        let mut table_privs = Vec::new();

        for p in &spec.privileges {
            let up = p.trim().to_ascii_uppercase();
            if up.is_empty() {
                continue;
            }
            if up == "ALL PRIVILEGES" || up == "ALL" {
                db_obj_privs = PG_DB_OBJECT_PRIVILEGES
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                table_privs = PG_TABLE_PRIVILEGES
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                break;
            }
            if PG_DB_OBJECT_PRIVILEGES.contains(&up.as_str()) {
                if !db_obj_privs.contains(&up) {
                    db_obj_privs.push(up);
                }
            } else if PG_TABLE_PRIVILEGES.contains(&up.as_str()) {
                if !table_privs.contains(&up) {
                    table_privs.push(up);
                }
            } else {
                return Err(anyhow!("Unsupported PostgreSQL database privilege '{}'", p));
            }
        }

        let mut db_stmts = vec![format!(
            "REVOKE ALL PRIVILEGES ON DATABASE {} FROM {}",
            quoted_db, quoted_user
        )];
        if !db_obj_privs.is_empty() {
            db_stmts.push(format!(
                "GRANT {} ON DATABASE {} TO {}{}",
                db_obj_privs.join(", "),
                quoted_db,
                quoted_user,
                grant_opt
            ));
        }

        let mut schema_stmts = vec![
            format!(
                "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA \"public\" FROM {}",
                quoted_user
            ),
            format!(
                "REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA \"public\" FROM {}",
                quoted_user
            ),
            format!(
                "ALTER DEFAULT PRIVILEGES IN SCHEMA \"public\" REVOKE ALL ON TABLES FROM {}",
                quoted_user
            ),
            format!(
                "ALTER DEFAULT PRIVILEGES IN SCHEMA \"public\" REVOKE ALL ON SEQUENCES FROM {}",
                quoted_user
            ),
        ];

        if !table_privs.is_empty() {
            schema_stmts.push(format!(
                "GRANT USAGE ON SCHEMA \"public\" TO {}{}",
                quoted_user, grant_opt
            ));
            let tbl_clause = table_privs.join(", ");
            schema_stmts.push(format!(
                "GRANT {} ON ALL TABLES IN SCHEMA \"public\" TO {}{}",
                tbl_clause, quoted_user, grant_opt
            ));
            schema_stmts.push(format!(
                "ALTER DEFAULT PRIVILEGES IN SCHEMA \"public\" GRANT {} ON TABLES TO {}{}",
                tbl_clause, quoted_user, grant_opt
            ));
            if table_privs
                .iter()
                .any(|p| matches!(p.as_str(), "SELECT" | "INSERT" | "UPDATE"))
            {
                schema_stmts.push(format!(
                    "GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA \"public\" TO {}{}",
                    quoted_user, grant_opt
                ));
                schema_stmts.push(format!(
                    "ALTER DEFAULT PRIVILEGES IN SCHEMA \"public\" GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO {}{}",
                    quoted_user, grant_opt
                ));
            }
        }

        Ok((db_stmts, schema_stmts))
    }

    fn format_revoke_db_permissions_stmts(
        username: &str,
        database: &str,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let empty_spec = DbPermissionSpec {
            username: username.to_string(),
            host: None,
            database: database.to_string(),
            privileges: Vec::new(),
            with_grant_option: false,
        };
        Self::format_set_db_permissions_stmts(&empty_spec)
    }

    pub(crate) fn build_table_ddl_parts(
        table: &str,
        columns: &[ColumnInfo],
        indexes: &[IndexInfo],
        relations: &[ForeignKeyInfo],
        include_drop: bool,
    ) -> TableDdlParts {
        let fq_table = Self::qualified_table(table);
        let mut pre_create_statements = Vec::new();
        let mut col_defs = Vec::with_capacity(columns.len());

        for col in columns {
            let enum_vals_opt =
                parse_enum_data_type(&col.extra).or_else(|| parse_enum_data_type(&col.data_type));
            let col_type = if let Some(ref vals) = enum_vals_opt {
                let type_name = if parse_enum_data_type(&col.data_type).is_some() {
                    Self::pg_enum_type_name(table, &col.name)
                } else {
                    col.data_type.trim().to_string()
                };
                if let Some(vals_sql) = format_sql_value_list(vals) {
                    let quoted_type = quote_pg_ident(&type_name);
                    pre_create_statements.push(format!(
                        "DO $$ BEGIN\n    CREATE TYPE {} AS ENUM ({});\nEXCEPTION\n    WHEN duplicate_object THEN NULL;\nEND $$;",
                        quoted_type, vals_sql
                    ));
                }
                quote_pg_ident(&type_name)
            } else {
                col.data_type.trim().to_string()
            };

            if let Some(def) = &col.default_value {
                if let Some(rest) = def.trim().strip_prefix("nextval('") {
                    if let Some(end_quote) = rest.find('\'') {
                        let raw_seq = &rest[..end_quote];
                        let seq_short = raw_seq
                            .rsplit('.')
                            .next()
                            .unwrap_or(raw_seq)
                            .trim_matches('"');
                        if !seq_short.is_empty() {
                            pre_create_statements.push(format!(
                                "CREATE SEQUENCE IF NOT EXISTS \"public\".{};",
                                quote_pg_ident(seq_short)
                            ));
                        }
                    }
                }
            }

            let mut line = format!("  {} {}", quote_pg_ident(&col.name), col_type);
            let identity_gen = Self::parse_identity_generation(Some(&col.extra));
            if !col.is_nullable || identity_gen.is_some() {
                line.push_str(" NOT NULL");
            }
            if let Some(def) = &col.default_value {
                if !def.trim().is_empty() && identity_gen.is_none() {
                    line.push_str(&format!(" DEFAULT {}", Self::format_default_expr(def)));
                }
            }
            if let Some(gen_kind) = identity_gen {
                line.push_str(&format!(" GENERATED {} AS IDENTITY", gen_kind));
            }
            col_defs.push(line);
        }

        let pk_cols: Vec<String> = if let Some(pk_idx) = indexes.iter().find(|i| i.is_primary) {
            pk_idx.columns.iter().map(|c| quote_pg_ident(c)).collect()
        } else {
            columns
                .iter()
                .filter(|c| c.is_primary_key)
                .map(|c| quote_pg_ident(&c.name))
                .collect()
        };

        if !pk_cols.is_empty() {
            col_defs.push(format!("  PRIMARY KEY ({})", pk_cols.join(", ")));
        }

        let mut create_and_indexes_sql = String::new();
        if include_drop {
            create_and_indexes_sql.push_str(&format!(
                "DROP TABLE IF EXISTS {} CASCADE;\n",
                fq_table
            ));
        }
        create_and_indexes_sql.push_str(&format!(
            "CREATE TABLE {} (\n{}\n);",
            fq_table,
            col_defs.join(",\n")
        ));

        for idx in indexes {
            if !idx.is_primary {
                let spec = IndexSpec {
                    name: idx.name.clone(),
                    columns: idx.columns.clone(),
                    index_type: idx.index_type.clone(),
                    method: Some(idx.method.clone()),
                };
                if let Ok(stmt) = Self::format_create_index_stmt(table, &spec) {
                    create_and_indexes_sql.push('\n');
                    create_and_indexes_sql.push_str(&stmt);
                    create_and_indexes_sql.push(';');
                }
            }
        }

        let mut post_data_constraints = Vec::with_capacity(relations.len());
        for fk in relations {
            let fq_ref_table = Self::qualified_table(&fk.referenced_table);
            let constraint_part = if fk.constraint_name.trim().is_empty() {
                String::new()
            } else {
                format!("CONSTRAINT {} ", quote_pg_ident(fk.constraint_name.trim()))
            };
            post_data_constraints.push(format!(
                "ALTER TABLE {} ADD {}FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {} ON UPDATE {};",
                fq_table,
                constraint_part,
                quote_pg_ident(&fk.column_name),
                fq_ref_table,
                quote_pg_ident(&fk.referenced_column),
                fk.delete_rule,
                fk.update_rule
            ));
        }

        TableDdlParts {
            pre_create_statements,
            create_and_indexes_sql,
            post_data_constraints,
        }
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
    fn engine(&self) -> DbEngine {
        DbEngine::Postgres
    }

    async fn list_databases(&self) -> Result<Vec<String>> {
        let query = "SELECT datname FROM pg_database WHERE datistemplate = false ORDER BY datname";
        let pool = self.pool_for_db("").await?;
        let rows = match sqlx::raw_sql(query).fetch_all(&pool).await {
            Ok(rows) => rows,
            Err(_) => {
                let fallback_db = "postgres";
                let fallback_pool = match Self::create_pool(
                    &self.host,
                    self.port,
                    &self.username,
                    &self.password,
                    fallback_db,
                )
                .await
                {
                    Ok(p) => (fallback_db.to_string(), p),
                    Err(first_err) if self.username != "postgres" => {
                        let p = Self::create_pool(
                            &self.host,
                            self.port,
                            &self.username,
                            &self.password,
                            &self.username,
                        )
                        .await
                        .map_err(|_| first_err)?;
                        (self.username.clone(), p)
                    }
                    Err(e) => return Err(e),
                };
                let rows = sqlx::raw_sql(query)
                    .fetch_all(&fallback_pool.1)
                    .await
                    .context("Failed to list PostgreSQL databases")?;
                *self.active_pool.write().await = fallback_pool;
                rows
            }
        };

        let mut dbs = Vec::with_capacity(rows.len());
        for row in rows {
            if let DbValue::Text(name) = decode_pg_cell(&row, 0) {
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
            .with_context(|| format!("Failed to create PostgreSQL database '{}'", spec.name))?;
        Ok(())
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
            ColumnAlterOp::Add(spec) => Self::format_add_column_stmt(table, &spec),
            ColumnAlterOp::Modify { old_name, spec } => {
                let id_sql = format!(
                    "SELECT identity_generation \
                     FROM information_schema.columns \
                     WHERE table_schema = 'public' \
                       AND table_name = {} \
                       AND column_name = {}",
                    escape_sql_literal(&DbValue::Text(table.to_string())),
                    escape_sql_literal(&DbValue::Text(old_name.clone()))
                );
                let existing_identity = sqlx::raw_sql(&id_sql)
                    .fetch_all(&pool)
                    .await
                    .ok()
                    .and_then(|rows| {
                        rows.first().and_then(|r| match decode_pg_cell(r, 0) {
                            DbValue::Null => None,
                            DbValue::Text(s) if s.trim().is_empty() => None,
                            DbValue::Text(s) => Some(s),
                        })
                    });
                Self::format_modify_column_stmt(
                    table,
                    &old_name,
                    existing_identity.as_deref(),
                    &spec,
                )
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

    async fn list_users(&self) -> Result<Vec<DbUser>> {
        let pool = self.pool_for_db("").await?;
        let sql = "SELECT rolname, rolsuper, rolcreatedb, rolcreaterole, rolcanlogin, rolreplication, rolbypassrls \
                   FROM pg_catalog.pg_roles \
                   WHERE rolname NOT LIKE 'pg_%' \
                   ORDER BY rolname";
        let rows = sqlx::raw_sql(sql)
            .fetch_all(&pool)
            .await
            .context("Failed to list PostgreSQL roles")?;

        let mut users = Vec::with_capacity(rows.len());
        for row in rows {
            let username = decode_pg_cell(&row, 0).display_text().trim().to_string();
            if username.is_empty() {
                continue;
            }
            let is_true = |idx: usize| {
                let val = decode_pg_cell(&row, idx).display_text().to_ascii_lowercase();
                val == "true" || val == "t" || val == "1"
            };
            let rolsuper = is_true(1);
            let rolcreatedb = is_true(2);
            let rolcreaterole = is_true(3);
            let rolcanlogin = is_true(4);
            let rolreplication = is_true(5);
            let rolbypassrls = is_true(6);

            let mut global_privileges = Vec::new();
            if rolcanlogin {
                global_privileges.push("LOGIN".to_string());
            }
            if rolsuper {
                global_privileges.push("SUPERUSER".to_string());
            }
            if rolcreatedb {
                global_privileges.push("CREATEDB".to_string());
            }
            if rolcreaterole {
                global_privileges.push("CREATEROLE".to_string());
            }
            if rolreplication {
                global_privileges.push("REPLICATION".to_string());
            }
            if rolbypassrls {
                global_privileges.push("BYPASSRLS".to_string());
            }

            users.push(DbUser {
                username,
                host: None,
                global_privileges,
                has_grant_option: rolcreaterole || rolsuper,
            });
        }
        Ok(users)
    }

    async fn create_user(&self, spec: &CreateUserSpec) -> Result<()> {
        let pool = self.pool_for_db("").await?;
        let sql = Self::format_create_user_stmt(spec)?;
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to create role: {}", sql))?;
        Ok(())
    }

    async fn update_user(&self, spec: &UpdateUserSpec) -> Result<()> {
        let pool = self.pool_for_db("").await?;
        let stmts = Self::format_update_user_stmts(spec)?;
        for sql in stmts {
            sqlx::raw_sql(&sql)
                .execute(&pool)
                .await
                .with_context(|| format!("Failed to update role: {}", sql))?;
        }
        Ok(())
    }

    async fn drop_user(&self, username: &str, _host: Option<&str>) -> Result<()> {
        let pool = self.pool_for_db("").await?;
        let sql = Self::format_drop_user_stmt(username)?;
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to drop role: {}", sql))?;
        Ok(())
    }

    async fn get_database_permissions(
        &self,
        username: &str,
        _host: Option<&str>,
    ) -> Result<Vec<DatabasePermission>> {
        use std::collections::BTreeMap;

        let user = username.trim();
        if user.is_empty() {
            return Ok(Vec::new());
        }
        let escaped_user = escape_sql_literal(&DbValue::Text(user.to_string()));
        let pool = self.pool_for_db("").await?;

        let mut db_map: BTreeMap<String, (Vec<String>, bool)> = BTreeMap::new();

        // 1. Database-level explicit grants (CONNECT, CREATE, TEMPORARY)
        let db_acl_sql = format!(
            "SELECT d.datname, a.privilege_type, a.is_grantable \
             FROM pg_catalog.pg_database d \
             CROSS JOIN LATERAL aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) a \
             JOIN pg_catalog.pg_roles r ON r.oid = a.grantee \
             WHERE d.datistemplate = false \
               AND r.rolname = {} \
             ORDER BY d.datname",
            escaped_user
        );
        if let Ok(rows) = sqlx::raw_sql(&db_acl_sql).fetch_all(&pool).await {
            for row in rows {
                let db_name = decode_pg_cell(&row, 0).display_text().trim().to_string();
                let priv_type = decode_pg_cell(&row, 1)
                    .display_text()
                    .trim()
                    .to_ascii_uppercase();
                let grantable_str = decode_pg_cell(&row, 2)
                    .display_text()
                    .trim()
                    .to_ascii_lowercase();
                let is_grantable =
                    grantable_str == "true" || grantable_str == "t" || grantable_str == "yes";

                if db_name.is_empty() || !PG_DB_PRIVILEGES.contains(&priv_type.as_str()) {
                    continue;
                }
                let entry = db_map
                    .entry(db_name)
                    .or_insert_with(|| (Vec::new(), false));
                if is_grantable {
                    entry.1 = true;
                }
                if !entry.0.contains(&priv_type) {
                    entry.0.push(priv_type);
                }
            }
        }

        // 2. Public schema table and default table grants per database
        let databases = self.list_databases().await.unwrap_or_default();
        let tbl_grants_sql = format!(
            "SELECT DISTINCT privilege_type, is_grantable \
             FROM information_schema.role_table_grants \
             WHERE grantee = {u} AND table_schema = 'public' \
             UNION \
             SELECT DISTINCT a.privilege_type, CASE WHEN a.is_grantable THEN 'YES' ELSE 'NO' END \
             FROM pg_catalog.pg_default_acl d \
             JOIN pg_catalog.pg_namespace n ON n.oid = d.defaclnamespace \
             CROSS JOIN LATERAL aclexplode(d.defaclacl) a \
             JOIN pg_catalog.pg_roles r ON r.oid = a.grantee \
             WHERE n.nspname = 'public' AND d.defaclobjtype = 'r' AND r.rolname = {u}",
            u = escaped_user
        );

        for db_name in databases {
            if let Ok(db_pool) = self.pool_for_db(&db_name).await {
                if let Ok(rows) = sqlx::raw_sql(&tbl_grants_sql).fetch_all(&db_pool).await {
                    for row in rows {
                        let priv_type = decode_pg_cell(&row, 0)
                            .display_text()
                            .trim()
                            .to_ascii_uppercase();
                        let grantable_str = decode_pg_cell(&row, 1)
                            .display_text()
                            .trim()
                            .to_ascii_lowercase();
                        let is_grantable = grantable_str == "yes"
                            || grantable_str == "true"
                            || grantable_str == "t";

                        if !PG_DB_PRIVILEGES.contains(&priv_type.as_str()) {
                            continue;
                        }
                        let entry = db_map
                            .entry(db_name.clone())
                            .or_insert_with(|| (Vec::new(), false));
                        if is_grantable {
                            entry.1 = true;
                        }
                        if !entry.0.contains(&priv_type) {
                            entry.0.push(priv_type);
                        }
                    }
                }
            }
        }

        let out = db_map
            .into_iter()
            .filter(|(_, (privs, _))| !privs.is_empty())
            .map(|(database, (mut privileges, has_grant_option))| {
                privileges.sort_by_key(|p| {
                    PG_DB_PRIVILEGES
                        .iter()
                        .position(|&k| k == p.as_str())
                        .unwrap_or(usize::MAX)
                });
                DatabasePermission {
                    username: user.to_string(),
                    host: None,
                    database,
                    privileges,
                    has_grant_option,
                }
            })
            .collect();

        Ok(out)
    }

    async fn set_database_permissions(&self, spec: &DbPermissionSpec) -> Result<()> {
        let (db_stmts, schema_stmts) = Self::format_set_db_permissions_stmts(spec)?;
        let cluster_pool = self.pool_for_db("").await?;
        for sql in db_stmts {
            sqlx::raw_sql(&sql)
                .execute(&cluster_pool)
                .await
                .with_context(|| format!("Failed to execute: {}", sql))?;
        }
        let target_pool = self.pool_for_db(&spec.database).await?;
        for sql in schema_stmts {
            sqlx::raw_sql(&sql)
                .execute(&target_pool)
                .await
                .with_context(|| format!("Failed to execute on '{}': {}", spec.database, sql))?;
        }
        Ok(())
    }

    async fn revoke_database_permissions(
        &self,
        username: &str,
        _host: Option<&str>,
        database: &str,
    ) -> Result<()> {
        let (db_stmts, schema_stmts) = Self::format_revoke_db_permissions_stmts(username, database)?;
        let cluster_pool = self.pool_for_db("").await?;
        for sql in db_stmts {
            sqlx::raw_sql(&sql)
                .execute(&cluster_pool)
                .await
                .with_context(|| format!("Failed to execute: {}", sql))?;
        }
        if let Ok(target_pool) = self.pool_for_db(database).await {
            for sql in schema_stmts {
                let _ = sqlx::raw_sql(&sql).execute(&target_pool).await;
            }
        }
        Ok(())
    }

    async fn get_create_database_ddl(&self, db_name: &str) -> Result<String> {
        let quoted = quote_pg_ident(db_name);
        Ok(format!(
            "-- DBASED_DATABASE: {}\nCREATE DATABASE {};\n\\connect {}",
            db_name, quoted, quoted
        ))
    }

    async fn get_table_ddl(
        &self,
        db_name: &str,
        table: &str,
        include_drop: bool,
    ) -> Result<TableDdlParts> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(table);
        let escaped_table = escape_sql_literal(&DbValue::Text(table.to_string()));
        let view_sql = format!(
            "SELECT pg_get_viewdef(c.oid, true) \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relname = {} AND c.relkind = 'v'",
            escaped_table
        );
        if let Ok(rows) = sqlx::raw_sql(&view_sql).fetch_all(&pool).await {
            if let Some(first) = rows.first() {
                if let DbValue::Text(view_def) = decode_pg_cell(first, 0) {
                    let mut create_and_indexes_sql = String::new();
                    if include_drop {
                        create_and_indexes_sql.push_str(&format!(
                            "DROP VIEW IF EXISTS {} CASCADE;\n",
                            fq_table
                        ));
                    }
                    create_and_indexes_sql.push_str(&format!(
                        "CREATE OR REPLACE VIEW {} AS\n{};",
                        fq_table,
                        view_def.trim().trim_end_matches(';')
                    ));
                    return Ok(TableDdlParts {
                        pre_create_statements: Vec::new(),
                        create_and_indexes_sql,
                        post_data_constraints: Vec::new(),
                    });
                }
            }
        }

        let columns = self.get_table_structure(db_name, table).await?;
        let indexes = self.get_table_indexes(db_name, table).await?;
        let relations = self.get_relations(db_name, table).await?;
        Ok(Self::build_table_ddl_parts(
            table,
            &columns,
            &indexes,
            &relations,
            include_drop,
        ))
    }

    async fn fetch_table_rows_batch(
        &self,
        db_name: &str,
        table: &str,
        col_count: usize,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<Vec<DbValue>>> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(table);
        let sql = format!("SELECT * FROM {} LIMIT {} OFFSET {}", fq_table, limit, offset);
        let raw_rows = sqlx::raw_sql(&sql)
            .fetch_all(&pool)
            .await
            .with_context(|| format!("Failed to fetch rows from {}", fq_table))?;

        let mut rows = Vec::with_capacity(raw_rows.len());
        for raw_row in raw_rows {
            let n = if col_count > 0 {
                col_count
            } else {
                raw_row.columns().len()
            };
            let mut vals = Vec::with_capacity(n);
            for idx in 0..n {
                vals.push(decode_pg_cell(&raw_row, idx));
            }
            rows.push(vals);
        }
        Ok(rows)
    }

    async fn truncate_table(&self, db_name: &str, table: &str) -> Result<()> {
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(table);
        let sql = format!("TRUNCATE TABLE {} RESTART IDENTITY CASCADE", fq_table);
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to truncate table {}", fq_table))?;
        Ok(())
    }

    async fn insert_rows_batch(
        &self,
        db_name: &str,
        table: &str,
        columns: &[String],
        rows: &[Vec<DbValue>],
    ) -> Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let pool = self.pool_for_db(db_name).await?;
        let fq_table = Self::qualified_table(table);
        let quoted_cols: Vec<String> = columns.iter().map(|c| quote_pg_ident(c)).collect();
        let mut tuple_list = Vec::with_capacity(rows.len());
        for row in rows {
            let vals: Vec<String> = row
                .iter()
                .map(|v| escape_sql_literal_for_engine(DbEngine::Postgres, v))
                .collect();
            tuple_list.push(format!("({})", vals.join(", ")));
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES {}",
            fq_table,
            quoted_cols.join(", "),
            tuple_list.join(", ")
        );
        let res = sqlx::raw_sql(&sql).execute(&pool).await.map_err(|e| {
            anyhow!(
                "Failed to insert batch into {}: {}\n\nStatement: {}",
                fq_table,
                e,
                truncate_sql_preview(&sql, 240)
            )
        })?;
        Ok(res.rows_affected())
    }

    async fn execute_sql_statement(&self, db_name: &str, sql: &str) -> Result<()> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let upper = trimmed.to_ascii_uppercase();
        if upper.starts_with("CREATE DATABASE ") {
            let normalized = if upper.starts_with("CREATE DATABASE IF NOT EXISTS ") {
                format!(
                    "CREATE DATABASE {}",
                    trimmed["CREATE DATABASE IF NOT EXISTS ".len()..].trim()
                )
            } else {
                trimmed.to_string()
            };
            let pool = self.pool_for_db("").await?;
            if let Err(e) = sqlx::raw_sql(&normalized).execute(&pool).await {
                let err_str = format!("{:#}", e).to_ascii_lowercase();
                if err_str.contains("already exists") || err_str.contains("42p04") {
                    return Ok(());
                }
                return Err(anyhow!(
                    "Failed to create database: {}\n\nStatement: {}",
                    e,
                    truncate_sql_preview(&normalized, 240)
                ));
            }
            return Ok(());
        }
        let pool = self.pool_for_db(db_name).await?;
        sqlx::raw_sql(trimmed).execute(&pool).await.map_err(|e| {
            anyhow!(
                "SQL error in database '{}': {}\n\nStatement: {}",
                if db_name.is_empty() { "(default)" } else { db_name },
                e,
                truncate_sql_preview(trimmed, 240)
            )
        })?;
        Ok(())
    }

    async fn execute_sql_batch(
        &self,
        db_name: &str,
        statements: &[String],
        _legacy_no_backslash_escapes: bool,
    ) -> Result<()> {
        for stmt in statements {
            self.execute_sql_statement(db_name, stmt).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pg_build_table_ddl_parts() {
        let cols = vec![
            ColumnInfo {
                ordinal: 1,
                name: "id".to_string(),
                data_type: "int8".to_string(),
                is_nullable: false,
                is_primary_key: true,
                default_value: None,
                extra: "BY DEFAULT".to_string(),
            },
            ColumnInfo {
                ordinal: 2,
                name: "role".to_string(),
                data_type: "user_role_enum".to_string(),
                is_nullable: false,
                is_primary_key: false,
                default_value: Some("'member'::user_role_enum".to_string()),
                extra: "enum('admin','member')".to_string(),
            },
            ColumnInfo {
                ordinal: 3,
                name: "org_id".to_string(),
                data_type: "int8".to_string(),
                is_nullable: true,
                is_primary_key: false,
                default_value: None,
                extra: String::new(),
            },
        ];
        let indexes = vec![
            IndexInfo {
                name: "users_pkey".to_string(),
                columns: vec!["id".to_string()],
                is_unique: true,
                is_primary: true,
                index_type: "PRIMARY".to_string(),
                method: "BTREE".to_string(),
            },
            IndexInfo {
                name: "idx_users_org".to_string(),
                columns: vec!["org_id".to_string()],
                is_unique: false,
                is_primary: false,
                index_type: "INDEX".to_string(),
                method: "BTREE".to_string(),
            },
        ];
        let fks = vec![ForeignKeyInfo {
            constraint_name: "fk_users_org".to_string(),
            column_name: "org_id".to_string(),
            referenced_table: "orgs".to_string(),
            referenced_column: "id".to_string(),
            update_rule: "CASCADE".to_string(),
            delete_rule: "SET NULL".to_string(),
        }];

        let parts = PostgresDriver::build_table_ddl_parts("users", &cols, &indexes, &fks, true);
        assert_eq!(parts.pre_create_statements.len(), 1);
        assert!(parts.pre_create_statements[0].contains("CREATE TYPE \"user_role_enum\" AS ENUM ('admin', 'member')"));
        assert!(parts.create_and_indexes_sql.contains("DROP TABLE IF EXISTS \"public\".\"users\" CASCADE;"));
        assert!(parts.create_and_indexes_sql.contains("\"id\" int8 NOT NULL GENERATED BY DEFAULT AS IDENTITY"));
        assert!(parts.create_and_indexes_sql.contains("PRIMARY KEY (\"id\")"));
        assert!(parts.create_and_indexes_sql.contains("CREATE INDEX \"idx_users_org\" ON \"public\".\"users\" USING btree (\"org_id\");"));
        assert_eq!(parts.post_data_constraints.len(), 1);
        assert_eq!(
            parts.post_data_constraints[0],
            "ALTER TABLE \"public\".\"users\" ADD CONSTRAINT \"fk_users_org\" FOREIGN KEY (\"org_id\") REFERENCES \"public\".\"orgs\" (\"id\") ON DELETE SET NULL ON UPDATE CASCADE;"
        );
    }

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

    #[test]
    fn test_pg_column_sql_formatting_with_extra() {
        let add_id = ColumnSpec {
            name: "id".to_string(),
            data_type: "BIGINT".to_string(),
            is_nullable: false,
            default_value: None,
            extra: Some("GENERATED BY DEFAULT AS IDENTITY".to_string()),
            is_primary_key: true,
        };
        assert_eq!(
            PostgresDriver::format_add_column_stmt("orders", &add_id),
            "ALTER TABLE \"public\".\"orders\" ADD COLUMN \"id\" BIGINT NOT NULL GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY"
        );

        let mod_to_always = ColumnSpec {
            name: "id".to_string(),
            data_type: "BIGINT".to_string(),
            is_nullable: false,
            default_value: None,
            extra: Some("GENERATED ALWAYS AS IDENTITY".to_string()),
            is_primary_key: true,
        };
        assert_eq!(
            PostgresDriver::format_modify_column_stmt("orders", "id", Some("BY DEFAULT"), &mod_to_always),
            "ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"id\" TYPE BIGINT USING \"id\"::BIGINT;\n\
             ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"id\" SET NOT NULL;\n\
             ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"id\" SET GENERATED ALWAYS"
        );

        let mod_drop_identity = ColumnSpec {
            name: "id".to_string(),
            data_type: "BIGINT".to_string(),
            is_nullable: false,
            default_value: None,
            extra: None,
            is_primary_key: true,
        };
        assert_eq!(
            PostgresDriver::format_modify_column_stmt("orders", "id", Some("ALWAYS"), &mod_drop_identity),
            "ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"id\" TYPE BIGINT USING \"id\"::BIGINT;\n\
             ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"id\" SET NOT NULL;\n\
             ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"id\" DROP IDENTITY IF EXISTS"
        );

        let add_enum = ColumnSpec {
            name: "status".to_string(),
            data_type: "ENUM('pending', 'shipped', 'delivered')".to_string(),
            is_nullable: false,
            default_value: Some("pending".to_string()),
            extra: None,
            is_primary_key: false,
        };
        assert_eq!(
            PostgresDriver::format_add_column_stmt("orders", &add_enum),
            "DROP TYPE IF EXISTS \"orders_status_enum\";\n\
             CREATE TYPE \"orders_status_enum\" AS ENUM ('pending', 'shipped', 'delivered');\n\
             ALTER TABLE \"public\".\"orders\" ADD COLUMN \"status\" \"orders_status_enum\" NOT NULL DEFAULT 'pending'"
        );

        let mod_enum = ColumnSpec {
            name: "status".to_string(),
            data_type: "ENUM('pending', 'processing', 'shipped')".to_string(),
            is_nullable: true,
            default_value: Some("processing".to_string()),
            extra: None,
            is_primary_key: false,
        };
        assert_eq!(
            PostgresDriver::format_modify_column_stmt("orders", "status", None, &mod_enum),
            "ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"status\" DROP DEFAULT;\n\
             DROP TYPE IF EXISTS \"orders_status_enum_new\";\n\
             CREATE TYPE \"orders_status_enum_new\" AS ENUM ('pending', 'processing', 'shipped');\n\
             ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"status\" TYPE \"orders_status_enum_new\" USING \"status\"::text::\"orders_status_enum_new\";\n\
             DROP TYPE IF EXISTS \"orders_status_enum\";\n\
             ALTER TYPE \"orders_status_enum_new\" RENAME TO \"orders_status_enum\";\n\
             ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"status\" DROP NOT NULL;\n\
             ALTER TABLE \"public\".\"orders\" ALTER COLUMN \"status\" SET DEFAULT 'processing'"
        );
    }

    #[test]
    fn test_pg_create_database_sql_formatting() {
        let basic = CreateDatabaseSpec {
            name: "billing_prod".to_string(),
            charset_or_encoding: None,
            collation: None,
            template: None,
        };
        assert_eq!(
            PostgresDriver::format_create_database_stmt(&basic).unwrap(),
            "CREATE DATABASE \"billing_prod\""
        );

        let with_enc_and_template = CreateDatabaseSpec {
            name: "my\"db".to_string(),
            charset_or_encoding: Some("UTF8".to_string()),
            collation: None,
            template: Some("template0".to_string()),
        };
        assert_eq!(
            PostgresDriver::format_create_database_stmt(&with_enc_and_template).unwrap(),
            "CREATE DATABASE \"my\"\"db\" WITH ENCODING = 'UTF8' TEMPLATE = \"template0\""
        );

        let empty = CreateDatabaseSpec {
            name: " ".to_string(),
            charset_or_encoding: None,
            collation: None,
            template: None,
        };
        assert!(PostgresDriver::format_create_database_stmt(&empty).is_err());
    }

    #[test]
    fn test_pg_user_and_permission_sql_formatting() {
        let create_spec = CreateUserSpec {
            username: "analytics_ro".to_string(),
            host: None,
            password: " secret ".to_string(),
            global_privileges: vec!["LOGIN".to_string(), "CREATEDB".to_string()],
            with_grant_option: false,
        };
        assert_eq!(
            PostgresDriver::format_create_user_stmt(&create_spec).unwrap(),
            "CREATE ROLE \"analytics_ro\" WITH LOGIN NOSUPERUSER CREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD ' secret '"
        );

        let update_spec = UpdateUserSpec {
            old_username: "analytics_ro".to_string(),
            old_host: None,
            new_username: "analytics_admin".to_string(),
            new_host: None,
            new_password: None,
            global_privileges: vec!["LOGIN".to_string(), "SUPERUSER".to_string()],
            with_grant_option: true,
        };
        assert_eq!(
            PostgresDriver::format_update_user_stmts(&update_spec).unwrap(),
            vec![
                "ALTER ROLE \"analytics_ro\" RENAME TO \"analytics_admin\"",
                "ALTER ROLE \"analytics_admin\" WITH LOGIN SUPERUSER NOCREATEDB CREATEROLE NOREPLICATION NOBYPASSRLS"
            ]
        );

        let db_perm = DbPermissionSpec {
            username: "analytics_ro".to_string(),
            host: None,
            database: "warehouse".to_string(),
            privileges: vec!["CONNECT".to_string(), "SELECT".to_string()],
            with_grant_option: false,
        };
        let (db_stmts, schema_stmts) =
            PostgresDriver::format_set_db_permissions_stmts(&db_perm).unwrap();
        assert_eq!(
            db_stmts,
            vec![
                "REVOKE ALL PRIVILEGES ON DATABASE \"warehouse\" FROM \"analytics_ro\"",
                "GRANT CONNECT ON DATABASE \"warehouse\" TO \"analytics_ro\""
            ]
        );
        assert_eq!(
            schema_stmts,
            vec![
                "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA \"public\" FROM \"analytics_ro\"",
                "REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA \"public\" FROM \"analytics_ro\"",
                "ALTER DEFAULT PRIVILEGES IN SCHEMA \"public\" REVOKE ALL ON TABLES FROM \"analytics_ro\"",
                "ALTER DEFAULT PRIVILEGES IN SCHEMA \"public\" REVOKE ALL ON SEQUENCES FROM \"analytics_ro\"",
                "GRANT USAGE ON SCHEMA \"public\" TO \"analytics_ro\"",
                "GRANT SELECT ON ALL TABLES IN SCHEMA \"public\" TO \"analytics_ro\"",
                "ALTER DEFAULT PRIVILEGES IN SCHEMA \"public\" GRANT SELECT ON TABLES TO \"analytics_ro\"",
                "GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA \"public\" TO \"analytics_ro\"",
                "ALTER DEFAULT PRIVILEGES IN SCHEMA \"public\" GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO \"analytics_ro\""
            ]
        );
    }
}
