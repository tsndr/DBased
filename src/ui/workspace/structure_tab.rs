use crate::db::models::{BackendCommand, ColumnAlterOp, ColumnInfo, ColumnSpec};
use crate::ui::workspace::{build_resizable_header_cell, wrap_resizable_row_cell};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;

const STRUCTURE_HEADERS: &[(&str, i32)] = &[
    ("#", 68),
    ("COLUMN NAME", 200),
    ("DATA TYPE", 160),
    ("NULLABLE", 95),
    ("KEY", 75),
    ("DEFAULT", 145),
    ("EXTRA", 140),
    ("ACTIONS", 96),
];

#[derive(Clone)]
pub struct StructureTab {
    pub root: gtk::Box,
    list_box: gtk::ListBox,
    header_label: gtk::Label,
    add_btn: gtk::Button,
    refresh_btn: gtk::Button,
    current_db: Rc<RefCell<String>>,
    current_table: Rc<RefCell<String>>,
    columns: Rc<RefCell<Vec<ColumnInfo>>>,
    col_widths: Rc<RefCell<Vec<i32>>>,
    row_cells: Rc<RefCell<Vec<Vec<gtk::Box>>>>,
    dragged_from_idx: Rc<RefCell<Option<usize>>>,
    cmd_tx: Sender<BackendCommand>,
}

impl StructureTab {
    pub fn new(cmd_tx: Sender<BackendCommand>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_hexpand(true);

        // Top toolbar
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.set_margin_top(10);
        bar.set_margin_bottom(10);
        bar.set_margin_start(16);
        bar.set_margin_end(16);

        let header_label = gtk::Label::new(Some("Select a table to inspect its columns"));
        header_label.add_css_class("heading");
        header_label.set_halign(gtk::Align::Start);
        header_label.set_hexpand(true);

        let hint_lbl = gtk::Label::new(Some("Drag rows to reorder · Drag column borders to resize"));
        hint_lbl.add_css_class("caption");
        hint_lbl.add_css_class("dim-label");

        let refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_btn.set_tooltip_text(Some("Refresh Structure"));
        refresh_btn.set_sensitive(false);

        let add_btn = gtk::Button::with_label("Add Column");
        add_btn.add_css_class("suggested-action");
        add_btn.set_sensitive(false);

        bar.append(&header_label);
        bar.append(&hint_lbl);
        bar.append(&refresh_btn);
        bar.append(&add_btn);

        root.append(&bar);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let initial_widths: Vec<i32> = STRUCTURE_HEADERS.iter().map(|(_, w)| *w).collect();
        let col_widths = Rc::new(RefCell::new(initial_widths));
        let row_cells: Rc<RefCell<Vec<Vec<gtk::Box>>>> = Rc::new(RefCell::new(Vec::new()));

        // Column header row
        let col_header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        col_header.set_margin_top(8);
        col_header.set_margin_bottom(8);
        col_header.set_margin_start(16);
        col_header.set_margin_end(16);

        for (col_idx, (title, init_w)) in STRUCTURE_HEADERS.iter().enumerate() {
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
        let columns = Rc::new(RefCell::new(Vec::new()));
        let dragged_from_idx = Rc::new(RefCell::new(None));

        let tab = Self {
            root,
            list_box,
            header_label,
            add_btn: add_btn.clone(),
            refresh_btn: refresh_btn.clone(),
            current_db: current_db.clone(),
            current_table: current_table.clone(),
            columns,
            col_widths,
            row_cells,
            dragged_from_idx,
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
                    let _ = cmd_tx.send_blocking(BackendCommand::RefreshStructure {
                        database: db,
                        table: tbl,
                    });
                }
            });
        }

        // Add Column button
        {
            let tab_clone = tab.clone();
            add_btn.connect_clicked(move |btn| {
                tab_clone.open_column_dialog(btn.upcast_ref::<gtk::Widget>(), None);
            });
        }

        tab
    }

    pub fn set_structure(&self, database: &str, table: &str, cols: Vec<ColumnInfo>) {
        *self.current_db.borrow_mut() = database.to_string();
        *self.current_table.borrow_mut() = table.to_string();
        *self.columns.borrow_mut() = cols;
        self.render_rows();
    }

    fn render_rows(&self) {
        let table = self.current_table.borrow().clone();
        let cols = self.columns.borrow().clone();

        self.header_label.set_text(&format!(
            "Table '{}' — {} column(s)",
            table,
            cols.len()
        ));
        self.add_btn.set_sensitive(!table.is_empty());
        self.refresh_btn.set_sensitive(!table.is_empty());

        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }
        self.row_cells.borrow_mut().clear();

        let widths = self.col_widths.borrow().clone();

        for (row_idx, col) in cols.into_iter().enumerate() {
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row_box.set_margin_top(8);
            row_box.set_margin_bottom(8);

            // Column 0: Drag handle + ordinal
            let ord_inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let drag_icon = gtk::Image::from_icon_name("list-drag-handle-symbolic");
            drag_icon.add_css_class("structure-drag-handle");
            drag_icon.set_cursor_from_name(Some("grab"));
            drag_icon.set_tooltip_text(Some("Drag to reorder column"));

            let ord_lbl = gtk::Label::new(Some(&(row_idx + 1).to_string()));
            ord_lbl.add_css_class("dim-label");
            ord_lbl.add_css_class("monospace");
            ord_lbl.set_halign(gtk::Align::Start);
            ord_lbl.set_xalign(0.0);
            ord_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            ord_lbl.set_max_width_chars(1);
            ord_lbl.set_hexpand(true);

            ord_inner.append(&drag_icon);
            ord_inner.append(&ord_lbl);

            let name_lbl = gtk::Label::new(Some(&col.name));
            name_lbl.add_css_class("heading");
            name_lbl.set_halign(gtk::Align::Fill);
            name_lbl.set_xalign(0.0);
            name_lbl.set_hexpand(true);
            name_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name_lbl.set_max_width_chars(1);
            name_lbl.set_tooltip_text(Some(&col.name));

            let type_lbl = gtk::Label::new(Some(&col.data_type));
            type_lbl.add_css_class("monospace");
            type_lbl.set_halign(gtk::Align::Fill);
            type_lbl.set_xalign(0.0);
            type_lbl.set_hexpand(true);
            type_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            type_lbl.set_max_width_chars(1);
            type_lbl.set_tooltip_text(Some(&col.data_type));

            let null_lbl = gtk::Label::new(Some(if col.is_nullable { "YES" } else { "NO" }));
            if !col.is_nullable {
                null_lbl.add_css_class("warning");
            } else {
                null_lbl.add_css_class("dim-label");
            }
            null_lbl.set_halign(gtk::Align::Fill);
            null_lbl.set_xalign(0.0);
            null_lbl.set_hexpand(true);
            null_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            null_lbl.set_max_width_chars(1);

            let key_lbl = gtk::Label::new(Some(if col.is_primary_key { "PRI" } else { "" }));
            if col.is_primary_key {
                key_lbl.add_css_class("accent");
                key_lbl.add_css_class("heading");
            }
            key_lbl.set_halign(gtk::Align::Fill);
            key_lbl.set_xalign(0.0);
            key_lbl.set_hexpand(true);
            key_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            key_lbl.set_max_width_chars(1);

            let def_text = col.default_value.as_deref().unwrap_or("NULL");
            let def_lbl = gtk::Label::new(Some(def_text));
            def_lbl.add_css_class("monospace");
            if col.default_value.is_none() {
                def_lbl.add_css_class("dim-label");
            }
            def_lbl.set_halign(gtk::Align::Fill);
            def_lbl.set_xalign(0.0);
            def_lbl.set_hexpand(true);
            def_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            def_lbl.set_max_width_chars(1);
            def_lbl.set_tooltip_text(Some(def_text));

            let extra_lbl = gtk::Label::new(Some(&col.extra));
            extra_lbl.add_css_class("dim-label");
            extra_lbl.set_halign(gtk::Align::Fill);
            extra_lbl.set_xalign(0.0);
            extra_lbl.set_hexpand(true);
            extra_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            extra_lbl.set_max_width_chars(1);
            if !col.extra.is_empty() {
                extra_lbl.set_tooltip_text(Some(&col.extra));
            }

            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);

            let edit_btn = gtk::Button::from_icon_name("document-edit-symbolic");
            edit_btn.add_css_class("flat");
            edit_btn.set_tooltip_text(Some("Edit Column"));

            let del_btn = gtk::Button::from_icon_name("user-trash-symbolic");
            del_btn.add_css_class("flat");
            del_btn.add_css_class("error");
            del_btn.set_tooltip_text(Some("Drop Column"));

            actions.append(&edit_btn);
            actions.append(&del_btn);

            let cells = vec![
                wrap_resizable_row_cell(&ord_inner, widths[0]),
                wrap_resizable_row_cell(&name_lbl, widths[1]),
                wrap_resizable_row_cell(&type_lbl, widths[2]),
                wrap_resizable_row_cell(&null_lbl, widths[3]),
                wrap_resizable_row_cell(&key_lbl, widths[4]),
                wrap_resizable_row_cell(&def_lbl, widths[5]),
                wrap_resizable_row_cell(&extra_lbl, widths[6]),
                wrap_resizable_row_cell(&actions, widths[7]),
            ];

            for cell in &cells {
                row_box.append(cell);
            }
            self.row_cells.borrow_mut().push(cells);

            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&row_box));
            self.list_box.append(&row);

            // DragSource for reordering database columns
            {
                let drag_source = gtk::DragSource::builder()
                    .actions(gtk::gdk::DragAction::MOVE)
                    .build();

                let dragged_from_idx = self.dragged_from_idx.clone();
                drag_source.connect_prepare(move |_, _, _| {
                    *dragged_from_idx.borrow_mut() = Some(row_idx);
                    Some(gtk::gdk::ContentProvider::for_value(
                        &(row_idx as u32).to_value(),
                    ))
                });

                let row_for_begin = row.clone();
                drag_source.connect_drag_begin(move |source, _| {
                    row_for_begin.add_css_class("structure-row-dragging");
                    let paintable = gtk::WidgetPaintable::new(Some(&row_for_begin));
                    source.set_icon(Some(&paintable), 16, 16);
                });

                let row_for_end = row.clone();
                let dragged_from_end = self.dragged_from_idx.clone();
                drag_source.connect_drag_end(move |_, _, _| {
                    row_for_end.remove_css_class("structure-row-dragging");
                    *dragged_from_end.borrow_mut() = None;
                });

                row.add_controller(drag_source);
            }

            // DropTarget for receiving reordered column
            {
                let drop_target = gtk::DropTarget::new(
                    gtk::glib::types::Type::U32,
                    gtk::gdk::DragAction::MOVE,
                );

                let row_for_enter = row.clone();
                let dragged_from_enter = self.dragged_from_idx.clone();
                drop_target.connect_enter(move |_, _, _| {
                    if let Some(from) = *dragged_from_enter.borrow() {
                        if from > row_idx {
                            row_for_enter.add_css_class("structure-drop-above");
                            row_for_enter.remove_css_class("structure-drop-below");
                        } else if from < row_idx {
                            row_for_enter.add_css_class("structure-drop-below");
                            row_for_enter.remove_css_class("structure-drop-above");
                        }
                    }
                    gtk::gdk::DragAction::MOVE
                });

                let row_for_leave = row.clone();
                drop_target.connect_leave(move |_| {
                    row_for_leave.remove_css_class("structure-drop-above");
                    row_for_leave.remove_css_class("structure-drop-below");
                });

                let tab_clone = self.clone();
                let row_for_drop = row.clone();
                drop_target.connect_drop(move |_, val, _, _| {
                    row_for_drop.remove_css_class("structure-drop-above");
                    row_for_drop.remove_css_class("structure-drop-below");
                    if let Ok(from_u32) = val.get::<u32>() {
                        let from_idx = from_u32 as usize;
                        if from_idx != row_idx {
                            tab_clone.reorder_column(from_idx, row_idx);
                            return true;
                        }
                    }
                    false
                });

                row.add_controller(drop_target);
            }

            // Edit column handler
            {
                let tab_clone = self.clone();
                let col_clone = col.clone();
                edit_btn.connect_clicked(move |btn| {
                    tab_clone.open_column_dialog(
                        btn.upcast_ref::<gtk::Widget>(),
                        Some(col_clone.clone()),
                    );
                });
            }

            // Delete column handler
            {
                let tab_clone = self.clone();
                let col_name = col.name.clone();
                del_btn.connect_clicked(move |btn| {
                    tab_clone.confirm_drop_column(btn.upcast_ref::<gtk::Widget>(), &col_name);
                });
            }
        }
    }

    fn reorder_column(&self, from_idx: usize, to_idx: usize) {
        let (moved_col, after_column, new_order) = {
            let mut cols = self.columns.borrow_mut();
            if from_idx >= cols.len() || to_idx >= cols.len() || from_idx == to_idx {
                return;
            }
            let moved = cols.remove(from_idx);
            cols.insert(to_idx, moved.clone());
            for (idx, c) in cols.iter_mut().enumerate() {
                c.ordinal = (idx + 1) as u32;
            }
            let after_col = if to_idx == 0 {
                None
            } else {
                cols.get(to_idx - 1).map(|c| c.name.clone())
            };
            let order: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
            (moved, after_col, order)
        };

        self.render_rows();

        let db = self.current_db.borrow().clone();
        let table = self.current_table.borrow().clone();
        if !table.is_empty() {
            let _ = self.cmd_tx.send_blocking(BackendCommand::AlterColumn {
                database: db,
                table,
                op: ColumnAlterOp::Reorder {
                    column: moved_col,
                    after_column,
                    new_order,
                },
            });
        }
    }

    fn open_column_dialog(&self, parent: &gtk::Widget, existing: Option<ColumnInfo>) {
        let is_edit = existing.is_some();
        let dialog = adw::Dialog::builder()
            .title(if is_edit { "Edit Column" } else { "Add Column" })
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

        let name_row = adw::EntryRow::builder()
            .title("Column Name")
            .text(
                existing
                    .as_ref()
                    .map(|c| c.name.as_str())
                    .unwrap_or("new_column"),
            )
            .build();

        let type_row = adw::EntryRow::builder()
            .title("Data Type (e.g. VARCHAR(255), INT, TEXT)")
            .text(
                existing
                    .as_ref()
                    .map(|c| c.data_type.as_str())
                    .unwrap_or("VARCHAR(255)"),
            )
            .build();

        let nullable_row = adw::SwitchRow::builder()
            .title("Nullable")
            .active(existing.as_ref().map(|c| c.is_nullable).unwrap_or(true))
            .build();

        let default_row = adw::EntryRow::builder()
            .title("Default Value (leave empty for none)")
            .text(
                existing
                    .as_ref()
                    .and_then(|c| c.default_value.as_deref())
                    .unwrap_or(""),
            )
            .build();

        let pk_row = adw::SwitchRow::builder()
            .title("Primary Key")
            .active(existing.as_ref().map(|c| c.is_primary_key).unwrap_or(false))
            .visible(!is_edit)
            .build();

        prefs_group.add(&name_row);
        prefs_group.add(&type_row);
        prefs_group.add(&nullable_row);
        prefs_group.add(&default_row);
        prefs_group.add(&pk_row);

        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_margin_top(12);

        let cancel_btn = gtk::Button::with_label("Cancel");
        let save_btn = gtk::Button::with_label(if is_edit { "Apply Changes" } else { "Add Column" });
        save_btn.add_css_class("suggested-action");

        btn_box.append(&cancel_btn);
        btn_box.append(&save_btn);
        prefs_group.add(&btn_box);

        toolbar_view.set_content(Some(&prefs_group));
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
            let old_col = existing;
            save_btn.connect_clicked(move |_| {
                let col_name = name_row.text().trim().to_string();
                let data_type = type_row.text().trim().to_string();
                if col_name.is_empty() || data_type.is_empty() {
                    return;
                }
                let def_txt = default_row.text().trim().to_string();
                let spec = ColumnSpec {
                    name: col_name,
                    data_type,
                    is_nullable: nullable_row.is_active(),
                    default_value: if def_txt.is_empty() {
                        None
                    } else {
                        Some(def_txt)
                    },
                    is_primary_key: pk_row.is_active(),
                };
                let op = match &old_col {
                    Some(prev) => ColumnAlterOp::Modify {
                        old_name: prev.name.clone(),
                        spec,
                    },
                    None => ColumnAlterOp::Add(spec),
                };
                let _ = cmd_tx.send_blocking(BackendCommand::AlterColumn {
                    database: db.clone(),
                    table: table.clone(),
                    op,
                });
                dialog.close();
            });
        }

        dialog.present(Some(parent));
    }

    fn confirm_drop_column(&self, parent: &gtk::Widget, col_name: &str) {
        let table = self.current_table.borrow().clone();
        let db = self.current_db.borrow().clone();
        let alert = adw::AlertDialog::builder()
            .heading(format!("Drop Column '{}'?", col_name))
            .body(format!(
                "This will permanently execute ALTER TABLE {} DROP COLUMN {} and delete all data in this column.",
                table, col_name
            ))
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("drop", "Drop Column");
        alert.set_response_appearance("drop", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let cmd_tx = self.cmd_tx.clone();
        let col_str = col_name.to_string();
        alert.connect_response(None, move |_, resp| {
            if resp == "drop" {
                let _ = cmd_tx.send_blocking(BackendCommand::AlterColumn {
                    database: db.clone(),
                    table: table.clone(),
                    op: ColumnAlterOp::Drop {
                        name: col_str.clone(),
                    },
                });
            }
        });

        alert.present(Some(parent));
    }
}
