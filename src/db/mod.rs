pub mod driver;
pub mod models;
pub mod mysql;
pub mod postgres;
pub mod ssh;

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
                            let mut err_msg = None;
                            let mut updated_count = 0u64;
                            let mut inserted_count = 0u64;

                            if !updates.is_empty() {
                                match sess
                                    .driver
                                    .commit_cell_updates(&database, &table, &updates)
                                    .await
                                {
                                    Ok(cnt) => updated_count += cnt,
                                    Err(e) => err_msg = Some(format!("{:#}", e)),
                                }
                            }

                            if err_msg.is_none() {
                                for ins in &inserts {
                                    match sess.driver.insert_row(&database, &table, ins).await {
                                        Ok(()) => inserted_count += 1,
                                        Err(e) => {
                                            err_msg = Some(format!("{:#}", e));
                                            break;
                                        }
                                    }
                                }
                            }

                            if let Some(err) = err_msg {
                                let _ = evt_tx.send(BackendEvent::Error(err)).await;
                            } else {
                                let toast = match (updated_count > 0, inserted_count > 0) {
                                    (true, true) => Some(format!(
                                        "Saved changes ({} updated, {} inserted)",
                                        updated_count, inserted_count
                                    )),
                                    (true, false) => {
                                        Some(format!("Committed {} row update(s)", updated_count))
                                    }
                                    (false, true) => {
                                        Some(format!("Inserted {} row(s)", inserted_count))
                                    }
                                    (false, false) => Some("Changes committed".to_string()),
                                };
                                match sess
                                    .driver
                                    .get_table_content(&database, &table, &query_for_refresh)
                                    .await
                                {
                                    Ok(page) => {
                                        let _ = evt_tx
                                            .send(BackendEvent::ContentLoaded { page, toast })
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
                }
            }
        });
    });

    (cmd_tx, evt_rx)
}
