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

    fn pump_events(ms: u64) {
        let ctx = gtk::glib::MainContext::default();
        let steps = (ms / 10).max(1);
        for _ in 0..steps {
            while ctx.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        while ctx.iteration(false) {}
    }

    fn save_window_screenshot(win: &adw::Window, path: &str) {
        gtk::prelude::RootExt::set_focus(win, None::<&gtk::Widget>);
        pump_events(250);
        let paintable = gtk::WidgetPaintable::new(Some(win));
        let width = win.width().max(1440);
        let height = win.height().max(920);
        let snapshot = gtk::Snapshot::new();
        gtk::prelude::PaintableExt::snapshot(&paintable, &snapshot, width as f64, height as f64);
        if let Some(node) = snapshot.to_node()
            && let Some(renderer) = win.renderer()
        {
            let rect = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
            let texture = renderer.render_texture(&node, Some(&rect));
            texture
                .save_to_png(path)
                .expect("Failed to save PNG screenshot");
        } else {
            panic!("Failed to render window snapshot for {path}");
        }
    }

    #[test]
    #[ignore]
    fn generate_all_screenshots() {
        use crate::config::bookmarks::{
            BookmarkStore, ConnectionBookmark, SshAuthMethod, SshConfig,
        };
        use crate::db::models::{
            DatabasePermission, DbUser, DbValue, IndexInfo, PrimaryKeyValue, RawQueryOutcome,
            RawQueryResultSet, SortDirection, TableRowData,
        };

        crate::ui::test_util::run_gtk_test(|| {
            let style_manager = adw::StyleManager::default();
            style_manager.set_color_scheme(adw::ColorScheme::ForceDark);

            if let Some(settings) = gtk::Settings::default() {
                settings.set_gtk_icon_theme_name(Some("Adwaita"));
            }

            let temp_cfg =
                std::env::temp_dir().join(format!("dbased-screenshots-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&temp_cfg);
            std::fs::create_dir_all(&temp_cfg).expect("Failed to create temp config dir");
            // SAFETY: single-threaded GTK test runner
            unsafe {
                std::env::set_var("XDG_CONFIG_HOME", &temp_cfg);
            }

            let store = BookmarkStore::new();
            let bookmarks = vec![
                ConnectionBookmark {
                    id: "bm-1".to_string(),
                    name: "Production Replica".to_string(),
                    engine: DbEngine::Postgres,
                    host: "db-ro.internal.acme.io".to_string(),
                    port: 5432,
                    username: "readonly_analyst".to_string(),
                    default_database: "ecommerce_prod".to_string(),
                    ssh: SshConfig {
                        enabled: true,
                        host: "bastion.eu-central-1.acme.io".to_string(),
                        port: 22,
                        username: "deploy".to_string(),
                        auth_method: SshAuthMethod::PrivateKey {
                            key_path: "~/.ssh/id_ed25519_acme".to_string(),
                        },
                    },
                },
                ConnectionBookmark {
                    id: "bm-2".to_string(),
                    name: "Staging Billing DB".to_string(),
                    engine: DbEngine::MySql,
                    host: "staging-mysql.acme.internal".to_string(),
                    port: 3306,
                    username: "billing_svc".to_string(),
                    default_database: "billing_stage".to_string(),
                    ssh: SshConfig::default(),
                },
                ConnectionBookmark {
                    id: "bm-3".to_string(),
                    name: "Analytics Warehouse".to_string(),
                    engine: DbEngine::Postgres,
                    host: "10.24.8.110".to_string(),
                    port: 5432,
                    username: "bi_reader".to_string(),
                    default_database: "warehouse_events".to_string(),
                    ssh: SshConfig {
                        enabled: true,
                        host: "jump.corp.acme.io".to_string(),
                        port: 2222,
                        username: "ops".to_string(),
                        auth_method: SshAuthMethod::PrivateKey {
                            key_path: "~/.ssh/id_ed25519".to_string(),
                        },
                    },
                },
                ConnectionBookmark {
                    id: "bm-4".to_string(),
                    name: "Local Dev Cluster".to_string(),
                    engine: DbEngine::MySql,
                    host: "127.0.0.1".to_string(),
                    port: 3306,
                    username: "root".to_string(),
                    default_database: "acme_dev".to_string(),
                    ssh: SshConfig::default(),
                },
            ];
            let _ = store.save_all(&bookmarks);
            let creds_dir = temp_cfg.join("dbased");
            let _ = std::fs::create_dir_all(&creds_dir);
            let _ = std::fs::write(
                creds_dir.join(".credentials.toml"),
                "[entries.bm-1]\ndb_password = \"prod-ro-secret-2026\"\nssh_secret = \"ed25519-key-passphrase\"\n",
            );

            // Compact preferences page CSS so all Connection & SSH fields fit cleanly in 920px height
            let conn_css = gtk::CssProvider::new();
            conn_css.load_from_string(
                "preferencespage scrolledwindow viewport clamp > box,
                 preferencespage clamp > box {
                     margin-top: 6px;
                     margin-bottom: 6px;
                     border-spacing: 10px;
                 }
                 preferencesgroup > box > box.labels {
                     margin-bottom: 4px;
                 }
                 preferencespage row,
                 preferencespage row box.header {
                     min-height: 38px;
                     padding-top: 0px;
                     padding-bottom: 0px;
                 }
                 preferencespage row box.editable-area {
                     margin-top: 0px;
                     margin-bottom: 0px;
                 }
                 preferencespage row box.editable-area text {
                     margin-top: 12px;
                     margin-bottom: 0px;
                     min-height: 18px;
                 }
                 preferencespage row box.prefixes,
                 preferencespage row box.suffixes {
                     margin-top: 0px;
                     margin-bottom: 0px;
                 }
                 preferencespage row list.nested > row,
                 preferencespage row list.nested > row box.header {
                     min-height: 38px;
                     padding-top: 0px;
                     padding-bottom: 0px;
                 }
                 preferencespage row box.header button {
                     min-height: 24px;
                     min-width: 24px;
                     padding: 2px;
                     margin-top: 2px;
                     margin-bottom: 2px;
                 }",
            );
            let display = gtk::gdk::Display::default().expect("Default GDK display required");
            gtk::style_context_add_provider_for_display(
                &display,
                &conn_css,
                gtk::STYLE_PROVIDER_PRIORITY_USER,
            );

            let (cmd_tx, _cmd_rx) = async_channel::unbounded();
            let toast_overlay = adw::ToastOverlay::new();
            let root_stack = gtk::Stack::new();

            let conn_page = ConnectionPage::new(cmd_tx.clone(), |_| {});
            let sidebar = WorkspaceSidebar::new(cmd_tx.clone());
            let workspace_content = WorkspaceContent::new(cmd_tx, |_| {});

            let workspace_split = adw::NavigationSplitView::builder()
                .sidebar(&sidebar.page)
                .content(&workspace_content.page)
                .min_sidebar_width(250.0)
                .max_sidebar_width(290.0)
                .build();

            root_stack.add_named(&conn_page.root, Some("connection"));
            root_stack.add_named(&workspace_split, Some("workspace"));
            root_stack.set_visible_child_name("connection");
            toast_overlay.set_child(Some(&root_stack));

            let window = adw::Window::builder()
                .title("DBased")
                .default_width(1440)
                .default_height(920)
                .content(&toast_overlay)
                .build();
            window.set_size_request(1440, 920);
            window.present();
            pump_events(400);

            // 1. Save assets/connection.png
            save_window_screenshot(&window, "assets/connection.png");

            // Remove temporary compact connection page CSS before capturing workspace screenshots
            gtk::style_context_remove_provider_for_display(&display, &conn_css);

            // Transition to workspace
            let databases = vec![
                "ecommerce_prod".to_string(),
                "analytics_warehouse".to_string(),
                "billing_service".to_string(),
                "auth_identity".to_string(),
            ];
            let tables = vec![
                TableInfo {
                    name: "customers".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "orders".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "order_items".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "products".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "invoices".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "payments".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "shipments".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "audit_events".to_string(),
                    is_view: false,
                },
                TableInfo {
                    name: "v_monthly_revenue".to_string(),
                    is_view: true,
                },
                TableInfo {
                    name: "v_active_subscriptions".to_string(),
                    is_view: true,
                },
            ];
            let all_table_names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();

            handle_backend_event(
                BackendEvent::Connected {
                    connection_name: "Production Replica".to_string(),
                    engine: DbEngine::Postgres,
                    engine_label: "PostgreSQL 16.4".to_string(),
                    databases: databases.clone(),
                    selected_database: Some("ecommerce_prod".to_string()),
                    tables: tables.clone(),
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );
            pump_events(250);

            // Populate rich table data for "orders"
            let structure = vec![
                ColumnInfo {
                    ordinal: 1,
                    name: "id".to_string(),
                    data_type: "bigint".to_string(),
                    is_nullable: false,
                    is_primary_key: true,
                    default_value: Some("nextval('orders_id_seq'::regclass)".to_string()),
                    extra: "GENERATED BY DEFAULT AS IDENTITY".to_string(),
                },
                ColumnInfo {
                    ordinal: 2,
                    name: "order_number".to_string(),
                    data_type: "varchar(32)".to_string(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 3,
                    name: "customer_id".to_string(),
                    data_type: "bigint".to_string(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 4,
                    name: "status".to_string(),
                    data_type: "enum('pending','processing','shipped','delivered','refunded','cancelled')".to_string(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: Some("'pending'".to_string()),
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 5,
                    name: "total_amount".to_string(),
                    data_type: "numeric(12,2)".to_string(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: Some("0.00".to_string()),
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 6,
                    name: "currency".to_string(),
                    data_type: "char(3)".to_string(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: Some("'EUR'".to_string()),
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 7,
                    name: "invoice_id".to_string(),
                    data_type: "bigint".to_string(),
                    is_nullable: true,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 8,
                    name: "metadata".to_string(),
                    data_type: "jsonb".to_string(),
                    is_nullable: true,
                    is_primary_key: false,
                    default_value: Some("'{}'::jsonb".to_string()),
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 9,
                    name: "created_at".to_string(),
                    data_type: "timestamptz".to_string(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: Some("now()".to_string()),
                    extra: String::new(),
                },
            ];

            let indexes = vec![
                IndexInfo {
                    name: "orders_pkey".to_string(),
                    columns: vec!["id".to_string()],
                    is_unique: true,
                    is_primary: true,
                    index_type: "PRIMARY".to_string(),
                    method: "btree".to_string(),
                },
                IndexInfo {
                    name: "idx_orders_order_number_uniq".to_string(),
                    columns: vec!["order_number".to_string()],
                    is_unique: true,
                    is_primary: false,
                    index_type: "UNIQUE".to_string(),
                    method: "btree".to_string(),
                },
                IndexInfo {
                    name: "idx_orders_customer_created".to_string(),
                    columns: vec!["customer_id".to_string(), "created_at".to_string()],
                    is_unique: false,
                    is_primary: false,
                    index_type: "INDEX".to_string(),
                    method: "btree".to_string(),
                },
                IndexInfo {
                    name: "idx_orders_status_active".to_string(),
                    columns: vec!["status".to_string()],
                    is_unique: false,
                    is_primary: false,
                    index_type: "INDEX".to_string(),
                    method: "btree".to_string(),
                },
                IndexInfo {
                    name: "idx_orders_metadata_gin".to_string(),
                    columns: vec!["metadata".to_string()],
                    is_unique: false,
                    is_primary: false,
                    index_type: "INDEX".to_string(),
                    method: "gin".to_string(),
                },
            ];

            let relations = vec![
                ForeignKeyInfo {
                    constraint_name: "fk_orders_customer_id".to_string(),
                    column_name: "customer_id".to_string(),
                    referenced_table: "customers".to_string(),
                    referenced_column: "id".to_string(),
                    update_rule: "CASCADE".to_string(),
                    delete_rule: "RESTRICT".to_string(),
                },
                ForeignKeyInfo {
                    constraint_name: "fk_orders_invoice_id".to_string(),
                    column_name: "invoice_id".to_string(),
                    referenced_table: "invoices".to_string(),
                    referenced_column: "id".to_string(),
                    update_rule: "CASCADE".to_string(),
                    delete_rule: "SET NULL".to_string(),
                },
                ForeignKeyInfo {
                    constraint_name: "fk_orders_shipping_method".to_string(),
                    column_name: "shipping_method_id".to_string(),
                    referenced_table: "shipments".to_string(),
                    referenced_column: "id".to_string(),
                    update_rule: "NO ACTION".to_string(),
                    delete_rule: "SET NULL".to_string(),
                },
            ];

            let raw_rows = vec![
                vec![
                    DbValue::Text("10482".to_string()),
                    DbValue::Text("ORD-2026-08941".to_string()),
                    DbValue::Text("418".to_string()),
                    DbValue::Text("delivered".to_string()),
                    DbValue::Text("1249.90".to_string()),
                    DbValue::Text("EUR".to_string()),
                    DbValue::Text("88210".to_string()),
                    DbValue::Text("{\n  \"channel\": \"web_checkout\",\n  \"payment_provider\": \"stripe\",\n  \"payment_intent\": \"pi_3Q9xLm2eZvKYlo2C18aB\",\n  \"shipping\": {\n    \"carrier\": \"DHL Express\",\n    \"tracking_code\": \"JJD009384712004\",\n    \"priority\": true,\n    \"warehouse\": \"FRA-02\"\n  },\n  \"discounts\": [\n    {\n      \"code\": \"SPRING26\",\n      \"amount_eur\": 150.00\n    }\n  ],\n  \"gift_wrap\": false\n}".to_string()),
                    DbValue::Text("2026-04-08 09:14:22+00".to_string()),
                ],
                vec![
                    DbValue::Text("10483".to_string()),
                    DbValue::Text("ORD-2026-08942".to_string()),
                    DbValue::Text("1092".to_string()),
                    DbValue::Text("processing".to_string()),
                    DbValue::Text("389.50".to_string()),
                    DbValue::Text("EUR".to_string()),
                    DbValue::Text("88211".to_string()),
                    DbValue::Text("{\"channel\": \"ios_app\", \"payment_provider\": \"apple_pay\", \"priority\": false}".to_string()),
                    DbValue::Text("2026-04-08 09:28:05+00".to_string()),
                ],
                vec![
                    DbValue::Text("10484".to_string()),
                    DbValue::Text("ORD-2026-08943".to_string()),
                    DbValue::Text("734".to_string()),
                    DbValue::Text("shipped".to_string()),
                    DbValue::Text("2840.00".to_string()),
                    DbValue::Text("USD".to_string()),
                    DbValue::Text("88214".to_string()),
                    DbValue::Text("{\"channel\": \"b2b_portal\", \"po_number\": \"PO-99412\", \"net_terms\": 30}".to_string()),
                    DbValue::Text("2026-04-08 10:02:49+00".to_string()),
                ],
                vec![
                    DbValue::Text("10485".to_string()),
                    DbValue::Text("ORD-2026-08944".to_string()),
                    DbValue::Text("215".to_string()),
                    DbValue::Text("pending".to_string()),
                    DbValue::Text("74.99".to_string()),
                    DbValue::Text("EUR".to_string()),
                    DbValue::Null,
                    DbValue::Text("{\"channel\": \"web_checkout\", \"guest\": true}".to_string()),
                    DbValue::Text("2026-04-08 10:19:11+00".to_string()),
                ],
                vec![
                    DbValue::Text("10486".to_string()),
                    DbValue::Text("ORD-2026-08945".to_string()),
                    DbValue::Text("418".to_string()),
                    DbValue::Text("delivered".to_string()),
                    DbValue::Text("512.20".to_string()),
                    DbValue::Text("EUR".to_string()),
                    DbValue::Text("88219".to_string()),
                    DbValue::Text("{\"channel\": \"web_checkout\", \"loyalty_tier\": \"gold\"}".to_string()),
                    DbValue::Text("2026-04-08 11:04:37+00".to_string()),
                ],
                vec![
                    DbValue::Text("10487".to_string()),
                    DbValue::Text("ORD-2026-08946".to_string()),
                    DbValue::Text("1509".to_string()),
                    DbValue::Text("refunded".to_string()),
                    DbValue::Text("199.00".to_string()),
                    DbValue::Text("GBP".to_string()),
                    DbValue::Text("88222".to_string()),
                    DbValue::Text("{\"refund_reason\": \"sizing_exchange\", \"rma_id\": \"RMA-4012\"}".to_string()),
                    DbValue::Text("2026-04-08 11:41:08+00".to_string()),
                ],
                vec![
                    DbValue::Text("10488".to_string()),
                    DbValue::Text("ORD-2026-08947".to_string()),
                    DbValue::Text("881".to_string()),
                    DbValue::Text("shipped".to_string()),
                    DbValue::Text("920.00".to_string()),
                    DbValue::Text("EUR".to_string()),
                    DbValue::Text("88225".to_string()),
                    DbValue::Text("{\"channel\": \"android_app\", \"carrier\": \"UPS\"}".to_string()),
                    DbValue::Text("2026-04-08 12:15:54+00".to_string()),
                ],
                vec![
                    DbValue::Text("10489".to_string()),
                    DbValue::Text("ORD-2026-08948".to_string()),
                    DbValue::Text("304".to_string()),
                    DbValue::Text("processing".to_string()),
                    DbValue::Text("1640.75".to_string()),
                    DbValue::Text("USD".to_string()),
                    DbValue::Text("88229".to_string()),
                    DbValue::Text("{\"channel\": \"b2b_portal\", \"account_mgr\": \"s.meyer\"}".to_string()),
                    DbValue::Text("2026-04-08 12:47:30+00".to_string()),
                ],
                vec![
                    DbValue::Text("10490".to_string()),
                    DbValue::Text("ORD-2026-08949".to_string()),
                    DbValue::Text("1290".to_string()),
                    DbValue::Text("cancelled".to_string()),
                    DbValue::Text("59.00".to_string()),
                    DbValue::Text("EUR".to_string()),
                    DbValue::Null,
                    DbValue::Null,
                    DbValue::Text("2026-04-08 13:05:19+00".to_string()),
                ],
                vec![
                    DbValue::Text("10491".to_string()),
                    DbValue::Text("ORD-2026-08950".to_string()),
                    DbValue::Text("612".to_string()),
                    DbValue::Text("delivered".to_string()),
                    DbValue::Text("445.00".to_string()),
                    DbValue::Text("EUR".to_string()),
                    DbValue::Text("88234".to_string()),
                    DbValue::Text("{\"channel\": \"web_checkout\", \"coupon\": \"WELCOME10\"}".to_string()),
                    DbValue::Text("2026-04-08 13:39:42+00".to_string()),
                ],
            ];

            let rows: Vec<TableRowData> = raw_rows
                .into_iter()
                .map(|values| {
                    let id_str = values[0].display_text().to_string();
                    TableRowData {
                        pk: Some(PrimaryKeyValue {
                            columns: vec![("id".to_string(), id_str)],
                        }),
                        values,
                    }
                })
                .collect();

            let page = TablePage {
                database: "ecommerce_prod".to_string(),
                table: "orders".to_string(),
                columns: structure.clone(),
                rows,
                total_rows: 1842,
                page_limit: 100,
                page_offset: 0,
            };

            let content_query = ContentQuery {
                search_text: String::new(),
                sort_column: Some("id".to_string()),
                sort_direction: SortDirection::Asc,
                limit: 100,
                offset: 0,
            };

            sidebar.set_selected_table("orders");
            handle_backend_event(
                BackendEvent::TableLoaded {
                    database: "ecommerce_prod".to_string(),
                    table: "orders".to_string(),
                    all_tables: all_table_names,
                    structure,
                    indexes,
                    content: page,
                    relations,
                    content_query,
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );
            pump_events(300);

            // Set compact column widths on ContentTab so columns + JSONB inspector fit cleanly
            {
                let mut widths = workspace_content.content_tab.col_widths.borrow_mut();
                widths.insert("id".to_string(), 78);
                widths.insert("order_number".to_string(), 148);
                widths.insert("customer_id".to_string(), 118);
                widths.insert("status".to_string(), 132);
                widths.insert("total_amount".to_string(), 120);
                widths.insert("currency".to_string(), 92);
                widths.insert("invoice_id".to_string(), 105);
                widths.insert("metadata".to_string(), 220);
                widths.insert("created_at".to_string(), 185);
            }
            workspace_content.content_tab.refresh_grid();
            pump_events(250);

            // Mark row 2 status as edited ("shipped") to showcase dirty cell highlighting + pending changes badge
            if let Some(status_cell_row2) = workspace_content.content_tab.grid.child_at(4, 2)
                && let Some(cell_box) = status_cell_row2.downcast_ref::<gtk::Box>()
                && let Some(first) = cell_box.first_child()
                && let Some(dd) = first.downcast_ref::<gtk::DropDown>()
            {
                dd.set_selected(2); // "shipped"
            }
            pump_events(150);

            // Open the JSONB side inspector on row 1 metadata column (column index 8, row 1)
            if let Some(meta_cell_row1) = workspace_content.content_tab.grid.child_at(8, 1)
                && let Some(cell_box) = meta_cell_row1.downcast_ref::<gtk::Box>()
            {
                let mut child_opt = cell_box.first_child();
                while let Some(ch) = child_opt {
                    if let Some(btn) = ch.downcast_ref::<gtk::Button>() {
                        btn.emit_clicked();
                        break;
                    }
                    child_opt = ch.next_sibling();
                }
            }
            pump_events(250);
            workspace_content.content_tab.content_paned.set_position(778);
            pump_events(200);
            // Place cursor away from line 0 so JSONB syntax highlighting is crisp
            let mut iter = workspace_content
                .content_tab
                .panel_buffer
                .iter_at_line_offset(12, 1)
                .unwrap_or_else(|| workspace_content.content_tab.panel_buffer.end_iter());
            workspace_content
                .content_tab
                .panel_buffer
                .place_cursor(&mut iter);
            pump_events(150);

            // 2. Save assets/content.png
            workspace_content.stack.set_visible_child_name("content");
            save_window_screenshot(&window, "assets/content.png");

            // 3. Save assets/structure.png
            workspace_content.stack.set_visible_child_name("structure");
            save_window_screenshot(&window, "assets/structure.png");

            // 4. Save assets/relations.png
            workspace_content.stack.set_visible_child_name("relations");
            save_window_screenshot(&window, "assets/relations.png");

            // 5. Populate and save assets/query.png
            workspace_content
                .query_tab
                .set_sql_text(
                    "SELECT\n    c.id AS customer_id,\n    c.full_name,\n    c.email,\n    c.country_code,\n    COUNT(o.id) AS orders_count,\n    SUM(o.total_amount) AS lifetime_value_eur,\n    MAX(o.created_at) AS last_order_at\nFROM customers c\nJOIN orders o ON o.customer_id = c.id\nWHERE o.status IN ('delivered', 'shipped')\n  AND o.created_at >= '2026-01-01'\nGROUP BY c.id, c.full_name, c.email, c.country_code\nHAVING COUNT(o.id) >= 3\nORDER BY lifetime_value_eur DESC\nLIMIT 25;",
                );
            let query_outcome = RawQueryOutcome::Rows(RawQueryResultSet {
                columns: vec![
                    "customer_id".to_string(),
                    "full_name".to_string(),
                    "email".to_string(),
                    "country_code".to_string(),
                    "orders_count".to_string(),
                    "lifetime_value_eur".to_string(),
                    "last_order_at".to_string(),
                ],
                rows: vec![
                    vec![
                        DbValue::Text("418".to_string()),
                        DbValue::Text("Clara Lindqvist".to_string()),
                        DbValue::Text("c.lindqvist@nordic-labs.se".to_string()),
                        DbValue::Text("SE".to_string()),
                        DbValue::Text("14".to_string()),
                        DbValue::Text("18420.50".to_string()),
                        DbValue::Text("2026-04-08 11:04:37+00".to_string()),
                    ],
                    vec![
                        DbValue::Text("734".to_string()),
                        DbValue::Text("Marcus Vance".to_string()),
                        DbValue::Text("mvance@aperture-systems.io".to_string()),
                        DbValue::Text("US".to_string()),
                        DbValue::Text("11".to_string()),
                        DbValue::Text("15980.00".to_string()),
                        DbValue::Text("2026-04-08 10:02:49+00".to_string()),
                    ],
                    vec![
                        DbValue::Text("304".to_string()),
                        DbValue::Text("Elena Rostova".to_string()),
                        DbValue::Text("elena@kinetix-gmbh.de".to_string()),
                        DbValue::Text("DE".to_string()),
                        DbValue::Text("9".to_string()),
                        DbValue::Text("12310.75".to_string()),
                        DbValue::Text("2026-04-07 18:22:10+00".to_string()),
                    ],
                    vec![
                        DbValue::Text("1092".to_string()),
                        DbValue::Text("Henrik Sørensen".to_string()),
                        DbValue::Text("hs@copenhagen-design.dk".to_string()),
                        DbValue::Text("DK".to_string()),
                        DbValue::Text("8".to_string()),
                        DbValue::Text("9845.20".to_string()),
                        DbValue::Text("2026-04-06 14:51:03+00".to_string()),
                    ],
                    vec![
                        DbValue::Text("881".to_string()),
                        DbValue::Text("Sophie Laurent".to_string()),
                        DbValue::Text("s.laurent@atelier-lumiere.fr".to_string()),
                        DbValue::Text("FR".to_string()),
                        DbValue::Text("7".to_string()),
                        DbValue::Text("8490.00".to_string()),
                        DbValue::Text("2026-04-08 12:15:54+00".to_string()),
                    ],
                    vec![
                        DbValue::Text("612".to_string()),
                        DbValue::Text("Tobias Weber".to_string()),
                        DbValue::Text("t.weber@alpen-cloud.ch".to_string()),
                        DbValue::Text("CH".to_string()),
                        DbValue::Text("6".to_string()),
                        DbValue::Text("6720.40".to_string()),
                        DbValue::Text("2026-04-08 13:39:42+00".to_string()),
                    ],
                    vec![
                        DbValue::Text("519".to_string()),
                        DbValue::Text("Hannah van Dijk".to_string()),
                        DbValue::Text("hannah@velox-logistics.nl".to_string()),
                        DbValue::Text("NL".to_string()),
                        DbValue::Text("5".to_string()),
                        DbValue::Text("5190.80".to_string()),
                        DbValue::Text("2026-04-05 09:12:44+00".to_string()),
                    ],
                ],
                execution_time_ms: 14,
            });
            handle_backend_event(
                BackendEvent::RawQueryFinished(Ok(query_outcome)),
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );
            workspace_content.stack.set_visible_child_name("query");
            workspace_content.query_tab.root.set_position(360);
            save_window_screenshot(&window, "assets/query.png");

            // 6. Populate and save assets/users.png
            let users = vec![
                DbUser {
                    username: "postgres".to_string(),
                    host: None,
                    global_privileges: vec![
                        "SUPERUSER".to_string(),
                        "LOGIN".to_string(),
                        "CREATEDB".to_string(),
                        "CREATEROLE".to_string(),
                        "REPLICATION".to_string(),
                        "BYPASSRLS".to_string(),
                    ],
                    has_grant_option: true,
                },
                DbUser {
                    username: "app_service".to_string(),
                    host: None,
                    global_privileges: vec!["LOGIN".to_string()],
                    has_grant_option: false,
                },
                DbUser {
                    username: "readonly_analyst".to_string(),
                    host: None,
                    global_privileges: vec!["LOGIN".to_string()],
                    has_grant_option: false,
                },
                DbUser {
                    username: "billing_worker".to_string(),
                    host: None,
                    global_privileges: vec!["LOGIN".to_string()],
                    has_grant_option: false,
                },
                DbUser {
                    username: "migration_runner".to_string(),
                    host: None,
                    global_privileges: vec!["LOGIN".to_string(), "CREATEDB".to_string()],
                    has_grant_option: false,
                },
                DbUser {
                    username: "reporting_group".to_string(),
                    host: None,
                    global_privileges: Vec::new(),
                    has_grant_option: false,
                },
            ];
            let db_permissions = vec![
                DatabasePermission {
                    username: "app_service".to_string(),
                    host: None,
                    database: "ecommerce_prod".to_string(),
                    privileges: vec![
                        "CONNECT".to_string(),
                        "TEMPORARY".to_string(),
                        "SELECT".to_string(),
                        "INSERT".to_string(),
                        "UPDATE".to_string(),
                        "DELETE".to_string(),
                        "TRUNCATE".to_string(),
                        "REFERENCES".to_string(),
                        "TRIGGER".to_string(),
                    ],
                    has_grant_option: false,
                },
                DatabasePermission {
                    username: "app_service".to_string(),
                    host: None,
                    database: "billing_service".to_string(),
                    privileges: vec![
                        "CONNECT".to_string(),
                        "SELECT".to_string(),
                        "INSERT".to_string(),
                        "UPDATE".to_string(),
                    ],
                    has_grant_option: false,
                },
                DatabasePermission {
                    username: "app_service".to_string(),
                    host: None,
                    database: "analytics_warehouse".to_string(),
                    privileges: vec!["CONNECT".to_string(), "SELECT".to_string()],
                    has_grant_option: false,
                },
            ];
            handle_backend_event(
                BackendEvent::UsersLoaded {
                    users,
                    selected_user: Some(("app_service".to_string(), None)),
                    db_permissions,
                    toast: None,
                },
                &conn_page,
                &sidebar,
                &workspace_content,
                &root_stack,
                None,
                &|_| {},
            );
            workspace_content.stack.set_visible_child_name("users");
            save_window_screenshot(&window, "assets/users.png");

            let _ = std::fs::remove_dir_all(&temp_cfg);
        });
    }
}

