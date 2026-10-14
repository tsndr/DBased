use crate::config::bookmarks::DbEngine;
use crate::db::models::{BackendCommand, ContentQuery, CreateDatabaseSpec, TableInfo};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;

const MYSQL_CHARSETS: &[&str] = &["Default", "utf8mb4", "utf8", "latin1", "ascii", "binary"];
const PG_ENCODINGS: &[&str] = &["Default", "UTF8", "LATIN1", "SQL_ASCII"];
const PG_TEMPLATES: &[&str] = &["Default", "template0", "template1"];

#[derive(Clone)]
pub struct WorkspaceSidebar {
    pub page: adw::NavigationPage,
    window_title: adw::WindowTitle,
    db_dropdown: gtk::DropDown,
    db_model: gtk::StringList,
    table_search: gtk::SearchEntry,
    table_list: gtk::ListBox,
    all_tables: Rc<RefCell<Vec<TableInfo>>>,
    current_engine: Rc<RefCell<DbEngine>>,
    selected_db: Rc<RefCell<String>>,
    selected_table: Rc<RefCell<Option<String>>>,
    suppress_db_signal: Rc<RefCell<bool>>,
    suppress_table_signal: Rc<RefCell<bool>>,
    cmd_tx: Sender<BackendCommand>,
}

impl WorkspaceSidebar {
    pub fn new(cmd_tx: Sender<BackendCommand>) -> Self {
        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let window_title = adw::WindowTitle::new("DBased", "Connected");
        header.set_title_widget(Some(&window_title));

        let disconnect_btn = gtk::Button::from_icon_name("network-offline-symbolic");
        disconnect_btn.set_tooltip_text(Some("Disconnect"));
        header.pack_start(&disconnect_btn);

        let refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_btn.set_tooltip_text(Some("Refresh"));
        header.pack_end(&refresh_btn);

        toolbar_view.add_top_bar(&header);

        let top_controls = gtk::Box::new(gtk::Orientation::Vertical, 8);
        top_controls.set_margin_top(10);
        top_controls.set_margin_bottom(8);
        top_controls.set_margin_start(10);
        top_controls.set_margin_end(10);

        let db_label = gtk::Label::new(Some("DATABASE"));
        db_label.add_css_class("caption-heading");
        db_label.add_css_class("dim-label");
        db_label.set_halign(gtk::Align::Start);

        let db_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let db_model = gtk::StringList::new(&[]);
        let db_dropdown = gtk::DropDown::new(Some(db_model.clone()), gtk::Expression::NONE);
        db_dropdown.set_hexpand(true);
        db_dropdown.set_tooltip_text(Some("Select Database"));

        let create_db_btn = gtk::Button::from_icon_name("list-add-symbolic");
        create_db_btn.set_tooltip_text(Some("Create Database"));

        db_row.append(&db_dropdown);
        db_row.append(&create_db_btn);

        let tables_label = gtk::Label::new(Some("TABLES"));
        tables_label.add_css_class("caption-heading");
        tables_label.add_css_class("dim-label");
        tables_label.set_halign(gtk::Align::Start);
        tables_label.set_margin_top(4);

        let table_search = gtk::SearchEntry::new();
        table_search.set_placeholder_text(Some("Filter tables..."));

        top_controls.append(&db_label);
        top_controls.append(&db_row);
        top_controls.append(&tables_label);
        top_controls.append(&table_search);

        let table_list = gtk::ListBox::new();
        table_list.add_css_class("navigation-sidebar");
        table_list.set_selection_mode(gtk::SelectionMode::Single);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&table_list)
            .build();

        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_box.append(&top_controls);
        main_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        main_box.append(&scroll);

        toolbar_view.set_content(Some(&main_box));

        let page = adw::NavigationPage::builder()
            .title("Database Browser")
            .child(&toolbar_view)
            .build();

        let all_tables = Rc::new(RefCell::new(Vec::<TableInfo>::new()));
        let current_engine = Rc::new(RefCell::new(DbEngine::MySql));
        let selected_db = Rc::new(RefCell::new(String::new()));
        let selected_table = Rc::new(RefCell::new(None::<String>));
        let suppress_db_signal = Rc::new(RefCell::new(false));
        let suppress_table_signal = Rc::new(RefCell::new(false));

        // Disconnect button handler
        {
            let cmd_tx = cmd_tx.clone();
            disconnect_btn.connect_clicked(move |_| {
                let _ = cmd_tx.send_blocking(BackendCommand::Disconnect);
            });
        }

        // Refresh button handler
        {
            let cmd_tx = cmd_tx.clone();
            let selected_db = selected_db.clone();
            refresh_btn.connect_clicked(move |_| {
                let db = selected_db.borrow().clone();
                let current_database = if db.is_empty() { None } else { Some(db) };
                let _ = cmd_tx.send_blocking(BackendCommand::Refresh { current_database });
            });
        }

        // Database dropdown selection handler
        {
            let cmd_tx = cmd_tx.clone();
            let db_model = db_model.clone();
            let selected_db = selected_db.clone();
            let suppress_db_signal = suppress_db_signal.clone();
            db_dropdown.connect_selected_notify(move |dd| {
                if *suppress_db_signal.borrow() {
                    return;
                }
                let idx = dd.selected();
                if let Some(item) = db_model.string(idx) {
                    let db_str = item.to_string();
                    *selected_db.borrow_mut() = db_str.clone();
                    let _ = cmd_tx.send_blocking(BackendCommand::SelectDatabase(db_str));
                }
            });
        }

        // Table search filter handler
        let sidebar = Self {
            page,
            window_title,
            db_dropdown,
            db_model,
            table_search: table_search.clone(),
            table_list: table_list.clone(),
            all_tables,
            current_engine,
            selected_db: selected_db.clone(),
            selected_table: selected_table.clone(),
            suppress_db_signal,
            suppress_table_signal: suppress_table_signal.clone(),
            cmd_tx: cmd_tx.clone(),
        };

        // Create Database button handler
        {
            let sidebar_clone = sidebar.clone();
            create_db_btn.connect_clicked(move |btn| {
                sidebar_clone.open_create_database_dialog(btn.upcast_ref::<gtk::Widget>());
            });
        }

        {
            let sidebar_clone = sidebar.clone();
            table_search.connect_search_changed(move |_| {
                sidebar_clone.rebuild_table_list();
            });
        }

        // Table row selection handler
        {
            let cmd_tx = cmd_tx.clone();
            let selected_db = selected_db.clone();
            let selected_table = selected_table.clone();
            table_list.connect_row_selected(move |_, opt_row| {
                if *suppress_table_signal.borrow() {
                    return;
                }
                if let Some(row) = opt_row {
                    let table_name = row.widget_name().to_string();
                    if !table_name.is_empty() {
                        *selected_table.borrow_mut() = Some(table_name.clone());
                        let db = selected_db.borrow().clone();
                        let _ = cmd_tx.send_blocking(BackendCommand::SelectTable {
                            database: db,
                            table: table_name,
                            content_query: ContentQuery::default(),
                        });
                    }
                }
            });
        }

        sidebar
    }

    pub fn set_connection_state(
        &self,
        connection_name: &str,
        engine: DbEngine,
        engine_label: &str,
        databases: &[String],
        initial_db: Option<&str>,
        tables: Vec<TableInfo>,
    ) {
        *self.current_engine.borrow_mut() = engine;
        self.window_title.set_title(connection_name);
        self.window_title.set_subtitle(engine_label);
        *self.selected_table.borrow_mut() = None;
        self.set_databases_and_tables(databases, initial_db, tables);
    }

    pub fn set_databases_and_tables(
        &self,
        databases: &[String],
        selected_db: Option<&str>,
        tables: Vec<TableInfo>,
    ) {
        let prev_db = self.selected_db.borrow().clone();

        *self.suppress_db_signal.borrow_mut() = true;
        let count = self.db_model.n_items();
        let str_refs: Vec<&str> = databases.iter().map(|s| s.as_str()).collect();
        self.db_model.splice(0, count, &str_refs);

        let mut chosen_idx = 0u32;
        if let Some(target) = selected_db {
            if let Some(pos) = databases.iter().position(|d| d == target) {
                chosen_idx = pos as u32;
                *self.selected_db.borrow_mut() = target.to_string();
            } else if let Some(first) = databases.first() {
                *self.selected_db.borrow_mut() = first.clone();
            } else {
                self.selected_db.borrow_mut().clear();
            }
        } else if let Some(first) = databases.first() {
            *self.selected_db.borrow_mut() = first.clone();
        } else {
            self.selected_db.borrow_mut().clear();
        }
        if !databases.is_empty() {
            self.db_dropdown.set_selected(chosen_idx);
        }
        *self.suppress_db_signal.borrow_mut() = false;

        let new_db = self.selected_db.borrow().clone();
        if prev_db == new_db {
            let prev_table = self.selected_table.borrow().clone();
            *self.all_tables.borrow_mut() = tables;
            if let Some(ref tbl_name) = prev_table {
                if !self.all_tables.borrow().iter().any(|t| &t.name == tbl_name) {
                    *self.selected_table.borrow_mut() = None;
                }
            }
            self.rebuild_table_list();
        } else {
            self.set_tables(tables);
        }
    }

    pub fn set_tables(&self, tables: Vec<TableInfo>) {
        *self.all_tables.borrow_mut() = tables;
        *self.selected_table.borrow_mut() = None;
        self.rebuild_table_list();
    }

    pub fn set_selected_table(&self, table_name: &str) {
        *self.selected_table.borrow_mut() = Some(table_name.to_string());
        *self.suppress_table_signal.borrow_mut() = true;
        let mut child = self.table_list.first_child();
        let mut matched = false;
        while let Some(widget) = child {
            if let Ok(row) = widget.clone().downcast::<gtk::ListBoxRow>() {
                if row.widget_name().as_str() == table_name {
                    self.table_list.select_row(Some(&row));
                    matched = true;
                    break;
                }
            }
            child = widget.next_sibling();
        }
        if !matched {
            self.table_list.unselect_all();
        }
        *self.suppress_table_signal.borrow_mut() = false;
    }

    pub fn current_database(&self) -> String {
        self.selected_db.borrow().clone()
    }

    pub fn rebuild_table_list(&self) {
        *self.suppress_table_signal.borrow_mut() = true;
        while let Some(child) = self.table_list.first_child() {
            self.table_list.remove(&child);
        }

        let filter = self.table_search.text().to_lowercase();
        let tables = self.all_tables.borrow();
        let active_table = self.selected_table.borrow().clone();

        for tbl in tables.iter() {
            if !filter.is_empty() && !tbl.name.to_lowercase().contains(&filter) {
                continue;
            }
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row_box.set_margin_top(6);
            row_box.set_margin_bottom(6);
            row_box.set_margin_start(8);
            row_box.set_margin_end(8);

            let icon_name = if tbl.is_view {
                "view-reveal-symbolic"
            } else {
                "x-office-spreadsheet-symbolic"
            };
            let icon = gtk::Image::from_icon_name(icon_name);
            icon.add_css_class("dim-label");

            let label = gtk::Label::new(Some(&tbl.name));
            label.set_halign(gtk::Align::Start);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);

            row_box.append(&icon);
            row_box.append(&label);

            if tbl.is_view {
                let badge = gtk::Label::new(Some("VIEW"));
                badge.add_css_class("caption");
                badge.add_css_class("dim-label");
                row_box.append(&badge);
            }

            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&tbl.name);
            row.set_child(Some(&row_box));
            self.table_list.append(&row);

            if active_table.as_deref() == Some(tbl.name.as_str()) {
                self.table_list.select_row(Some(&row));
            }
        }
        *self.suppress_table_signal.borrow_mut() = false;
    }

    fn mysql_collations_for_charset(charset: &str) -> &'static [&'static str] {
        match charset {
            "utf8mb4" => &[
                "Default",
                "utf8mb4_unicode_ci",
                "utf8mb4_general_ci",
                "utf8mb4_0900_ai_ci",
                "utf8mb4_bin",
            ],
            "utf8" => &["Default", "utf8_unicode_ci", "utf8_general_ci", "utf8_bin"],
            "latin1" => &[
                "Default",
                "latin1_swedish_ci",
                "latin1_general_ci",
                "latin1_bin",
            ],
            "ascii" => &["Default", "ascii_general_ci", "ascii_bin"],
            "binary" => &["Default", "binary"],
            _ => &["Default"],
        }
    }

    fn open_create_database_dialog(&self, parent: &gtk::Widget) {
        let engine = *self.current_engine.borrow();
        let dialog = adw::Dialog::builder()
            .title("Create Database")
            .content_width(420)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar_view.add_top_bar(&header);

        let prefs_group = adw::PreferencesGroup::new();
        prefs_group.set_margin_top(12);
        prefs_group.set_margin_bottom(12);
        prefs_group.set_margin_start(12);
        prefs_group.set_margin_end(12);

        let name_row = adw::EntryRow::builder().title("Database Name").build();
        prefs_group.add(&name_row);

        let charset_or_enc_options = match engine {
            DbEngine::MySql => MYSQL_CHARSETS,
            DbEngine::Postgres => PG_ENCODINGS,
        };
        let charset_or_enc_model = gtk::StringList::new(charset_or_enc_options);
        let charset_or_enc_row = adw::ComboRow::builder()
            .title(match engine {
                DbEngine::MySql => "Character Set",
                DbEngine::Postgres => "Encoding",
            })
            .model(&charset_or_enc_model)
            .selected(0)
            .build();
        prefs_group.add(&charset_or_enc_row);

        let secondary_options = match engine {
            DbEngine::MySql => Self::mysql_collations_for_charset("Default"),
            DbEngine::Postgres => PG_TEMPLATES,
        };
        let secondary_model = gtk::StringList::new(secondary_options);
        let secondary_row = adw::ComboRow::builder()
            .title(match engine {
                DbEngine::MySql => "Collation",
                DbEngine::Postgres => "Template",
            })
            .model(&secondary_model)
            .selected(0)
            .sensitive(engine == DbEngine::Postgres)
            .build();
        prefs_group.add(&secondary_row);

        if engine == DbEngine::MySql {
            let charset_model = charset_or_enc_model.clone();
            let collation_model = secondary_model.clone();
            let collation_row = secondary_row.clone();
            charset_or_enc_row.connect_selected_notify(move |combo| {
                let selected_cs = charset_model
                    .string(combo.selected())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Default".to_string());
                let collations = Self::mysql_collations_for_charset(&selected_cs);
                let count = collation_model.n_items();
                collation_model.splice(0, count, collations);
                collation_row.set_selected(0);
                collation_row.set_sensitive(selected_cs != "Default");
            });
        }

        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_margin_top(12);

        let cancel_btn = gtk::Button::with_label("Cancel");
        let create_btn = gtk::Button::with_label("Create Database");
        create_btn.add_css_class("suggested-action");
        create_btn.set_sensitive(false);

        btn_box.append(&cancel_btn);
        btn_box.append(&create_btn);
        prefs_group.add(&btn_box);

        toolbar_view.set_content(Some(&prefs_group));
        dialog.set_child(Some(&toolbar_view));

        {
            let create_btn = create_btn.clone();
            name_row.connect_changed(move |entry| {
                create_btn.set_sensitive(!entry.text().trim().is_empty());
            });
        }

        {
            let create_btn = create_btn.clone();
            name_row.connect_entry_activated(move |_| {
                if create_btn.is_sensitive() {
                    create_btn.emit_clicked();
                }
            });
        }

        {
            let dialog = dialog.clone();
            cancel_btn.connect_clicked(move |_| {
                dialog.close();
            });
        }

        {
            let dialog = dialog.clone();
            let cmd_tx = self.cmd_tx.clone();
            create_btn.connect_clicked(move |_| {
                let name = name_row.text().trim().to_string();
                if name.is_empty() {
                    return;
                }

                let primary_opt = charset_or_enc_model
                    .string(charset_or_enc_row.selected())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("Default"));

                let secondary_opt = secondary_model
                    .string(secondary_row.selected())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("Default"));

                let (collation, template) = match engine {
                    DbEngine::MySql => (secondary_opt, None),
                    DbEngine::Postgres => (None, secondary_opt),
                };

                let spec = CreateDatabaseSpec {
                    name,
                    charset_or_encoding: primary_opt,
                    collation,
                    template,
                };
                let _ = cmd_tx.send_blocking(BackendCommand::CreateDatabase(spec));
                dialog.close();
            });
        }

        dialog.present(Some(parent));
    }
}
