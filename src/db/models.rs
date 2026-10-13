use crate::config::bookmarks::ConnectionProfile;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub default_value: Option<String>,
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

/// Commands sent from the GTK4 UI thread to the background Tokio database worker.
#[derive(Debug, Clone)]
pub enum BackendCommand {
    TestConnection(ConnectionProfile),
    Connect(ConnectionProfile),
    Disconnect,
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
}

/// Events sent from the background Tokio database worker back to the GTK4 UI thread.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    ConnectionTestResult(Result<String, String>),
    Connected {
        connection_name: String,
        engine_label: String,
        databases: Vec<String>,
        selected_database: Option<String>,
        tables: Vec<TableInfo>,
    },
    Disconnected,
    DatabaseSelected {
        database: String,
        tables: Vec<TableInfo>,
    },
    TableLoaded {
        database: String,
        table: String,
        all_tables: Vec<String>,
        structure: Vec<ColumnInfo>,
        content: TablePage,
        relations: Vec<ForeignKeyInfo>,
        content_query: ContentQuery,
    },
    StructureUpdated {
        database: String,
        table: String,
        structure: Vec<ColumnInfo>,
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
    Error(String),
    InfoToast(String),
}
