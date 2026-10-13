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
        .default_width(1240)
        .default_height(800)
        .content(&toast_overlay)
        .build();

    // Listen for BackendEvent messages on the GTK main loop
    let window_weak = window.downgrade();
    gtk::glib::spawn_future_local(async move {
        while let Ok(evt) = evt_rx.recv().await {
            match evt {
                BackendEvent::ConnectionTestResult(res) => match res {
                    Ok(msg) => {
                        conn_page.set_busy(false, "Connection OK");
                        show_toast(&msg);
                    }
                    Err(err) => {
                        conn_page.set_busy(false, "Connection failed");
                        if let Some(win) = window_weak.upgrade() {
                            show_error_dialog(win.upcast_ref::<gtk::Widget>(), "Connection Test Failed", &err);
                        }
                    }
                },
                BackendEvent::Connected {
                    connection_name,
                    engine_label,
                    databases,
                    selected_database,
                    tables,
                } => {
                    conn_page.set_busy(false, "");
                    let init_db = selected_database.as_deref();
                    if let Some(db) = init_db {
                        workspace_content.query_tab.set_database(db);
                    }
                    sidebar.set_connection_state(
                        &connection_name,
                        &engine_label,
                        &databases,
                        init_db,
                        tables,
                    );
                    root_stack.set_visible_child_name("workspace");
                    show_toast(&format!("Connected to {}", connection_name));
                }
                BackendEvent::Disconnected => {
                    workspace_content.content_tab.clear_navigation_history();
                    root_stack.set_visible_child_name("connection");
                    show_toast("Disconnected");
                }
                BackendEvent::DatabaseSelected { database, tables } => {
                    workspace_content.content_tab.clear_navigation_history();
                    workspace_content.query_tab.set_database(&database);
                    sidebar.set_tables(tables);
                }
                BackendEvent::TableLoaded {
                    database,
                    table,
                    all_tables,
                    structure,
                    content,
                    relations,
                    content_query,
                } => {
                    sidebar.set_selected_table(&table);
                    let local_cols: Vec<String> =
                        structure.iter().map(|c| c.name.clone()).collect();
                    workspace_content.structure_tab.set_structure(
                        &database,
                        &table,
                        structure,
                    );
                    workspace_content
                        .content_tab
                        .set_query_for_table(&database, content_query);
                    workspace_content
                        .content_tab
                        .set_relations(relations.clone());
                    workspace_content.content_tab.set_page(content);
                    workspace_content.relations_tab.set_context(
                        &database,
                        &table,
                        local_cols,
                        all_tables,
                    );
                    workspace_content.relations_tab.set_relations(
                        &database,
                        &table,
                        relations,
                    );
                    workspace_content.query_tab.set_database(&database);
                }
                BackendEvent::StructureUpdated {
                    database,
                    table,
                    structure,
                    toast,
                } => {
                    workspace_content.structure_tab.set_structure(
                        &database,
                        &table,
                        structure,
                    );
                    workspace_content.content_tab.request_fetch();
                    if let Some(msg) = toast {
                        show_toast(&msg);
                    }
                }
                BackendEvent::ContentLoaded { page, toast } => {
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
                    workspace_content
                        .content_tab
                        .set_relations(relations.clone());
                    workspace_content.content_tab.refresh_grid();
                    workspace_content.relations_tab.set_relations(
                        &database,
                        &table,
                        relations,
                    );
                    if let Some(msg) = toast {
                        show_toast(&msg);
                    }
                }
                BackendEvent::TargetTableColumnsLoaded { .. } => {}
                BackendEvent::RawQueryFinished(outcome) => {
                    workspace_content.query_tab.show_outcome(outcome);
                }
                BackendEvent::Error(err) => {
                    conn_page.set_busy(false, "");
                    if let Some(win) = window_weak.upgrade() {
                        show_error_dialog(win.upcast_ref::<gtk::Widget>(), "Database Error", &err);
                    }
                }
                BackendEvent::InfoToast(msg) => {
                    show_toast(&msg);
                }
            }
        }
    });

    window
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
