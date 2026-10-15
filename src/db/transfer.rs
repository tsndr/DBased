use crate::config::bookmarks::DbEngine;
use crate::db::driver::{
    DatabaseDriver, escape_sql_literal_for_engine, quote_mysql_ident, quote_pg_ident,
};
use crate::db::models::*;
use anyhow::{Context, Result, anyhow};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const EXPORT_FETCH_BATCH_SIZE: u32 = 1000;
const SQL_INSERT_BATCH_SIZE: usize = 250;
const IMPORT_INSERT_BATCH_SIZE: usize = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlScriptItem {
    UseDatabase(String),
    Statement(String),
}

pub async fn run_export(driver: &dyn DatabaseDriver, spec: &ExportSpec) -> Result<String> {
    let active_selections: Vec<DatabaseTableSelection> = spec
        .selections
        .iter()
        .filter(|s| !s.database.trim().is_empty() && !s.tables.is_empty())
        .cloned()
        .collect();

    if active_selections.is_empty() {
        return Err(anyhow!(
            "Please select at least one database and one table to export"
        ));
    }

    let total_dbs = active_selections.len();
    let total_tables: usize = active_selections.iter().map(|s| s.tables.len()).sum();

    match spec.format {
        TransferFormat::Sql => match &spec.destination {
            ExportDestination::SingleFile(path) => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        fs::create_dir_all(parent)?;
                    }
                }
                let file = File::create(path)
                    .with_context(|| format!("Failed to create '{}'", path.display()))?;
                let mut writer = BufWriter::new(file);
                export_selections_to_sql(
                    driver,
                    &active_selections,
                    spec.sql_mode,
                    spec.include_create_database,
                    spec.include_drop_table,
                    &mut writer,
                )
                .await?;
                writer.flush()?;
                Ok(format!(
                    "Exported {} table(s) across {} database(s) to {}",
                    total_tables,
                    total_dbs,
                    path.display()
                ))
            }
            ExportDestination::Directory(dir) => {
                fs::create_dir_all(dir)
                    .with_context(|| format!("Failed to create directory '{}'", dir.display()))?;
                for sel in &active_selections {
                    let file_path = dir.join(format!("{}.sql", sel.database));
                    let file = File::create(&file_path).with_context(|| {
                        format!("Failed to create '{}'", file_path.display())
                    })?;
                    let mut writer = BufWriter::new(file);
                    export_selections_to_sql(
                        driver,
                        std::slice::from_ref(sel),
                        spec.sql_mode,
                        spec.include_create_database,
                        spec.include_drop_table,
                        &mut writer,
                    )
                    .await?;
                    writer.flush()?;
                }
                Ok(format!(
                    "Exported {} database SQL file(s) ({} table(s)) to {}",
                    total_dbs,
                    total_tables,
                    dir.display()
                ))
            }
        },
        TransferFormat::Csv => match &spec.destination {
            ExportDestination::SingleFile(path) => {
                if total_tables != 1 {
                    return Err(anyhow!(
                        "CSV single-file export requires exactly 1 table (use a target folder for multiple tables)"
                    ));
                }
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        fs::create_dir_all(parent)?;
                    }
                }
                let sel = &active_selections[0];
                let table = &sel.tables[0];
                let rows_written = export_table_to_csv_path(driver, &sel.database, table, path).await?;
                Ok(format!(
                    "Exported {} row(s) from '{}.{}' to {}",
                    rows_written,
                    sel.database,
                    table,
                    path.display()
                ))
            }
            ExportDestination::Directory(dir) => {
                fs::create_dir_all(dir)
                    .with_context(|| format!("Failed to create directory '{}'", dir.display()))?;
                let mut total_rows = 0u64;
                for sel in &active_selections {
                    let target_dir = if total_dbs > 1 {
                        dir.join(&sel.database)
                    } else {
                        dir.clone()
                    };
                    fs::create_dir_all(&target_dir)?;
                    for table in &sel.tables {
                        let csv_path = target_dir.join(format!("{}.csv", table));
                        total_rows +=
                            export_table_to_csv_path(driver, &sel.database, table, &csv_path)
                                .await?;
                    }
                }
                Ok(format!(
                    "Exported {} table(s) ({} row(s)) as CSV to {}",
                    total_tables,
                    total_rows,
                    dir.display()
                ))
            }
        },
        TransferFormat::Json => match &spec.destination {
            ExportDestination::SingleFile(path) => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        fs::create_dir_all(parent)?;
                    }
                }
                if total_dbs == 1 && total_tables == 1 {
                    let sel = &active_selections[0];
                    let table = &sel.tables[0];
                    let rows_written =
                        export_table_to_json_path(driver, &sel.database, table, path).await?;
                    Ok(format!(
                        "Exported {} row(s) from '{}.{}' to {}",
                        rows_written,
                        sel.database,
                        table,
                        path.display()
                    ))
                } else {
                    let rows_written =
                        export_bundle_to_json_path(driver, &active_selections, path).await?;
                    Ok(format!(
                        "Exported {} table(s) across {} database(s) ({} row(s)) to {}",
                        total_tables,
                        total_dbs,
                        rows_written,
                        path.display()
                    ))
                }
            }
            ExportDestination::Directory(dir) => {
                fs::create_dir_all(dir)
                    .with_context(|| format!("Failed to create directory '{}'", dir.display()))?;
                let mut total_rows = 0u64;
                for sel in &active_selections {
                    let target_dir = if total_dbs > 1 {
                        dir.join(&sel.database)
                    } else {
                        dir.clone()
                    };
                    fs::create_dir_all(&target_dir)?;
                    for table in &sel.tables {
                        let json_path = target_dir.join(format!("{}.json", table));
                        total_rows +=
                            export_table_to_json_path(driver, &sel.database, table, &json_path)
                                .await?;
                    }
                }
                Ok(format!(
                    "Exported {} table(s) ({} row(s)) as JSON to {}",
                    total_tables,
                    total_rows,
                    dir.display()
                ))
            }
        },
    }
}

pub async fn export_selections_to_sql<W: Write>(
    driver: &dyn DatabaseDriver,
    selections: &[DatabaseTableSelection],
    sql_mode: SqlExportMode,
    include_create_database: bool,
    include_drop_table: bool,
    writer: &mut W,
) -> Result<()> {
    let engine = driver.engine();
    writeln!(
        writer,
        "-- DBased SQL Export\n-- Engine: {}\n",
        engine.display_name()
    )?;

    if engine == DbEngine::MySql {
        writeln!(
            writer,
            "SET FOREIGN_KEY_CHECKS = 0;\nSET SQL_MODE = \"NO_AUTO_VALUE_ON_ZERO\";\n"
        )?;
    }

    let multi_db = selections.len() > 1;

    for sel in selections {
        let db = sel.database.trim();
        if db.is_empty() || sel.tables.is_empty() {
            continue;
        }

        writeln!(
            writer,
            "-- ==========================================\n-- Database: {}\n-- ==========================================",
            db
        )?;

        if include_create_database {
            let db_ddl = driver.get_create_database_ddl(db).await?;
            writeln!(writer, "{}\n", db_ddl.trim())?;
        } else if multi_db {
            match engine {
                DbEngine::MySql => {
                    writeln!(writer, "USE {};\n", quote_mysql_ident(db))?;
                }
                DbEngine::Postgres => {
                    writeln!(
                        writer,
                        "-- DBASED_DATABASE: {}\n\\connect {}\n",
                        db,
                        quote_pg_ident(db)
                    )?;
                }
            }
        } else {
            writeln!(writer)?;
        }

        let all_table_infos = driver.list_tables(db).await.unwrap_or_default();
        let mut deferred_constraints = Vec::new();

        for table in &sel.tables {
            let is_view = all_table_infos
                .iter()
                .find(|ti| &ti.name == table)
                .is_some_and(|ti| ti.is_view);

            if sql_mode.includes_structure() {
                let ddl_parts = driver
                    .get_table_ddl(db, table, include_drop_table)
                    .await?;
                writeln!(writer, "-- Structure for `{}`", table)?;
                for pre in &ddl_parts.pre_create_statements {
                    writeln!(writer, "{}", pre.trim())?;
                }
                writeln!(writer, "{}\n", ddl_parts.create_and_indexes_sql.trim())?;
                deferred_constraints.extend(ddl_parts.post_data_constraints);
            }

            if sql_mode.includes_data() && !is_view {
                let cols = driver.get_table_structure(db, table).await?;
                if !cols.is_empty() {
                    let col_names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
                    let col_count = col_names.len();
                    let mut offset = 0u64;
                    let mut wrote_header = false;

                    loop {
                        let batch = driver
                            .fetch_table_rows_batch(
                                db,
                                table,
                                col_count,
                                offset,
                                EXPORT_FETCH_BATCH_SIZE,
                            )
                            .await?;
                        if batch.is_empty() {
                            break;
                        }
                        if !wrote_header {
                            writeln!(writer, "-- Data for `{}`", table)?;
                            wrote_header = true;
                        }
                        for chunk in batch.chunks(SQL_INSERT_BATCH_SIZE) {
                            let stmt = format_sql_insert_batch(engine, table, &col_names, chunk);
                            writeln!(writer, "{}", stmt)?;
                        }
                        let fetched = batch.len() as u64;
                        offset += fetched;
                        if fetched < EXPORT_FETCH_BATCH_SIZE as u64 {
                            break;
                        }
                    }
                    if wrote_header {
                        writeln!(writer)?;
                    }
                }
            }
        }

        if sql_mode.includes_structure() && !deferred_constraints.is_empty() {
            writeln!(writer, "-- Foreign key constraints for `{}`", db)?;
            for fk_stmt in deferred_constraints {
                writeln!(writer, "{}", fk_stmt.trim())?;
            }
            writeln!(writer)?;
        }
    }

    if engine == DbEngine::MySql {
        writeln!(writer, "SET FOREIGN_KEY_CHECKS = 1;")?;
    }

    Ok(())
}

pub fn format_sql_insert_batch(
    engine: DbEngine,
    table: &str,
    columns: &[String],
    rows: &[Vec<DbValue>],
) -> String {
    let quote_ident = |ident: &str| match engine {
        DbEngine::MySql => quote_mysql_ident(ident),
        DbEngine::Postgres => quote_pg_ident(ident),
    };
    let table_ident = match engine {
        DbEngine::MySql => quote_mysql_ident(table),
        DbEngine::Postgres => format!("\"public\".{}", quote_pg_ident(table)),
    };
    let quoted_cols: Vec<String> = columns.iter().map(|c| quote_ident(c)).collect();
    let tuples: Vec<String> = rows
        .iter()
        .map(|row| {
            let vals: Vec<String> = row
                .iter()
                .map(|v| escape_sql_literal_for_engine(engine, v))
                .collect();
            format!("({})", vals.join(", "))
        })
        .collect();
    format!(
        "INSERT INTO {} ({}) VALUES\n  {};",
        table_ident,
        quoted_cols.join(", "),
        tuples.join(",\n  ")
    )
}

async fn export_table_to_csv_path(
    driver: &dyn DatabaseDriver,
    db_name: &str,
    table: &str,
    path: &Path,
) -> Result<u64> {
    let cols = driver.get_table_structure(db_name, table).await?;
    let mut wtr = csv::Writer::from_path(path)
        .with_context(|| format!("Failed to create CSV file '{}'", path.display()))?;

    let headers: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    wtr.write_record(&headers)?;

    let col_count = cols.len();
    let mut offset = 0u64;
    let mut total_rows = 0u64;

    loop {
        let batch = driver
            .fetch_table_rows_batch(db_name, table, col_count, offset, EXPORT_FETCH_BATCH_SIZE)
            .await?;
        if batch.is_empty() {
            break;
        }
        for row in &batch {
            let record: Vec<&str> = row
                .iter()
                .map(|v| match v {
                    DbValue::Null => "",
                    DbValue::Text(s) => s.as_str(),
                })
                .collect();
            wtr.write_record(&record)?;
        }
        let fetched = batch.len() as u64;
        total_rows += fetched;
        offset += fetched;
        if fetched < EXPORT_FETCH_BATCH_SIZE as u64 {
            break;
        }
    }

    wtr.flush()?;
    Ok(total_rows)
}

async fn fetch_table_as_json_rows(
    driver: &dyn DatabaseDriver,
    db_name: &str,
    table: &str,
) -> Result<Vec<serde_json::Value>> {
    let cols = driver.get_table_structure(db_name, table).await?;
    let col_count = cols.len();
    let mut offset = 0u64;
    let mut json_rows = Vec::new();

    loop {
        let batch = driver
            .fetch_table_rows_batch(db_name, table, col_count, offset, EXPORT_FETCH_BATCH_SIZE)
            .await?;
        if batch.is_empty() {
            break;
        }
        for row in &batch {
            let mut obj = serde_json::Map::new();
            for (idx, col) in cols.iter().enumerate() {
                let val = match row.get(idx) {
                    Some(DbValue::Text(s)) => serde_json::Value::String(s.clone()),
                    _ => serde_json::Value::Null,
                };
                obj.insert(col.name.clone(), val);
            }
            json_rows.push(serde_json::Value::Object(obj));
        }
        let fetched = batch.len() as u64;
        offset += fetched;
        if fetched < EXPORT_FETCH_BATCH_SIZE as u64 {
            break;
        }
    }

    Ok(json_rows)
}

async fn export_table_to_json_path(
    driver: &dyn DatabaseDriver,
    db_name: &str,
    table: &str,
    path: &Path,
) -> Result<u64> {
    let json_rows = fetch_table_as_json_rows(driver, db_name, table).await?;
    let row_count = json_rows.len() as u64;
    let formatted = serde_json::to_string_pretty(&json_rows)?;
    let mut f = File::create(path)
        .with_context(|| format!("Failed to create JSON file '{}'", path.display()))?;
    f.write_all(formatted.as_bytes())?;
    Ok(row_count)
}

async fn export_bundle_to_json_path(
    driver: &dyn DatabaseDriver,
    selections: &[DatabaseTableSelection],
    path: &Path,
) -> Result<u64> {
    let mut databases_map = serde_json::Map::new();
    let mut total_rows = 0u64;

    for sel in selections {
        let mut tables_map = serde_json::Map::new();
        for table in &sel.tables {
            let rows = fetch_table_as_json_rows(driver, &sel.database, table).await?;
            total_rows += rows.len() as u64;
            tables_map.insert(table.clone(), serde_json::Value::Array(rows));
        }
        databases_map.insert(sel.database.clone(), serde_json::Value::Object(tables_map));
    }

    let mut root = serde_json::Map::new();
    root.insert(
        "databases".to_string(),
        serde_json::Value::Object(databases_map),
    );
    let formatted = serde_json::to_string_pretty(&serde_json::Value::Object(root))?;
    let mut f = File::create(path)
        .with_context(|| format!("Failed to create JSON bundle '{}'", path.display()))?;
    f.write_all(formatted.as_bytes())?;
    Ok(total_rows)
}

// ==========================================
// Import Implementation
// ==========================================

pub async fn run_import(driver: &mut dyn DatabaseDriver, spec: &ImportSpec) -> Result<String> {
    match spec.format {
        TransferFormat::Sql => run_sql_import(driver, spec).await,
        TransferFormat::Csv => run_csv_import(driver, spec).await,
        TransferFormat::Json => run_json_import(driver, spec).await,
    }
}

async fn run_sql_import(driver: &mut dyn DatabaseDriver, spec: &ImportSpec) -> Result<String> {
    let sql_files = collect_source_files(&spec.source, "sql")?;
    if sql_files.is_empty() {
        return Err(anyhow!("No .sql files found to import"));
    }

    let explicit_target_db = spec
        .target_database
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut current_db = explicit_target_db.unwrap_or("").to_string();
    let mut statements_executed = 0usize;

    for file_path in &sql_files {
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read SQL file '{}'", file_path.display()))?;
        let legacy_no_backslash = is_legacy_dbased_mysql_export(&content);
        let items = parse_sql_script_items_with_mode(&content, legacy_no_backslash);

        // Determine how many distinct databases this script file explicitly targets via
        // `USE` / `\connect` / `-- DBASED_DATABASE:` / `CREATE DATABASE`.
        let mut script_dbs: Vec<String> = Vec::new();
        for item in &items {
            match item {
                SqlScriptItem::UseDatabase(db) => {
                    if !db.is_empty() && !script_dbs.iter().any(|d| d.eq_ignore_ascii_case(db)) {
                        script_dbs.push(db.clone());
                    }
                }
                SqlScriptItem::Statement(stmt) => {
                    if let Some(created_db) = extract_created_database_name(stmt) {
                        if !created_db.is_empty()
                            && !script_dbs.iter().any(|d| d.eq_ignore_ascii_case(&created_db))
                        {
                            script_dbs.push(created_db);
                        }
                    }
                }
            }
        }

        // If the user explicitly selected a target database (not "(From Script / Multi-Database)")
        // and the SQL file is a single-database dump, import into the user's selected target
        // database rather than switching back to the source database name in the dump.
        let force_target_db = explicit_target_db.is_some() && script_dbs.len() <= 1;
        if let Some(target_db) = explicit_target_db {
            if force_target_db {
                current_db = target_db.to_string();
            }
        }

        let mut pending_stmts: Vec<String> = Vec::new();

        for item in items {
            match item {
                SqlScriptItem::UseDatabase(db) => {
                    if force_target_db {
                        continue;
                    }
                    if db != current_db {
                        if !pending_stmts.is_empty() {
                            driver
                                .execute_sql_batch(
                                    &current_db,
                                    &pending_stmts,
                                    legacy_no_backslash,
                                )
                                .await?;
                            statements_executed += pending_stmts.len();
                            pending_stmts.clear();
                        }
                        current_db = db;
                    }
                }
                SqlScriptItem::Statement(stmt) => {
                    if let Some(created_db) = extract_created_database_name(&stmt) {
                        if force_target_db
                            && explicit_target_db
                                .is_some_and(|t| !t.eq_ignore_ascii_case(&created_db))
                        {
                            continue;
                        }
                        if !pending_stmts.is_empty() {
                            driver
                                .execute_sql_batch(
                                    &current_db,
                                    &pending_stmts,
                                    legacy_no_backslash,
                                )
                                .await?;
                            statements_executed += pending_stmts.len();
                            pending_stmts.clear();
                        }
                        driver.execute_sql_statement("", &stmt).await?;
                        statements_executed += 1;
                        if current_db.is_empty() {
                            current_db = created_db;
                        }
                    } else {
                        pending_stmts.push(stmt);
                    }
                }
            }
        }

        if !pending_stmts.is_empty() {
            driver
                .execute_sql_batch(&current_db, &pending_stmts, legacy_no_backslash)
                .await?;
            statements_executed += pending_stmts.len();
        }
    }

    Ok(format!(
        "Imported {} SQL file(s) ({} statement(s) executed)",
        sql_files.len(),
        statements_executed
    ))
}

async fn run_csv_import(driver: &mut dyn DatabaseDriver, spec: &ImportSpec) -> Result<String> {
    let targets = resolve_tabular_file_targets(driver, spec, "csv").await?;
    if targets.is_empty() {
        return Err(anyhow!("No matching CSV files found to import"));
    }

    let mut total_rows = 0u64;
    for (db, table, path) in &targets {
        let cols = driver.get_table_structure(db, table).await?;
        if cols.is_empty() {
            return Err(anyhow!(
                "Target table '{}.{}' does not exist or has no columns",
                db,
                table
            ));
        }

        let mut rdr = csv::Reader::from_path(path)
            .with_context(|| format!("Failed to open CSV file '{}'", path.display()))?;
        let headers = rdr
            .headers()
            .with_context(|| format!("Failed to read CSV headers from '{}'", path.display()))?
            .clone();

        if headers.is_empty() {
            return Err(anyhow!("CSV file '{}' has an empty header row", path.display()));
        }

        let mut mapped_cols: Vec<ColumnInfo> = Vec::with_capacity(headers.len());
        for h in headers.iter() {
            let trimmed_h = h.trim();
            let found = cols
                .iter()
                .find(|c| c.name == trimmed_h)
                .or_else(|| cols.iter().find(|c| c.name.eq_ignore_ascii_case(trimmed_h)))
                .ok_or_else(|| {
                    anyhow!(
                        "CSV column '{}' in '{}' does not match any existing column in '{}.{}'",
                        trimmed_h,
                        path.display(),
                        db,
                        table
                    )
                })?;
            mapped_cols.push(found.clone());
        }

        if spec.truncate_before_import {
            driver.truncate_table(db, table).await?;
        }

        let col_names: Vec<String> = mapped_cols.iter().map(|c| c.name.clone()).collect();
        let mut batch: Vec<Vec<DbValue>> = Vec::with_capacity(IMPORT_INSERT_BATCH_SIZE);

        for result in rdr.records() {
            let record = result
                .with_context(|| format!("Failed to read record in '{}'", path.display()))?;
            let mut row_vals = Vec::with_capacity(mapped_cols.len());
            for (idx, col_info) in mapped_cols.iter().enumerate() {
                let raw_cell = record.get(idx).unwrap_or("");
                row_vals.push(coerce_imported_str_value(raw_cell, col_info));
            }
            batch.push(row_vals);
            if batch.len() >= IMPORT_INSERT_BATCH_SIZE {
                total_rows += driver.insert_rows_batch(db, table, &col_names, &batch).await?;
                batch.clear();
            }
        }

        if !batch.is_empty() {
            total_rows += driver.insert_rows_batch(db, table, &col_names, &batch).await?;
        }
    }

    Ok(format!(
        "Imported {} row(s) into {} table(s) from CSV",
        total_rows,
        targets.len()
    ))
}

async fn run_json_import(driver: &mut dyn DatabaseDriver, spec: &ImportSpec) -> Result<String> {
    match &spec.source {
        ImportSource::Files(paths) if paths.len() == 1 => {
            let path = &paths[0];
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read JSON file '{}'", path.display()))?;
            let val: serde_json::Value = serde_json::from_str(&content)
                .with_context(|| format!("Invalid JSON in '{}'", path.display()))?;

            match val {
                serde_json::Value::Array(arr) => {
                    let db = spec
                        .target_database
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| anyhow!("Please select a target database for JSON import"))?;
                    let existing_tables = driver.list_tables(db).await?;
                    let requested_table = spec
                        .target_table
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .or_else(|| path.file_stem().and_then(|s| s.to_str()))
                        .ok_or_else(|| anyhow!("Please select a target table for JSON import"))?;
                    let matched_table = find_existing_table(&existing_tables, requested_table)
                        .ok_or_else(|| {
                            anyhow!(
                                "Target table '{}' does not exist in database '{}'",
                                requested_table,
                                db
                            )
                        })?;
                    let rows_imported = import_json_array_into_table(
                        driver,
                        db,
                        &matched_table,
                        &arr,
                        spec.truncate_before_import,
                    )
                    .await?;
                    Ok(format!(
                        "Imported {} row(s) into '{}.{}' from JSON",
                        rows_imported, db, matched_table
                    ))
                }
                serde_json::Value::Object(map) => {
                    let (tables_count, rows_imported) = import_json_object_bundle(
                        driver,
                        &map,
                        spec.target_database.as_deref(),
                        spec.truncate_before_import,
                    )
                    .await?;
                    Ok(format!(
                        "Imported {} row(s) into {} table(s) from JSON bundle",
                        rows_imported, tables_count
                    ))
                }
                _ => Err(anyhow!(
                    "Expected JSON array of rows or JSON object bundle in '{}'",
                    path.display()
                )),
            }
        }
        _ => {
            let targets = resolve_tabular_file_targets(driver, spec, "json").await?;
            if targets.is_empty() {
                return Err(anyhow!("No matching JSON files found to import"));
            }
            let mut total_rows = 0u64;
            for (db, table, path) in &targets {
                let content = fs::read_to_string(path)
                    .with_context(|| format!("Failed to read JSON file '{}'", path.display()))?;
                let val: serde_json::Value = serde_json::from_str(&content)
                    .with_context(|| format!("Invalid JSON in '{}'", path.display()))?;
                if let serde_json::Value::Array(arr) = val {
                    total_rows += import_json_array_into_table(
                        driver,
                        db,
                        table,
                        &arr,
                        spec.truncate_before_import,
                    )
                    .await?;
                } else {
                    return Err(anyhow!(
                        "Expected JSON array of row objects in per-table file '{}'",
                        path.display()
                    ));
                }
            }
            Ok(format!(
                "Imported {} row(s) into {} table(s) from JSON",
                total_rows,
                targets.len()
            ))
        }
    }
}

async fn import_json_object_bundle(
    driver: &mut dyn DatabaseDriver,
    map: &serde_json::Map<String, serde_json::Value>,
    default_db: Option<&str>,
    truncate_before_import: bool,
) -> Result<(usize, u64)> {
    let existing_dbs = driver.list_databases().await?;
    let mut total_tables = 0usize;
    let mut total_rows = 0u64;

    if let Some(serde_json::Value::Object(dbs_obj)) = map.get("databases") {
        for (db_key, db_val) in dbs_obj {
            let matched_db = existing_dbs
                .iter()
                .find(|d| *d == db_key)
                .or_else(|| existing_dbs.iter().find(|d| d.eq_ignore_ascii_case(db_key)))
                .ok_or_else(|| anyhow!("Target database '{}' does not exist", db_key))?
                .clone();

            let tables_obj = match db_val {
                serde_json::Value::Object(m) => {
                    if let Some(serde_json::Value::Object(inner_tables)) = m.get("tables") {
                        inner_tables
                    } else {
                        m
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "Expected object mapping table names to row arrays for database '{}'",
                        db_key
                    ));
                }
            };

            let existing_tables = driver.list_tables(&matched_db).await?;
            for (tbl_key, rows_val) in tables_obj {
                let matched_tbl = find_existing_table(&existing_tables, tbl_key).ok_or_else(|| {
                    anyhow!(
                        "Target table '{}' does not exist in database '{}'",
                        tbl_key,
                        matched_db
                    )
                })?;
                let arr = rows_val.as_array().ok_or_else(|| {
                    anyhow!(
                        "Expected JSON array of rows for table '{}.{}'",
                        matched_db,
                        matched_tbl
                    )
                })?;
                total_rows += import_json_array_into_table(
                    driver,
                    &matched_db,
                    &matched_tbl,
                    arr,
                    truncate_before_import,
                )
                .await?;
                total_tables += 1;
            }
        }
        return Ok((total_tables, total_rows));
    }

    // Otherwise treat top-level keys (or "tables" sub-object) as table names in `default_db`
    let target_db = default_db
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Please select a target database for multi-table JSON import"))?;
    let matched_db = existing_dbs
        .iter()
        .find(|d| d.as_str() == target_db)
        .or_else(|| existing_dbs.iter().find(|d| d.eq_ignore_ascii_case(target_db)))
        .ok_or_else(|| anyhow!("Target database '{}' does not exist", target_db))?
        .clone();

    let tables_obj = if let Some(serde_json::Value::Object(inner)) = map.get("tables") {
        inner
    } else {
        map
    };

    let existing_tables = driver.list_tables(&matched_db).await?;
    for (tbl_key, rows_val) in tables_obj {
        let matched_tbl = find_existing_table(&existing_tables, tbl_key).ok_or_else(|| {
            anyhow!(
                "Target table '{}' does not exist in database '{}'",
                tbl_key,
                matched_db
            )
        })?;
        let arr = rows_val.as_array().ok_or_else(|| {
            anyhow!(
                "Expected JSON array of rows for table '{}.{}'",
                matched_db,
                matched_tbl
            )
        })?;
        total_rows += import_json_array_into_table(
            driver,
            &matched_db,
            &matched_tbl,
            arr,
            truncate_before_import,
        )
        .await?;
        total_tables += 1;
    }

    Ok((total_tables, total_rows))
}

async fn import_json_array_into_table(
    driver: &mut dyn DatabaseDriver,
    db: &str,
    table: &str,
    rows: &[serde_json::Value],
    truncate_before_import: bool,
) -> Result<u64> {
    let cols = driver.get_table_structure(db, table).await?;
    if cols.is_empty() {
        return Err(anyhow!(
            "Target table '{}.{}' does not exist or has no columns",
            db,
            table
        ));
    }

    if truncate_before_import {
        driver.truncate_table(db, table).await?;
    }

    if rows.is_empty() {
        return Ok(0);
    }

    // Collect union of column names present in the JSON objects in table column order
    let mut present_cols: Vec<ColumnInfo> = Vec::new();
    for row_val in rows {
        let obj = row_val.as_object().ok_or_else(|| {
            anyhow!(
                "Expected JSON object for each row when importing into '{}.{}'",
                db,
                table
            )
        })?;
        for key in obj.keys() {
            let trimmed_k = key.trim();
            let matched = cols
                .iter()
                .find(|c| c.name == trimmed_k)
                .or_else(|| cols.iter().find(|c| c.name.eq_ignore_ascii_case(trimmed_k)))
                .ok_or_else(|| {
                    anyhow!(
                        "JSON property '{}' does not match any existing column in '{}.{}'",
                        trimmed_k,
                        db,
                        table
                    )
                })?;
            if !present_cols.iter().any(|c| c.name == matched.name) {
                present_cols.push(matched.clone());
            }
        }
    }

    present_cols.sort_by_key(|c| c.ordinal);
    let col_names: Vec<String> = present_cols.iter().map(|c| c.name.clone()).collect();

    let mut total_inserted = 0u64;
    let mut batch: Vec<Vec<DbValue>> = Vec::with_capacity(IMPORT_INSERT_BATCH_SIZE);

    for row_val in rows {
        let obj = row_val.as_object().unwrap();
        let mut row_vals = Vec::with_capacity(present_cols.len());
        for col_info in &present_cols {
            let json_cell = obj
                .get(&col_info.name)
                .or_else(|| {
                    obj.iter()
                        .find(|(k, _)| k.trim().eq_ignore_ascii_case(&col_info.name))
                        .map(|(_, v)| v)
                });
            let db_val = match json_cell {
                None | Some(serde_json::Value::Null) => DbValue::Null,
                Some(serde_json::Value::String(s)) => coerce_imported_str_value(s, col_info),
                Some(serde_json::Value::Bool(b)) => DbValue::Text(b.to_string()),
                Some(serde_json::Value::Number(n)) => DbValue::Text(n.to_string()),
                Some(other) => DbValue::Text(other.to_string()),
            };
            row_vals.push(db_val);
        }
        batch.push(row_vals);
        if batch.len() >= IMPORT_INSERT_BATCH_SIZE {
            total_inserted += driver.insert_rows_batch(db, table, &col_names, &batch).await?;
            batch.clear();
        }
    }

    if !batch.is_empty() {
        total_inserted += driver.insert_rows_batch(db, table, &col_names, &batch).await?;
    }

    Ok(total_inserted)
}

async fn resolve_tabular_file_targets(
    driver: &dyn DatabaseDriver,
    spec: &ImportSpec,
    ext: &str,
) -> Result<Vec<(String, String, PathBuf)>> {
    let existing_dbs = driver.list_databases().await?;
    let mut resolved = Vec::new();

    match &spec.source {
        ImportSource::Files(paths) => {
            let target_db = spec
                .target_database
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("Please select a target database for import"))?;
            let matched_db = existing_dbs
                .iter()
                .find(|d| d.as_str() == target_db)
                .or_else(|| existing_dbs.iter().find(|d| d.eq_ignore_ascii_case(target_db)))
                .ok_or_else(|| anyhow!("Target database '{}' does not exist", target_db))?
                .clone();
            let existing_tables = driver.list_tables(&matched_db).await?;

            if paths.len() == 1 {
                let path = &paths[0];
                let table_candidate = spec
                    .target_table
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .or_else(|| path.file_stem().and_then(|s| s.to_str()))
                    .ok_or_else(|| anyhow!("Please select a target table for import"))?;
                let matched_tbl = find_existing_table(&existing_tables, table_candidate)
                    .ok_or_else(|| {
                        anyhow!(
                            "Target table '{}' does not exist in database '{}'",
                            table_candidate,
                            matched_db
                        )
                    })?;
                resolved.push((matched_db, matched_tbl, path.clone()));
            } else {
                for path in paths {
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| anyhow!("Invalid file name '{}'", path.display()))?;
                    let matched_tbl =
                        find_existing_table(&existing_tables, stem).ok_or_else(|| {
                            anyhow!(
                                "No existing table matching file '{}' in database '{}'",
                                path.display(),
                                matched_db
                            )
                        })?;
                    resolved.push((matched_db.clone(), matched_tbl, path.clone()));
                }
            }
        }
        ImportSource::Directory(dir) => {
            let mut entries: Vec<PathBuf> = fs::read_dir(dir)
                .with_context(|| format!("Failed to read directory '{}'", dir.display()))?
                .filter_map(|e| e.ok().map(|de| de.path()))
                .collect();
            entries.sort();

            let mut subdirs = Vec::new();
            let mut direct_files = Vec::new();
            for entry in entries {
                if entry.is_dir() {
                    subdirs.push(entry);
                } else if entry
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case(ext))
                {
                    direct_files.push(entry);
                }
            }

            // 1. Check subdirectories named after existing databases (<dir>/<database>/<table>.<ext>)
            for subdir in &subdirs {
                let Some(dir_name) = subdir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if let Some(matched_db) = existing_dbs
                    .iter()
                    .find(|d| d.as_str() == dir_name)
                    .or_else(|| existing_dbs.iter().find(|d| d.eq_ignore_ascii_case(dir_name)))
                {
                    let existing_tables = driver.list_tables(matched_db).await?;
                    let mut db_files: Vec<PathBuf> = fs::read_dir(subdir)?
                        .filter_map(|e| e.ok().map(|de| de.path()))
                        .filter(|p| {
                            p.is_file()
                                && p.extension()
                                    .and_then(|e| e.to_str())
                                    .is_some_and(|e| e.eq_ignore_ascii_case(ext))
                        })
                        .collect();
                    db_files.sort();
                    for file_path in db_files {
                        let stem = file_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("");
                        let matched_tbl =
                            find_existing_table(&existing_tables, stem).ok_or_else(|| {
                                anyhow!(
                                    "Table '{}' (from '{}') does not exist in database '{}'",
                                    stem,
                                    file_path.display(),
                                    matched_db
                                )
                            })?;
                        resolved.push((matched_db.clone(), matched_tbl, file_path));
                    }
                }
            }

            // 2. Direct files in `<dir>/<table>.<ext>` mapped to `spec.target_database`
            if !direct_files.is_empty() {
                let target_db = spec
                    .target_database
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow!(
                            "Please select a target database for files in '{}'",
                            dir.display()
                        )
                    })?;
                let matched_db = existing_dbs
                    .iter()
                    .find(|d| d.as_str() == target_db)
                    .or_else(|| existing_dbs.iter().find(|d| d.eq_ignore_ascii_case(target_db)))
                    .ok_or_else(|| anyhow!("Target database '{}' does not exist", target_db))?
                    .clone();
                let existing_tables = driver.list_tables(&matched_db).await?;

                for file_path in direct_files {
                    let stem = file_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    let matched_tbl =
                        find_existing_table(&existing_tables, stem).ok_or_else(|| {
                            anyhow!(
                                "Table '{}' (from '{}') does not exist in database '{}'",
                                stem,
                                file_path.display(),
                                matched_db
                            )
                        })?;
                    resolved.push((matched_db.clone(), matched_tbl, file_path));
                }
            }
        }
    }

    Ok(resolved)
}

fn find_existing_table(tables: &[TableInfo], candidate: &str) -> Option<String> {
    let trimmed = candidate.trim();
    tables
        .iter()
        .find(|t| !t.is_view && t.name == trimmed)
        .or_else(|| {
            tables
                .iter()
                .find(|t| !t.is_view && t.name.eq_ignore_ascii_case(trimmed))
        })
        .map(|t| t.name.clone())
}

fn collect_source_files(source: &ImportSource, ext: &str) -> Result<Vec<PathBuf>> {
    match source {
        ImportSource::Files(paths) => Ok(paths.clone()),
        ImportSource::Directory(dir) => {
            let mut files = Vec::new();
            collect_files_recursive(dir, ext, &mut files)?;
            files.sort();
            Ok(files)
        }
    }
}

fn collect_files_recursive(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory '{}'", dir.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            collect_files_recursive(&path, ext, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Converts a raw CSV or JSON string value into `DbValue`, coercing empty strings `""`
/// or `"NULL"` into `DbValue::Null` when appropriate for the column's schema.
pub fn coerce_imported_str_value(raw: &str, col: &ColumnInfo) -> DbValue {
    if col.is_nullable && raw.eq_ignore_ascii_case("NULL") {
        return DbValue::Null;
    }
    if raw.is_empty() && col.is_nullable {
        // For nullable columns, CSV exports write DbValue::Null as `""`, and non-text types
        // (numeric, boolean, date/time, uuid, enum) reject empty strings `""`.
        if !is_text_like_column(&col.data_type) || col.enum_values().is_some() {
            return DbValue::Null;
        }
        return DbValue::Null;
    }
    DbValue::Text(raw.to_string())
}

fn is_text_like_column(data_type: &str) -> bool {
    let dt = data_type.trim().to_ascii_lowercase();
    dt.contains("char") || dt.contains("text") || dt.contains("clob")
}

/// Detects whether a SQL script was generated by an earlier version of DBased MySQL export
/// that did not emit `NO_AUTO_VALUE_ON_ZERO` and used standard SQL `''` escaping without
/// backslash-escaping `\` (`NO_BACKSLASH_ESCAPES` semantics).
pub fn is_legacy_dbased_mysql_export(script: &str) -> bool {
    let header: String = script.lines().take(10).collect::<Vec<_>>().join("\n");
    header.contains("-- DBased SQL Export")
        && header.contains("-- Engine: MySQL")
        && !header.contains("NO_AUTO_VALUE_ON_ZERO")
}

/// Splits a SQL script into sequential `SqlScriptItem`s (`UseDatabase` directives and individual
/// executable `Statement`s), properly handling single-quoted literals, double-quoted identifiers,
/// backtick-quoted identifiers, `--` line comments, `/* ... */` block comments, and PostgreSQL
/// `$tag$...$tag$` dollar-quoted strings.
pub fn parse_sql_script_items(script: &str) -> Vec<SqlScriptItem> {
    let legacy_no_backslash = is_legacy_dbased_mysql_export(script);
    parse_sql_script_items_with_mode(script, legacy_no_backslash)
}

pub fn parse_sql_script_items_with_mode(
    script: &str,
    no_backslash_escapes: bool,
) -> Vec<SqlScriptItem> {
    let mut items = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = script.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // 1. Line comment `-- ...`
        if ch == '-' && i + 1 < len && chars[i + 1] == '-' {
            let start = i;
            i += 2;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            let comment_line: String = chars[start..i].iter().collect();
            if let Some(db_directive) = comment_line
                .trim()
                .strip_prefix("-- DBASED_DATABASE:")
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                flush_pending_statement(&mut current, &mut items);
                items.push(SqlScriptItem::UseDatabase(unquote_ident(db_directive)));
            }
            if i < len && chars[i] == '\n' {
                i += 1;
            }
            continue;
        }

        // 2. psql `\c` or `\connect` meta-command at start of line
        if ch == '\\' && (i == 0 || chars[i - 1] == '\n') {
            let start = i;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            let meta_line: String = chars[start..i].iter().collect();
            let trimmed_meta = meta_line.trim();
            if let Some(rest) = trimmed_meta
                .strip_prefix("\\connect ")
                .or_else(|| trimmed_meta.strip_prefix("\\c "))
            {
                let db_token = rest.split_whitespace().next().unwrap_or("").trim_end_matches(';');
                if !db_token.is_empty() {
                    flush_pending_statement(&mut current, &mut items);
                    items.push(SqlScriptItem::UseDatabase(unquote_ident(db_token)));
                }
            }
            if i < len && chars[i] == '\n' {
                i += 1;
            }
            continue;
        }

        // 3. Block comment `/* ... */` (note: preserve MySQL conditional comments `/*! ... */`)
        if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
            let is_mysql_bang = i + 2 < len && chars[i + 2] == '!';
            let start = i;
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            if i + 1 < len {
                i += 2;
            } else {
                i = len;
            }
            if is_mysql_bang {
                for &c in &chars[start..i] {
                    current.push(c);
                }
            }
            continue;
        }

        // 4. Quoted strings / identifiers (`'`, `"`, `` ` ``)
        if ch == '\'' || ch == '"' || ch == '`' {
            let quote = ch;
            current.push(ch);
            i += 1;
            while i < len {
                let c = chars[i];
                current.push(c);
                i += 1;
                if !no_backslash_escapes && c == '\\' && quote == '\'' && i < len {
                    current.push(chars[i]);
                    i += 1;
                } else if c == quote {
                    if i < len && chars[i] == quote {
                        current.push(chars[i]);
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            continue;
        }

        // 5. PostgreSQL dollar-quoted string `$tag$...$tag$`
        if ch == '$' {
            if let Some(tag_len) = match_dollar_quote_tag(&chars[i..]) {
                let tag: String = chars[i..i + tag_len].iter().collect();
                current.push_str(&tag);
                i += tag_len;
                let tag_chars: Vec<char> = tag.chars().collect();
                while i < len {
                    if i + tag_len <= len && chars[i..i + tag_len] == tag_chars[..] {
                        current.push_str(&tag);
                        i += tag_len;
                        break;
                    }
                    current.push(chars[i]);
                    i += 1;
                }
                continue;
            }
        }

        // 6. Statement terminator `;`
        if ch == ';' {
            flush_pending_statement(&mut current, &mut items);
            i += 1;
            continue;
        }

        current.push(ch);
        i += 1;
    }

    flush_pending_statement(&mut current, &mut items);
    items
}

fn match_dollar_quote_tag(slice: &[char]) -> Option<usize> {
    if slice.first() != Some(&'$') {
        return None;
    }
    if slice.get(1) == Some(&'$') {
        return Some(2);
    }
    let mut idx = 1;
    while idx < slice.len() {
        let c = slice[idx];
        if c == '$' {
            return if idx > 1 { Some(idx + 1) } else { None };
        }
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        idx += 1;
    }
    None
}

fn flush_pending_statement(current: &mut String, items: &mut Vec<SqlScriptItem>) {
    let trimmed = current.trim();
    if trimmed.is_empty() {
        current.clear();
        return;
    }

    if let Some(use_db) = extract_use_database_name(trimmed) {
        items.push(SqlScriptItem::UseDatabase(use_db));
    } else {
        items.push(SqlScriptItem::Statement(trimmed.to_string()));
    }
    current.clear();
}

fn extract_use_database_name(stmt: &str) -> Option<String> {
    let mut parts = stmt.split_whitespace();
    let first = parts.next()?;
    if !first.eq_ignore_ascii_case("USE") {
        return None;
    }
    let second = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let db = unquote_ident(second.trim_end_matches(';'));
    if db.is_empty() { None } else { Some(db) }
}

fn extract_created_database_name(stmt: &str) -> Option<String> {
    let upper = stmt.trim().to_ascii_uppercase();
    let rest = if upper.starts_with("CREATE DATABASE IF NOT EXISTS ") {
        &stmt.trim()["CREATE DATABASE IF NOT EXISTS ".len()..]
    } else if upper.starts_with("CREATE DATABASE ") {
        &stmt.trim()["CREATE DATABASE ".len()..]
    } else {
        return None;
    };
    let first_tok = rest.split_whitespace().next()?.trim_end_matches(';');
    let unquoted = unquote_ident(first_tok);
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted)
    }
}

pub fn unquote_ident(raw: &str) -> String {
    let t = raw.trim();
    if t.len() >= 2 {
        if t.starts_with('`') && t.ends_with('`') {
            return t[1..t.len() - 1].replace("``", "`");
        }
        if t.starts_with('"') && t.ends_with('"') {
            return t[1..t.len() - 1].replace("\"\"", "\"");
        }
    }
    t.to_string()
}

/// Builds a map of `<database> -> Vec<table_name>` from a JSON bundle string for previewing
/// or validating before import.
pub fn inspect_json_bundle_tables(content: &str) -> Option<BTreeMap<String, Vec<String>>> {
    let val: serde_json::Value = serde_json::from_str(content).ok()?;
    let obj = val.as_object()?;
    let mut out = BTreeMap::new();
    if let Some(serde_json::Value::Object(dbs)) = obj.get("databases") {
        for (db, tbls_val) in dbs {
            if let Some(tbls_obj) = tbls_val.as_object() {
                let inner = tbls_obj
                    .get("tables")
                    .and_then(|v| v.as_object())
                    .unwrap_or(tbls_obj);
                out.insert(db.clone(), inner.keys().cloned().collect());
            }
        }
        return Some(out);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    struct MockDriver {
        engine: DbEngine,
        databases: Vec<String>,
        schemas: BTreeMap<(String, String), Vec<ColumnInfo>>,
        views: BTreeSet<(String, String)>,
        custom_ddl: BTreeMap<(String, String), TableDdlParts>,
        rows: Mutex<BTreeMap<(String, String), Vec<Vec<DbValue>>>>,
        executed_sql: Mutex<Vec<(String, String)>>,
        executed_batches: Mutex<Vec<(String, Vec<String>, bool)>>,
        inserted_batches: Mutex<Vec<(String, String, Vec<String>, Vec<Vec<DbValue>>)>>,
    }

    impl MockDriver {
        fn new(engine: DbEngine) -> Self {
            let mut schemas = BTreeMap::new();
            let mut rows = BTreeMap::new();

            let user_cols = vec![
                ColumnInfo {
                    ordinal: 1,
                    name: "id".to_string(),
                    data_type: "int".to_string(),
                    is_nullable: false,
                    is_primary_key: true,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 2,
                    name: "name".to_string(),
                    data_type: "varchar(100)".to_string(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 3,
                    name: "score".to_string(),
                    data_type: "int".to_string(),
                    is_nullable: true,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
            ];

            schemas.insert(
                ("db_one".to_string(), "users".to_string()),
                user_cols.clone(),
            );
            rows.insert(
                ("db_one".to_string(), "users".to_string()),
                vec![
                    vec![
                        DbValue::Text("1".to_string()),
                        DbValue::Text("Alice".to_string()),
                        DbValue::Text("95".to_string()),
                    ],
                    vec![
                        DbValue::Text("2".to_string()),
                        DbValue::Text("Bob".to_string()),
                        DbValue::Null,
                    ],
                ],
            );

            schemas.insert(
                ("db_one".to_string(), "audit_logs".to_string()),
                vec![
                    ColumnInfo {
                        ordinal: 1,
                        name: "id".to_string(),
                        data_type: "int".to_string(),
                        is_nullable: false,
                        is_primary_key: true,
                        default_value: None,
                        extra: String::new(),
                    },
                    ColumnInfo {
                        ordinal: 2,
                        name: "action".to_string(),
                        data_type: "text".to_string(),
                        is_nullable: false,
                        is_primary_key: false,
                        default_value: None,
                        extra: String::new(),
                    },
                ],
            );
            rows.insert(
                ("db_one".to_string(), "audit_logs".to_string()),
                vec![vec![
                    DbValue::Text("100".to_string()),
                    DbValue::Text("login".to_string()),
                ]],
            );

            schemas.insert(("db_two".to_string(), "items".to_string()), user_cols);
            rows.insert(
                ("db_two".to_string(), "items".to_string()),
                vec![vec![
                    DbValue::Text("10".to_string()),
                    DbValue::Text("Widget".to_string()),
                    DbValue::Text("5".to_string()),
                ]],
            );

            Self {
                engine,
                databases: vec!["db_one".to_string(), "db_two".to_string()],
                schemas,
                views: BTreeSet::new(),
                custom_ddl: BTreeMap::new(),
                rows: Mutex::new(rows),
                executed_sql: Mutex::new(Vec::new()),
                executed_batches: Mutex::new(Vec::new()),
                inserted_batches: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl DatabaseDriver for MockDriver {
        fn engine(&self) -> DbEngine {
            self.engine
        }
        async fn list_databases(&self) -> Result<Vec<String>> {
            Ok(self.databases.clone())
        }
        async fn create_database(&self, _spec: &CreateDatabaseSpec) -> Result<()> {
            Ok(())
        }
        async fn use_database(&mut self, _db_name: &str) -> Result<()> {
            Ok(())
        }
        async fn list_tables(&self, db_name: &str) -> Result<Vec<TableInfo>> {
            let mut names: BTreeSet<String> = self
                .schemas
                .keys()
                .filter(|(d, _)| d == db_name)
                .map(|(_, t)| t.clone())
                .collect();
            for (d, v) in &self.views {
                if d == db_name {
                    names.insert(v.clone());
                }
            }
            let out = names
                .into_iter()
                .map(|t| TableInfo {
                    is_view: self.views.contains(&(db_name.to_string(), t.clone())),
                    name: t,
                })
                .collect();
            Ok(out)
        }
        async fn get_table_structure(&self, db_name: &str, table: &str) -> Result<Vec<ColumnInfo>> {
            Ok(self
                .schemas
                .get(&(db_name.to_string(), table.to_string()))
                .cloned()
                .unwrap_or_default())
        }
        async fn alter_column(&self, _: &str, _: &str, _: ColumnAlterOp) -> Result<()> {
            Ok(())
        }
        async fn get_table_indexes(&self, _: &str, _: &str) -> Result<Vec<IndexInfo>> {
            Ok(Vec::new())
        }
        async fn alter_index(&self, _: &str, _: &str, _: IndexAlterOp) -> Result<()> {
            Ok(())
        }
        async fn get_table_content(
            &self,
            _: &str,
            _: &str,
            _: &ContentQuery,
        ) -> Result<TablePage> {
            unreachable!()
        }
        async fn commit_cell_updates(&self, _: &str, _: &str, _: &[RowUpdate]) -> Result<u64> {
            Ok(0)
        }
        async fn insert_row(&self, _: &str, _: &str, _: &[(String, DbValue)]) -> Result<()> {
            Ok(())
        }
        async fn delete_rows(&self, _: &str, _: &str, _: &[PrimaryKeyValue]) -> Result<u64> {
            Ok(0)
        }
        async fn get_relations(&self, _: &str, _: &str) -> Result<Vec<ForeignKeyInfo>> {
            Ok(Vec::new())
        }
        async fn add_relation(&self, _: &str, _: &str, _: &NewForeignKey) -> Result<()> {
            Ok(())
        }
        async fn drop_relation(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn execute_raw_query(&self, _: &str, _: &str) -> Result<RawQueryOutcome> {
            unreachable!()
        }
        async fn list_users(&self) -> Result<Vec<DbUser>> {
            Ok(Vec::new())
        }
        async fn create_user(&self, _: &CreateUserSpec) -> Result<()> {
            Ok(())
        }
        async fn update_user(&self, _: &UpdateUserSpec) -> Result<()> {
            Ok(())
        }
        async fn drop_user(&self, _: &str, _: Option<&str>) -> Result<()> {
            Ok(())
        }
        async fn get_database_permissions(
            &self,
            _: &str,
            _: Option<&str>,
        ) -> Result<Vec<DatabasePermission>> {
            Ok(Vec::new())
        }
        async fn set_database_permissions(&self, _: &DbPermissionSpec) -> Result<()> {
            Ok(())
        }
        async fn revoke_database_permissions(
            &self,
            _: &str,
            _: Option<&str>,
            _: &str,
        ) -> Result<()> {
            Ok(())
        }
        async fn get_create_database_ddl(&self, db_name: &str) -> Result<String> {
            match self.engine {
                DbEngine::MySql => Ok(format!(
                    "CREATE DATABASE IF NOT EXISTS `{}`;\nUSE `{}`;",
                    db_name, db_name
                )),
                DbEngine::Postgres => Ok(format!(
                    "CREATE DATABASE \"{}\";\n-- DBASED_DATABASE: {}\n\\connect \"{}\"",
                    db_name, db_name, db_name
                )),
            }
        }
        async fn get_table_ddl(
            &self,
            db_name: &str,
            table: &str,
            include_drop: bool,
        ) -> Result<TableDdlParts> {
            if let Some(custom) = self.custom_ddl.get(&(db_name.to_string(), table.to_string())) {
                return Ok(custom.clone());
            }
            let is_view = self.views.contains(&(db_name.to_string(), table.to_string()));
            let mut sql = String::new();
            if include_drop {
                if is_view {
                    sql.push_str(&format!("DROP VIEW IF EXISTS `{}`;\n", table));
                } else {
                    sql.push_str(&format!("DROP TABLE IF EXISTS `{}`;\n", table));
                }
            }
            if is_view {
                sql.push_str(&format!("CREATE VIEW `{}` AS SELECT 1;", table));
            } else {
                sql.push_str(&format!("CREATE TABLE `{}` (`id` int);", table));
            }
            Ok(TableDdlParts {
                pre_create_statements: Vec::new(),
                create_and_indexes_sql: sql,
                post_data_constraints: Vec::new(),
            })
        }
        async fn fetch_table_rows_batch(
            &self,
            db_name: &str,
            table: &str,
            _col_count: usize,
            offset: u64,
            limit: u32,
        ) -> Result<Vec<Vec<DbValue>>> {
            let guard = self.rows.lock().unwrap();
            let Some(all) = guard.get(&(db_name.to_string(), table.to_string())) else {
                return Ok(Vec::new());
            };
            let start = (offset as usize).min(all.len());
            let end = (start + limit as usize).min(all.len());
            Ok(all[start..end].to_vec())
        }
        async fn truncate_table(&self, db_name: &str, table: &str) -> Result<()> {
            let mut guard = self.rows.lock().unwrap();
            if let Some(r) = guard.get_mut(&(db_name.to_string(), table.to_string())) {
                r.clear();
            }
            Ok(())
        }
        async fn insert_rows_batch(
            &self,
            db_name: &str,
            table: &str,
            columns: &[String],
            new_rows: &[Vec<DbValue>],
        ) -> Result<u64> {
            self.inserted_batches.lock().unwrap().push((
                db_name.to_string(),
                table.to_string(),
                columns.to_vec(),
                new_rows.to_vec(),
            ));
            let mut guard = self.rows.lock().unwrap();
            let entry = guard
                .entry((db_name.to_string(), table.to_string()))
                .or_default();
            entry.extend_from_slice(new_rows);
            Ok(new_rows.len() as u64)
        }
        async fn execute_sql_statement(&self, db_name: &str, sql: &str) -> Result<()> {
            self.executed_sql
                .lock()
                .unwrap()
                .push((db_name.to_string(), sql.to_string()));
            Ok(())
        }
        async fn execute_sql_batch(
            &self,
            db_name: &str,
            statements: &[String],
            legacy_no_backslash_escapes: bool,
        ) -> Result<()> {
            self.executed_batches.lock().unwrap().push((
                db_name.to_string(),
                statements.to_vec(),
                legacy_no_backslash_escapes,
            ));
            let mut guard = self.executed_sql.lock().unwrap();
            for stmt in statements {
                guard.push((db_name.to_string(), stmt.clone()));
            }
            Ok(())
        }
    }

    // =========================================================================
    // 1. SQL Lexer / Parser & Escaping Unit Tests
    // =========================================================================

    #[test]
    fn test_parse_sql_script_items_handles_quotes_comments_and_db_switches() {
        let script = r#"
-- DBased SQL Export
SET FOREIGN_KEY_CHECKS = 0;

CREATE DATABASE IF NOT EXISTS `shop_db`;
USE `shop_db`;

CREATE TABLE `users` (
  `id` int NOT NULL,
  `bio` text
);

INSERT INTO `users` (`id`, `bio`) VALUES (1, 'Hello; world -- not a comment');

-- DBASED_DATABASE: analytics_db
\connect "analytics_db"

DO $$ BEGIN
    CREATE TYPE "evt_enum" AS ENUM ('click', 'view');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

INSERT INTO "public"."events" ("kind") VALUES ('click');
"#;

        let items = parse_sql_script_items(script);
        assert_eq!(
            items,
            vec![
                SqlScriptItem::Statement("SET FOREIGN_KEY_CHECKS = 0".to_string()),
                SqlScriptItem::Statement("CREATE DATABASE IF NOT EXISTS `shop_db`".to_string()),
                SqlScriptItem::UseDatabase("shop_db".to_string()),
                SqlScriptItem::Statement(
                    "CREATE TABLE `users` (\n  `id` int NOT NULL,\n  `bio` text\n)".to_string()
                ),
                SqlScriptItem::Statement(
                    "INSERT INTO `users` (`id`, `bio`) VALUES (1, 'Hello; world -- not a comment')"
                        .to_string()
                ),
                SqlScriptItem::UseDatabase("analytics_db".to_string()),
                SqlScriptItem::UseDatabase("analytics_db".to_string()),
                SqlScriptItem::Statement(
                    "DO $$ BEGIN\n    CREATE TYPE \"evt_enum\" AS ENUM ('click', 'view');\nEXCEPTION\n    WHEN duplicate_object THEN NULL;\nEND $$"
                        .to_string()
                ),
                SqlScriptItem::Statement(
                    "INSERT INTO \"public\".\"events\" (\"kind\") VALUES ('click')".to_string()
                ),
            ]
        );
    }

    #[test]
    fn test_parse_sql_script_items_tagged_dollar_quotes_and_mysql_conditional_comments() {
        let script = r#"
/* Regular block comment with ; semicolons ; that must be stripped */
CREATE DATABASE IF NOT EXISTS `manual` /*!40100 DEFAULT CHARACTER SET utf8mb4 */ /*!80016 DEFAULT ENCRYPTION='N' */;
\c "my""db";
CREATE FUNCTION test_fn() RETURNS void AS $func_body$
BEGIN
    PERFORM 'inner;string', $$nested;dollar$$;
END;
$func_body$ LANGUAGE plpgsql;
INSERT INTO "tbl;name" (`col;name`) VALUES ('val;1');
"#;

        let items = parse_sql_script_items(script);
        assert_eq!(
            items,
            vec![
                SqlScriptItem::Statement(
                    "CREATE DATABASE IF NOT EXISTS `manual` /*!40100 DEFAULT CHARACTER SET utf8mb4 */ /*!80016 DEFAULT ENCRYPTION='N' */"
                        .to_string()
                ),
                SqlScriptItem::UseDatabase("my\"db".to_string()),
                SqlScriptItem::Statement(
                    "CREATE FUNCTION test_fn() RETURNS void AS $func_body$\nBEGIN\n    PERFORM 'inner;string', $$nested;dollar$$;\nEND;\n$func_body$ LANGUAGE plpgsql"
                        .to_string()
                ),
                SqlScriptItem::Statement(
                    "INSERT INTO \"tbl;name\" (`col;name`) VALUES ('val;1')".to_string()
                ),
            ]
        );
    }

    #[test]
    fn test_parse_sql_script_items_legacy_vs_modern_mysql_backslash_escaping() {
        // Legacy DBased export (no NO_AUTO_VALUE_ON_ZERO header): `\` is literal, `''` escapes `'`.
        // Even if a value ends with a single `\` before the closing `'`, or has `\''`, it must not eat `'`.
        let legacy_script = "-- DBased SQL Export\n-- Engine: MySQL\n\nSET FOREIGN_KEY_CHECKS = 0;\nINSERT INTO `items` VALUES ('C:\\'), ('echo \\''hello\\'';');\nSET FOREIGN_KEY_CHECKS = 1;\n";
        assert!(is_legacy_dbased_mysql_export(legacy_script));
        let legacy_items = parse_sql_script_items(legacy_script);
        assert_eq!(legacy_items.len(), 3);
        assert_eq!(
            legacy_items[1],
            SqlScriptItem::Statement(
                "INSERT INTO `items` VALUES ('C:\\'), ('echo \\''hello\\'';')".to_string()
            )
        );

        // Modern DBased export (with NO_AUTO_VALUE_ON_ZERO header) and standard mysqldump:
        let modern_script = "-- DBased SQL Export\n-- Engine: MySQL\n\nSET FOREIGN_KEY_CHECKS = 0;\nSET SQL_MODE = \"NO_AUTO_VALUE_ON_ZERO\";\nINSERT INTO `items` VALUES ('C:\\\\'), ('mysqldump \\'escaped\\' ; quote');\n";
        assert!(!is_legacy_dbased_mysql_export(modern_script));
        let modern_items = parse_sql_script_items(modern_script);
        assert_eq!(modern_items.len(), 3);
        assert_eq!(
            modern_items[2],
            SqlScriptItem::Statement(
                "INSERT INTO `items` VALUES ('C:\\\\'), ('mysqldump \\'escaped\\' ; quote')"
                    .to_string()
            )
        );
    }

    #[test]
    fn test_unquote_ident_and_extract_db_directives() {
        assert_eq!(unquote_ident("`manual`"), "manual");
        assert_eq!(unquote_ident("`my``db`"), "my`db");
        assert_eq!(unquote_ident("\"pg_db\""), "pg_db");
        assert_eq!(unquote_ident("\"my\"\"db\""), "my\"db");
        assert_eq!(unquote_ident("plain_db"), "plain_db");

        assert_eq!(
            extract_use_database_name("USE `manual`"),
            Some("manual".to_string())
        );
        assert_eq!(
            extract_use_database_name("use prod_db;"),
            Some("prod_db".to_string())
        );
        assert_eq!(extract_use_database_name("USE a b"), None);
        assert_eq!(extract_use_database_name("SELECT 1"), None);

        assert_eq!(
            extract_created_database_name(
                "CREATE DATABASE IF NOT EXISTS `manual` /*!40100 DEFAULT CHARACTER SET utf8mb4 */"
            ),
            Some("manual".to_string())
        );
        assert_eq!(
            extract_created_database_name("CREATE DATABASE \"analytics\";"),
            Some("analytics".to_string())
        );
        assert_eq!(extract_created_database_name("CREATE TABLE `users` (`id` int)"), None);
    }

    #[test]
    fn test_format_sql_insert_batch_mysql_and_pg_with_special_chars() {
        let cols = vec!["id".to_string(), "code".to_string()];
        let rows = vec![
            vec![
                DbValue::Text("1".to_string()),
                DbValue::Text(r"$escape = @$argf['e'] ?: '\\';".to_string()),
            ],
            vec![DbValue::Text("2".to_string()), DbValue::Null],
        ];

        let mysql_sql = format_sql_insert_batch(DbEngine::MySql, "snippets", &cols, &rows);
        assert_eq!(
            mysql_sql,
            "INSERT INTO `snippets` (`id`, `code`) VALUES\n  ('1', '$escape = @$argf[''e''] ?: ''\\\\\\\\'';'),\n  ('2', NULL);"
        );

        let pg_sql = format_sql_insert_batch(DbEngine::Postgres, "snippets", &cols, &rows);
        assert_eq!(
            pg_sql,
            "INSERT INTO \"public\".\"snippets\" (\"id\", \"code\") VALUES\n  ('1', '$escape = @$argf[''e''] ?: ''\\\\'';'),\n  ('2', NULL);"
        );
    }

    #[test]
    fn test_coerce_imported_str_value_across_column_types() {
        let nullable_int = ColumnInfo {
            ordinal: 1,
            name: "age".to_string(),
            data_type: "int".to_string(),
            is_nullable: true,
            is_primary_key: false,
            default_value: None,
            extra: String::new(),
        };
        assert_eq!(coerce_imported_str_value("", &nullable_int), DbValue::Null);
        assert_eq!(
            coerce_imported_str_value("NULL", &nullable_int),
            DbValue::Null
        );
        assert_eq!(
            coerce_imported_str_value("null", &nullable_int),
            DbValue::Null
        );
        assert_eq!(
            coerce_imported_str_value("42", &nullable_int),
            DbValue::Text("42".to_string())
        );

        let nullable_enum = ColumnInfo {
            ordinal: 2,
            name: "kind".to_string(),
            data_type: "enum('folder','file')".to_string(),
            is_nullable: true,
            is_primary_key: false,
            default_value: None,
            extra: String::new(),
        };
        assert_eq!(coerce_imported_str_value("", &nullable_enum), DbValue::Null);
        assert_eq!(
            coerce_imported_str_value("folder", &nullable_enum),
            DbValue::Text("folder".to_string())
        );

        let not_null_text = ColumnInfo {
            ordinal: 3,
            name: "note".to_string(),
            data_type: "varchar(255)".to_string(),
            is_nullable: false,
            is_primary_key: false,
            default_value: None,
            extra: String::new(),
        };
        assert_eq!(
            coerce_imported_str_value("", &not_null_text),
            DbValue::Text(String::new())
        );
        // Non-nullable column must keep literal "NULL" text rather than coercing to SQL NULL
        assert_eq!(
            coerce_imported_str_value("NULL", &not_null_text),
            DbValue::Text("NULL".to_string())
        );
    }

    // =========================================================================
    // 2. SQL Export Modes, Views, Deferred FKs, Pagination & Directory Tests
    // =========================================================================

    #[tokio::test]
    async fn test_sql_export_modes_structure_only_data_only_and_views() {
        let mut driver = MockDriver::new(DbEngine::MySql);
        driver
            .views
            .insert(("db_one".to_string(), "active_users_view".to_string()));
        driver.schemas.insert(
            ("db_one".to_string(), "active_users_view".to_string()),
            vec![ColumnInfo {
                ordinal: 1,
                name: "id".to_string(),
                data_type: "int".to_string(),
                is_nullable: false,
                is_primary_key: false,
                default_value: None,
                extra: String::new(),
            }],
        );
        driver.rows.lock().unwrap().insert(
            ("db_one".to_string(), "active_users_view".to_string()),
            vec![vec![DbValue::Text("1".to_string())]],
        );

        let selections = vec![DatabaseTableSelection {
            database: "db_one".to_string(),
            tables: vec!["users".to_string(), "active_users_view".to_string()],
        }];

        // A. StructureOnly without DROP TABLE and without CREATE DATABASE
        let mut buf_struct = Vec::new();
        export_selections_to_sql(
            &driver,
            &selections,
            SqlExportMode::StructureOnly,
            false,
            false,
            &mut buf_struct,
        )
        .await
        .unwrap();
        let sql_struct = String::from_utf8(buf_struct).unwrap();
        assert!(sql_struct.contains("SET SQL_MODE = \"NO_AUTO_VALUE_ON_ZERO\";"));
        assert!(!sql_struct.contains("CREATE DATABASE"));
        assert!(!sql_struct.contains("USE `db_one`"));
        assert!(!sql_struct.contains("DROP TABLE"));
        assert!(!sql_struct.contains("DROP VIEW"));
        assert!(sql_struct.contains("CREATE TABLE `users`"));
        assert!(sql_struct.contains("CREATE VIEW `active_users_view`"));
        assert!(!sql_struct.contains("INSERT INTO"));

        // B. DataOnly (must include table data, must skip view data, must omit CREATE TABLE)
        let mut buf_data = Vec::new();
        export_selections_to_sql(
            &driver,
            &selections,
            SqlExportMode::DataOnly,
            false,
            true,
            &mut buf_data,
        )
        .await
        .unwrap();
        let sql_data = String::from_utf8(buf_data).unwrap();
        assert!(!sql_data.contains("CREATE TABLE"));
        assert!(!sql_data.contains("CREATE VIEW"));
        assert!(sql_data.contains("INSERT INTO `users`"));
        assert!(!sql_data.contains("active_users_view"));

        // C. StructureAndData with DROP enabled (view has DROP VIEW + CREATE VIEW, no INSERT for view)
        let mut buf_both = Vec::new();
        export_selections_to_sql(
            &driver,
            &selections,
            SqlExportMode::StructureAndData,
            false,
            true,
            &mut buf_both,
        )
        .await
        .unwrap();
        let sql_both = String::from_utf8(buf_both).unwrap();
        assert!(sql_both.contains("DROP TABLE IF EXISTS `users`;"));
        assert!(sql_both.contains("DROP VIEW IF EXISTS `active_users_view`;"));
        assert!(sql_both.contains("INSERT INTO `users`"));
        assert!(!sql_both.contains("INSERT INTO `active_users_view`"));
    }

    #[tokio::test]
    async fn test_sql_export_postgres_deferred_foreign_keys_and_pre_create_enums() {
        let mut driver = MockDriver::new(DbEngine::Postgres);
        driver.custom_ddl.insert(
            ("db_one".to_string(), "users".to_string()),
            TableDdlParts {
                pre_create_statements: vec![
                    "DO $$ BEGIN CREATE TYPE \"user_role\" AS ENUM ('admin', 'member'); EXCEPTION WHEN duplicate_object THEN NULL; END $$;".to_string(),
                ],
                create_and_indexes_sql: "CREATE TABLE \"public\".\"users\" (\"id\" int PRIMARY KEY);".to_string(),
                post_data_constraints: vec![
                    "ALTER TABLE ONLY \"public\".\"users\" ADD CONSTRAINT \"users_org_fk\" FOREIGN KEY (\"org_id\") REFERENCES \"public\".\"orgs\"(\"id\");".to_string(),
                ],
            },
        );

        let selections = vec![DatabaseTableSelection {
            database: "db_one".to_string(),
            tables: vec!["users".to_string()],
        }];

        let mut buf = Vec::new();
        export_selections_to_sql(
            &driver,
            &selections,
            SqlExportMode::StructureAndData,
            true,
            true,
            &mut buf,
        )
        .await
        .unwrap();
        let sql = String::from_utf8(buf).unwrap();

        let enum_pos = sql.find("CREATE TYPE \"user_role\"").unwrap();
        let create_pos = sql.find("CREATE TABLE \"public\".\"users\"").unwrap();
        let insert_pos = sql.find("INSERT INTO \"public\".\"users\"").unwrap();
        let fk_pos = sql.find("ADD CONSTRAINT \"users_org_fk\"").unwrap();

        // Pre-create enum must appear before CREATE TABLE, which is before INSERT, which is before FK constraints!
        assert!(enum_pos < create_pos);
        assert!(create_pos < insert_pos);
        assert!(insert_pos < fk_pos);
    }

    #[tokio::test]
    async fn test_sql_export_large_table_pagination_and_chunking() {
        let driver = MockDriver::new(DbEngine::MySql);
        // Insert 1,100 rows into db_two.items (exceeds EXPORT_FETCH_BATCH_SIZE=1000 and SQL_INSERT_BATCH_SIZE=250)
        {
            let mut guard = driver.rows.lock().unwrap();
            let items_rows = guard
                .get_mut(&("db_two".to_string(), "items".to_string()))
                .unwrap();
            items_rows.clear();
            for i in 1..=1100 {
                items_rows.push(vec![
                    DbValue::Text(i.to_string()),
                    DbValue::Text(format!("Item {}", i)),
                    DbValue::Text((i % 100).to_string()),
                ]);
            }
        }

        let selections = vec![DatabaseTableSelection {
            database: "db_two".to_string(),
            tables: vec!["items".to_string()],
        }];

        let mut buf = Vec::new();
        export_selections_to_sql(
            &driver,
            &selections,
            SqlExportMode::DataOnly,
            false,
            false,
            &mut buf,
        )
        .await
        .unwrap();
        let sql = String::from_utf8(buf).unwrap();

        // 1,100 rows fetched in 2 batches (1000 + 100), chunked by 250 -> 4 chunks + 1 chunk = 5 INSERT statements
        let insert_count = sql.matches("INSERT INTO `items`").count();
        assert_eq!(insert_count, 5);
        assert!(sql.contains("('1', 'Item 1', '1')"));
        assert!(sql.contains("('1100', 'Item 1100', '0')"));
    }

    #[tokio::test]
    async fn test_sql_export_directory_destination_creates_per_db_files() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_sql_dir_test_{}", uuid::Uuid::new_v4()));
        let driver = MockDriver::new(DbEngine::MySql);
        let selections = vec![
            DatabaseTableSelection {
                database: "db_one".to_string(),
                tables: vec!["users".to_string()],
            },
            DatabaseTableSelection {
                database: "db_two".to_string(),
                tables: vec!["items".to_string()],
            },
        ];

        let spec = ExportSpec {
            format: TransferFormat::Sql,
            selections,
            sql_mode: SqlExportMode::StructureAndData,
            include_create_database: false,
            include_drop_table: true,
            destination: ExportDestination::Directory(tmp_dir.clone()),
        };

        run_export(&driver, &spec).await.unwrap();
        let db1_sql = fs::read_to_string(tmp_dir.join("db_one.sql")).unwrap();
        let db2_sql = fs::read_to_string(tmp_dir.join("db_two.sql")).unwrap();
        assert!(db1_sql.contains("CREATE TABLE `users`"));
        assert!(!db1_sql.contains("CREATE TABLE `items`"));
        assert!(db2_sql.contains("CREATE TABLE `items`"));
        assert!(!db2_sql.contains("CREATE TABLE `users`"));

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    // =========================================================================
    // 3. SQL Import Routing & Execution Tests
    // =========================================================================

    #[tokio::test]
    async fn test_single_db_sql_import_respects_explicit_target_database() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_single_sql_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        let sql_path = tmp_dir.join("manual.sql");
        let legacy_sql = r#"-- DBased SQL Export
-- Engine: MySQL

SET FOREIGN_KEY_CHECKS = 0;

-- ==========================================
-- Database: manual
-- ==========================================
CREATE DATABASE IF NOT EXISTS `manual`;
USE `manual`;

DROP TABLE IF EXISTS `items`;
CREATE TABLE `items` (`id` int unsigned NOT NULL AUTO_INCREMENT, `content` text);
INSERT INTO `items` (`id`, `content`) VALUES
  ('0', '$escape = @$argf[''e''] ?: ''\'';');
SET FOREIGN_KEY_CHECKS = 1;
"#;
        fs::write(&sql_path, legacy_sql).unwrap();

        assert!(is_legacy_dbased_mysql_export(legacy_sql));

        let mut driver = MockDriver::new(DbEngine::MySql);
        let imp_spec = ImportSpec {
            format: TransferFormat::Sql,
            source: ImportSource::Files(vec![sql_path]),
            target_database: Some("test_db".to_string()),
            target_table: None,
            truncate_before_import: false,
            current_ui_database: Some("test_db".to_string()),
        };

        run_import(&mut driver, &imp_spec).await.unwrap();
        let executed = driver.executed_sql.lock().unwrap().clone();
        let batches = driver.executed_batches.lock().unwrap().clone();

        // Every statement should have been executed against `test_db`, NOT `manual`,
        // `CREATE DATABASE IF NOT EXISTS \`manual\`` should have been skipped,
        // and `legacy_no_backslash_escapes` should be true on the batch.
        assert!(!executed.iter().any(|(db, _)| db == "manual"));
        assert!(!executed
            .iter()
            .any(|(_, s)| s.contains("CREATE DATABASE IF NOT EXISTS `manual`")));
        assert!(executed.iter().all(|(db, _)| db == "test_db"));
        assert!(executed
            .iter()
            .any(|(db, s)| db == "test_db" && s.starts_with("INSERT INTO `items`")));
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].0, "test_db");
        assert!(batches[0].2); // legacy_no_backslash_escapes == true

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[tokio::test]
    async fn test_sql_import_from_script_mode_and_multi_db_routing() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_multidb_sql_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        // 1. Single-DB script when target_database is None ("From Script / Multi-Database")
        let single_path = tmp_dir.join("single.sql");
        fs::write(
            &single_path,
            "CREATE DATABASE IF NOT EXISTS `new_db`;\nCREATE TABLE `t1` (`id` int);\n",
        )
        .unwrap();

        let mut driver = MockDriver::new(DbEngine::MySql);
        run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Sql,
                source: ImportSource::Files(vec![single_path]),
                target_database: None,
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        let executed = driver.executed_sql.lock().unwrap().clone();
        assert_eq!(
            executed,
            vec![
                (
                    "".to_string(),
                    "CREATE DATABASE IF NOT EXISTS `new_db`".to_string()
                ),
                ("new_db".to_string(), "CREATE TABLE `t1` (`id` int)".to_string()),
            ]
        );

        // 2. Multi-DB script (2 distinct databases) even when target_database is Some("other_db")
        // must honour the per-database switches inside the multi-DB dump.
        let multi_path = tmp_dir.join("multi.sql");
        fs::write(
            &multi_path,
            "USE `db_one`;\nINSERT INTO `users` VALUES (1);\nUSE `db_two`;\nINSERT INTO `items` VALUES (2);\n",
        )
        .unwrap();

        let mut driver2 = MockDriver::new(DbEngine::MySql);
        run_import(
            &mut driver2,
            &ImportSpec {
                format: TransferFormat::Sql,
                source: ImportSource::Files(vec![multi_path]),
                target_database: Some("other_db".to_string()),
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        let executed2 = driver2.executed_sql.lock().unwrap().clone();
        assert_eq!(
            executed2,
            vec![
                ("db_one".to_string(), "INSERT INTO `users` VALUES (1)".to_string()),
                ("db_two".to_string(), "INSERT INTO `items` VALUES (2)".to_string()),
            ]
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[tokio::test]
    async fn test_sql_import_directory_recursive_sorted_execution() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_sql_rec_test_{}", uuid::Uuid::new_v4()));
        let sub_dir = tmp_dir.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();

        fs::write(tmp_dir.join("01_schema.sql"), "CREATE TABLE `users` (`id` int);").unwrap();
        fs::write(sub_dir.join("02_seed.sql"), "INSERT INTO `users` VALUES (1);").unwrap();

        let mut driver = MockDriver::new(DbEngine::MySql);
        run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Sql,
                source: ImportSource::Directory(tmp_dir.clone()),
                target_database: Some("db_one".to_string()),
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        let executed = driver.executed_sql.lock().unwrap().clone();
        assert_eq!(
            executed,
            vec![
                ("db_one".to_string(), "CREATE TABLE `users` (`id` int)".to_string()),
                ("db_one".to_string(), "INSERT INTO `users` VALUES (1)".to_string()),
            ]
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    // =========================================================================
    // 4. CSV Export & Import Tests (Single File, Directory, Reordered Headers, Special Chars, Errors)
    // =========================================================================

    #[tokio::test]
    async fn test_csv_export_and_import_single_file_custom_table_and_reordered_headers() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_csv_single_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        let driver = MockDriver::new(DbEngine::MySql);
        let csv_path = tmp_dir.join("users_export.csv");

        // Export single table to single CSV file
        run_export(
            &driver,
            &ExportSpec {
                format: TransferFormat::Csv,
                selections: vec![DatabaseTableSelection {
                    database: "db_one".to_string(),
                    tables: vec!["users".to_string()],
                }],
                sql_mode: SqlExportMode::DataOnly,
                include_create_database: false,
                include_drop_table: false,
                destination: ExportDestination::SingleFile(csv_path.clone()),
            },
        )
        .await
        .unwrap();

        let exported_csv = fs::read_to_string(&csv_path).unwrap();
        assert_eq!(exported_csv, "id,name,score\n1,Alice,95\n2,Bob,\n");

        // Now write a CSV with case-insensitive, reordered subset of columns (`SCORE,NAME,ID`)
        // and import it into explicit target_table `users` despite filename `custom_upload.csv`
        let custom_csv_path = tmp_dir.join("custom_upload.csv");
        fs::write(
            &custom_csv_path,
            "SCORE,NAME,ID\n100,\"Carol, Jr.\",3\nNULL,\"Dave\nMultiline\",4\n",
        )
        .unwrap();

        let mut import_driver = MockDriver::new(DbEngine::MySql);
        run_import(
            &mut import_driver,
            &ImportSpec {
                format: TransferFormat::Csv,
                source: ImportSource::Files(vec![custom_csv_path]),
                target_database: Some("db_one".to_string()),
                target_table: Some("users".to_string()),
                truncate_before_import: true,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        let batches = import_driver.inserted_batches.lock().unwrap().clone();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].0, "db_one");
        assert_eq!(batches[0].1, "users");
        assert_eq!(
            batches[0].2,
            vec!["score".to_string(), "name".to_string(), "id".to_string()]
        );
        assert_eq!(
            batches[0].3,
            vec![
                vec![
                    DbValue::Text("100".to_string()),
                    DbValue::Text("Carol, Jr.".to_string()),
                    DbValue::Text("3".to_string()),
                ],
                vec![
                    DbValue::Null,
                    DbValue::Text("Dave\nMultiline".to_string()),
                    DbValue::Text("4".to_string()),
                ],
            ]
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[tokio::test]
    async fn test_csv_import_multiple_files_and_flat_directory() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_csv_multi_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        let driver = MockDriver::new(DbEngine::MySql);
        // Single-DB multi-table CSV directory export writes `<dir>/users.csv` and `<dir>/audit_logs.csv`
        run_export(
            &driver,
            &ExportSpec {
                format: TransferFormat::Csv,
                selections: vec![DatabaseTableSelection {
                    database: "db_one".to_string(),
                    tables: vec!["users".to_string(), "audit_logs".to_string()],
                }],
                sql_mode: SqlExportMode::DataOnly,
                include_create_database: false,
                include_drop_table: false,
                destination: ExportDestination::Directory(tmp_dir.clone()),
            },
        )
        .await
        .unwrap();

        assert!(tmp_dir.join("users.csv").exists());
        assert!(tmp_dir.join("audit_logs.csv").exists());

        // Import from flat directory into db_one with truncate=true
        let mut import_driver = MockDriver::new(DbEngine::MySql);
        run_import(
            &mut import_driver,
            &ImportSpec {
                format: TransferFormat::Csv,
                source: ImportSource::Directory(tmp_dir.clone()),
                target_database: Some("db_one".to_string()),
                target_table: None,
                truncate_before_import: true,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        // Also import multiple explicit files (`ImportSource::Files` with 2 files) in append mode
        run_import(
            &mut import_driver,
            &ImportSpec {
                format: TransferFormat::Csv,
                source: ImportSource::Files(vec![
                    tmp_dir.join("users.csv"),
                    tmp_dir.join("audit_logs.csv"),
                ]),
                target_database: Some("db_one".to_string()),
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        let rows = import_driver.rows.lock().unwrap().clone();
        assert_eq!(rows[&("db_one".to_string(), "users".to_string())].len(), 4);
        assert_eq!(
            rows[&("db_one".to_string(), "audit_logs".to_string())].len(),
            2
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[tokio::test]
    async fn test_csv_export_and_import_validation_errors() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_csv_err_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        let mut driver = MockDriver::new(DbEngine::MySql);
        driver
            .views
            .insert(("db_one".to_string(), "users_view".to_string()));

        // 1. Empty export selection
        let err = run_export(
            &driver,
            &ExportSpec {
                format: TransferFormat::Csv,
                selections: vec![],
                sql_mode: SqlExportMode::DataOnly,
                include_create_database: false,
                include_drop_table: false,
                destination: ExportDestination::SingleFile(tmp_dir.join("out.csv")),
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("select at least one database"));

        // 2. Multi-table CSV export to SingleFile must error
        let err = run_export(
            &driver,
            &ExportSpec {
                format: TransferFormat::Csv,
                selections: vec![DatabaseTableSelection {
                    database: "db_one".to_string(),
                    tables: vec!["users".to_string(), "audit_logs".to_string()],
                }],
                sql_mode: SqlExportMode::DataOnly,
                include_create_database: false,
                include_drop_table: false,
                destination: ExportDestination::SingleFile(tmp_dir.join("out.csv")),
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("requires exactly 1 table"));

        // 3. Unknown column in CSV header
        let bad_col_csv = tmp_dir.join("users.csv");
        fs::write(&bad_col_csv, "id,non_existent_col\n1,foo\n").unwrap();
        let err = run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Csv,
                source: ImportSource::Files(vec![bad_col_csv]),
                target_database: Some("db_one".to_string()),
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("non_existent_col"));

        // 4. Attempting to import CSV into a VIEW (views are excluded from target matching)
        let view_csv = tmp_dir.join("users_view.csv");
        fs::write(&view_csv, "id\n1\n").unwrap();
        let err = run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Csv,
                source: ImportSource::Files(vec![view_csv]),
                target_database: Some("db_one".to_string()),
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"));

        // 5. Missing target_database on single-file CSV import
        let valid_csv = tmp_dir.join("users_valid.csv");
        fs::write(&valid_csv, "id,name\n1,Alice\n").unwrap();
        let err = run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Csv,
                source: ImportSource::Files(vec![valid_csv]),
                target_database: None,
                target_table: Some("users".to_string()),
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("select a target database"));

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    // =========================================================================
    // 5. JSON Export & Import Tests (Single Array, Bundle, Directory, Sparse Rows, Errors)
    // =========================================================================

    #[tokio::test]
    async fn test_json_single_table_array_export_and_import_with_sparse_rows_and_types() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_json_single_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        let driver = MockDriver::new(DbEngine::Postgres);
        let json_path = tmp_dir.join("users.json");

        // Export 1 table to SingleFile -> produces top-level JSON array
        run_export(
            &driver,
            &ExportSpec {
                format: TransferFormat::Json,
                selections: vec![DatabaseTableSelection {
                    database: "db_one".to_string(),
                    tables: vec!["users".to_string()],
                }],
                sql_mode: SqlExportMode::DataOnly,
                include_create_database: false,
                include_drop_table: false,
                destination: ExportDestination::SingleFile(json_path.clone()),
            },
        )
        .await
        .unwrap();

        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&json_path).unwrap()).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert_eq!(parsed[0]["name"], "Alice");
        assert!(parsed[1]["score"].is_null());

        // Now test importing sparse rows with heterogeneous JSON value types (numbers, booleans, nulls)
        let custom_json = tmp_dir.join("custom_rows.json");
        fs::write(
            &custom_json,
            r#"[
                {"id": 10, "name": "NumericId"},
                {"id": 11, "name": "WithScore", "score": 77}
            ]"#,
        )
        .unwrap();

        let mut import_driver = MockDriver::new(DbEngine::Postgres);
        run_import(
            &mut import_driver,
            &ImportSpec {
                format: TransferFormat::Json,
                source: ImportSource::Files(vec![custom_json]),
                target_database: Some("db_one".to_string()),
                target_table: Some("users".to_string()),
                truncate_before_import: true,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        let batches = import_driver.inserted_batches.lock().unwrap().clone();
        assert_eq!(batches.len(), 1);
        // Columns must be sorted by table ordinal: id (1), name (2), score (3)
        assert_eq!(
            batches[0].2,
            vec!["id".to_string(), "name".to_string(), "score".to_string()]
        );
        assert_eq!(
            batches[0].3,
            vec![
                vec![
                    DbValue::Text("10".to_string()),
                    DbValue::Text("NumericId".to_string()),
                    DbValue::Null,
                ],
                vec![
                    DbValue::Text("11".to_string()),
                    DbValue::Text("WithScore".to_string()),
                    DbValue::Text("77".to_string()),
                ],
            ]
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[tokio::test]
    async fn test_json_single_db_multi_table_object_and_directory_modes() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_json_modes_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        // 1. Top-level object mapping table names to arrays (for a single target_database)
        let single_db_bundle = tmp_dir.join("db_one_tables.json");
        fs::write(
            &single_db_bundle,
            r#"{
                "users": [{"id": "5", "name": "Eve", "score": "88"}],
                "audit_logs": [{"id": "200", "action": "logout"}]
            }"#,
        )
        .unwrap();

        let mut driver = MockDriver::new(DbEngine::MySql);
        run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Json,
                source: ImportSource::Files(vec![single_db_bundle]),
                target_database: Some("db_one".to_string()),
                target_table: None,
                truncate_before_import: true,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        let rows = driver.rows.lock().unwrap().clone();
        assert_eq!(rows[&("db_one".to_string(), "users".to_string())].len(), 1);
        assert_eq!(
            rows[&("db_one".to_string(), "audit_logs".to_string())].len(),
            1
        );

        // 2. Multi-DB Directory Export & Import of per-table JSON files (`<dir>/<db>/<table>.json`)
        let json_dir = tmp_dir.join("json_dir_export");
        run_export(
            &driver,
            &ExportSpec {
                format: TransferFormat::Json,
                selections: vec![
                    DatabaseTableSelection {
                        database: "db_one".to_string(),
                        tables: vec!["users".to_string()],
                    },
                    DatabaseTableSelection {
                        database: "db_two".to_string(),
                        tables: vec!["items".to_string()],
                    },
                ],
                sql_mode: SqlExportMode::DataOnly,
                include_create_database: false,
                include_drop_table: false,
                destination: ExportDestination::Directory(json_dir.clone()),
            },
        )
        .await
        .unwrap();

        assert!(json_dir.join("db_one").join("users.json").exists());
        assert!(json_dir.join("db_two").join("items.json").exists());

        run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Json,
                source: ImportSource::Directory(json_dir),
                target_database: None,
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap();

        let rows_after = driver.rows.lock().unwrap().clone();
        assert_eq!(
            rows_after[&("db_one".to_string(), "users".to_string())].len(),
            2
        );
        assert_eq!(
            rows_after[&("db_two".to_string(), "items".to_string())].len(),
            2
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[tokio::test]
    async fn test_json_inspect_bundle_tables_and_validation_errors() {
        let bundle_str = r#"{
            "databases": {
                "db_one": {
                    "users": [],
                    "audit_logs": []
                },
                "db_two": {
                    "tables": {
                        "items": []
                    }
                }
            }
        }"#;
        let map = inspect_json_bundle_tables(bundle_str).unwrap();
        assert_eq!(
            map.get("db_one").unwrap(),
            &vec!["audit_logs".to_string(), "users".to_string()]
        );
        assert_eq!(map.get("db_two").unwrap(), &vec!["items".to_string()]);
        assert!(inspect_json_bundle_tables("[1, 2, 3]").is_none());

        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_json_err_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let mut driver = MockDriver::new(DbEngine::MySql);

        // 1. Unknown JSON key
        let bad_key_json = tmp_dir.join("users.json");
        fs::write(&bad_key_json, r#"[{"id": "1", "unknown_field": "val"}]"#).unwrap();
        let err = run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Json,
                source: ImportSource::Files(vec![bad_key_json]),
                target_database: Some("db_one".to_string()),
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("unknown_field"));

        // 2. Non-existent database in JSON bundle
        let bad_db_json = tmp_dir.join("bundle.json");
        fs::write(
            &bad_db_json,
            r#"{"databases": {"missing_db": {"users": []}}}"#,
        )
        .unwrap();
        let err = run_import(
            &mut driver,
            &ImportSpec {
                format: TransferFormat::Json,
                source: ImportSource::Files(vec![bad_db_json]),
                target_database: None,
                target_table: None,
                truncate_before_import: false,
                current_ui_database: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("missing_db"));

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    // =========================================================================
    // 6. Full Multi-Database End-to-End Round-Trip (SQL, CSV, JSON)
    // =========================================================================

    #[tokio::test]
    async fn test_multi_db_export_and_import_roundtrip_sql_csv_json() {
        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_transfer_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        let selections = vec![
            DatabaseTableSelection {
                database: "db_one".to_string(),
                tables: vec!["users".to_string()],
            },
            DatabaseTableSelection {
                database: "db_two".to_string(),
                tables: vec!["items".to_string()],
            },
        ];

        // 1. SQL Multi-DB Export & Import
        let mut driver = MockDriver::new(DbEngine::MySql);
        let sql_path = tmp_dir.join("dump.sql");
        let sql_exp_spec = ExportSpec {
            format: TransferFormat::Sql,
            selections: selections.clone(),
            sql_mode: SqlExportMode::StructureAndData,
            include_create_database: true,
            include_drop_table: true,
            destination: ExportDestination::SingleFile(sql_path.clone()),
        };
        run_export(&driver, &sql_exp_spec).await.unwrap();
        let sql_imp_spec = ImportSpec {
            format: TransferFormat::Sql,
            source: ImportSource::Files(vec![sql_path]),
            target_database: None,
            target_table: None,
            truncate_before_import: false,
            current_ui_database: None,
        };
        run_import(&mut driver, &sql_imp_spec).await.unwrap();
        let executed = driver.executed_sql.lock().unwrap().clone();
        assert!(executed
            .iter()
            .any(|(db, s)| db == "db_one" && s.contains("INSERT INTO `users`")));
        assert!(executed
            .iter()
            .any(|(db, s)| db == "db_two" && s.contains("INSERT INTO `items`")));

        // 2. CSV Multi-DB Directory Export & Import (with truncate)
        let csv_dir = tmp_dir.join("csv_export");
        let csv_exp_spec = ExportSpec {
            format: TransferFormat::Csv,
            selections: selections.clone(),
            sql_mode: SqlExportMode::DataOnly,
            include_create_database: false,
            include_drop_table: false,
            destination: ExportDestination::Directory(csv_dir.clone()),
        };
        run_export(&driver, &csv_exp_spec).await.unwrap();
        assert!(csv_dir.join("db_one").join("users.csv").exists());
        assert!(csv_dir.join("db_two").join("items.csv").exists());

        let csv_imp_spec = ImportSpec {
            format: TransferFormat::Csv,
            source: ImportSource::Directory(csv_dir),
            target_database: None,
            target_table: None,
            truncate_before_import: true,
            current_ui_database: None,
        };
        run_import(&mut driver, &csv_imp_spec).await.unwrap();
        let rows_after_csv = driver.rows.lock().unwrap().clone();
        assert_eq!(
            rows_after_csv[&("db_one".to_string(), "users".to_string())].len(),
            2
        );
        assert_eq!(
            rows_after_csv[&("db_one".to_string(), "users".to_string())][1][2],
            DbValue::Null
        );

        // 3. JSON Multi-DB Bundle Export & Import (append mode -> doubles rows!)
        let json_path = tmp_dir.join("bundle.json");
        let json_exp_spec = ExportSpec {
            format: TransferFormat::Json,
            selections,
            sql_mode: SqlExportMode::DataOnly,
            include_create_database: false,
            include_drop_table: false,
            destination: ExportDestination::SingleFile(json_path.clone()),
        };
        run_export(&driver, &json_exp_spec).await.unwrap();
        let json_imp_spec = ImportSpec {
            format: TransferFormat::Json,
            source: ImportSource::Files(vec![json_path]),
            target_database: None,
            target_table: None,
            truncate_before_import: false,
            current_ui_database: None,
        };
        run_import(&mut driver, &json_imp_spec).await.unwrap();
        let rows_after_json = driver.rows.lock().unwrap().clone();
        assert_eq!(
            rows_after_json[&("db_one".to_string(), "users".to_string())].len(),
            4
        );
        assert_eq!(
            rows_after_json[&("db_two".to_string(), "items".to_string())].len(),
            2
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
