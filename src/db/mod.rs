pub mod driver;
pub mod models;
pub mod mysql;
pub mod postgres;
pub mod ssh;
pub mod transfer;

use crate::config::bookmarks::{ConnectionProfile, DbEngine};
use crate::db::driver::DatabaseDriver;
use crate::db::models::*;
use crate::db::mysql::MySqlDriver;
use crate::db::postgres::PostgresDriver;
use crate::db::ssh::SshTunnel;
use anyhow::Result;
use async_channel::{Receiver, Sender};

struct ActiveSession {
    _tunnel: Option<SshTunnel>,
    driver: Box<dyn DatabaseDriver>,
    cached_tables: Vec<String>,
}

async fn establish_session(profile: &ConnectionProfile) -> Result<ActiveSession> {
    let bm = &profile.bookmark;
    let (target_host, target_port, tunnel) = if bm.ssh.enabled {
        let ssh_tunnel = SshTunnel::establish(
            &bm.ssh,
            &profile.ssh_password_or_passphrase,
            &bm.host,
            bm.port,
        )
        .await?;
        let local_port = ssh_tunnel.local_port();
        ("127.0.0.1".to_string(), local_port, Some(ssh_tunnel))
    } else {
        (bm.host.clone(), bm.port, None)
    };

    let driver: Box<dyn DatabaseDriver> = match bm.engine {
        DbEngine::MySql => {
            let d = MySqlDriver::connect(
                &target_host,
                target_port,
                &bm.username,
                &profile.db_password,
                &bm.default_database,
            )
            .await?;
            Box::new(d)
        }
        DbEngine::Postgres => {
            let d = PostgresDriver::connect(
                &target_host,
                target_port,
                &bm.username,
                &profile.db_password,
                &bm.default_database,
            )
            .await?;
            Box::new(d)
        }
    };

    Ok(ActiveSession {
        _tunnel: tunnel,
        driver,
        cached_tables: Vec::new(),
    })
}

pub fn spawn_db_worker() -> (Sender<BackendCommand>, Receiver<BackendEvent>) {
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<BackendCommand>();
    let (evt_tx, evt_rx) = async_channel::unbounded::<BackendEvent>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to initialize Tokio runtime");

        rt.block_on(async move {
            let mut session: Option<ActiveSession> = None;

            while let Ok(cmd) = cmd_rx.recv().await {
                match cmd {
                    BackendCommand::TestConnection(profile) => {
                        let res = match establish_session(&profile).await {
                            Ok(s) => match s.driver.list_databases().await {
                                Ok(dbs) => Ok(format!(
                                    "Connected to {} ({} database(s) accessible)",
                                    profile.bookmark.engine.display_name(),
                                    dbs.len()
                                )),
                                Err(e) => Err(format!("{:#}", e)),
                            },
                            Err(e) => Err(format!("{:#}", e)),
                        };
                        let _ = evt_tx.send(BackendEvent::ConnectionTestResult(res)).await;
                    }
                    BackendCommand::Connect(profile) => {
                        match establish_session(&profile).await {
                            Ok(mut new_session) => {
                                match new_session.driver.list_databases().await {
                                    Ok(databases) => {
                                        let default_db = profile.bookmark.default_database.trim();
                                        let selected_database = if !default_db.is_empty()
                                            && databases.iter().any(|d| d == default_db)
                                        {
                                            Some(default_db.to_string())
                                        } else {
                                            databases.first().cloned()
                                        };

                                        let mut tables = Vec::new();
                                        if let Some(ref db) = selected_database {
                                            let _ = new_session.driver.use_database(db).await;
                                            if let Ok(t) = new_session.driver.list_tables(db).await
                                            {
                                                new_session.cached_tables =
                                                    t.iter().map(|ti| ti.name.clone()).collect();
                                                tables = t;
                                            }
                                        }

                                        let engine_label = format!(
                                            "{}{}",
                                            profile.bookmark.engine.display_name(),
                                            if profile.bookmark.ssh.enabled {
                                                " (SSH)"
                                            } else {
                                                ""
                                            }
                                        );

                                        session = Some(new_session);
                                        let _ = evt_tx
                                            .send(BackendEvent::Connected {
                                                connection_name: profile.bookmark.name.clone(),
                                                engine: profile.bookmark.engine,
                                                engine_label,
                                                databases,
                                                selected_database,
                                                tables,
                                            })
                                            .await;
                                    }
                                    Err(e) => {
                                        let _ = evt_tx
                                            .send(BackendEvent::Error(format!("{:#}", e)))
                                            .await;
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = evt_tx
                                    .send(BackendEvent::Error(format!("{:#}", e)))
                                    .await;
                            }
                        }
                    }
                    BackendCommand::Disconnect => {
                        session = None;
                        let _ = evt_tx.send(BackendEvent::Disconnected).await;
                    }
                    BackendCommand::Refresh { current_database } => {
                        if let Some(ref mut sess) = session {
                            match sess.driver.list_databases().await {
                                Ok(databases) => {
                                    let selected_database = match current_database.as_deref() {
                                        Some(cur)
                                            if !cur.is_empty()
                                                && databases.iter().any(|d| d == cur) =>
                                        {
                                            Some(cur.to_string())
                                        }
                                        _ => databases.first().cloned(),
                                    };

                                    let mut tables = Vec::new();
                                    if let Some(ref db) = selected_database {
                                        if let Err(e) = sess.driver.use_database(db).await {
                                            let _ = evt_tx
                                                .send(BackendEvent::Error(format!("{:#}", e)))
                                                .await;
                                            continue;
                                        }
                                        match sess.driver.list_tables(db).await {
                                            Ok(t) => {
                                                sess.cached_tables =
                                                    t.iter().map(|ti| ti.name.clone()).collect();
                                                tables = t;
                                            }
                                            Err(e) => {
                                                let _ = evt_tx
                                                    .send(BackendEvent::Error(format!("{:#}", e)))
                                                    .await;
                                                continue;
                                            }
                                        }
                                    } else {
                                        sess.cached_tables.clear();
                                    }

                                    let _ = evt_tx
                                        .send(BackendEvent::Refreshed {
                                            databases,
                                            selected_database,
                                            tables,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::CreateDatabase(spec) => {
                        if let Some(ref mut sess) = session {
                            let db_name = spec.name.trim().to_string();
                            if let Err(e) = sess.driver.create_database(&spec).await {
                                let _ = evt_tx
                                    .send(BackendEvent::Error(format!("{:#}", e)))
                                    .await;
                                continue;
                            }
                            if let Err(e) = sess.driver.use_database(&db_name).await {
                                let _ = evt_tx
                                    .send(BackendEvent::Error(format!("{:#}", e)))
                                    .await;
                                continue;
                            }
                            match sess.driver.list_databases().await {
                                Ok(databases) => {
                                    let tables = sess
                                        .driver
                                        .list_tables(&db_name)
                                        .await
                                        .unwrap_or_default();
                                    sess.cached_tables =
                                        tables.iter().map(|t| t.name.clone()).collect();
                                    let _ = evt_tx
                                        .send(BackendEvent::DatabaseCreated {
                                            databases,
                                            selected_database: db_name.clone(),
                                            tables,
                                            toast: format!("Created database '{}'", db_name),
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::SelectDatabase(database) => {
                        if let Some(ref mut sess) = session {
                            if let Err(e) = sess.driver.use_database(&database).await {
                                let _ = evt_tx
                                    .send(BackendEvent::Error(format!("{:#}", e)))
                                    .await;
                                continue;
                            }
                            match sess.driver.list_tables(&database).await {
                                Ok(tables) => {
                                    sess.cached_tables =
                                        tables.iter().map(|t| t.name.clone()).collect();
                                    let _ = evt_tx
                                        .send(BackendEvent::DatabaseSelected { database, tables })
                                        .await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::SelectTable {
                        database,
                        table,
                        content_query,
                    } => {
                        if let Some(ref sess) = session {
                            let structure_res =
                                sess.driver.get_table_structure(&database, &table).await;
                            let indexes_res =
                                sess.driver.get_table_indexes(&database, &table).await;
                            let content_res = sess
                                .driver
                                .get_table_content(&database, &table, &content_query)
                                .await;
                            let relations_res =
                                sess.driver.get_relations(&database, &table).await;

                            match (structure_res, indexes_res, content_res, relations_res) {
                                (Ok(structure), Ok(indexes), Ok(content), Ok(relations)) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::TableLoaded {
                                            database,
                                            table,
                                            all_tables: sess.cached_tables.clone(),
                                            structure,
                                            indexes,
                                            content,
                                            relations,
                                            content_query,
                                        })
                                        .await;
                                }
                                (Err(e), _, _, _)
                                | (_, Err(e), _, _)
                                | (_, _, Err(e), _)
                                | (_, _, _, Err(e)) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::RefreshStructure { database, table } => {
                        if let Some(ref sess) = session {
                            let structure_res =
                                sess.driver.get_table_structure(&database, &table).await;
                            let indexes_res =
                                sess.driver.get_table_indexes(&database, &table).await;
                            match (structure_res, indexes_res) {
                                (Ok(structure), Ok(indexes)) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::StructureUpdated {
                                            database,
                                            table,
                                            structure,
                                            indexes,
                                            toast: None,
                                        })
                                        .await;
                                }
                                (Err(e), _) | (_, Err(e)) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::AlterColumn {
                        database,
                        table,
                        op,
                    } => {
                        if let Some(ref sess) = session {
                            let msg = match &op {
                                ColumnAlterOp::Add(s) => format!("Added column '{}'", s.name),
                                ColumnAlterOp::Modify { spec, .. } => {
                                    format!("Updated column '{}'", spec.name)
                                }
                                ColumnAlterOp::Drop { name } => format!("Dropped column '{}'", name),
                                ColumnAlterOp::Reorder { column, .. } => {
                                    format!("Moved column '{}'", column.name)
                                }
                            };
                            match sess.driver.alter_column(&database, &table, op).await {
                                Ok(()) => {
                                    if let (Ok(structure), Ok(indexes)) = (
                                        sess.driver.get_table_structure(&database, &table).await,
                                        sess.driver.get_table_indexes(&database, &table).await,
                                    ) {
                                        let _ = evt_tx
                                            .send(BackendEvent::StructureUpdated {
                                                database,
                                                table,
                                                structure,
                                                indexes,
                                                toast: Some(msg),
                                            })
                                            .await;
                                    }
                                }
                                Err(e) => {
                                    if let (Ok(structure), Ok(indexes)) = (
                                        sess.driver.get_table_structure(&database, &table).await,
                                        sess.driver.get_table_indexes(&database, &table).await,
                                    ) {
                                        let _ = evt_tx
                                            .send(BackendEvent::StructureUpdated {
                                                database,
                                                table,
                                                structure,
                                                indexes,
                                                toast: None,
                                            })
                                            .await;
                                    }
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::AlterIndex {
                        database,
                        table,
                        op,
                    } => {
                        if let Some(ref sess) = session {
                            let msg = match &op {
                                IndexAlterOp::Add(s) => {
                                    let label = if s.name.trim().is_empty() {
                                        s.index_type.as_str()
                                    } else {
                                        s.name.as_str()
                                    };
                                    format!("Added index '{}'", label)
                                }
                                IndexAlterOp::Modify { spec, old_name, .. } => {
                                    let label = if spec.name.trim().is_empty() {
                                        old_name.as_str()
                                    } else {
                                        spec.name.as_str()
                                    };
                                    format!("Updated index '{}'", label)
                                }
                                IndexAlterOp::Drop { name, .. } => {
                                    format!("Dropped index '{}'", name)
                                }
                            };
                            match sess.driver.alter_index(&database, &table, op).await {
                                Ok(()) => {
                                    if let (Ok(structure), Ok(indexes)) = (
                                        sess.driver.get_table_structure(&database, &table).await,
                                        sess.driver.get_table_indexes(&database, &table).await,
                                    ) {
                                        let _ = evt_tx
                                            .send(BackendEvent::StructureUpdated {
                                                database,
                                                table,
                                                structure,
                                                indexes,
                                                toast: Some(msg),
                                            })
                                            .await;
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::FetchTableContent {
                        database,
                        table,
                        query,
                    } => {
                        if let Some(ref sess) = session {
                            match sess
                                .driver
                                .get_table_content(&database, &table, &query)
                                .await
                            {
                                Ok(page) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::ContentLoaded { page, toast: None })
                                        .await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::CommitContentChanges {
                        database,
                        table,
                        updates,
                        inserts,
                        query_for_refresh,
                    } => {
                        if let Some(ref sess) = session {
                            for evt in commit_content_changes_with_driver(
                                sess.driver.as_ref(),
                                &database,
                                &table,
                                &updates,
                                &inserts,
                                &query_for_refresh,
                            )
                            .await
                            {
                                let _ = evt_tx.send(evt).await;
                            }
                        }
                    }
                    BackendCommand::DeleteRows {
                        database,
                        table,
                        pks,
                        query_for_refresh,
                    } => {
                        if let Some(ref sess) = session {
                            match sess.driver.delete_rows(&database, &table, &pks).await {
                                Ok(deleted) => {
                                    if let Ok(page) = sess
                                        .driver
                                        .get_table_content(&database, &table, &query_for_refresh)
                                        .await
                                    {
                                        let _ = evt_tx
                                            .send(BackendEvent::ContentLoaded {
                                                page,
                                                toast: Some(format!("Deleted {} row(s)", deleted)),
                                            })
                                            .await;
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::RefreshRelations { database, table } => {
                        if let Some(ref sess) = session {
                            match sess.driver.get_relations(&database, &table).await {
                                Ok(relations) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::RelationsUpdated {
                                            database,
                                            table,
                                            relations,
                                            toast: None,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::AddRelation {
                        database,
                        table,
                        fk,
                    } => {
                        if let Some(ref sess) = session {
                            match sess.driver.add_relation(&database, &table, &fk).await {
                                Ok(()) => {
                                    if let Ok(relations) =
                                        sess.driver.get_relations(&database, &table).await
                                    {
                                        let _ = evt_tx
                                            .send(BackendEvent::RelationsUpdated {
                                                database,
                                                table,
                                                relations,
                                                toast: Some("Added foreign key relation".to_string()),
                                            })
                                            .await;
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::DropRelation {
                        database,
                        table,
                        constraint_name,
                    } => {
                        if let Some(ref sess) = session {
                            match sess
                                .driver
                                .drop_relation(&database, &table, &constraint_name)
                                .await
                            {
                                Ok(()) => {
                                    if let Ok(relations) =
                                        sess.driver.get_relations(&database, &table).await
                                    {
                                        let _ = evt_tx
                                            .send(BackendEvent::RelationsUpdated {
                                                database,
                                                table,
                                                relations,
                                                toast: Some(format!(
                                                    "Dropped relation '{}'",
                                                    constraint_name
                                                )),
                                            })
                                            .await;
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::FetchTargetTableColumns { database, table } => {
                        if let Some(ref sess) = session {
                            if let Ok(cols) =
                                sess.driver.get_table_structure(&database, &table).await
                            {
                                let col_names = cols.into_iter().map(|c| c.name).collect();
                                let _ = evt_tx
                                    .send(BackendEvent::TargetTableColumnsLoaded {
                                        table,
                                        columns: col_names,
                                    })
                                    .await;
                            }
                        }
                    }
                    BackendCommand::ExecuteRawQuery { database, sql } => {
                        if let Some(ref sess) = session {
                            let res = sess
                                .driver
                                .execute_raw_query(&database, &sql)
                                .await
                                .map_err(|e| format!("{:#}", e));
                            let _ = evt_tx.send(BackendEvent::RawQueryFinished(res)).await;
                        } else {
                            let _ = evt_tx
                                .send(BackendEvent::RawQueryFinished(Err(
                                    "Not connected to any database".to_string(),
                                )))
                                .await;
                        }
                    }
                    BackendCommand::RefreshUsers { selected_user } => {
                        if let Some(ref sess) = session {
                            match load_users_and_permissions(&*sess.driver, selected_user, None)
                                .await
                            {
                                Ok(evt) => {
                                    let _ = evt_tx.send(evt).await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::SelectUserForPermissions { username, host } => {
                        if let Some(ref sess) = session {
                            match sess
                                .driver
                                .get_database_permissions(&username, host.as_deref())
                                .await
                            {
                                Ok(db_permissions) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::UserDbPermissionsLoaded {
                                            username,
                                            host,
                                            db_permissions,
                                            toast: None,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::CreateUser(spec) => {
                        if let Some(ref sess) = session {
                            let target = (spec.username.trim().to_string(), spec.host.clone());
                            let label = match &spec.host {
                                Some(h) if !h.trim().is_empty() => {
                                    format!("'{}'@'{}'", target.0, h.trim())
                                }
                                _ => format!("'{}'", target.0),
                            };
                            match sess.driver.create_user(&spec).await {
                                Ok(()) => {
                                    match load_users_and_permissions(
                                        &*sess.driver,
                                        Some(target),
                                        Some(format!("Created user {}", label)),
                                    )
                                    .await
                                    {
                                        Ok(evt) => {
                                            let _ = evt_tx.send(evt).await;
                                        }
                                        Err(e) => {
                                            let _ = evt_tx
                                                .send(BackendEvent::Error(format!("{:#}", e)))
                                                .await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::UpdateUser(spec) => {
                        if let Some(ref sess) = session {
                            let target = (spec.new_username.trim().to_string(), spec.new_host.clone());
                            let label = match &spec.new_host {
                                Some(h) if !h.trim().is_empty() => {
                                    format!("'{}'@'{}'", target.0, h.trim())
                                }
                                _ => format!("'{}'", target.0),
                            };
                            match sess.driver.update_user(&spec).await {
                                Ok(()) => {
                                    match load_users_and_permissions(
                                        &*sess.driver,
                                        Some(target),
                                        Some(format!("Updated user {}", label)),
                                    )
                                    .await
                                    {
                                        Ok(evt) => {
                                            let _ = evt_tx.send(evt).await;
                                        }
                                        Err(e) => {
                                            let _ = evt_tx
                                                .send(BackendEvent::Error(format!("{:#}", e)))
                                                .await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::DropUser { username, host } => {
                        if let Some(ref sess) = session {
                            let label = match &host {
                                Some(h) if !h.trim().is_empty() => {
                                    format!("'{}'@'{}'", username, h.trim())
                                }
                                _ => format!("'{}'", username),
                            };
                            match sess.driver.drop_user(&username, host.as_deref()).await {
                                Ok(()) => {
                                    match load_users_and_permissions(
                                        &*sess.driver,
                                        None,
                                        Some(format!("Dropped user {}", label)),
                                    )
                                    .await
                                    {
                                        Ok(evt) => {
                                            let _ = evt_tx.send(evt).await;
                                        }
                                        Err(e) => {
                                            let _ = evt_tx
                                                .send(BackendEvent::Error(format!("{:#}", e)))
                                                .await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::SetDatabasePermissions(spec) => {
                        if let Some(ref sess) = session {
                            let username = spec.username.clone();
                            let host = spec.host.clone();
                            let db = spec.database.clone();
                            match sess.driver.set_database_permissions(&spec).await {
                                Ok(()) => {
                                    match sess
                                        .driver
                                        .get_database_permissions(&username, host.as_deref())
                                        .await
                                    {
                                        Ok(db_permissions) => {
                                            let _ = evt_tx
                                                .send(BackendEvent::UserDbPermissionsLoaded {
                                                    username,
                                                    host,
                                                    db_permissions,
                                                    toast: Some(format!(
                                                        "Updated permissions on '{}'",
                                                        db
                                                    )),
                                                })
                                                .await;
                                        }
                                        Err(e) => {
                                            let _ = evt_tx
                                                .send(BackendEvent::Error(format!("{:#}", e)))
                                                .await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::RevokeDatabasePermissions {
                        username,
                        host,
                        database,
                    } => {
                        if let Some(ref sess) = session {
                            match sess
                                .driver
                                .revoke_database_permissions(&username, host.as_deref(), &database)
                                .await
                            {
                                Ok(()) => {
                                    match sess
                                        .driver
                                        .get_database_permissions(&username, host.as_deref())
                                        .await
                                    {
                                        Ok(db_permissions) => {
                                            let _ = evt_tx
                                                .send(BackendEvent::UserDbPermissionsLoaded {
                                                    username,
                                                    host,
                                                    db_permissions,
                                                    toast: Some(format!(
                                                        "Revoked permissions on '{}'",
                                                        database
                                                    )),
                                                })
                                                .await;
                                        }
                                        Err(e) => {
                                            let _ = evt_tx
                                                .send(BackendEvent::Error(format!("{:#}", e)))
                                                .await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::FetchDatabaseTablesForTransfer { database } => {
                        if let Some(ref sess) = session {
                            match sess.driver.list_tables(&database).await {
                                Ok(tables) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::TransferDatabaseTablesLoaded {
                                            database,
                                            tables,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::Error(format!("{:#}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    BackendCommand::RunExport(spec) => {
                        if let Some(ref sess) = session {
                            let res = crate::db::transfer::run_export(&*sess.driver, &spec)
                                .await
                                .map_err(|e| format!("{:#}", e));
                            let _ = evt_tx.send(BackendEvent::ExportFinished(res)).await;
                        } else {
                            let _ = evt_tx
                                .send(BackendEvent::ExportFinished(Err(
                                    "Not connected to any database".to_string(),
                                )))
                                .await;
                        }
                    }
                    BackendCommand::RunImport(spec) => {
                        if let Some(ref mut sess) = session {
                            let current_ui_db = spec.current_ui_database.clone();
                            let preferred_db = spec
                                .target_database
                                .clone()
                                .filter(|s| !s.trim().is_empty())
                                .or(current_ui_db);
                            match crate::db::transfer::run_import(&mut *sess.driver, &spec).await {
                                Ok(summary) => {
                                    let databases =
                                        sess.driver.list_databases().await.unwrap_or_default();
                                    let selected_database = match preferred_db.as_deref() {
                                        Some(cur)
                                            if !cur.is_empty()
                                                && databases.iter().any(|d| d == cur) =>
                                        {
                                            Some(cur.to_string())
                                        }
                                        _ => databases.first().cloned(),
                                    };
                                    let mut tables = Vec::new();
                                    if let Some(ref db) = selected_database {
                                        let _ = sess.driver.use_database(db).await;
                                        if let Ok(t) = sess.driver.list_tables(db).await {
                                            sess.cached_tables =
                                                t.iter().map(|ti| ti.name.clone()).collect();
                                            tables = t;
                                        }
                                    } else {
                                        sess.cached_tables.clear();
                                    }
                                    let _ = evt_tx
                                        .send(BackendEvent::ImportFinished(Ok(ImportOutcome {
                                            summary,
                                            databases,
                                            selected_database,
                                            tables,
                                        })))
                                        .await;
                                }
                                Err(e) => {
                                    let _ = evt_tx
                                        .send(BackendEvent::ImportFinished(Err(format!(
                                            "{:#}",
                                            e
                                        ))))
                                        .await;
                                }
                            }
                        } else {
                            let _ = evt_tx
                                .send(BackendEvent::ImportFinished(Err(
                                    "Not connected to any database".to_string(),
                                )))
                                .await;
                        }
                    }
                }
            }
        });
    });

    (cmd_tx, evt_rx)
}

async fn load_users_and_permissions(
    driver: &dyn DatabaseDriver,
    preferred_user: Option<(String, Option<String>)>,
    toast: Option<String>,
) -> Result<BackendEvent> {
    let users = driver.list_users().await?;
    let selected_user = preferred_user
        .filter(|(u, h)| users.iter().any(|du| &du.username == u && &du.host == h))
        .or_else(|| {
            users
                .first()
                .map(|du| (du.username.clone(), du.host.clone()))
        });

    let db_permissions = if let Some((ref u, ref h)) = selected_user {
        driver
            .get_database_permissions(u, h.as_deref())
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(BackendEvent::UsersLoaded {
        users,
        selected_user,
        db_permissions,
        toast,
    })
}

async fn commit_content_changes_with_driver(
    driver: &dyn DatabaseDriver,
    database: &str,
    table: &str,
    updates: &[RowUpdate],
    inserts: &[Vec<(String, DbValue)>],
    query_for_refresh: &ContentQuery,
) -> Vec<BackendEvent> {
    let mut err_msg = None;
    let mut updated_count = 0u64;
    let mut inserted_count = 0u64;

    if !updates.is_empty() {
        match driver.commit_cell_updates(database, table, updates).await {
            Ok(cnt) => updated_count += cnt,
            Err(e) => err_msg = Some(format!("{:#}", e)),
        }
    }

    if err_msg.is_none() {
        for ins in inserts {
            match driver.insert_row(database, table, ins).await {
                Ok(()) => inserted_count += 1,
                Err(e) => {
                    err_msg = Some(format!("{:#}", e));
                    break;
                }
            }
        }
    }

    let mut events = Vec::new();
    if let Some(err) = err_msg {
        if updated_count > 0 || inserted_count > 0 {
            if let Ok(page) = driver
                .get_table_content(database, table, query_for_refresh)
                .await
            {
                events.push(BackendEvent::ContentLoaded { page, toast: None });
            }
        }
        events.push(BackendEvent::Error(err));
    } else {
        let toast = match (updated_count > 0, inserted_count > 0) {
            (true, true) => Some(format!(
                "Saved changes ({} updated, {} inserted)",
                updated_count, inserted_count
            )),
            (true, false) => Some(format!("Committed {} row update(s)", updated_count)),
            (false, true) => Some(format!("Inserted {} row(s)", inserted_count)),
            (false, false) => Some("Changes committed".to_string()),
        };
        match driver
            .get_table_content(database, table, query_for_refresh)
            .await
        {
            Ok(page) => {
                events.push(BackendEvent::ContentLoaded { page, toast });
            }
            Err(e) => {
                events.push(BackendEvent::Error(format!("{:#}", e)));
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct WorkerMockDriver {
        users: Vec<DbUser>,
        fail_on_insert_idx: Option<usize>,
        inserted_count: Mutex<usize>,
        updated_batches: Mutex<usize>,
    }

    impl WorkerMockDriver {
        fn new() -> Self {
            Self {
                users: vec![
                    DbUser {
                        username: "alice".to_string(),
                        host: Some("%".to_string()),
                        global_privileges: vec!["SELECT".to_string()],
                        has_grant_option: false,
                    },
                    DbUser {
                        username: "bob".to_string(),
                        host: Some("localhost".to_string()),
                        global_privileges: vec![],
                        has_grant_option: false,
                    },
                ],
                fail_on_insert_idx: None,
                inserted_count: Mutex::new(0),
                updated_batches: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl DatabaseDriver for WorkerMockDriver {
        fn engine(&self) -> DbEngine {
            DbEngine::MySql
        }
        async fn list_databases(&self) -> Result<Vec<String>> {
            Ok(vec!["shop".to_string()])
        }
        async fn create_database(&self, _: &CreateDatabaseSpec) -> Result<()> {
            Ok(())
        }
        async fn use_database(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_tables(&self, _: &str) -> Result<Vec<TableInfo>> {
            Ok(vec![])
        }
        async fn get_table_structure(&self, _: &str, _: &str) -> Result<Vec<ColumnInfo>> {
            Ok(vec![])
        }
        async fn alter_column(&self, _: &str, _: &str, _: ColumnAlterOp) -> Result<()> {
            Ok(())
        }
        async fn get_table_indexes(&self, _: &str, _: &str) -> Result<Vec<IndexInfo>> {
            Ok(vec![])
        }
        async fn alter_index(&self, _: &str, _: &str, _: IndexAlterOp) -> Result<()> {
            Ok(())
        }
        async fn get_table_content(
            &self,
            db_name: &str,
            table: &str,
            query: &ContentQuery,
        ) -> Result<TablePage> {
            Ok(TablePage {
                database: db_name.to_string(),
                table: table.to_string(),
                columns: vec![],
                rows: vec![],
                total_rows: (*self.inserted_count.lock().unwrap()) as u64,
                page_limit: query.limit,
                page_offset: query.offset,
            })
        }
        async fn commit_cell_updates(&self, _: &str, _: &str, updates: &[RowUpdate]) -> Result<u64> {
            *self.updated_batches.lock().unwrap() += 1;
            Ok(updates.len() as u64)
        }
        async fn insert_row(&self, _: &str, _: &str, _: &[(String, DbValue)]) -> Result<()> {
            let mut cnt = self.inserted_count.lock().unwrap();
            if self.fail_on_insert_idx == Some(*cnt) {
                return Err(anyhow!("Duplicate key on insert #{}", *cnt));
            }
            *cnt += 1;
            Ok(())
        }
        async fn delete_rows(&self, _: &str, _: &str, _: &[PrimaryKeyValue]) -> Result<u64> {
            Ok(0)
        }
        async fn get_relations(&self, _: &str, _: &str) -> Result<Vec<ForeignKeyInfo>> {
            Ok(vec![])
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
            Ok(self.users.clone())
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
            username: &str,
            host: Option<&str>,
        ) -> Result<Vec<DatabasePermission>> {
            Ok(vec![DatabasePermission {
                username: username.to_string(),
                host: host.map(|s| s.to_string()),
                database: format!("db_for_{}_{}", username, host.unwrap_or("")),
                privileges: vec!["SELECT".to_string()],
                has_grant_option: false,
            }])
        }
        async fn set_database_permissions(&self, _: &DbPermissionSpec) -> Result<()> {
            Ok(())
        }
        async fn revoke_database_permissions(&self, _: &str, _: Option<&str>, _: &str) -> Result<()> {
            Ok(())
        }
        async fn get_create_database_ddl(&self, _: &str) -> Result<String> {
            Ok(String::new())
        }
        async fn get_table_ddl(&self, _: &str, _: &str, _: bool) -> Result<TableDdlParts> {
            Ok(TableDdlParts::default())
        }
        async fn fetch_table_rows_batch(
            &self,
            _: &str,
            _: &str,
            _: usize,
            _: u64,
            _: u32,
        ) -> Result<Vec<Vec<DbValue>>> {
            Ok(vec![])
        }
        async fn truncate_table(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn insert_rows_batch(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: &[Vec<DbValue>],
        ) -> Result<u64> {
            Ok(0)
        }
        async fn execute_sql_statement(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_load_users_and_permissions_preferred_vs_fallback() {
        let driver = WorkerMockDriver::new();

        // 1. Preferred user exists -> selects bob@localhost
        let evt = load_users_and_permissions(
            &driver,
            Some(("bob".to_string(), Some("localhost".to_string()))),
            Some("Updated user".to_string()),
        )
        .await
        .unwrap();
        match evt {
            BackendEvent::UsersLoaded {
                selected_user,
                db_permissions,
                toast,
                ..
            } => {
                assert_eq!(
                    selected_user,
                    Some(("bob".to_string(), Some("localhost".to_string())))
                );
                assert_eq!(db_permissions[0].database, "db_for_bob_localhost");
                assert_eq!(toast.as_deref(), Some("Updated user"));
            }
            _ => panic!("Expected UsersLoaded"),
        }

        // 2. Preferred user was dropped -> falls back to first user (alice@%)
        let evt_fallback = load_users_and_permissions(
            &driver,
            Some(("deleted_user".to_string(), Some("%".to_string()))),
            None,
        )
        .await
        .unwrap();
        match evt_fallback {
            BackendEvent::UsersLoaded {
                selected_user,
                db_permissions,
                ..
            } => {
                assert_eq!(
                    selected_user,
                    Some(("alice".to_string(), Some("%".to_string())))
                );
                assert_eq!(db_permissions[0].database, "db_for_alice_%");
            }
            _ => panic!("Expected UsersLoaded"),
        }
    }

    #[tokio::test]
    async fn test_commit_content_changes_reloads_page_on_partial_failure() {
        let mut driver = WorkerMockDriver::new();
        // Fail on the second insert (index 1) after 1 update and 1 insert have already succeeded
        driver.fail_on_insert_idx = Some(1);

        let updates = vec![RowUpdate {
            pk: PrimaryKeyValue {
                columns: vec![("id".to_string(), "1".to_string())],
            },
            changes: vec![("name".to_string(), DbValue::Text("Updated".to_string()))],
        }];
        let inserts = vec![
            vec![("name".to_string(), DbValue::Text("Row A".to_string()))],
            vec![("name".to_string(), DbValue::Text("Row B".to_string()))],
        ];
        let events = commit_content_changes_with_driver(
            &driver,
            "shop",
            "users",
            &updates,
            &inserts,
            &ContentQuery::default(),
        )
        .await;

        // Because 1 update and 1 insert were already committed before insert #2 failed,
        // it must emit ContentLoaded (to clear stale dirty state) followed by Error!
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            BackendEvent::ContentLoaded { page, toast: None } if page.total_rows == 1
        ));
        assert!(matches!(
            &events[1],
            BackendEvent::Error(msg) if msg.contains("Duplicate key on insert #1")
        ));
    }
}

