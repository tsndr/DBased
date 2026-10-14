use crate::db::models::{BackendCommand, ForeignKeyInfo, NewForeignKey};
use crate::ui::workspace::{
    CELL_EXTRA_PADDING, DEFAULT_DATA_COL_WIDTH, HEADER_EXTRA_PADDING, MIN_COLUMN_WIDTH,
    build_resizable_header_cell, compute_auto_column_widths, create_spreadsheet_label_cell,
    measure_single_line_text,
};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const DEFAULT_RELATIONS_INDEX_COL_WIDTH: i32 = 48;
const DEFAULT_ACTION_COL_WIDTH: i32 = 76;

const FK_ACTIONS: &[&str] = &["NO ACTION", "CASCADE", "SET NULL", "RESTRICT"];

const RELATIONS_HEADERS: &[&str] = &[
    "Constraint",
    "Local Column",
    "Target Table",
    "Target Column",
    "On Update",
    "On Delete",
    "Actions",
];

#[derive(Clone)]
pub struct RelationsTab {
    pub root: gtk::Box,
    scroll: gtk::ScrolledWindow,
    pub(crate) grid: gtk::Grid,
    header_label: gtk::Label,
    add_btn: gtk::Button,
    refresh_btn: gtk::Button,
    current_db: Rc<RefCell<String>>,
    current_table: Rc<RefCell<String>>,
    local_columns: Rc<RefCell<Vec<String>>>,
    all_tables: Rc<RefCell<Vec<String>>>,
    relations: Rc<RefCell<Vec<ForeignKeyInfo>>>,
    col_widths: Rc<RefCell<HashMap<usize, i32>>>,
    natural_col_widths: Rc<RefCell<Vec<i32>>>,
    header_cells: Rc<RefCell<Vec<(usize, gtk::Box)>>>,
    last_viewport_width: Rc<RefCell<i32>>,
    index_col_width: Rc<RefCell<i32>>,
    cmd_tx: Sender<BackendCommand>,
}

impl RelationsTab {
    pub fn new(cmd_tx: Sender<BackendCommand>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_hexpand(true);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.set_margin_top(8);
        bar.set_margin_bottom(8);
        bar.set_margin_start(12);
        bar.set_margin_end(12);

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

        let grid = gtk::Grid::new();
        grid.add_css_class("spreadsheet-grid");
        grid.set_row_spacing(0);
        grid.set_column_spacing(0);
        grid.set_halign(gtk::Align::Start);
        grid.set_valign(gtk::Align::Start);

        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&grid)
            .build();

        root.append(&scroll);

        let current_db = Rc::new(RefCell::new(String::new()));
        let current_table = Rc::new(RefCell::new(String::new()));
        let local_columns = Rc::new(RefCell::new(Vec::new()));
        let all_tables = Rc::new(RefCell::new(Vec::new()));
        let relations = Rc::new(RefCell::new(Vec::new()));

        let tab = Self {
            root,
            scroll: scroll.clone(),
            grid,
            header_label,
            add_btn: add_btn.clone(),
            refresh_btn: refresh_btn.clone(),
            current_db: current_db.clone(),
            current_table: current_table.clone(),
            local_columns,
            all_tables,
            relations,
            col_widths: Rc::new(RefCell::new(HashMap::new())),
            natural_col_widths: Rc::new(RefCell::new(Vec::new())),
            header_cells: Rc::new(RefCell::new(Vec::new())),
            last_viewport_width: Rc::new(RefCell::new(0)),
            index_col_width: Rc::new(RefCell::new(DEFAULT_RELATIONS_INDEX_COL_WIDTH)),
            cmd_tx: cmd_tx.clone(),
        };

        // Recompute auto column widths when the viewport width changes
        {
            let tab_clone = tab.clone();
            scroll.hadjustment().connect_page_size_notify(move |_| {
                let tab_for_idle = tab_clone.clone();
                gtk::glib::idle_add_local_once(move || {
                    tab_for_idle.reapply_column_widths_if_viewport_changed();
                });
            });
        }
        {
            let tab_clone = tab.clone();
            scroll.connect_map(move |_| {
                let tab_for_idle = tab_clone.clone();
                gtk::glib::idle_add_local_once(move || {
                    tab_for_idle.reapply_column_widths_if_viewport_changed();
                });
            });
        }

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

        tab.render_grid();

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

    fn available_viewport_width(&self) -> i32 {
        let hadj_w = self.scroll.hadjustment().page_size().round() as i32;
        if hadj_w > 0 {
            return hadj_w;
        }
        let scroll_w = self.scroll.width();
        if scroll_w > 0 {
            return scroll_w;
        }
        let root_w = self.root.width();
        if root_w > 0 {
            return root_w;
        }
        if let Some(win) = self
            .root
            .root()
            .and_then(|r| r.downcast::<gtk::Window>().ok())
        {
            let win_w = win.width();
            if win_w > 280 {
                return win_w - 280;
            }
        }
        960
    }

    fn reapply_column_widths_if_viewport_changed(&self) {
        let viewport_w = self.available_viewport_width();
        if viewport_w != *self.last_viewport_width.borrow() {
            self.apply_column_widths();
        }
    }

    fn apply_column_widths(&self) {
        let viewport_w = self.available_viewport_width();
        *self.last_viewport_width.borrow_mut() = viewport_w;

        let header_cells = self.header_cells.borrow();
        let natural_widths = self.natural_col_widths.borrow();
        if header_cells.is_empty() || header_cells.len() != natural_widths.len() {
            return;
        }

        let actions_col_idx = RELATIONS_HEADERS.len().saturating_sub(1);
        let manual_map = self.col_widths.borrow();
        let manual_overrides: Vec<Option<i32>> = header_cells
            .iter()
            .map(|(col_idx, _)| {
                if let Some(&w) = manual_map.get(col_idx) {
                    Some(w)
                } else if *col_idx == actions_col_idx {
                    Some(DEFAULT_ACTION_COL_WIDTH)
                } else {
                    None
                }
            })
            .collect();

        let idx_w = *self.index_col_width.borrow();
        let total_avail = (viewport_w - idx_w - 2).max(0);
        let widths = compute_auto_column_widths(&natural_widths, &manual_overrides, total_avail);

        for ((_, hdr_cell), target_w) in header_cells.iter().zip(widths) {
            if hdr_cell.width_request() != target_w {
                hdr_cell.set_width_request(target_w);
            }
        }
    }

    pub fn set_relations(&self, database: &str, table: &str, relations: Vec<ForeignKeyInfo>) {
        let table_changed = *self.current_db.borrow() != database
            || *self.current_table.borrow() != table;
        if table_changed {
            self.col_widths.borrow_mut().clear();
        }
        *self.current_db.borrow_mut() = database.to_string();
        *self.current_table.borrow_mut() = table.to_string();
        *self.relations.borrow_mut() = relations;
        self.render_grid();
    }

    fn render_grid(&self) {
        let table = self.current_table.borrow().clone();
        let relations = self.relations.borrow().clone();

        if table.is_empty() {
            self.header_label
                .set_text("Select a table to manage its relations");
        } else {
            self.header_label.set_text(&format!(
                "Table '{}' — {} foreign key relation(s)",
                table,
                relations.len()
            ));
        }
        self.add_btn.set_sensitive(!table.is_empty());
        self.refresh_btn.set_sensitive(!table.is_empty());

        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        self.header_cells.borrow_mut().clear();
        self.natural_col_widths.borrow_mut().clear();

        // Header row (grid row 0)
        let corner_lbl = gtk::Label::new(Some("#"));
        corner_lbl.add_css_class("dim-label");
        corner_lbl.set_halign(gtk::Align::Fill);
        corner_lbl.set_xalign(0.5);
        corner_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        corner_lbl.set_max_width_chars(1);
        corner_lbl.set_margin_top(6);
        corner_lbl.set_margin_bottom(6);

        let idx_width = *self.index_col_width.borrow();
        let idx_width_ref = self.index_col_width.clone();
        let corner_cell = build_resizable_header_cell(
            &corner_lbl,
            idx_width,
            Some("spreadsheet-header-box"),
            move |new_w| {
                *idx_width_ref.borrow_mut() = new_w;
            },
        );
        self.grid.attach(&corner_cell, 0, 0, 1, 1);

        let mut header_labels: Vec<(gtk::Label, &'static str)> =
            Vec::with_capacity(RELATIONS_HEADERS.len());

        for (col_idx, &title) in RELATIONS_HEADERS.iter().enumerate() {
            let hdr_lbl = gtk::Label::new(Some(title));
            hdr_lbl.add_css_class("heading");
            hdr_lbl.set_halign(gtk::Align::Fill);
            hdr_lbl.set_xalign(0.0);
            hdr_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            hdr_lbl.set_max_width_chars(1);
            hdr_lbl.set_margin_top(6);
            hdr_lbl.set_margin_bottom(6);
            hdr_lbl.set_margin_start(10);
            hdr_lbl.set_margin_end(6);
            hdr_lbl.set_tooltip_text(Some(title));

            let col_w = self
                .col_widths
                .borrow()
                .get(&col_idx)
                .copied()
                .unwrap_or(if col_idx + 1 == RELATIONS_HEADERS.len() {
                    DEFAULT_ACTION_COL_WIDTH
                } else {
                    DEFAULT_DATA_COL_WIDTH
                });
            let col_widths_ref = self.col_widths.clone();
            let hdr_cell = build_resizable_header_cell(
                &hdr_lbl,
                col_w,
                Some("spreadsheet-header-box"),
                move |new_w| {
                    col_widths_ref.borrow_mut().insert(col_idx, new_w);
                },
            );

            self.grid.attach(&hdr_cell, (col_idx + 1) as i32, 0, 1, 1);
            header_labels.push((hdr_lbl, title));
            self.header_cells.borrow_mut().push((col_idx, hdr_cell));
        }

        let mut sample_lbl: Option<gtk::Label> = None;
        let mut widest_texts: Vec<String> = vec![String::new(); RELATIONS_HEADERS.len()];

        for (row_idx, fk) in relations.into_iter().enumerate() {
            let grid_row = (row_idx + 1) as i32;

            let num_lbl = gtk::Label::new(Some(&(row_idx + 1).to_string()));
            num_lbl.add_css_class("spreadsheet-row-num");
            num_lbl.add_css_class("monospace");
            num_lbl.add_css_class("caption");
            num_lbl.add_css_class("dim-label");
            num_lbl.set_halign(gtk::Align::Fill);
            num_lbl.set_xalign(0.5);
            num_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            num_lbl.set_max_width_chars(1);
            self.grid.attach(&num_lbl, 0, grid_row, 1, 1);

            let (c_cell, c_lbl) = create_spreadsheet_label_cell(
                &fk.constraint_name,
                &[],
                Some(&fk.constraint_name),
            );
            if sample_lbl.is_none() {
                sample_lbl = Some(c_lbl);
            }

            let (l_cell, _) =
                create_spreadsheet_label_cell(&fk.column_name, &[], Some(&fk.column_name));
            let (rt_cell, _) = create_spreadsheet_label_cell(
                &fk.referenced_table,
                &["accent"],
                Some(&fk.referenced_table),
            );
            let (rc_cell, _) = create_spreadsheet_label_cell(
                &fk.referenced_column,
                &[],
                Some(&fk.referenced_column),
            );
            let (upd_cell, _) =
                create_spreadsheet_label_cell(&fk.update_rule, &["dim-label"], None);
            let (del_cell, _) =
                create_spreadsheet_label_cell(&fk.delete_rule, &["dim-label"], None);

            let row_texts = [
                fk.constraint_name.as_str(),
                fk.column_name.as_str(),
                fk.referenced_table.as_str(),
                fk.referenced_column.as_str(),
                fk.update_rule.as_str(),
                fk.delete_rule.as_str(),
            ];
            for (i, txt) in row_texts.iter().enumerate() {
                if txt.chars().count() > widest_texts[i].chars().count() {
                    widest_texts[i] = (*txt).to_string();
                }
            }

            let action_cell = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            action_cell.add_css_class("spreadsheet-cell");
            action_cell.set_overflow(gtk::Overflow::Hidden);

            let drop_btn = gtk::Button::from_icon_name("user-trash-symbolic");
            drop_btn.add_css_class("flat");
            drop_btn.add_css_class("error");
            drop_btn.add_css_class("cell-action-btn");
            drop_btn.set_valign(gtk::Align::Center);
            drop_btn.set_margin_start(6);
            drop_btn.set_tooltip_text(Some("Delete Foreign Key Relation"));

            action_cell.append(&drop_btn);

            let data_cells = [
                c_cell,
                l_cell,
                rt_cell,
                rc_cell,
                upd_cell,
                del_cell,
                action_cell,
            ];
            for (col_idx, cell) in data_cells.iter().enumerate() {
                self.grid.attach(cell, (col_idx + 1) as i32, grid_row, 1, 1);
            }

            let tab_clone = self.clone();
            let cname = fk.constraint_name.clone();
            drop_btn.connect_clicked(move |btn| {
                tab_clone.confirm_drop_relation(btn.upcast_ref::<gtk::Widget>(), &cname);
            });
        }

        // Compute natural full-content width for each column
        let mut natural_widths = Vec::with_capacity(RELATIONS_HEADERS.len());
        for (col_idx, (hdr_lbl, hdr_text)) in header_labels.iter().enumerate() {
            let hdr_w = measure_single_line_text(hdr_lbl, hdr_text) + HEADER_EXTRA_PADDING;
            if col_idx + 1 == RELATIONS_HEADERS.len() {
                natural_widths.push(hdr_w.max(DEFAULT_ACTION_COL_WIDTH));
            } else {
                let sample = &widest_texts[col_idx];
                let cell_w = if sample.is_empty() {
                    0
                } else if let Some(ref lbl) = sample_lbl {
                    measure_single_line_text(lbl, sample) + CELL_EXTRA_PADDING
                } else {
                    measure_single_line_text(hdr_lbl, sample) + CELL_EXTRA_PADDING
                };
                natural_widths.push(hdr_w.max(cell_w).max(MIN_COLUMN_WIDTH));
            }
        }

        *self.natural_col_widths.borrow_mut() = natural_widths;
        self.apply_column_widths();

        let tab_for_idle = self.clone();
        gtk::glib::idle_add_local_once(move || {
            tab_for_idle.reapply_column_widths_if_viewport_changed();
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn verify_live_gtk_relations_grid_alignment() {
        use crate::ui::workspace::WorkspaceContent;
        use adw::prelude::*;

        let _ = adw::init();
        sourceview5::init();
        crate::app::load_css();

        let (cmd_tx, _cmd_rx) = async_channel::unbounded();
        let workspace_content = WorkspaceContent::new(cmd_tx, |_| {});
        workspace_content.stack.set_visible_child_name("relations");

        let fks = vec![
            ForeignKeyInfo {
                constraint_name: "fk_orders_customer_id".into(),
                column_name: "customer_id".into(),
                referenced_table: "customers".into(),
                referenced_column: "id".into(),
                update_rule: "CASCADE".into(),
                delete_rule: "RESTRICT".into(),
            },
            ForeignKeyInfo {
                constraint_name: "fk_orders_coupon_id".into(),
                column_name: "coupon_id".into(),
                referenced_table: "coupons".into(),
                referenced_column: "code".into(),
                update_rule: "CASCADE".into(),
                delete_rule: "SET NULL".into(),
            },
        ];

        workspace_content
            .relations_tab
            .set_relations("commerce_prod", "orders", fks);

        let win = adw::Window::builder()
            .title("DBased")
            .default_width(1440)
            .default_height(920)
            .content(&workspace_content.page)
            .build();
        win.present();

        let ctx = gtk::glib::MainContext::default();
        for _ in 0..60 {
            while ctx.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let grid = &workspace_content.relations_tab.grid;
        assert!(grid.has_css_class("spreadsheet-grid"));
        for col in 0..=RELATIONS_HEADERS.len() as i32 {
            let hdr = grid.child_at(col, 0).expect("relations header cell");
            assert!(hdr.has_css_class("spreadsheet-header-box"));
            let hdr_bounds = hdr.compute_bounds(grid).expect("hdr bounds");
            for row in 1..=2 {
                let cell = grid.child_at(col, row).expect("relations row cell");
                if col == 0 {
                    assert!(cell.has_css_class("spreadsheet-row-num"));
                } else {
                    assert!(cell.has_css_class("spreadsheet-cell"));
                }
                let cell_bounds = cell.compute_bounds(grid).expect("cell bounds");
                assert_eq!(hdr_bounds.x(), cell_bounds.x());
                assert_eq!(hdr_bounds.width(), cell_bounds.width());
            }
        }

        win.close();
    }
}
