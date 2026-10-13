use crate::db::models::{BackendCommand, ContentQuery, TableInfo};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct WorkspaceSidebar {
    pub page: adw::NavigationPage,
    window_title: adw::WindowTitle,
    db_dropdown: gtk::DropDown,
    db_model: gtk::StringList,
    table_search: gtk::SearchEntry,
    table_list: gtk::ListBox,
    all_tables: Rc<RefCell<Vec<TableInfo>>>,
    selected_db: Rc<RefCell<String>>,
    selected_table: Rc<RefCell<Option<String>>>,
    suppress_db_signal: Rc<RefCell<bool>>,
    suppress_table_signal: Rc<RefCell<bool>>,
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

        let refresh_tables_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_tables_btn.set_tooltip_text(Some("Refresh Tables"));
        header.pack_end(&refresh_tables_btn);

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

        let db_model = gtk::StringList::new(&[]);
        let db_dropdown = gtk::DropDown::new(Some(db_model.clone()), gtk::Expression::NONE);
        db_dropdown.set_hexpand(true);
        db_dropdown.set_tooltip_text(Some("Select Database"));

        let tables_label = gtk::Label::new(Some("TABLES"));
        tables_label.add_css_class("caption-heading");
        tables_label.add_css_class("dim-label");
        tables_label.set_halign(gtk::Align::Start);
        tables_label.set_margin_top(4);

        let table_search = gtk::SearchEntry::new();
        table_search.set_placeholder_text(Some("Filter tables..."));

        top_controls.append(&db_label);
        top_controls.append(&db_dropdown);
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

        // Refresh tables button handler
        {
            let cmd_tx = cmd_tx.clone();
            let selected_db = selected_db.clone();
            refresh_tables_btn.connect_clicked(move |_| {
                let db = selected_db.borrow().clone();
                if !db.is_empty() {
                    let _ = cmd_tx.send_blocking(BackendCommand::SelectDatabase(db));
                }
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
            selected_db: selected_db.clone(),
            selected_table: selected_table.clone(),
            suppress_db_signal,
            suppress_table_signal: suppress_table_signal.clone(),
        };

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
        engine_label: &str,
        databases: &[String],
        initial_db: Option<&str>,
        tables: Vec<TableInfo>,
    ) {
        self.window_title.set_title(connection_name);
        self.window_title.set_subtitle(engine_label);

        *self.suppress_db_signal.borrow_mut() = true;
        let count = self.db_model.n_items();
        let str_refs: Vec<&str> = databases.iter().map(|s| s.as_str()).collect();
        self.db_model.splice(0, count, &str_refs);

        let mut chosen_idx = 0u32;
        if let Some(target) = initial_db {
            if let Some(pos) = databases.iter().position(|d| d == target) {
                chosen_idx = pos as u32;
                *self.selected_db.borrow_mut() = target.to_string();
            }
        } else if let Some(first) = databases.first() {
            *self.selected_db.borrow_mut() = first.clone();
        }
        if !databases.is_empty() {
            self.db_dropdown.set_selected(chosen_idx);
        }
        *self.suppress_db_signal.borrow_mut() = false;

        self.set_tables(tables);
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
}
