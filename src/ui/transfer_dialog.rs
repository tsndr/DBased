use crate::db::models::{
    BackendCommand, DatabaseTableSelection, ExportDestination, ExportSpec, ImportSource,
    ImportSpec, SqlExportMode, TableInfo, TransferFormat,
};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

type TableCacheMap = Rc<RefCell<BTreeMap<String, Vec<TableInfo>>>>;
type TableLoadListener = Rc<RefCell<Option<Rc<dyn Fn(&str, &[TableInfo])>>>>;

#[derive(Clone)]
pub struct TransferDialogController {
    cmd_tx: Sender<BackendCommand>,
    databases: Rc<RefCell<Vec<String>>>,
    table_cache: TableCacheMap,
    on_tables_loaded: TableLoadListener,
}

impl TransferDialogController {
    pub fn new(cmd_tx: Sender<BackendCommand>) -> Self {
        Self {
            cmd_tx,
            databases: Rc::new(RefCell::new(Vec::new())),
            table_cache: Rc::new(RefCell::new(BTreeMap::new())),
            on_tables_loaded: Rc::new(RefCell::new(None)),
        }
    }

    pub fn set_databases(&self, dbs: &[String]) {
        *self.databases.borrow_mut() = dbs.to_vec();
        self.table_cache.borrow_mut().clear();
    }

    pub fn clear(&self) {
        self.databases.borrow_mut().clear();
        self.table_cache.borrow_mut().clear();
        *self.on_tables_loaded.borrow_mut() = None;
    }

    pub fn cache_database_tables(&self, database: &str, tables: &[TableInfo]) {
        if !database.trim().is_empty() {
            self.table_cache
                .borrow_mut()
                .insert(database.to_string(), tables.to_vec());
        }
        let cb_opt = self.on_tables_loaded.borrow().clone();
        if let Some(cb) = cb_opt {
            cb(database, tables);
        }
    }

    pub fn open_export_dialog(
        &self,
        parent: &gtk::Widget,
        active_db: &str,
        active_table: Option<&str>,
    ) {
        let dialog = adw::Dialog::builder()
            .title("Export Databases & Tables")
            .content_width(560)
            .content_height(640)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar_view.add_top_bar(&header);

        let outer_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let content_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content_box.set_margin_top(12);
        content_box.set_margin_bottom(12);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ==========================================
        // Group 1: Format & Options
        // ==========================================
        let format_group = adw::PreferencesGroup::builder()
            .title("Format &amp; Options")
            .build();

        let format_labels: Vec<&str> = TransferFormat::ALL.iter().map(|f| f.label()).collect();
        let format_model = gtk::StringList::new(&format_labels);
        let format_row = adw::ComboRow::builder()
            .title("Export Format")
            .model(&format_model)
            .selected(0)
            .build();
        format_group.add(&format_row);

        let sql_mode_labels: Vec<&str> = SqlExportMode::ALL.iter().map(|m| m.label()).collect();
        let sql_mode_model = gtk::StringList::new(&sql_mode_labels);
        let sql_mode_row = adw::ComboRow::builder()
            .title("SQL Content")
            .model(&sql_mode_model)
            .selected(0)
            .build();
        format_group.add(&sql_mode_row);

        let create_db_switch = adw::SwitchRow::builder()
            .title("Include CREATE DATABASE / USE")
            .subtitle("Add database creation and selection statements (recommended for multi-DB dumps)")
            .active(false)
            .build();
        format_group.add(&create_db_switch);

        let drop_table_switch = adw::SwitchRow::builder()
            .title("Include DROP TABLE IF EXISTS")
            .subtitle("Drop existing tables/views before creating them")
            .active(true)
            .build();
        format_group.add(&drop_table_switch);

        let json_packaging_model = gtk::StringList::new(&[
            "Single JSON Bundle (.json)",
            "Folder of per-table JSON files",
        ]);
        let json_packaging_row = adw::ComboRow::builder()
            .title("Multi-Table Packaging")
            .model(&json_packaging_model)
            .selected(0)
            .visible(false)
            .build();
        format_group.add(&json_packaging_row);

        content_box.append(&format_group);

        // ==========================================
        // Group 2: Databases & Tables Checklist
        // ==========================================
        let selection_group = adw::PreferencesGroup::builder()
            .title("Databases &amp; Tables")
            .description("Expand a database to pick individual tables, or check a database to include all of its tables.")
            .build();

        let sel_btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let select_all_dbs_btn = gtk::Button::with_label("Select All");
        select_all_dbs_btn.add_css_class("flat");
        let clear_dbs_btn = gtk::Button::with_label("Clear");
        clear_dbs_btn.add_css_class("flat");
        sel_btn_box.append(&select_all_dbs_btn);
        sel_btn_box.append(&clear_dbs_btn);
        selection_group.set_header_suffix(Some(&sel_btn_box));

        content_box.append(&selection_group);
        outer_scroll.set_child(Some(&content_box));

        // ==========================================
        // Bottom Bar
        // ==========================================
        let bottom_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bottom_bar.set_margin_top(10);
        bottom_bar.set_margin_bottom(10);
        bottom_bar.set_margin_start(16);
        bottom_bar.set_margin_end(16);

        let summary_label = gtk::Label::new(Some("0 tables selected"));
        summary_label.add_css_class("dim-label");
        summary_label.set_halign(gtk::Align::Start);
        summary_label.set_hexpand(true);

        let cancel_btn = gtk::Button::with_label("Cancel");
        let export_btn = gtk::Button::with_label("Export...");
        export_btn.add_css_class("suggested-action");

        bottom_bar.append(&summary_label);
        bottom_bar.append(&cancel_btn);
        bottom_bar.append(&export_btn);

        let main_vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_vbox.append(&outer_scroll);
        main_vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        main_vbox.append(&bottom_bar);

        toolbar_view.set_content(Some(&main_vbox));
        dialog.set_child(Some(&toolbar_view));

        // State tracking: `selected_tables_by_db` maps `db_name -> BTreeSet<table_name>`
        // `pending_select_all_dbs` tracks databases checked before their table list was fetched.
        let selected_tables_by_db: Rc<RefCell<BTreeMap<String, BTreeSet<String>>>> =
            Rc::new(RefCell::new(BTreeMap::new()));
        let pending_select_all_dbs: Rc<RefCell<BTreeSet<String>>> =
            Rc::new(RefCell::new(BTreeSet::new()));
        let db_check_widgets: Rc<RefCell<BTreeMap<String, gtk::CheckButton>>> =
            Rc::new(RefCell::new(BTreeMap::new()));
        let table_check_widgets: Rc<RefCell<BTreeMap<(String, String), gtk::CheckButton>>> =
            Rc::new(RefCell::new(BTreeMap::new()));
        let db_expander_rows: Rc<RefCell<BTreeMap<String, adw::ExpanderRow>>> =
            Rc::new(RefCell::new(BTreeMap::new()));
        let populated_expanders: Rc<RefCell<BTreeSet<String>>> =
            Rc::new(RefCell::new(BTreeSet::new()));
        let suppress_checks = Rc::new(RefCell::new(false));
        let user_toggled_create_db = Rc::new(RefCell::new(false));
        let suppress_create_db_toggle = Rc::new(RefCell::new(false));

        {
            let user_toggled_create_db = user_toggled_create_db.clone();
            let suppress_create_db_toggle = suppress_create_db_toggle.clone();
            create_db_switch.connect_active_notify(move |_| {
                if !*suppress_create_db_toggle.borrow() {
                    *user_toggled_create_db.borrow_mut() = true;
                }
            });
        }

        // Pre-populate selection with current database / table
        if !active_db.trim().is_empty() {
            let mut sel_map = selected_tables_by_db.borrow_mut();
            let entry = sel_map.entry(active_db.to_string()).or_default();
            if let Some(tbl) = active_table.filter(|s| !s.trim().is_empty()) {
                entry.insert(tbl.to_string());
            } else if let Some(cached) = self.table_cache.borrow().get(active_db) {
                for t in cached {
                    entry.insert(t.name.clone());
                }
            } else {
                pending_select_all_dbs
                    .borrow_mut()
                    .insert(active_db.to_string());
            }
        }

        let update_summary = {
            let selected_tables_by_db = selected_tables_by_db.clone();
            let pending_select_all_dbs = pending_select_all_dbs.clone();
            let summary_label = summary_label.clone();
            let export_btn = export_btn.clone();
            let create_db_switch = create_db_switch.clone();
            let user_toggled_create_db = user_toggled_create_db.clone();
            let suppress_create_db_toggle = suppress_create_db_toggle.clone();
            move || {
                let (db_count, tbl_count, has_pending) = {
                    let map = selected_tables_by_db.borrow();
                    let pending = pending_select_all_dbs.borrow();
                    let mut db_count = 0usize;
                    let mut tbl_count = 0usize;
                    for (db, tbls) in map.iter() {
                        if !tbls.is_empty() || pending.contains(db) {
                            db_count += 1;
                            tbl_count += tbls.len();
                        }
                    }
                    for p_db in pending.iter() {
                        if !map.contains_key(p_db) {
                            db_count += 1;
                        }
                    }
                    (db_count, tbl_count, !pending.is_empty())
                };
                if !*user_toggled_create_db.borrow() {
                    *suppress_create_db_toggle.borrow_mut() = true;
                    create_db_switch.set_active(db_count > 1);
                    *suppress_create_db_toggle.borrow_mut() = false;
                }
                if tbl_count > 0 {
                    summary_label.set_label(&format!(
                        "{} database(s), {} table(s) selected",
                        db_count, tbl_count
                    ));
                    export_btn.set_sensitive(true);
                } else if has_pending {
                    summary_label.set_label(&format!(
                        "{} database(s) selected (loading tables...)",
                        db_count
                    ));
                    export_btn.set_sensitive(false);
                } else {
                    summary_label.set_label("No tables selected");
                    export_btn.set_sensitive(false);
                }
            }
        };

        let populate_expander_tables = {
            let selected_tables_by_db = selected_tables_by_db.clone();
            let pending_select_all_dbs = pending_select_all_dbs.clone();
            let db_check_widgets = db_check_widgets.clone();
            let table_check_widgets = table_check_widgets.clone();
            let db_expander_rows = db_expander_rows.clone();
            let populated_expanders = populated_expanders.clone();
            let suppress_checks = suppress_checks.clone();
            let update_summary = update_summary.clone();

            move |db_name: &str, tables: &[TableInfo]| {
                if populated_expanders.borrow().contains(db_name) {
                    return;
                }
                let Some(expander) = db_expander_rows.borrow().get(db_name).cloned() else {
                    return;
                };
                populated_expanders.borrow_mut().insert(db_name.to_string());

                if pending_select_all_dbs.borrow_mut().remove(db_name) {
                    let mut map = selected_tables_by_db.borrow_mut();
                    let set = map.entry(db_name.to_string()).or_default();
                    for t in tables {
                        set.insert(t.name.clone());
                    }
                }

                let is_all_checked = {
                    let map = selected_tables_by_db.borrow();
                    let set_len = map.get(db_name).map(|s| s.len()).unwrap_or(0);
                    !tables.is_empty() && set_len == tables.len()
                };
                let db_chk_opt = db_check_widgets.borrow().get(db_name).cloned();
                if let Some(db_chk) = db_chk_opt {
                    *suppress_checks.borrow_mut() = true;
                    db_chk.set_active(is_all_checked);
                    *suppress_checks.borrow_mut() = false;
                }

                if tables.is_empty() {
                    let empty_row = adw::ActionRow::builder()
                        .title("No tables in this database")
                        .sensitive(false)
                        .build();
                    expander.add_row(&empty_row);
                    update_summary();
                    return;
                }

                for tbl in tables {
                    let is_checked = selected_tables_by_db
                        .borrow()
                        .get(db_name)
                        .is_some_and(|s| s.contains(&tbl.name));

                    let tbl_chk = gtk::CheckButton::new();
                    tbl_chk.set_active(is_checked);
                    tbl_chk.set_valign(gtk::Align::Center);

                    let row_title = if tbl.is_view {
                        format!("{} (VIEW)", tbl.name)
                    } else {
                        tbl.name.clone()
                    };
                    let row = adw::ActionRow::builder()
                        .title(&row_title)
                        .activatable_widget(&tbl_chk)
                        .build();
                    row.add_prefix(&tbl_chk);
                    expander.add_row(&row);

                    table_check_widgets
                        .borrow_mut()
                        .insert((db_name.to_string(), tbl.name.clone()), tbl_chk.clone());

                    let db_str = db_name.to_string();
                    let tbl_str = tbl.name.clone();
                    let total_db_tables = tables.len();
                    let selected_tables_by_db = selected_tables_by_db.clone();
                    let db_check_widgets = db_check_widgets.clone();
                    let suppress_checks = suppress_checks.clone();
                    let update_summary = update_summary.clone();

                    tbl_chk.connect_toggled(move |chk| {
                        if *suppress_checks.borrow() {
                            return;
                        }
                        let mut map = selected_tables_by_db.borrow_mut();
                        let set = map.entry(db_str.clone()).or_default();
                        if chk.is_active() {
                            set.insert(tbl_str.clone());
                        } else {
                            set.remove(&tbl_str);
                        }
                        let all_selected = set.len() == total_db_tables && total_db_tables > 0;
                        drop(map);

                        let db_chk_opt = db_check_widgets.borrow().get(&db_str).cloned();
                        if let Some(db_chk) = db_chk_opt {
                            *suppress_checks.borrow_mut() = true;
                            db_chk.set_active(all_selected);
                            *suppress_checks.borrow_mut() = false;
                        }
                        update_summary();
                    });
                }

                update_summary();
            }
        };

        // Register listener for async table loads while dialog is open
        {
            let populate_cb = populate_expander_tables.clone();
            *self.on_tables_loaded.borrow_mut() =
                Some(Rc::new(move |db_name: &str, tables: &[TableInfo]| {
                    populate_cb(db_name, tables);
                }));
        }

        let databases = self.databases.borrow().clone();
        for db_name in &databases {
            let is_active_db = db_name == active_db;
            let db_chk = gtk::CheckButton::new();
            db_chk.set_valign(gtk::Align::Center);

            let expander = adw::ExpanderRow::builder()
                .title(db_name)
                .expanded(is_active_db)
                .build();
            expander.add_prefix(&db_chk);
            selection_group.add(&expander);

            db_check_widgets
                .borrow_mut()
                .insert(db_name.clone(), db_chk.clone());
            db_expander_rows
                .borrow_mut()
                .insert(db_name.clone(), expander.clone());

            if let Some(cached_tables) = self.table_cache.borrow().get(db_name).cloned() {
                populate_expander_tables(db_name, &cached_tables);
            } else if is_active_db {
                let _ = self.cmd_tx.send_blocking(
                    BackendCommand::FetchDatabaseTablesForTransfer {
                        database: db_name.clone(),
                    },
                );
            }

            // Expander notify: lazy-load tables if not yet loaded
            {
                let db_str = db_name.clone();
                let table_cache = self.table_cache.clone();
                let populated_expanders = populated_expanders.clone();
                let populate_expander_tables = populate_expander_tables.clone();
                let cmd_tx = self.cmd_tx.clone();
                expander.connect_expanded_notify(move |exp| {
                    if exp.is_expanded() && !populated_expanders.borrow().contains(&db_str) {
                        if let Some(cached) = table_cache.borrow().get(&db_str).cloned() {
                            populate_expander_tables(&db_str, &cached);
                        } else {
                            let _ = cmd_tx.send_blocking(
                                BackendCommand::FetchDatabaseTablesForTransfer {
                                    database: db_str.clone(),
                                },
                            );
                        }
                    }
                });
            }

            // Database checkbox toggle: select or deselect all tables in this database
            {
                let db_str = db_name.clone();
                let table_cache = self.table_cache.clone();
                let selected_tables_by_db = selected_tables_by_db.clone();
                let pending_select_all_dbs = pending_select_all_dbs.clone();
                let table_check_widgets = table_check_widgets.clone();
                let populated_expanders = populated_expanders.clone();
                let populate_expander_tables = populate_expander_tables.clone();
                let suppress_checks = suppress_checks.clone();
                let update_summary = update_summary.clone();
                let cmd_tx = self.cmd_tx.clone();

                db_chk.connect_toggled(move |chk| {
                    if *suppress_checks.borrow() {
                        return;
                    }
                    let active = chk.is_active();
                    if let Some(cached) = table_cache.borrow().get(&db_str).cloned() {
                        {
                            let mut map = selected_tables_by_db.borrow_mut();
                            let set = map.entry(db_str.clone()).or_default();
                            set.clear();
                            if active {
                                for t in &cached {
                                    set.insert(t.name.clone());
                                }
                            }
                        }
                        if !populated_expanders.borrow().contains(&db_str) {
                            populate_expander_tables(&db_str, &cached);
                        } else {
                            let checks: Vec<gtk::CheckButton> = {
                                let widgets = table_check_widgets.borrow();
                                cached
                                    .iter()
                                    .filter_map(|t| {
                                        widgets.get(&(db_str.clone(), t.name.clone())).cloned()
                                    })
                                    .collect()
                            };
                            *suppress_checks.borrow_mut() = true;
                            for t_chk in checks {
                                t_chk.set_active(active);
                            }
                            *suppress_checks.borrow_mut() = false;
                            update_summary();
                        }
                    } else {
                        if active {
                            pending_select_all_dbs.borrow_mut().insert(db_str.clone());
                            let _ = cmd_tx.send_blocking(
                                BackendCommand::FetchDatabaseTablesForTransfer {
                                    database: db_str.clone(),
                                },
                            );
                        } else {
                            pending_select_all_dbs.borrow_mut().remove(&db_str);
                            selected_tables_by_db.borrow_mut().remove(&db_str);
                        }
                        update_summary();
                    }
                });
            }
        }

        update_summary();

        // Select All / Clear buttons
        {
            let databases = databases.clone();
            let db_check_widgets = db_check_widgets.clone();
            select_all_dbs_btn.connect_clicked(move |_| {
                for db in &databases {
                    let chk_opt = db_check_widgets.borrow().get(db).cloned();
                    if let Some(chk) = chk_opt {
                        if !chk.is_active() {
                            chk.set_active(true);
                        } else {
                            chk.emit_by_name::<()>("toggled", &[]);
                        }
                    }
                }
            });
        }
        {
            let databases = databases.clone();
            let db_check_widgets = db_check_widgets.clone();
            let selected_tables_by_db = selected_tables_by_db.clone();
            let pending_select_all_dbs = pending_select_all_dbs.clone();
            let table_check_widgets = table_check_widgets.clone();
            let suppress_checks = suppress_checks.clone();
            let update_summary = update_summary.clone();
            clear_dbs_btn.connect_clicked(move |_| {
                *suppress_checks.borrow_mut() = true;
                selected_tables_by_db.borrow_mut().clear();
                pending_select_all_dbs.borrow_mut().clear();
                let db_checks: Vec<gtk::CheckButton> = {
                    let widgets = db_check_widgets.borrow();
                    databases
                        .iter()
                        .filter_map(|db| widgets.get(db).cloned())
                        .collect()
                };
                for chk in db_checks {
                    chk.set_active(false);
                }
                let tbl_checks: Vec<gtk::CheckButton> =
                    table_check_widgets.borrow().values().cloned().collect();
                for chk in tbl_checks {
                    chk.set_active(false);
                }
                *suppress_checks.borrow_mut() = false;
                update_summary();
            });
        }

        // Format selection visibility handler
        {
            let sql_mode_row = sql_mode_row.clone();
            let create_db_switch = create_db_switch.clone();
            let drop_table_switch = drop_table_switch.clone();
            let json_packaging_row = json_packaging_row.clone();
            format_row.connect_selected_notify(move |combo| {
                let fmt = TransferFormat::ALL
                    .get(combo.selected() as usize)
                    .copied()
                    .unwrap_or(TransferFormat::Sql);
                let is_sql = fmt == TransferFormat::Sql;
                sql_mode_row.set_visible(is_sql);
                create_db_switch.set_visible(is_sql);
                drop_table_switch.set_visible(is_sql);
                json_packaging_row.set_visible(fmt == TransferFormat::Json);
            });
        }

        // SQL mode visibility handler for DROP TABLE switch
        {
            let drop_table_switch = drop_table_switch.clone();
            sql_mode_row.connect_selected_notify(move |combo| {
                let mode = SqlExportMode::ALL
                    .get(combo.selected() as usize)
                    .copied()
                    .unwrap_or(SqlExportMode::StructureAndData);
                drop_table_switch.set_sensitive(mode.includes_structure());
            });
        }

        {
            let dialog = dialog.clone();
            let on_tables_loaded = self.on_tables_loaded.clone();
            cancel_btn.connect_clicked(move |_| {
                *on_tables_loaded.borrow_mut() = None;
                dialog.close();
            });
        }

        {
            let dialog = dialog.clone();
            let parent_widget = parent.clone();
            let cmd_tx = self.cmd_tx.clone();
            let on_tables_loaded = self.on_tables_loaded.clone();
            let selected_tables_by_db = selected_tables_by_db.clone();
            let databases_order = databases.clone();

            export_btn.connect_clicked(move |_| {
                let fmt = TransferFormat::ALL
                    .get(format_row.selected() as usize)
                    .copied()
                    .unwrap_or(TransferFormat::Sql);
                let sql_mode = SqlExportMode::ALL
                    .get(sql_mode_row.selected() as usize)
                    .copied()
                    .unwrap_or(SqlExportMode::StructureAndData);
                let include_create_database = create_db_switch.is_active();
                let include_drop_table = drop_table_switch.is_active();
                let json_use_folder = json_packaging_row.selected() == 1;

                let map = selected_tables_by_db.borrow();
                let mut selections = Vec::new();
                for db in &databases_order {
                    if let Some(tbl_set) = map.get(db) {
                        if !tbl_set.is_empty() {
                            selections.push(DatabaseTableSelection {
                                database: db.clone(),
                                tables: tbl_set.iter().cloned().collect(),
                            });
                        }
                    }
                }
                drop(map);

                if selections.is_empty() {
                    return;
                }

                let total_dbs = selections.len();
                let total_tables: usize = selections.iter().map(|s| s.tables.len()).sum();

                let use_directory = match fmt {
                    TransferFormat::Sql => false,
                    TransferFormat::Csv => total_dbs > 1 || total_tables > 1,
                    TransferFormat::Json => {
                        (total_dbs > 1 || total_tables > 1) && json_use_folder
                    }
                };

                let window = parent_widget
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());

                if use_directory {
                    let file_dialog = gtk::FileDialog::builder()
                        .title(format!(
                            "Select Destination Folder for {} Export",
                            fmt.extension().to_ascii_uppercase()
                        ))
                        .build();
                    let cmd_tx = cmd_tx.clone();
                    let dialog = dialog.clone();
                    let on_tables_loaded = on_tables_loaded.clone();
                    file_dialog.select_folder(
                        window.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        move |res| {
                            if let Ok(file) = res {
                                if let Some(dir_path) = file.path() {
                                    let spec = ExportSpec {
                                        format: fmt,
                                        selections,
                                        sql_mode,
                                        include_create_database,
                                        include_drop_table,
                                        destination: ExportDestination::Directory(dir_path),
                                    };
                                    let _ = cmd_tx.send_blocking(BackendCommand::RunExport(spec));
                                    *on_tables_loaded.borrow_mut() = None;
                                    dialog.close();
                                }
                            }
                        },
                    );
                } else {
                    let default_filename = if total_dbs == 1 && total_tables == 1 {
                        format!(
                            "{}.{}",
                            selections[0].tables[0],
                            fmt.extension()
                        )
                    } else if total_dbs == 1 {
                        format!("{}.{}", selections[0].database, fmt.extension())
                    } else {
                        format!("databases_export.{}", fmt.extension())
                    };

                    let file_dialog = gtk::FileDialog::builder()
                        .title(format!("Export to {}", fmt.label()))
                        .initial_name(&default_filename)
                        .build();
                    let cmd_tx = cmd_tx.clone();
                    let dialog = dialog.clone();
                    let on_tables_loaded = on_tables_loaded.clone();
                    file_dialog.save(
                        window.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        move |res| {
                            if let Ok(file) = res {
                                if let Some(file_path) = file.path() {
                                    let spec = ExportSpec {
                                        format: fmt,
                                        selections,
                                        sql_mode,
                                        include_create_database,
                                        include_drop_table,
                                        destination: ExportDestination::SingleFile(file_path),
                                    };
                                    let _ = cmd_tx.send_blocking(BackendCommand::RunExport(spec));
                                    *on_tables_loaded.borrow_mut() = None;
                                    dialog.close();
                                }
                            }
                        },
                    );
                }
            });
        }

        dialog.present(Some(parent));
    }

    pub fn open_import_dialog(
        &self,
        parent: &gtk::Widget,
        active_db: &str,
        active_table: Option<&str>,
    ) {
        let dialog = adw::Dialog::builder()
            .title("Import Databases & Tables")
            .content_width(520)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar_view.add_top_bar(&header);

        let content_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content_box.set_margin_top(12);
        content_box.set_margin_bottom(12);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ==========================================
        // Group 1: Format & Source
        // ==========================================
        let source_group = adw::PreferencesGroup::builder()
            .title("Source")
            .build();

        let format_labels: Vec<&str> = TransferFormat::ALL.iter().map(|f| f.label()).collect();
        let format_model = gtk::StringList::new(&format_labels);
        let format_row = adw::ComboRow::builder()
            .title("Import Format")
            .model(&format_model)
            .selected(0)
            .build();
        source_group.add(&format_row);

        let source_mode_model = gtk::StringList::new(&["File(s)", "Folder"]);
        let source_mode_row = adw::ComboRow::builder()
            .title("Source Type")
            .model(&source_mode_model)
            .selected(0)
            .build();
        source_group.add(&source_mode_row);

        let source_path_row = adw::ActionRow::builder()
            .title("Selected Source")
            .subtitle("No file or folder selected")
            .build();
        let browse_btn = gtk::Button::with_label("Browse...");
        browse_btn.set_valign(gtk::Align::Center);
        source_path_row.add_suffix(&browse_btn);
        source_path_row.set_activatable_widget(Some(&browse_btn));
        source_group.add(&source_path_row);

        content_box.append(&source_group);

        // ==========================================
        // Group 2: Target Database & Table
        // ==========================================
        let target_group = adw::PreferencesGroup::builder()
            .title("Target Destination")
            .description("CSV and JSON imports map columns to existing tables.")
            .build();

        let databases = self.databases.borrow().clone();
        let mut db_options = vec!["(From Script / Multi-Database)".to_string()];
        db_options.extend(databases.iter().cloned());
        let db_str_refs: Vec<&str> = db_options.iter().map(|s| s.as_str()).collect();
        let target_db_model = gtk::StringList::new(&db_str_refs);

        let initial_db_idx = databases
            .iter()
            .position(|d| d == active_db)
            .map(|idx| (idx + 1) as u32)
            .unwrap_or(if databases.is_empty() { 0 } else { 1 });

        let target_db_row = adw::ComboRow::builder()
            .title("Target Database")
            .model(&target_db_model)
            .selected(initial_db_idx)
            .build();
        target_group.add(&target_db_row);

        let target_table_model = gtk::StringList::new(&["(Auto-match by filename / bundle key)"]);
        let target_table_row = adw::ComboRow::builder()
            .title("Target Table")
            .model(&target_table_model)
            .selected(0)
            .visible(false)
            .build();
        target_group.add(&target_table_row);

        let truncate_switch = adw::SwitchRow::builder()
            .title("Truncate Target Table(s) Before Import")
            .subtitle("Clear existing rows in target table(s) before inserting imported data")
            .active(false)
            .visible(false)
            .build();
        target_group.add(&truncate_switch);

        content_box.append(&target_group);

        // ==========================================
        // Bottom Buttons
        // ==========================================
        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_margin_top(4);

        let cancel_btn = gtk::Button::with_label("Cancel");
        let import_btn = gtk::Button::with_label("Import");
        import_btn.add_css_class("suggested-action");
        import_btn.set_sensitive(false);

        btn_box.append(&cancel_btn);
        btn_box.append(&import_btn);
        content_box.append(&btn_box);

        toolbar_view.set_content(Some(&content_box));
        dialog.set_child(Some(&toolbar_view));

        let chosen_source: Rc<RefCell<Option<ImportSource>>> = Rc::new(RefCell::new(None));
        let active_table_initial = active_table.map(|s| s.to_string());

        let refresh_table_dropdown = {
            let target_db_model = target_db_model.clone();
            let target_db_row = target_db_row.clone();
            let target_table_model = target_table_model.clone();
            let target_table_row = target_table_row.clone();
            let table_cache = self.table_cache.clone();
            let cmd_tx = self.cmd_tx.clone();
            let active_table_initial = active_table_initial.clone();

            move |preferred_stem: Option<&str>| {
                let db_idx = target_db_row.selected();
                let Some(db_str) = target_db_model.string(db_idx).map(|s| s.to_string()) else {
                    return;
                };
                if db_idx == 0 || db_str.starts_with('(') {
                    let count = target_table_model.n_items();
                    target_table_model.splice(0, count, &["(Auto-match by filename / bundle key)"]);
                    target_table_row.set_selected(0);
                    return;
                }

                if let Some(cached) = table_cache.borrow().get(&db_str).cloned() {
                    let mut opts = vec!["(Auto-match by filename / bundle key)".to_string()];
                    for t in &cached {
                        if !t.is_view {
                            opts.push(t.name.clone());
                        }
                    }
                    let refs: Vec<&str> = opts.iter().map(|s| s.as_str()).collect();
                    let count = target_table_model.n_items();
                    target_table_model.splice(0, count, &refs);

                    let candidate = preferred_stem.or(active_table_initial.as_deref());
                    if let Some(cand) = candidate {
                        if let Some(pos) = opts
                            .iter()
                            .skip(1)
                            .position(|t| t.eq_ignore_ascii_case(cand))
                        {
                            target_table_row.set_selected((pos + 1) as u32);
                            return;
                        }
                    }
                    target_table_row.set_selected(0);
                } else {
                    let _ = cmd_tx.send_blocking(
                        BackendCommand::FetchDatabaseTablesForTransfer { database: db_str },
                    );
                }
            }
        };

        // Listen for table list loads while Import dialog is open
        {
            let refresh_cb = refresh_table_dropdown.clone();
            *self.on_tables_loaded.borrow_mut() =
                Some(Rc::new(move |_db_name: &str, _tables: &[TableInfo]| {
                    refresh_cb(None);
                }));
        }

        refresh_table_dropdown(None);

        // Update visibility when format changes
        {
            let target_table_row = target_table_row.clone();
            let truncate_switch = truncate_switch.clone();
            let chosen_source = chosen_source.clone();
            let source_path_row = source_path_row.clone();
            let import_btn = import_btn.clone();
            format_row.connect_selected_notify(move |combo| {
                let fmt = TransferFormat::ALL
                    .get(combo.selected() as usize)
                    .copied()
                    .unwrap_or(TransferFormat::Sql);
                let is_tabular = matches!(fmt, TransferFormat::Csv | TransferFormat::Json);
                target_table_row.set_visible(is_tabular);
                truncate_switch.set_visible(is_tabular);
                *chosen_source.borrow_mut() = None;
                source_path_row.set_subtitle("No file or folder selected");
                import_btn.set_sensitive(false);
            });
        }

        // Reset chosen source when switching File(s) vs Folder
        {
            let chosen_source = chosen_source.clone();
            let source_path_row = source_path_row.clone();
            let import_btn = import_btn.clone();
            source_mode_row.connect_selected_notify(move |_| {
                *chosen_source.borrow_mut() = None;
                source_path_row.set_subtitle("No file or folder selected");
                import_btn.set_sensitive(false);
            });
        }

        // Update tables when Target Database changes
        {
            let refresh_cb = refresh_table_dropdown.clone();
            target_db_row.connect_selected_notify(move |_| {
                refresh_cb(None);
            });
        }

        // Browse button handler
        {
            let parent_widget = parent.clone();
            let format_row = format_row.clone();
            let source_mode_row = source_mode_row.clone();
            let source_path_row = source_path_row.clone();
            let chosen_source = chosen_source.clone();
            let import_btn = import_btn.clone();
            let refresh_cb = refresh_table_dropdown.clone();

            browse_btn.connect_clicked(move |_| {
                let fmt = TransferFormat::ALL
                    .get(format_row.selected() as usize)
                    .copied()
                    .unwrap_or(TransferFormat::Sql);
                let is_folder = source_mode_row.selected() == 1;
                let window = parent_widget
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());

                if is_folder {
                    let file_dialog = gtk::FileDialog::builder()
                        .title(format!(
                            "Select Folder Containing .{} Files",
                            fmt.extension()
                        ))
                        .build();
                    let chosen_source = chosen_source.clone();
                    let source_path_row = source_path_row.clone();
                    let import_btn = import_btn.clone();
                    file_dialog.select_folder(
                        window.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        move |res| {
                            if let Ok(file) = res {
                                if let Some(path) = file.path() {
                                    source_path_row.set_subtitle(&path.display().to_string());
                                    *chosen_source.borrow_mut() =
                                        Some(ImportSource::Directory(path));
                                    import_btn.set_sensitive(true);
                                }
                            }
                        },
                    );
                } else {
                    let filter = gtk::FileFilter::new();
                    filter.set_name(Some(fmt.label()));
                    filter.add_suffix(fmt.extension());
                    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                    filters.append(&filter);

                    let file_dialog = gtk::FileDialog::builder()
                        .title(format!("Select {} File(s) to Import", fmt.label()))
                        .filters(&filters)
                        .default_filter(&filter)
                        .build();

                    let chosen_source = chosen_source.clone();
                    let source_path_row = source_path_row.clone();
                    let import_btn = import_btn.clone();
                    let refresh_cb = refresh_cb.clone();
                    file_dialog.open_multiple(
                        window.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        move |res| {
                            if let Ok(files) = res {
                                let mut paths = Vec::new();
                                for i in 0..files.n_items() {
                                    if let Some(file) = files
                                        .item(i)
                                        .and_then(|obj| obj.downcast::<gtk::gio::File>().ok())
                                    {
                                        if let Some(p) = file.path() {
                                            paths.push(p);
                                        }
                                    }
                                }
                                if !paths.is_empty() {
                                    if paths.len() == 1 {
                                        source_path_row
                                            .set_subtitle(&paths[0].display().to_string());
                                        if let Some(stem) =
                                            paths[0].file_stem().and_then(|s| s.to_str())
                                        {
                                            refresh_cb(Some(stem));
                                        }
                                    } else {
                                        source_path_row.set_subtitle(&format!(
                                            "{} files selected",
                                            paths.len()
                                        ));
                                    }
                                    *chosen_source.borrow_mut() = Some(ImportSource::Files(paths));
                                    import_btn.set_sensitive(true);
                                }
                            }
                        },
                    );
                }
            });
        }

        {
            let dialog = dialog.clone();
            let on_tables_loaded = self.on_tables_loaded.clone();
            cancel_btn.connect_clicked(move |_| {
                *on_tables_loaded.borrow_mut() = None;
                dialog.close();
            });
        }

        {
            let dialog = dialog.clone();
            let cmd_tx = self.cmd_tx.clone();
            let on_tables_loaded = self.on_tables_loaded.clone();
            let chosen_source = chosen_source.clone();
            let active_ui_db = if active_db.trim().is_empty() {
                None
            } else {
                Some(active_db.to_string())
            };

            import_btn.connect_clicked(move |_| {
                let Some(source) = chosen_source.borrow().clone() else {
                    return;
                };
                let fmt = TransferFormat::ALL
                    .get(format_row.selected() as usize)
                    .copied()
                    .unwrap_or(TransferFormat::Sql);

                let db_idx = target_db_row.selected();
                let target_database = target_db_model
                    .string(db_idx)
                    .map(|s| s.to_string())
                    .filter(|s| db_idx > 0 && !s.starts_with('('));

                let tbl_idx = target_table_row.selected();
                let target_table = if matches!(fmt, TransferFormat::Csv | TransferFormat::Json) {
                    target_table_model
                        .string(tbl_idx)
                        .map(|s| s.to_string())
                        .filter(|s| tbl_idx > 0 && !s.starts_with('('))
                } else {
                    None
                };

                let spec = ImportSpec {
                    format: fmt,
                    source,
                    target_database,
                    target_table,
                    truncate_before_import: truncate_switch.is_active(),
                    current_ui_database: active_ui_db.clone(),
                };

                let _ = cmd_tx.send_blocking(BackendCommand::RunImport(spec));
                *on_tables_loaded.borrow_mut() = None;
                dialog.close();
            });
        }

        dialog.present(Some(parent));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_check_buttons(root: &gtk::Widget, out: &mut Vec<gtk::CheckButton>) {
        if let Ok(chk) = root.clone().downcast::<gtk::CheckButton>() {
            out.push(chk);
        }
        let mut child = root.first_child();
        while let Some(w) = child {
            find_check_buttons(&w, out);
            child = w.next_sibling();
        }
    }

    #[test]
    fn test_transfer_controller_set_databases_clears_stale_table_cache_and_allows_reentrant_listener() {
        let (cmd_tx, _cmd_rx) = async_channel::unbounded();
        let ctrl = TransferDialogController::new(cmd_tx);
        ctrl.set_databases(&["db1".to_string(), "db2".to_string()]);
        ctrl.cache_database_tables(
            "db1",
            &[TableInfo {
                name: "users".to_string(),
                is_view: false,
            }],
        );
        ctrl.cache_database_tables(
            "db2",
            &[TableInfo {
                name: "orders".to_string(),
                is_view: false,
            }],
        );
        assert_eq!(ctrl.table_cache.borrow().len(), 2);

        // Updating the database list (e.g. on Refresh / Reconnect) must invalidate cached table lists
        ctrl.set_databases(&["db1".to_string()]);
        assert!(
            ctrl.table_cache.borrow().is_empty(),
            "set_databases should clear stale table_cache entries"
        );

        // Re-entrantly clearing `on_tables_loaded` inside the listener must not panic with RefCell borrow error
        let ctrl_for_cb = ctrl.clone();
        *ctrl.on_tables_loaded.borrow_mut() = Some(Rc::new(move |_db, _tables| {
            *ctrl_for_cb.on_tables_loaded.borrow_mut() = None;
        }));
        ctrl.cache_database_tables(
            "db1",
            &[TableInfo {
                name: "users".to_string(),
                is_view: false,
            }],
        );
        assert!(ctrl.on_tables_loaded.borrow().is_none());
    }

    #[test]
    fn test_export_dialog_toggling_cached_unpopulated_db_checkbox_keeps_it_active() {
        crate::ui::test_util::run_gtk_test(|| {
            let (cmd_tx, _cmd_rx) = async_channel::unbounded();
            let ctrl = TransferDialogController::new(cmd_tx);
            ctrl.set_databases(&["db1".to_string(), "db2".to_string()]);
            ctrl.cache_database_tables(
                "db1",
                &[TableInfo {
                    name: "users".to_string(),
                    is_view: false,
                }],
            );

            let host_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let win = adw::Window::builder()
                .default_width(600)
                .default_height(500)
                .content(&host_box)
                .build();
            win.present();

            ctrl.open_export_dialog(host_box.upcast_ref::<gtk::Widget>(), "db1", None);
            let ctx = gtk::glib::MainContext::default();
            while ctx.iteration(false) {}

            // Populate cache for db2 directly without firing on_tables_loaded so db2's expander is unpopulated
            ctrl.table_cache.borrow_mut().insert(
                "db2".to_string(),
                vec![TableInfo {
                    name: "invoices".to_string(),
                    is_view: false,
                }],
            );

            let mut checks = Vec::new();
            find_check_buttons(win.upcast_ref::<gtk::Widget>(), &mut checks);
            // Expect: db1 db_chk (active), db1.users tbl_chk (active), db2 db_chk (inactive)
            let db2_chk = checks
                .iter()
                .find(|c| !c.is_active())
                .cloned()
                .expect("unchecked db2 checkbox");

            db2_chk.set_active(true);
            while ctx.iteration(false) {}

            assert!(
                db2_chk.is_active(),
                "Checking a cached unpopulated database checkbox must keep the database checkbox active"
            );

            win.close();
        });
    }
}

