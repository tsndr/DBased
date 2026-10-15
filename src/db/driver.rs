use crate::config::bookmarks::DbEngine;
use crate::db::models::*;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait DatabaseDriver: Send + Sync {
    fn engine(&self) -> DbEngine;
    async fn list_databases(&self) -> Result<Vec<String>>;
    async fn create_database(&self, spec: &CreateDatabaseSpec) -> Result<()>;
    async fn use_database(&mut self, db_name: &str) -> Result<()>;
    async fn list_tables(&self, db_name: &str) -> Result<Vec<TableInfo>>;
    async fn get_table_structure(&self, db_name: &str, table: &str) -> Result<Vec<ColumnInfo>>;
    async fn alter_column(&self, db_name: &str, table: &str, op: ColumnAlterOp) -> Result<()>;
    async fn get_table_indexes(&self, db_name: &str, table: &str) -> Result<Vec<IndexInfo>>;
    async fn alter_index(&self, db_name: &str, table: &str, op: IndexAlterOp) -> Result<()>;
    async fn get_table_content(
        &self,
        db_name: &str,
        table: &str,
        query: &ContentQuery,
    ) -> Result<TablePage>;
    async fn commit_cell_updates(
        &self,
        db_name: &str,
        table: &str,
        updates: &[RowUpdate],
    ) -> Result<u64>;
    async fn insert_row(
        &self,
        db_name: &str,
        table: &str,
        values: &[(String, DbValue)],
    ) -> Result<()>;
    async fn delete_rows(
        &self,
        db_name: &str,
        table: &str,
        pks: &[PrimaryKeyValue],
    ) -> Result<u64>;
    async fn get_relations(&self, db_name: &str, table: &str) -> Result<Vec<ForeignKeyInfo>>;
    async fn add_relation(&self, db_name: &str, table: &str, fk: &NewForeignKey) -> Result<()>;
    async fn drop_relation(
        &self,
        db_name: &str,
        table: &str,
        constraint_name: &str,
    ) -> Result<()>;
    async fn execute_raw_query(&self, db_name: &str, sql: &str) -> Result<RawQueryOutcome>;
    async fn list_users(&self) -> Result<Vec<DbUser>>;
    async fn create_user(&self, spec: &CreateUserSpec) -> Result<()>;
    async fn update_user(&self, spec: &UpdateUserSpec) -> Result<()>;
    async fn drop_user(&self, username: &str, host: Option<&str>) -> Result<()>;
    async fn get_database_permissions(
        &self,
        username: &str,
        host: Option<&str>,
    ) -> Result<Vec<DatabasePermission>>;
    async fn set_database_permissions(&self, spec: &DbPermissionSpec) -> Result<()>;
    async fn revoke_database_permissions(
        &self,
        username: &str,
        host: Option<&str>,
        database: &str,
    ) -> Result<()>;
    async fn get_create_database_ddl(&self, db_name: &str) -> Result<String>;
    async fn get_table_ddl(
        &self,
        db_name: &str,
        table: &str,
        include_drop: bool,
    ) -> Result<TableDdlParts>;
    async fn fetch_table_rows_batch(
        &self,
        db_name: &str,
        table: &str,
        col_count: usize,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<Vec<DbValue>>>;
    async fn truncate_table(&self, db_name: &str, table: &str) -> Result<()>;
    async fn insert_rows_batch(
        &self,
        db_name: &str,
        table: &str,
        columns: &[String],
        rows: &[Vec<DbValue>],
    ) -> Result<u64>;
    async fn execute_sql_statement(&self, db_name: &str, sql: &str) -> Result<()>;
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

/// Quote a MySQL identifier with backticks (`ident`).
pub fn quote_mysql_ident(ident: &str) -> String {
    format!("`{}`", ident.replace('`', "``"))
}

/// Quote a PostgreSQL identifier with double quotes ("ident").
pub fn quote_pg_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Escape a SQL string literal value safely (standard SQL / PostgreSQL).
pub fn escape_sql_literal(val: &DbValue) -> String {
    escape_sql_literal_for_engine(DbEngine::Postgres, val)
}

/// Escape a SQL string literal value safely for the given database engine.
/// MySQL treats `\` as an escape character inside string literals by default,
/// whereas PostgreSQL (`standard_conforming_strings = on`) treats `\` literally.
pub fn escape_sql_literal_for_engine(engine: DbEngine, val: &DbValue) -> String {
    match val {
        DbValue::Null => "NULL".to_string(),
        DbValue::Text(s) => match engine {
            DbEngine::MySql => {
                let mut escaped = String::with_capacity(s.len() + 8);
                for ch in s.chars() {
                    match ch {
                        '\\' => escaped.push_str("\\\\"),
                        '\'' => escaped.push_str("''"),
                        '\0' => escaped.push_str("\\0"),
                        _ => escaped.push(ch),
                    }
                }
                format!("'{}'", escaped)
            }
            DbEngine::Postgres => {
                let escaped = s.replace('\'', "''");
                format!("'{}'", escaped)
            }
        },
    }
}

/// Truncates a potentially large SQL statement into a compact single-line preview for error dialogs.
pub fn truncate_sql_preview(sql: &str, max_chars: usize) -> String {
    let collapsed: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let prefix: String = collapsed.chars().take(max_chars).collect();
        format!("{}...", prefix)
    }
}

/// Heuristic check if a SQL statement returns rows (e.g. SELECT, SHOW, DESCRIBE, EXPLAIN, WITH, TABLE, VALUES).
pub fn is_row_returning_query(sql: &str) -> bool {
    let trimmed = sql
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join(" ");
    let first_word = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        first_word.as_str(),
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "WITH" | "TABLE" | "VALUES"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ident_quoting() {
        assert_eq!(quote_mysql_ident("users"), "`users`");
        assert_eq!(quote_mysql_ident("my`table"), "`my``table`");
        assert_eq!(quote_pg_ident("users"), "\"users\"");
        assert_eq!(quote_pg_ident("my\"table"), "\"my\"\"table\"");
    }

    #[test]
    fn test_sql_literal_escaping() {
        assert_eq!(escape_sql_literal(&DbValue::Null), "NULL");
        assert_eq!(
            escape_sql_literal(&DbValue::Text("O'Reilly".to_string())),
            "'O''Reilly'"
        );
        assert_eq!(
            escape_sql_literal_for_engine(
                DbEngine::MySql,
                &DbValue::Text(r"$escape = @$argf['e'] ?: '\\';".to_string())
            ),
            r"'$escape = @$argf[''e''] ?: ''\\\\'';'"
        );
        assert_eq!(
            escape_sql_literal_for_engine(
                DbEngine::Postgres,
                &DbValue::Text(r"$escape = @$argf['e'] ?: '\\';".to_string())
            ),
            r"'$escape = @$argf[''e''] ?: ''\\'';'"
        );
        assert_eq!(
            escape_sql_literal_for_engine(
                DbEngine::MySql,
                &DbValue::Text("trailing\\\0null".to_string())
            ),
            "'trailing\\\\\\0null'"
        );
    }

    #[test]
    fn test_truncate_sql_preview() {
        let short = "SELECT   *  \n  FROM   users;";
        assert_eq!(truncate_sql_preview(short, 100), "SELECT * FROM users;");

        let long = format!("INSERT INTO `items` VALUES ('{}');", "x".repeat(500));
        let preview = truncate_sql_preview(&long, 40);
        assert!(preview.ends_with("..."));
        assert_eq!(preview.chars().count(), 43);
    }

    #[test]
    fn test_is_row_returning_query() {
        assert!(is_row_returning_query("SELECT * FROM users;"));
        assert!(is_row_returning_query("-- comment\n  select id from items"));
        assert!(is_row_returning_query("SHOW TABLES"));
        assert!(!is_row_returning_query("UPDATE users SET name = 'Bob'"));
        assert!(!is_row_returning_query("DELETE FROM users WHERE id = 1"));
    }
}
