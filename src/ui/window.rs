use crate::db::models::BackendEvent;
use crate::db::spawn_db_worker;
use crate::ui::connection_page::ConnectionPage;
use crate::ui::sidebar::WorkspaceSidebar;
use crate::ui::workspace::WorkspaceContent;
use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

pub fn build_main_window(app: &adw::Application) -> adw::ApplicationWindow {
    let (cmd_tx, evt_rx) = spawn_db_worker();

    let toast_overlay = adw::ToastOverlay::new();
    let root_stack = gtk::Stack::new();
    root_stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    let toast_overlay_for_cb = toast_overlay.clone();
    let show_toast = move |msg: &str| {
        let toast = adw::Toast::new(msg);
        toast.set_timeout(3);
        toast_overlay_for_cb.add_toast(toast);
    };

    let conn_page = ConnectionPage::new(cmd_tx.clone(), show_toast.clone());
    let sidebar = WorkspaceSidebar::new(cmd_tx.clone());
    let workspace_content = WorkspaceContent::new(cmd_tx, show_toast.clone());

    let workspace_split = adw::NavigationSplitView::builder()
        .sidebar(&sidebar.page)
        .content(&workspace_content.page)
        .min_sidebar_width(240.0)
        .max_sidebar_width(320.0)
        .build();

    root_stack.add_named(&conn_page.root, Some("connection"));
    root_stack.add_named(&workspace_split, Some("workspace"));
    root_stack.set_visible_child_name("connection");

    toast_overlay.set_child(Some(&root_stack));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("DBased")
        .icon_name("dev.tsndr.dbased")
        .default_width(1240)
        .default_height(800)
        .content(&toast_overlay)
        .build();

    // Listen for BackendEvent messages on the GTK main loop
    let window_weak = window.downgrade();
    gtk::glib::spawn_future_local(async move {
        while let Ok(evt) = evt_rx.recv().await {
            let win_opt = window_weak.upgrade();
            handle_backend_event(
                evt,
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                win_opt.as_ref().map(|w| w.upcast_ref::<gtk::Widget>()),
                &show_toast,
            );
        }
    });

    window
}

pub(crate) fn handle_backend_event<F>(
    evt: BackendEvent,
    conn_page: &ConnectionPage,
    sidebar: &WorkspaceSidebar,
    workspace_content: &WorkspaceContent,
    root_stack: &gtk::Stack,
    window: Option<&gtk::Widget>,
    show_toast: &F,
) where
    F: Fn(&str),
{
    match evt {
        BackendEvent::ConnectionTestResult(res) => match res {
            Ok(msg) => {
                conn_page.set_busy(false, "Connection OK");
                show_toast(&msg);
            }
            Err(err) => {
                conn_page.set_busy(false, "Connection failed");
                if let Some(win) = window {
                    show_error_dialog(win, "Connection Test Failed", &err);
                }
            }
        },
        BackendEvent::Connected {
            connection_name,
            engine,
            engine_label,
            databases,
            selected_database,
            tables,
        } => {
            conn_page.set_busy(false, "");
            workspace_content.clear_table_tabs();
            workspace_content.query_tab.clear();
            workspace_content.structure_tab.set_engine(engine);
            workspace_content
                .users_tab
                .set_connection_context(engine, databases.clone());
            workspace_content.users_tab.request_refresh();
            let init_db = selected_database.as_deref();
            if let Some(db) = init_db {
                workspace_content.query_tab.set_database(db);
            }
            let table_names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
            workspace_content.relations_tab.set_all_tables(table_names);
            sidebar.set_connection_state(
                &connection_name,
                engine,
                &engine_label,
                &databases,
                init_db,
                tables,
            );
            root_stack.set_visible_child_name("workspace");
        }
        BackendEvent::Disconnected => {
            sidebar.clear();
            workspace_content.clear_all();
            root_stack.set_visible_child_name("connection");
        }
        BackendEvent::Refreshed {
            databases,
            selected_database,
            tables,
        } => {
            let prev_db = sidebar.current_database();
            let new_db = selected_database.as_deref().unwrap_or("");
            workspace_content.query_tab.set_database(new_db);
            workspace_content.users_tab.set_databases(databases.clone());
            workspace_content.users_tab.request_refresh();
            let table_names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
            workspace_content.relations_tab.set_all_tables(table_names);
            sidebar.set_databases_and_tables(&databases, selected_database.as_deref(), tables);
            if prev_db != new_db || sidebar.current_table().is_none() {
                workspace_content.clear_table_tabs();
            }
        }
        BackendEvent::DatabaseCreated {
            databases,
            selected_database,
            tables,
            toast,
        } => {
            workspace_content.clear_table_tabs();
            workspace_content.query_tab.set_database(&selected_database);
            workspace_content.users_tab.set_databases(databases.clone());
            let table_names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
            workspace_content.relations_tab.set_all_tables(table_names);
            sidebar.set_databases_and_tables(&databases, Some(&selected_database), tables);
            show_toast(&toast);
        }
        BackendEvent::DatabaseSelected { database, tables } => {
            workspace_content.clear_table_tabs();
            workspace_content.query_tab.set_database(&database);
            let table_names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
            workspace_content.relations_tab.set_all_tables(table_names);
            sidebar.set_tables(tables);
        }
        BackendEvent::TableLoaded {
            database,
            table,
            all_tables,
            structure,
            indexes,
            content,
            relations,
            content_query,
        } => {
            if database != sidebar.current_database() {
                return;
            }
            if !workspace_content.content_tab.is_navigation_in_progress()
                && sidebar.current_table().as_deref() != Some(table.as_str())
            {
                return;
            }
            sidebar.set_selected_table(&table);
            let local_cols: Vec<String> = structure.iter().map(|c| c.name.clone()).collect();
            workspace_content
                .structure_tab
                .set_structure(&database, &table, structure, indexes);
            workspace_content
                .content_tab
                .set_query_for_table(&database, content_query);
            workspace_content
                .content_tab
                .set_relations(relations.clone());
            workspace_content.content_tab.set_page(content);
            workspace_content
                .relations_tab
                .set_context(&database, &table, local_cols, all_tables);
            workspace_content
                .relations_tab
                .set_relations(&database, &table, relations);
            workspace_content.query_tab.set_database(&database);
        }
        BackendEvent::StructureUpdated {
            database,
            table,
            structure,
            indexes,
            toast,
        } => {
            if database != sidebar.current_database()
                || sidebar.current_table().as_deref() != Some(table.as_str())
            {
                return;
            }
            workspace_content
                .structure_tab
                .set_structure(&database, &table, structure, indexes);
            workspace_content.content_tab.request_fetch();
            if let Some(msg) = toast {
                show_toast(&msg);
            }
        }
        BackendEvent::ContentLoaded { page, toast } => {
            if page.database != sidebar.current_database()
                || sidebar.current_table().as_deref() != Some(page.table.as_str())
            {
                return;
            }
            workspace_content.content_tab.set_page(page);
            if let Some(msg) = toast {
                show_toast(&msg);
            }
        }
        BackendEvent::RelationsUpdated {
            database,
            table,
            relations,
            toast,
        } => {
            if database != sidebar.current_database()
                || sidebar.current_table().as_deref() != Some(table.as_str())
            {
                return;
            }
            workspace_content
                .content_tab
                .set_relations(relations.clone());
            workspace_content.content_tab.refresh_grid();
            workspace_content
                .relations_tab
                .set_relations(&database, &table, relations);
            if let Some(msg) = toast {
                show_toast(&msg);
            }
        }
        BackendEvent::TargetTableColumnsLoaded { .. } => {}
        BackendEvent::RawQueryFinished(outcome) => {
            workspace_content.query_tab.show_outcome(outcome);
        }
        BackendEvent::UsersLoaded {
            users,
            selected_user,
            db_permissions,
            toast,
        } => {
            workspace_content.users_tab.set_users_and_permissions(
                users,
                selected_user,
                db_permissions,
            );
            if let Some(msg) = toast {
                show_toast(&msg);
            }
        }
        BackendEvent::UserDbPermissionsLoaded {
            username,
            host,
            db_permissions,
            toast,
        } => {
            workspace_content
                .users_tab
                .set_user_db_permissions(&username, host, db_permissions);
            if let Some(msg) = toast {
                show_toast(&msg);
            }
        }
        BackendEvent::TransferDatabaseTablesLoaded { database, tables } => {
            sidebar
                .transfer_controller
                .cache_database_tables(&database, &tables);
        }
        BackendEvent::ExportFinished(res) => match res {
            Ok(summary) => {
                show_toast(&summary);
            }
            Err(err) => {
                if let Some(win) = window {
                    show_error_dialog(win, "Export Failed", &err);
                }
            }
        },
        BackendEvent::ImportFinished(res) => match res {
            Ok(outcome) => {
                let prev_db = sidebar.current_database();
                let new_db = outcome.selected_database.as_deref().unwrap_or("");
                workspace_content.query_tab.set_database(new_db);
                workspace_content
                    .users_tab
                    .set_databases(outcome.databases.clone());
                let table_names: Vec<String> =
                    outcome.tables.iter().map(|t| t.name.clone()).collect();
                workspace_content.relations_tab.set_all_tables(table_names);
                sidebar.set_databases_and_tables(
                    &outcome.databases,
                    outcome.selected_database.as_deref(),
                    outcome.tables,
                );
                if prev_db != new_db || sidebar.current_table().is_none() {
                    workspace_content.clear_table_tabs();
                } else {
                    workspace_content.content_tab.request_fetch();
                }
                show_toast(&outcome.summary);
            }
            Err(err) => {
                if let Some(win) = window {
                    show_error_dialog(win, "Import Failed", &err);
                }
            }
        },
        BackendEvent::Error(err) => {
            conn_page.set_busy(false, "");
            if let Some(win) = window {
                show_error_dialog(win, "Database Error", &err);
            }
        }
        BackendEvent::InfoToast(msg) => {
            show_toast(&msg);
        }
    }
}

fn show_error_dialog(parent: &gtk::Widget, title: &str, details: &str) {
    let dialog = adw::AlertDialog::builder()
        .heading(title)
        .body(details)
        .build();
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::bookmarks::DbEngine;
    use crate::db::models::{ColumnInfo, ContentQuery, ForeignKeyInfo, TableInfo, TablePage};

    fn sample_col(name: &str) -> ColumnInfo {
        ColumnInfo {
            ordinal: 1,
            name: name.to_string(),
            data_type: "int".to_string(),
            is_nullable: false,
            is_primary_key: true,
            default_value: None,
            extra: String::new(),
        }
    }

    fn sample_page(db: &str, table: &str, col_name: &str) -> TablePage {
        TablePage {
            database: db.to_string(),
            table: table.to_string(),
            columns: vec![sample_col(col_name)],
            rows: Vec::new(),
            total_rows: 0,
            page_limit: 100,
            page_offset: 0,
        }
    }

    #[test]
    fn test_window_ignores_stale_table_events_after_switching_table_or_database() {
        crate::ui::test_util::run_gtk_test(|| {
            let (cmd_tx, _cmd_rx) = async_channel::unbounded();
            let root_stack = gtk::Stack::new();
            let conn_page = ConnectionPage::new(cmd_tx.clone(), |_| {});
            let sidebar = WorkspaceSidebar::new(cmd_tx.clone());
            let workspace_content = WorkspaceContent::new(cmd_tx, |_| {});
            root_stack.add_named(&conn_page.root, Some("connection"));
            root_stack.add_named(&workspace_content.page, Some("workspace"));

            let tables = vec![
                TableInfo {
                    name: "users".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "orders".to_string(),
                    is_view: false,
                },
            ];

            handle_backend_event(
                BackendEvent::Connected {
                    connection_name: "Test".to_string(),
                    engine: DbEngine::Postgres,
                    engine_label: "PostgreSQL".to_string(),
                    databases: vec!["db1".to_string(), "db2".to_string()],
                    selected_database: Some("db1".to_string()),
                    tables: tables.clone(),
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );

            // Select "orders" in sidebar and load it
            sidebar.set_selected_table("orders");
            handle_backend_event(
                BackendEvent::TableLoaded {
                    database: "db1".to_string(),
                    table: "orders".to_string(),
                    all_tables: vec!["users".to_string(), "orders".to_string()],
                    structure: vec![sample_col("order_id")],
                    indexes: Vec::new(),
                    content: sample_page("db1", "orders", "order_id"),
                    relations: Vec::new(),
                    content_query: ContentQuery::default(),
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );

            assert_eq!(
                workspace_content.content_tab.current_table_context(),
                Some(("db1".to_string(), "orders".to_string()))
            );

            // Now a late/stale ContentLoaded, StructureUpdated, and RelationsUpdated for "users" arrive
            handle_backend_event(
                BackendEvent::ContentLoaded {
                    page: sample_page("db1", "users", "user_id"),
                    toast: None,
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );
            handle_backend_event(
                BackendEvent::StructureUpdated {
                    database: "db1".to_string(),
                    table: "users".to_string(),
                    structure: vec![sample_col("user_id")],
                    indexes: Vec::new(),
                    toast: None,
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );
            handle_backend_event(
                BackendEvent::RelationsUpdated {
                    database: "db1".to_string(),
                    table: "users".to_string(),
                    relations: vec![ForeignKeyInfo {
                        constraint_name: "fk_stale".to_string(),
                        column_name: "user_id".to_string(),
                        referenced_table: "accounts".to_string(),
                        referenced_column: "id".to_string(),
                        update_rule: "NO ACTION".to_string(),
                        delete_rule: "NO ACTION".to_string(),
                    }],
                    toast: None,
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );

            assert_eq!(
                workspace_content.content_tab.current_table_context(),
                Some(("db1".to_string(), "orders".to_string())),
                "Stale ContentLoaded for 'users' must not overwrite active 'orders' page"
            );
            assert_eq!(
                workspace_content.structure_tab.current_table_context(),
                ("db1".to_string(), "orders".to_string()),
                "Stale StructureUpdated for 'users' must not overwrite active 'orders' structure"
            );
            assert_eq!(
                workspace_content.relations_tab.current_table_context(),
                ("db1".to_string(), "orders".to_string()),
                "Stale RelationsUpdated for 'users' must not overwrite active 'orders' relations"
            );
        });
    }

    #[test]
    fn test_window_clears_table_tabs_on_database_switch_and_disconnect() {
        crate::ui::test_util::run_gtk_test(|| {
            let (cmd_tx, _cmd_rx) = async_channel::unbounded();
            let root_stack = gtk::Stack::new();
            let conn_page = ConnectionPage::new(cmd_tx.clone(), |_| {});
            let sidebar = WorkspaceSidebar::new(cmd_tx.clone());
            let workspace_content = WorkspaceContent::new(cmd_tx, |_| {});
            root_stack.add_named(&conn_page.root, Some("connection"));
            root_stack.add_named(&workspace_content.page, Some("workspace"));

            handle_backend_event(
                BackendEvent::Connected {
                    connection_name: "Test".to_string(),
                    engine: DbEngine::Postgres,
                    engine_label: "PostgreSQL".to_string(),
                    databases: vec!["db1".to_string(), "db2".to_string()],
                    selected_database: Some("db1".to_string()),
                    tables: vec![TableInfo {
                        name: "users".to_string(),
                        is_view: false,
                    }],
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );

            sidebar.set_selected_table("users");
            handle_backend_event(
                BackendEvent::TableLoaded {
                    database: "db1".to_string(),
                    table: "users".to_string(),
                    all_tables: vec!["users".to_string()],
                    structure: vec![sample_col("id")],
                    indexes: Vec::new(),
                    content: sample_page("db1", "users", "id"),
                    relations: Vec::new(),
                    content_query: ContentQuery::default(),
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );

            // Switch to db2: structure_tab and relations_tab must not retain db1.users!
            handle_backend_event(
                BackendEvent::DatabaseSelected {
                    database: "db2".to_string(),
                    tables: Vec::new(),
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );

            assert_eq!(
                workspace_content.structure_tab.current_table_context(),
                (String::new(), String::new()),
                "StructureTab should be cleared when switching databases"
            );
            assert_eq!(
                workspace_content.relations_tab.current_table_context(),
                (String::new(), String::new()),
                "RelationsTab should be cleared when switching databases"
            );

            // Disconnect: sidebar current_database and current_table must be cleared
            handle_backend_event(
                BackendEvent::Disconnected,
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );
            assert!(
                sidebar.current_database().is_empty(),
                "Sidebar current_database should be empty after Disconnected"
            );
            assert!(
                sidebar.current_table().is_none(),
                "Sidebar current_table should be None after Disconnected"
            );
        });
    }
}

