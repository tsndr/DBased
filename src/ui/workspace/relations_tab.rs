use crate::db::models::{BackendCommand, ForeignKeyInfo, NewForeignKey};
use crate::ui::workspace::{build_resizable_header_cell, wrap_resizable_row_cell};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;

const FK_ACTIONS: &[&str] = &["NO ACTION", "CASCADE", "SET NULL", "RESTRICT"];

const RELATIONS_HEADERS: &[(&str, i32)] = &[
    ("CONSTRAINT", 200),
    ("LOCAL COLUMN", 150),
    ("TARGET TABLE", 160),
    ("TARGET COLUMN", 150),
    ("ON UPDATE", 115),
    ("ON DELETE", 115),
    ("ACTION", 70),
];

#[derive(Clone)]
pub struct RelationsTab {
    pub root: gtk::Box,
    list_box: gtk::ListBox,
    header_label: gtk::Label,
    add_btn: gtk::Button,
    refresh_btn: gtk::Button,
    current_db: Rc<RefCell<String>>,
    current_table: Rc<RefCell<String>>,
    local_columns: Rc<RefCell<Vec<String>>>,
    all_tables: Rc<RefCell<Vec<String>>>,
    col_widths: Rc<RefCell<Vec<i32>>>,
    row_cells: Rc<RefCell<Vec<Vec<gtk::Box>>>>,
    cmd_tx: Sender<BackendCommand>,
}

impl RelationsTab {
    pub fn new(cmd_tx: Sender<BackendCommand>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_hexpand(true);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.set_margin_top(10);
        bar.set_margin_bottom(10);
        bar.set_margin_start(16);
        bar.set_margin_end(16);

        let header_label = gtk::Label::new(Some("Select a table to manage its relations"));
        header_label.add_css_class("heading");
        header_label.set_halign(gtk::Align::Start);
        header_label.set_hexpand(true);

        let hint_lbl = gtk::Label::new(Some("Drag column borders to resize"));
        hint_lbl.add_css_class("caption");
        hint_lbl.add_css_class("dim-label");

        let refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_btn.set_tooltip_text(Some("Refresh Relations"));
        refresh_btn.set_sensitive(false);

        let add_btn = gtk::Button::with_label("Add Relation");
        add_btn.add_css_class("suggested-action");
        add_btn.set_sensitive(false);

        bar.append(&header_label);
        bar.append(&hint_lbl);
        bar.append(&refresh_btn);
        bar.append(&add_btn);

        root.append(&bar);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let initial_widths: Vec<i32> = RELATIONS_HEADERS.iter().map(|(_, w)| *w).collect();
        let col_widths = Rc::new(RefCell::new(initial_widths));
        let row_cells: Rc<RefCell<Vec<Vec<gtk::Box>>>> = Rc::new(RefCell::new(Vec::new()));

        let col_header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        col_header.set_margin_top(8);
        col_header.set_margin_bottom(8);
        col_header.set_margin_start(16);
        col_header.set_margin_end(16);

        for (col_idx, (title, init_w)) in RELATIONS_HEADERS.iter().enumerate() {
            let lbl = gtk::Label::new(Some(title));
            lbl.add_css_class("caption-heading");
            lbl.add_css_class("dim-label");
            lbl.set_halign(gtk::Align::Fill);
            lbl.set_xalign(0.0);
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            lbl.set_max_width_chars(1);
            lbl.set_margin_start(8);
            lbl.set_margin_end(4);

            let col_widths_ref = col_widths.clone();
            let row_cells_ref = row_cells.clone();
            let hdr_cell = build_resizable_header_cell(
                &lbl,
                *init_w,
                Some("table-header-cell"),
                move |new_w| {
                    if let Some(w) = col_widths_ref.borrow_mut().get_mut(col_idx) {
                        *w = new_w;
                    }
                    for row in row_cells_ref.borrow().iter() {
                        if let Some(cell) = row.get(col_idx) {
                            cell.set_width_request(new_w);
                        }
                    }
                },
            );
            col_header.append(&hdr_cell);
        }

        let list_box = gtk::ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::None);
        list_box.add_css_class("boxed-list");
        list_box.add_css_class("resizable-boxed-list");
        list_box.set_halign(gtk::Align::Start);
        list_box.set_margin_top(12);
        list_box.set_margin_bottom(16);
        list_box.set_margin_start(16);
        list_box.set_margin_end(16);

        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&list_box)
            .build();

        let header_scroll = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(false)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hadjustment(&scroll.hadjustment())
            .child(&col_header)
            .build();

        root.append(&header_scroll);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&scroll);

        let current_db = Rc::new(RefCell::new(String::new()));
        let current_table = Rc::new(RefCell::new(String::new()));
        let local_columns = Rc::new(RefCell::new(Vec::new()));
        let all_tables = Rc::new(RefCell::new(Vec::new()));

        let tab = Self {
            root,
            list_box,
            header_label,
            add_btn: add_btn.clone(),
            refresh_btn: refresh_btn.clone(),
            current_db: current_db.clone(),
            current_table: current_table.clone(),
            local_columns,
            all_tables,
            col_widths,
            row_cells,
            cmd_tx: cmd_tx.clone(),
        };

        // Refresh button
        {
            let cmd_tx = cmd_tx.clone();
            let current_db = current_db.clone();
            let current_table = current_table.clone();
            refresh_btn.connect_clicked(move |_| {
                let db = current_db.borrow().clone();
                let tbl = current_table.borrow().clone();
                if !tbl.is_empty() {
                    let _ = cmd_tx.send_blocking(BackendCommand::RefreshRelations {
                        database: db,
                        table: tbl,
                    });
                }
            });
        }

        // Add Relation button
        {
            let tab_clone = tab.clone();
            add_btn.connect_clicked(move |btn| {
                tab_clone.open_add_relation_dialog(btn.upcast_ref::<gtk::Widget>());
            });
        }

        tab
    }

    pub fn set_context(
        &self,
        database: &str,
        table: &str,
        local_cols: Vec<String>,
        tables: Vec<String>,
    ) {
        *self.current_db.borrow_mut() = database.to_string();
        *self.current_table.borrow_mut() = table.to_string();
        *self.local_columns.borrow_mut() = local_cols;
        *self.all_tables.borrow_mut() = tables;
    }

    pub fn set_relations(&self, database: &str, table: &str, relations: Vec<ForeignKeyInfo>) {
        *self.current_db.borrow_mut() = database.to_string();
        *self.current_table.borrow_mut() = table.to_string();
        self.header_label.set_text(&format!(
            "Table '{}' — {} foreign key relation(s)",
            table,
            relations.len()
        ));
        self.add_btn.set_sensitive(!table.is_empty());
        self.refresh_btn.set_sensitive(!table.is_empty());

        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }
        self.row_cells.borrow_mut().clear();

        if relations.is_empty() {
            let empty_row = adw::ActionRow::builder()
                .title("No foreign key relations defined on this table")
                .subtitle("Click 'Add Relation' to create a foreign key constraint")
                .build();
            self.list_box.append(&empty_row);
            return;
        }

        let widths = self.col_widths.borrow().clone();

        for fk in relations {
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row_box.set_margin_top(8);
            row_box.set_margin_bottom(8);

            let c_lbl = gtk::Label::new(Some(&fk.constraint_name));
            c_lbl.add_css_class("heading");
            c_lbl.set_halign(gtk::Align::Fill);
            c_lbl.set_xalign(0.0);
            c_lbl.set_hexpand(true);
            c_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            c_lbl.set_max_width_chars(1);
            c_lbl.set_tooltip_text(Some(&fk.constraint_name));

            let l_lbl = gtk::Label::new(Some(&fk.column_name));
            l_lbl.add_css_class("monospace");
            l_lbl.set_halign(gtk::Align::Fill);
            l_lbl.set_xalign(0.0);
            l_lbl.set_hexpand(true);
            l_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l_lbl.set_max_width_chars(1);
            l_lbl.set_tooltip_text(Some(&fk.column_name));

            let rt_lbl = gtk::Label::new(Some(&fk.referenced_table));
            rt_lbl.add_css_class("accent");
            rt_lbl.set_halign(gtk::Align::Fill);
            rt_lbl.set_xalign(0.0);
            rt_lbl.set_hexpand(true);
            rt_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            rt_lbl.set_max_width_chars(1);
            rt_lbl.set_tooltip_text(Some(&fk.referenced_table));

            let rc_lbl = gtk::Label::new(Some(&fk.referenced_column));
            rc_lbl.add_css_class("monospace");
            rc_lbl.set_halign(gtk::Align::Fill);
            rc_lbl.set_xalign(0.0);
            rc_lbl.set_hexpand(true);
            rc_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            rc_lbl.set_max_width_chars(1);
            rc_lbl.set_tooltip_text(Some(&fk.referenced_column));

            let upd_lbl = gtk::Label::new(Some(&fk.update_rule));
            upd_lbl.add_css_class("dim-label");
            upd_lbl.set_halign(gtk::Align::Fill);
            upd_lbl.set_xalign(0.0);
            upd_lbl.set_hexpand(true);
            upd_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            upd_lbl.set_max_width_chars(1);

            let del_lbl = gtk::Label::new(Some(&fk.delete_rule));
            del_lbl.add_css_class("dim-label");
            del_lbl.set_halign(gtk::Align::Fill);
            del_lbl.set_xalign(0.0);
            del_lbl.set_hexpand(true);
            del_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            del_lbl.set_max_width_chars(1);

            let drop_btn = gtk::Button::from_icon_name("user-trash-symbolic");
            drop_btn.add_css_class("flat");
            drop_btn.add_css_class("error");
            drop_btn.set_tooltip_text(Some("Delete Foreign Key Relation"));

            let cells = vec![
                wrap_resizable_row_cell(&c_lbl, widths[0]),
                wrap_resizable_row_cell(&l_lbl, widths[1]),
                wrap_resizable_row_cell(&rt_lbl, widths[2]),
                wrap_resizable_row_cell(&rc_lbl, widths[3]),
                wrap_resizable_row_cell(&upd_lbl, widths[4]),
                wrap_resizable_row_cell(&del_lbl, widths[5]),
                wrap_resizable_row_cell(&drop_btn, widths[6]),
            ];

            for cell in &cells {
                row_box.append(cell);
            }
            self.row_cells.borrow_mut().push(cells);

            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&row_box));
            self.list_box.append(&row);

            let tab_clone = self.clone();
            let cname = fk.constraint_name.clone();
            drop_btn.connect_clicked(move |btn| {
                tab_clone.confirm_drop_relation(btn.upcast_ref::<gtk::Widget>(), &cname);
            });
        }
    }

    fn open_add_relation_dialog(&self, parent: &gtk::Widget) {
        let dialog = adw::Dialog::builder()
            .title("Add Foreign Key Relation")
            .content_width(440)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar_view.add_top_bar(&header);

        let group = adw::PreferencesGroup::new();
        group.set_margin_top(12);
        group.set_margin_bottom(12);
        group.set_margin_start(12);
        group.set_margin_end(12);

        let table = self.current_table.borrow().clone();
        let constraint_row = adw::EntryRow::builder()
            .title("Constraint Name (optional)")
            .text(format!("fk_{}_ref", table))
            .build();

        let local_cols = self.local_columns.borrow().clone();
        let local_col_refs: Vec<&str> = local_cols.iter().map(|s| s.as_str()).collect();
        let local_col_model = gtk::StringList::new(&local_col_refs);
        let local_col_row = adw::ComboRow::builder()
            .title("Local Column")
            .model(&local_col_model)
            .build();

        let tables = self.all_tables.borrow().clone();
        let table_refs: Vec<&str> = tables.iter().map(|s| s.as_str()).collect();
        let target_table_model = gtk::StringList::new(&table_refs);
        let target_table_row = adw::ComboRow::builder()
            .title("Referenced Table")
            .model(&target_table_model)
            .build();

        let target_col_row = adw::EntryRow::builder()
            .title("Referenced Column")
            .text("id")
            .build();

        let on_delete_model = gtk::StringList::new(FK_ACTIONS);
        let on_delete_row = adw::ComboRow::builder()
            .title("ON DELETE")
            .model(&on_delete_model)
            .selected(0)
            .build();

        let on_update_model = gtk::StringList::new(FK_ACTIONS);
        let on_update_row = adw::ComboRow::builder()
            .title("ON UPDATE")
            .model(&on_update_model)
            .selected(0)
            .build();

        group.add(&constraint_row);
        group.add(&local_col_row);
        group.add(&target_table_row);
        group.add(&target_col_row);
        group.add(&on_delete_row);
        group.add(&on_update_row);

        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_margin_top(12);

        let cancel_btn = gtk::Button::with_label("Cancel");
        let add_btn = gtk::Button::with_label("Create Relation");
        add_btn.add_css_class("suggested-action");

        btn_box.append(&cancel_btn);
        btn_box.append(&add_btn);
        group.add(&btn_box);

        toolbar_view.set_content(Some(&group));
        dialog.set_child(Some(&toolbar_view));

        {
            let dialog = dialog.clone();
            cancel_btn.connect_clicked(move |_| {
                dialog.close();
            });
        }

        {
            let dialog = dialog.clone();
            let cmd_tx = self.cmd_tx.clone();
            let db = self.current_db.borrow().clone();
            let table = self.current_table.borrow().clone();
            add_btn.connect_clicked(move |_| {
                let local_col = local_col_model
                    .string(local_col_row.selected())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let target_tbl = target_table_model
                    .string(target_table_row.selected())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let target_col = target_col_row.text().trim().to_string();

                if local_col.is_empty() || target_tbl.is_empty() || target_col.is_empty() {
                    return;
                }

                let on_delete = FK_ACTIONS
                    .get(on_delete_row.selected() as usize)
                    .copied()
                    .unwrap_or("NO ACTION")
                    .to_string();
                let on_update = FK_ACTIONS
                    .get(on_update_row.selected() as usize)
                    .copied()
                    .unwrap_or("NO ACTION")
                    .to_string();

                let fk = NewForeignKey {
                    constraint_name: constraint_row.text().trim().to_string(),
                    column_name: local_col,
                    referenced_table: target_tbl,
                    referenced_column: target_col,
                    on_update,
                    on_delete,
                };
                let _ = cmd_tx.send_blocking(BackendCommand::AddRelation {
                    database: db.clone(),
                    table: table.clone(),
                    fk,
                });
                dialog.close();
            });
        }

        dialog.present(Some(parent));
    }

    fn confirm_drop_relation(&self, parent: &gtk::Widget, constraint_name: &str) {
        let table = self.current_table.borrow().clone();
        let db = self.current_db.borrow().clone();
        let alert = adw::AlertDialog::builder()
            .heading(format!("Drop Foreign Key '{}'?", constraint_name))
            .body(format!(
                "Are you sure you want to remove the foreign key constraint '{}' from table '{}'?",
                constraint_name, table
            ))
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("drop", "Drop Constraint");
        alert.set_response_appearance("drop", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let cmd_tx = self.cmd_tx.clone();
        let cname = constraint_name.to_string();
        alert.connect_response(None, move |_, resp| {
            if resp == "drop" {
                let _ = cmd_tx.send_blocking(BackendCommand::DropRelation {
                    database: db.clone(),
                    table: table.clone(),
                    constraint_name: cname.clone(),
                });
            }
        });

        alert.present(Some(parent));
    }
}
