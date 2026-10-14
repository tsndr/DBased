use crate::db::models::*;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait DatabaseDriver: Send + Sync {
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
}

/// Quote a MySQL identifier with backticks (`ident`).
pub fn quote_mysql_ident(ident: &str) -> String {
    format!("`{}`", ident.replace('`', "``"))
}

/// Quote a PostgreSQL identifier with double quotes ("ident").
pub fn quote_pg_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Escape a SQL string literal value safely.
pub fn escape_sql_literal(val: &DbValue) -> String {
    match val {
        DbValue::Null => "NULL".to_string(),
        DbValue::Text(s) => {
            let escaped = s.replace('\'', "''");
            format!("'{}'", escaped)
        }
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
