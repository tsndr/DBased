use crate::config::bookmarks::DbEngine;
use crate::db::models::{
    BackendCommand, ColumnAlterOp, ColumnInfo, ColumnSpec, IndexAlterOp, IndexInfo, IndexSpec,
};
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

const INDEX_HEADERS: &[(&str, i32)] = &[
    ("KEY NAME", 220),
    ("TYPE", 120),
    ("COLUMNS", 260),
    ("UNIQUE", 95),
    ("METHOD", 110),
    ("ACTIONS", 96),
];

const INDEX_TYPES: &[&str] = &["INDEX", "UNIQUE", "PRIMARY", "FULLTEXT", "SPATIAL"];
const INDEX_METHODS: &[&str] = &["Default", "BTREE", "HASH", "GIN", "GIST", "BRIN"];

const MYSQL_COLUMN_DATA_TYPES: &[&str] = &[
    "VARCHAR",
    "CHAR",
    "TEXT",
    "MEDIUMTEXT",
    "LONGTEXT",
    "INT",
    "BIGINT",
    "SMALLINT",
    "TINYINT",
    "BOOLEAN",
    "DECIMAL",
    "NUMERIC",
    "FLOAT",
    "DOUBLE",
    "REAL",
    "DATE",
    "TIME",
    "DATETIME",
    "TIMESTAMP",
    "JSON",
    "ENUM",
    "SET",
    "BINARY",
    "VARBINARY",
    "BIT",
    "BLOB",
];

const POSTGRES_COLUMN_DATA_TYPES: &[&str] = &[
    "VARCHAR",
    "CHAR",
    "TEXT",
    "INT",
    "BIGINT",
    "SMALLINT",
    "SERIAL",
    "BIGSERIAL",
    "BOOLEAN",
    "DECIMAL",
    "NUMERIC",
    "FLOAT",
    "DOUBLE PRECISION",
    "REAL",
    "DATE",
    "TIME",
    "TIMESTAMP",
    "TIMESTAMPTZ",
    "UUID",
    "JSON",
    "JSONB",
    "BIT",
    "VARBIT",
    "BYTEA",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum TypeParamKind {
    Length,
    PrecisionScale,
    FloatPrecision,
    TimePrecision,
    ValueList,
}

#[derive(Clone, Copy)]
struct TypeParamSpec {
    title: &'static str,
    default_value: &'static str,
    required: bool,
    kind: TypeParamKind,
}

#[derive(Clone)]
pub struct StructureTab {
    pub root: gtk::Box,
    list_box: gtk::ListBox,
    header_label: gtk::Label,
    add_btn: gtk::Button,
    refresh_btn: gtk::Button,
    idx_list_box: gtk::ListBox,
    idx_header_label: gtk::Label,
    add_idx_btn: gtk::Button,
    idx_refresh_btn: gtk::Button,
    current_engine: Rc<RefCell<DbEngine>>,
    current_db: Rc<RefCell<String>>,
    current_table: Rc<RefCell<String>>,
    columns: Rc<RefCell<Vec<ColumnInfo>>>,
    indexes: Rc<RefCell<Vec<IndexInfo>>>,
    col_widths: Rc<RefCell<Vec<i32>>>,
    row_cells: Rc<RefCell<Vec<Vec<gtk::Box>>>>,
    idx_col_widths: Rc<RefCell<Vec<i32>>>,
    idx_row_cells: Rc<RefCell<Vec<Vec<gtk::Box>>>>,
    dragged_from_idx: Rc<RefCell<Option<usize>>>,
    cmd_tx: Sender<BackendCommand>,
}

impl StructureTab {
    pub fn new(cmd_tx: Sender<BackendCommand>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_hexpand(true);

        let paned = gtk::Paned::new(gtk::Orientation::Vertical);
        paned.set_position(390);
        paned.set_vexpand(true);
        paned.set_hexpand(true);
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);

        // ==========================================
        // Top Pane: Columns
        // ==========================================
        let top_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        top_box.set_vexpand(true);
        top_box.set_hexpand(true);

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

        top_box.append(&bar);
        top_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

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

        top_box.append(&header_scroll);
        top_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        top_box.append(&scroll);

        // ==========================================
        // Bottom Pane: Indexes (Sequel Ace style)
        // ==========================================
        let bottom_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bottom_box.set_vexpand(true);
        bottom_box.set_hexpand(true);

        let idx_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        idx_bar.set_margin_top(8);
        idx_bar.set_margin_bottom(8);
        idx_bar.set_margin_start(16);
        idx_bar.set_margin_end(16);

        let idx_header_label = gtk::Label::new(Some("Indexes"));
        idx_header_label.add_css_class("heading");
        idx_header_label.set_halign(gtk::Align::Start);
        idx_header_label.set_hexpand(true);

        let idx_hint_lbl = gtk::Label::new(Some("Drag column borders to resize"));
        idx_hint_lbl.add_css_class("caption");
        idx_hint_lbl.add_css_class("dim-label");

        let idx_refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        idx_refresh_btn.set_tooltip_text(Some("Refresh Indexes"));
        idx_refresh_btn.set_sensitive(false);

        let add_idx_btn = gtk::Button::with_label("Add Index");
        add_idx_btn.add_css_class("suggested-action");
        add_idx_btn.set_sensitive(false);

        idx_bar.append(&idx_header_label);
        idx_bar.append(&idx_hint_lbl);
        idx_bar.append(&idx_refresh_btn);
        idx_bar.append(&add_idx_btn);

        bottom_box.append(&idx_bar);
        bottom_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let init_idx_widths: Vec<i32> = INDEX_HEADERS.iter().map(|(_, w)| *w).collect();
        let idx_col_widths = Rc::new(RefCell::new(init_idx_widths));
        let idx_row_cells: Rc<RefCell<Vec<Vec<gtk::Box>>>> = Rc::new(RefCell::new(Vec::new()));

        let idx_col_header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        idx_col_header.set_margin_top(8);
        idx_col_header.set_margin_bottom(8);
        idx_col_header.set_margin_start(16);
        idx_col_header.set_margin_end(16);

        for (col_idx, (title, init_w)) in INDEX_HEADERS.iter().enumerate() {
            let lbl = gtk::Label::new(Some(title));
            lbl.add_css_class("caption-heading");
            lbl.add_css_class("dim-label");
            lbl.set_halign(gtk::Align::Fill);
            lbl.set_xalign(0.0);
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            lbl.set_max_width_chars(1);
            lbl.set_margin_start(8);
            lbl.set_margin_end(4);

            let idx_col_widths_ref = idx_col_widths.clone();
            let idx_row_cells_ref = idx_row_cells.clone();
            let hdr_cell = build_resizable_header_cell(
                &lbl,
                *init_w,
                Some("table-header-cell"),
                move |new_w| {
                    if let Some(w) = idx_col_widths_ref.borrow_mut().get_mut(col_idx) {
                        *w = new_w;
                    }
                    for row in idx_row_cells_ref.borrow().iter() {
                        if let Some(cell) = row.get(col_idx) {
                            cell.set_width_request(new_w);
                        }
                    }
                },
            );
            idx_col_header.append(&hdr_cell);
        }

        let idx_list_box = gtk::ListBox::new();
        idx_list_box.set_selection_mode(gtk::SelectionMode::None);
        idx_list_box.add_css_class("boxed-list");
        idx_list_box.add_css_class("resizable-boxed-list");
        idx_list_box.set_halign(gtk::Align::Start);
        idx_list_box.set_margin_top(12);
        idx_list_box.set_margin_bottom(16);
        idx_list_box.set_margin_start(16);
        idx_list_box.set_margin_end(16);

        let idx_scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&idx_list_box)
            .build();

        let idx_header_scroll = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(false)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hadjustment(&idx_scroll.hadjustment())
            .child(&idx_col_header)
            .build();

        bottom_box.append(&idx_header_scroll);
        bottom_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        bottom_box.append(&idx_scroll);

        paned.set_start_child(Some(&top_box));
        paned.set_end_child(Some(&bottom_box));
        root.append(&paned);

        let current_engine = Rc::new(RefCell::new(DbEngine::Postgres));
        let current_db = Rc::new(RefCell::new(String::new()));
        let current_table = Rc::new(RefCell::new(String::new()));
        let columns = Rc::new(RefCell::new(Vec::new()));
        let indexes = Rc::new(RefCell::new(Vec::new()));
        let dragged_from_idx = Rc::new(RefCell::new(None));

        let tab = Self {
            root,
            list_box,
            header_label,
            add_btn: add_btn.clone(),
            refresh_btn: refresh_btn.clone(),
            idx_list_box,
            idx_header_label,
            add_idx_btn: add_idx_btn.clone(),
            idx_refresh_btn: idx_refresh_btn.clone(),
            current_engine,
            current_db: current_db.clone(),
            current_table: current_table.clone(),
            columns,
            indexes,
            col_widths,
            row_cells,
            idx_col_widths,
            idx_row_cells,
            dragged_from_idx,
            cmd_tx: cmd_tx.clone(),
        };

        // Refresh button (top)
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

        // Refresh button (indexes panel)
        {
            let cmd_tx = cmd_tx.clone();
            let current_db = current_db.clone();
            let current_table = current_table.clone();
            idx_refresh_btn.connect_clicked(move |_| {
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

        // Add Index button
        {
            let tab_clone = tab.clone();
            add_idx_btn.connect_clicked(move |btn| {
                tab_clone.open_index_dialog(btn.upcast_ref::<gtk::Widget>(), None);
            });
        }

        tab
    }

    pub fn set_engine(&self, engine: DbEngine) {
        *self.current_engine.borrow_mut() = engine;
    }

    pub fn column_data_types_for_engine(engine: DbEngine) -> &'static [&'static str] {
        match engine {
            DbEngine::MySql => MYSQL_COLUMN_DATA_TYPES,
            DbEngine::Postgres => POSTGRES_COLUMN_DATA_TYPES,
        }
    }

    pub fn set_structure(
        &self,
        database: &str,
        table: &str,
        cols: Vec<ColumnInfo>,
        idxs: Vec<IndexInfo>,
    ) {
        *self.current_db.borrow_mut() = database.to_string();
        *self.current_table.borrow_mut() = table.to_string();
        *self.columns.borrow_mut() = cols;
        *self.indexes.borrow_mut() = idxs;
        self.render_rows();
        self.render_indexes();
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

    fn param_spec_for_type(base_type: &str) -> Option<TypeParamSpec> {
        match base_type.trim().to_ascii_uppercase().as_str() {
            "VARCHAR" => Some(TypeParamSpec {
                title: "Length (e.g. 255)",
                default_value: "255",
                required: true,
                kind: TypeParamKind::Length,
            }),
            "CHAR" => Some(TypeParamSpec {
                title: "Length (e.g. 36)",
                default_value: "36",
                required: true,
                kind: TypeParamKind::Length,
            }),
            "VARBINARY" => Some(TypeParamSpec {
                title: "Byte Length (e.g. 255)",
                default_value: "255",
                required: true,
                kind: TypeParamKind::Length,
            }),
            "BINARY" => Some(TypeParamSpec {
                title: "Byte Length (e.g. 16)",
                default_value: "16",
                required: true,
                kind: TypeParamKind::Length,
            }),
            "BIT" => Some(TypeParamSpec {
                title: "Bit Length (optional, e.g. 1, 8)",
                default_value: "1",
                required: false,
                kind: TypeParamKind::Length,
            }),
            "VARBIT" => Some(TypeParamSpec {
                title: "Bit Length (optional, e.g. 64)",
                default_value: "64",
                required: false,
                kind: TypeParamKind::Length,
            }),
            "DECIMAL" => Some(TypeParamSpec {
                title: "Precision, Scale (e.g. 10,2)",
                default_value: "10,2",
                required: false,
                kind: TypeParamKind::PrecisionScale,
            }),
            "NUMERIC" => Some(TypeParamSpec {
                title: "Precision, Scale (optional, e.g. 10,2)",
                default_value: "10,2",
                required: false,
                kind: TypeParamKind::PrecisionScale,
            }),
            "FLOAT" => Some(TypeParamSpec {
                title: "Precision (optional, e.g. 24)",
                default_value: "",
                required: false,
                kind: TypeParamKind::FloatPrecision,
            }),
            "TIME" | "DATETIME" | "TIMESTAMP" | "TIMESTAMPTZ" => Some(TypeParamSpec {
                title: "Fractional Seconds Precision (optional, 0–6)",
                default_value: "",
                required: false,
                kind: TypeParamKind::TimePrecision,
            }),
            "ENUM" => Some(TypeParamSpec {
                title: "Allowed Values (e.g. 'active', 'pending')",
                default_value: "'value1', 'value2'",
                required: true,
                kind: TypeParamKind::ValueList,
            }),
            "SET" => Some(TypeParamSpec {
                title: "Allowed Values (e.g. 'read', 'write')",
                default_value: "'value1', 'value2'",
                required: true,
                kind: TypeParamKind::ValueList,
            }),
            _ => None,
        }
    }

    fn parse_column_data_type(raw: &str) -> (&str, Option<String>) {
        let trimmed = raw.trim();
        if let Some((prefix_raw, rest)) = trimmed.split_once('(') {
            let prefix = prefix_raw.trim().to_ascii_lowercase();
            let (inner_raw, suffix_raw) = rest.rsplit_once(')').unwrap_or((rest, ""));
            let inner = inner_raw.trim();
            let suffix = suffix_raw.trim().to_ascii_lowercase();
            match prefix.as_str() {
                "varchar" | "character varying" | "nvarchar" => {
                    return ("VARCHAR", Some(inner.to_string()));
                }
                "char" | "character" | "bpchar" | "nchar" => {
                    return ("CHAR", Some(inner.to_string()));
                }
                "varbinary" => {
                    return ("VARBINARY", Some(inner.to_string()));
                }
                "binary" => {
                    return ("BINARY", Some(inner.to_string()));
                }
                "bit" => {
                    return ("BIT", Some(inner.to_string()));
                }
                "varbit" | "bit varying" => {
                    return ("VARBIT", Some(inner.to_string()));
                }
                "decimal" | "dec" => {
                    return ("DECIMAL", Some(inner.to_string()));
                }
                "numeric" => {
                    return ("NUMERIC", Some(inner.to_string()));
                }
                "float" => {
                    return ("FLOAT", Some(inner.to_string()));
                }
                "time" => {
                    return ("TIME", Some(inner.to_string()));
                }
                "datetime" => {
                    return ("DATETIME", Some(inner.to_string()));
                }
                "timestamp" => {
                    if suffix == "with time zone" {
                        return ("TIMESTAMPTZ", Some(inner.to_string()));
                    }
                    return ("TIMESTAMP", Some(inner.to_string()));
                }
                "timestamptz" => {
                    return ("TIMESTAMPTZ", Some(inner.to_string()));
                }
                "enum" => {
                    return ("ENUM", Some(inner.to_string()));
                }
                "set" => {
                    return ("SET", Some(inner.to_string()));
                }
                _ => {}
            }
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "varchar" | "character varying" | "nvarchar" => ("VARCHAR", Some("255".to_string())),
            "char" | "character" | "bpchar" | "nchar" => ("CHAR", Some("36".to_string())),
            "varbinary" => ("VARBINARY", Some("255".to_string())),
            "binary" => ("BINARY", Some("16".to_string())),
            "bit" => ("BIT", Some(String::new())),
            "varbit" | "bit varying" => ("VARBIT", Some(String::new())),
            "decimal" | "dec" => ("DECIMAL", Some(String::new())),
            "numeric" => ("NUMERIC", Some(String::new())),
            "float" => ("FLOAT", Some(String::new())),
            "time" | "time without time zone" => ("TIME", Some(String::new())),
            "datetime" => ("DATETIME", Some(String::new())),
            "timestamp" | "timestamp without time zone" => ("TIMESTAMP", Some(String::new())),
            "timestamptz" | "timestamp with time zone" => ("TIMESTAMPTZ", Some(String::new())),
            "int" | "integer" | "int4" | "int(11)" => ("INT", None),
            "bigint" | "int8" | "bigint(20)" => ("BIGINT", None),
            "smallint" | "int2" | "smallint(6)" => ("SMALLINT", None),
            "tinyint" | "tinyint(4)" => ("TINYINT", None),
            "bool" | "boolean" | "tinyint(1)" => ("BOOLEAN", None),
            "float4" | "real" => ("REAL", None),
            "float8" | "double precision" => ("DOUBLE PRECISION", None),
            _ => (trimmed, None),
        }
    }

    fn format_column_data_type(selected_type: &str, raw_param: &str) -> Option<String> {
        let selected_type = selected_type.trim();
        if selected_type.is_empty() {
            return None;
        }
        if let Some(spec) = Self::param_spec_for_type(selected_type) {
            let param = raw_param
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .trim();
            if param.is_empty() {
                if spec.required {
                    return None;
                } else {
                    return Some(selected_type.to_string());
                }
            }
            Some(format!("{}({})", selected_type, param))
        } else {
            Some(selected_type.to_string())
        }
    }

    fn build_column_type_options(
        engine: DbEngine,
        existing: Option<&ColumnInfo>,
    ) -> (Vec<String>, usize, Option<String>) {
        let mut type_options: Vec<String> = Self::column_data_types_for_engine(engine)
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        let (init_type_idx, parsed_param) = if let Some(col) = existing {
            let raw_dt = col.data_type.trim();
            let (canonical, param) = Self::parse_column_data_type(raw_dt);
            if let Some(pos) = type_options
                .iter()
                .position(|t| t.eq_ignore_ascii_case(canonical))
            {
                (pos, param)
            } else if !raw_dt.is_empty() {
                type_options.insert(0, raw_dt.to_string());
                (0, None)
            } else {
                (0, None)
            }
        } else {
            (0, None)
        };

        (type_options, init_type_idx, parsed_param)
    }

    fn open_column_dialog(&self, parent: &gtk::Widget, existing: Option<ColumnInfo>) {
        let is_edit = existing.is_some();
        let engine = *self.current_engine.borrow();
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

        let (type_options, init_type_idx, parsed_param) =
            Self::build_column_type_options(engine, existing.as_ref());

        let type_refs: Vec<&str> = type_options.iter().map(|s| s.as_str()).collect();
        let type_model = gtk::StringList::new(&type_refs);
        let type_row = adw::ComboRow::builder()
            .title("Data Type")
            .model(&type_model)
            .selected(init_type_idx as u32)
            .build();

        let init_selected_type = &type_options[init_type_idx];
        let init_spec = Self::param_spec_for_type(init_selected_type);
        let (init_param_visible, init_param_title, init_param_text) = match init_spec {
            Some(spec) => (
                true,
                spec.title,
                parsed_param.unwrap_or_else(|| spec.default_value.to_string()),
            ),
            None => (false, "Length", String::new()),
        };

        let type_param_row = adw::EntryRow::builder()
            .title(init_param_title)
            .text(&init_param_text)
            .visible(init_param_visible)
            .build();

        let last_auto_param = Rc::new(RefCell::new(init_param_text));
        let last_param_kind = Rc::new(RefCell::new(init_spec.map(|s| s.kind)));
        {
            let type_model = type_model.clone();
            let type_param_row = type_param_row.clone();
            let last_auto_param = last_auto_param.clone();
            let last_param_kind = last_param_kind.clone();
            type_row.connect_selected_notify(move |combo| {
                let selected_type = type_model
                    .string(combo.selected())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                if let Some(spec) = Self::param_spec_for_type(&selected_type) {
                    type_param_row.set_title(spec.title);
                    let cur_text = type_param_row.text().trim().to_string();
                    let prev_auto = last_auto_param.borrow().clone();
                    let prev_kind = *last_param_kind.borrow();
                    if cur_text == prev_auto || prev_kind != Some(spec.kind) {
                        type_param_row.set_text(spec.default_value);
                        *last_auto_param.borrow_mut() = spec.default_value.to_string();
                    }
                    *last_param_kind.borrow_mut() = Some(spec.kind);
                    type_param_row.set_visible(true);
                } else {
                    *last_param_kind.borrow_mut() = None;
                    type_param_row.set_visible(false);
                }
            });
        }

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
        prefs_group.add(&type_param_row);
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
            let type_model = type_model.clone();
            let type_param_row = type_param_row.clone();
            save_btn.connect_clicked(move |_| {
                let col_name = name_row.text().trim().to_string();
                let selected_type = type_model
                    .string(type_row.selected())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let Some(data_type) =
                    Self::format_column_data_type(&selected_type, &type_param_row.text())
                else {
                    return;
                };
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

    fn render_indexes(&self) {
        let table = self.current_table.borrow().clone();
        let cols = self.columns.borrow().clone();
        let indexes = self.indexes.borrow().clone();

        if table.is_empty() {
            self.idx_header_label.set_text("Indexes");
        } else {
            self.idx_header_label.set_text(&format!(
                "Indexes — {} index(es)",
                indexes.len()
            ));
        }
        self.add_idx_btn
            .set_sensitive(!table.is_empty() && !cols.is_empty());
        self.idx_refresh_btn.set_sensitive(!table.is_empty());

        while let Some(child) = self.idx_list_box.first_child() {
            self.idx_list_box.remove(&child);
        }
        self.idx_row_cells.borrow_mut().clear();

        if indexes.is_empty() {
            if !table.is_empty() {
                let empty_row = adw::ActionRow::builder()
                    .title("No indexes defined on this table")
                    .subtitle("Click 'Add Index' to create a single-column or composite index")
                    .build();
                self.idx_list_box.append(&empty_row);
            }
            return;
        }

        let widths = self.idx_col_widths.borrow().clone();

        for idx in indexes {
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row_box.set_margin_top(8);
            row_box.set_margin_bottom(8);

            let name_lbl = gtk::Label::new(Some(&idx.name));
            name_lbl.add_css_class("heading");
            name_lbl.set_halign(gtk::Align::Fill);
            name_lbl.set_xalign(0.0);
            name_lbl.set_hexpand(true);
            name_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name_lbl.set_max_width_chars(1);
            name_lbl.set_tooltip_text(Some(&idx.name));

            let type_lbl = gtk::Label::new(Some(&idx.index_type));
            type_lbl.add_css_class("monospace");
            if idx.is_primary {
                type_lbl.add_css_class("accent");
                type_lbl.add_css_class("heading");
            } else if idx.is_unique {
                type_lbl.add_css_class("accent");
            } else {
                type_lbl.add_css_class("dim-label");
            }
            type_lbl.set_halign(gtk::Align::Fill);
            type_lbl.set_xalign(0.0);
            type_lbl.set_hexpand(true);
            type_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            type_lbl.set_max_width_chars(1);
            type_lbl.set_tooltip_text(Some(&idx.index_type));

            let cols_text = idx.columns.join(", ");
            let cols_lbl = gtk::Label::new(Some(&cols_text));
            cols_lbl.add_css_class("monospace");
            cols_lbl.set_halign(gtk::Align::Fill);
            cols_lbl.set_xalign(0.0);
            cols_lbl.set_hexpand(true);
            cols_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            cols_lbl.set_max_width_chars(1);
            cols_lbl.set_tooltip_text(Some(&cols_text));

            let uniq_lbl = gtk::Label::new(Some(if idx.is_unique { "YES" } else { "NO" }));
            if idx.is_unique {
                uniq_lbl.add_css_class("accent");
            } else {
                uniq_lbl.add_css_class("dim-label");
            }
            uniq_lbl.set_halign(gtk::Align::Fill);
            uniq_lbl.set_xalign(0.0);
            uniq_lbl.set_hexpand(true);
            uniq_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            uniq_lbl.set_max_width_chars(1);

            let method_lbl = gtk::Label::new(Some(&idx.method));
            method_lbl.add_css_class("monospace");
            method_lbl.add_css_class("dim-label");
            method_lbl.set_halign(gtk::Align::Fill);
            method_lbl.set_xalign(0.0);
            method_lbl.set_hexpand(true);
            method_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            method_lbl.set_max_width_chars(1);
            if !idx.method.is_empty() {
                method_lbl.set_tooltip_text(Some(&idx.method));
            }

            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);

            let edit_btn = gtk::Button::from_icon_name("document-edit-symbolic");
            edit_btn.add_css_class("flat");
            edit_btn.set_tooltip_text(Some("Edit Index"));

            let del_btn = gtk::Button::from_icon_name("user-trash-symbolic");
            del_btn.add_css_class("flat");
            del_btn.add_css_class("error");
            del_btn.set_tooltip_text(Some("Drop Index"));

            actions.append(&edit_btn);
            actions.append(&del_btn);

            let cells = vec![
                wrap_resizable_row_cell(&name_lbl, widths[0]),
                wrap_resizable_row_cell(&type_lbl, widths[1]),
                wrap_resizable_row_cell(&cols_lbl, widths[2]),
                wrap_resizable_row_cell(&uniq_lbl, widths[3]),
                wrap_resizable_row_cell(&method_lbl, widths[4]),
                wrap_resizable_row_cell(&actions, widths[5]),
            ];

            for cell in &cells {
                row_box.append(cell);
            }
            self.idx_row_cells.borrow_mut().push(cells);

            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&row_box));
            self.idx_list_box.append(&row);

            // Edit index handler
            {
                let tab_clone = self.clone();
                let idx_clone = idx.clone();
                edit_btn.connect_clicked(move |btn| {
                    tab_clone.open_index_dialog(
                        btn.upcast_ref::<gtk::Widget>(),
                        Some(idx_clone.clone()),
                    );
                });
            }

            // Drop index handler
            {
                let tab_clone = self.clone();
                let idx_clone = idx.clone();
                del_btn.connect_clicked(move |btn| {
                    tab_clone.confirm_drop_index(btn.upcast_ref::<gtk::Widget>(), &idx_clone);
                });
            }
        }
    }

    fn suggest_index_name(table: &str, index_type: &str, columns: &[String]) -> String {
        if index_type.eq_ignore_ascii_case("PRIMARY") {
            return "PRIMARY".to_string();
        }
        let prefix = match index_type.to_ascii_uppercase().as_str() {
            "UNIQUE" => "uniq",
            "FULLTEXT" => "ft",
            "SPATIAL" => "sp",
            _ => "idx",
        };
        if columns.is_empty() {
            format!("{}_{}", prefix, table)
        } else {
            format!("{}_{}_{}", prefix, table, columns.join("_"))
        }
    }

    fn open_index_dialog(&self, parent: &gtk::Widget, existing: Option<IndexInfo>) {
        let is_edit = existing.is_some();
        let table = self.current_table.borrow().clone();
        let all_cols = self.columns.borrow().clone();

        let dialog = adw::Dialog::builder()
            .title(if is_edit { "Edit Index" } else { "Add Index" })
            .content_width(460)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar_view.add_top_bar(&header);

        let content_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content_box.set_margin_top(12);
        content_box.set_margin_bottom(12);
        content_box.set_margin_start(12);
        content_box.set_margin_end(12);

        let settings_group = adw::PreferencesGroup::new();

        let init_type_idx = existing
            .as_ref()
            .and_then(|idx| {
                INDEX_TYPES
                    .iter()
                    .position(|t| t.eq_ignore_ascii_case(&idx.index_type))
            })
            .unwrap_or(0);

        let type_model = gtk::StringList::new(INDEX_TYPES);
        let type_row = adw::ComboRow::builder()
            .title("Index Type")
            .model(&type_model)
            .selected(init_type_idx as u32)
            .build();

        let init_method_idx = existing
            .as_ref()
            .and_then(|idx| {
                INDEX_METHODS
                    .iter()
                    .position(|m| m.eq_ignore_ascii_case(&idx.method))
            })
            .unwrap_or(0);

        let method_model = gtk::StringList::new(INDEX_METHODS);
        let method_row = adw::ComboRow::builder()
            .title("Storage Method")
            .model(&method_model)
            .selected(init_method_idx as u32)
            .build();

        let init_selected_cols: Vec<String> = match &existing {
            Some(idx) => idx.columns.clone(),
            None => all_cols
                .iter()
                .find(|c| !c.is_primary_key)
                .or_else(|| all_cols.first())
                .map(|c| vec![c.name.clone()])
                .unwrap_or_default(),
        };

        let init_type_str = INDEX_TYPES[init_type_idx];
        let init_suggested_name =
            Self::suggest_index_name(&table, init_type_str, &init_selected_cols);
        let init_name = existing
            .as_ref()
            .map(|idx| idx.name.clone())
            .unwrap_or_else(|| init_suggested_name.clone());

        let name_row = adw::EntryRow::builder()
            .title("Key Name")
            .text(&init_name)
            .sensitive(!init_type_str.eq_ignore_ascii_case("PRIMARY"))
            .build();

        settings_group.add(&type_row);
        settings_group.add(&name_row);
        settings_group.add(&method_row);
        content_box.append(&settings_group);

        let cols_group = adw::PreferencesGroup::builder()
            .title("Indexed Columns")
            .description("Select one or more columns; use arrows to reorder composite keys")
            .build();

        let selected_cols = Rc::new(RefCell::new(init_selected_cols));
        let last_auto_name = Rc::new(RefCell::new(if is_edit {
            String::new()
        } else {
            init_suggested_name
        }));
        let updating_ui = Rc::new(RefCell::new(false));

        // Store per-column widgets so we can refresh order badges & arrow buttons
        struct ColRowWidgets {
            col_name: String,
            check: gtk::CheckButton,
            order_lbl: gtk::Label,
            up_btn: gtk::Button,
            down_btn: gtk::Button,
        }
        let col_widgets: Rc<RefCell<Vec<ColRowWidgets>>> = Rc::new(RefCell::new(Vec::new()));

        let save_btn = gtk::Button::with_label(if is_edit { "Apply Changes" } else { "Add Index" });
        save_btn.add_css_class("suggested-action");
        save_btn.set_sensitive(!selected_cols.borrow().is_empty());

        let refresh_col_ui: Rc<dyn Fn()> = {
            let col_widgets = col_widgets.clone();
            let selected_cols = selected_cols.clone();
            let type_row = type_row.clone();
            let name_row = name_row.clone();
            let last_auto_name = last_auto_name.clone();
            let updating_ui = updating_ui.clone();
            let save_btn = save_btn.clone();
            let table = table.clone();

            Rc::new(move || {
                *updating_ui.borrow_mut() = true;
                let sel = selected_cols.borrow();
                let total_sel = sel.len();
                for w in col_widgets.borrow().iter() {
                    if let Some(pos) = sel.iter().position(|c| c == &w.col_name) {
                        w.check.set_active(true);
                        w.order_lbl.set_text(&format!("#{}", pos + 1));
                        w.order_lbl.set_visible(true);
                        w.up_btn.set_visible(total_sel > 1);
                        w.down_btn.set_visible(total_sel > 1);
                        w.up_btn.set_sensitive(pos > 0);
                        w.down_btn.set_sensitive(pos + 1 < total_sel);
                    } else {
                        w.check.set_active(false);
                        w.order_lbl.set_visible(false);
                        w.up_btn.set_visible(false);
                        w.down_btn.set_visible(false);
                    }
                }
                save_btn.set_sensitive(total_sel > 0);

                let cur_type = INDEX_TYPES
                    .get(type_row.selected() as usize)
                    .copied()
                    .unwrap_or("INDEX");
                let suggested = Self::suggest_index_name(&table, cur_type, &sel);
                if cur_type.eq_ignore_ascii_case("PRIMARY") {
                    name_row.set_text("PRIMARY");
                    name_row.set_sensitive(false);
                } else {
                    name_row.set_sensitive(true);
                    let current_text = name_row.text().trim().to_string();
                    let prev_auto = last_auto_name.borrow().clone();
                    if current_text.is_empty()
                        || current_text.eq_ignore_ascii_case("PRIMARY")
                        || (!prev_auto.is_empty() && current_text == prev_auto)
                    {
                        name_row.set_text(&suggested);
                        *last_auto_name.borrow_mut() = suggested;
                    }
                }
                *updating_ui.borrow_mut() = false;
            })
        };

        for col in &all_cols {
            let row = adw::ActionRow::builder()
                .title(&col.name)
                .subtitle(&col.data_type)
                .build();

            let check = gtk::CheckButton::new();
            check.set_valign(gtk::Align::Center);
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));

            let order_lbl = gtk::Label::new(None);
            order_lbl.add_css_class("monospace");
            order_lbl.add_css_class("accent");
            order_lbl.add_css_class("caption-heading");
            order_lbl.set_valign(gtk::Align::Center);
            order_lbl.set_margin_end(4);

            let up_btn = gtk::Button::from_icon_name("go-up-symbolic");
            up_btn.add_css_class("flat");
            up_btn.set_valign(gtk::Align::Center);
            up_btn.set_tooltip_text(Some("Move column earlier in composite index"));

            let down_btn = gtk::Button::from_icon_name("go-down-symbolic");
            down_btn.add_css_class("flat");
            down_btn.set_valign(gtk::Align::Center);
            down_btn.set_tooltip_text(Some("Move column later in composite index"));

            row.add_suffix(&order_lbl);
            row.add_suffix(&up_btn);
            row.add_suffix(&down_btn);
            cols_group.add(&row);

            let col_name = col.name.clone();
            {
                let selected_cols = selected_cols.clone();
                let updating_ui = updating_ui.clone();
                let refresh_col_ui = refresh_col_ui.clone();
                let col_name = col_name.clone();
                check.connect_toggled(move |chk| {
                    if *updating_ui.borrow() {
                        return;
                    }
                    {
                        let mut sel = selected_cols.borrow_mut();
                        if chk.is_active() {
                            if !sel.contains(&col_name) {
                                sel.push(col_name.clone());
                            }
                        } else {
                            sel.retain(|c| c != &col_name);
                        }
                    }
                    refresh_col_ui();
                });
            }

            {
                let selected_cols = selected_cols.clone();
                let refresh_col_ui = refresh_col_ui.clone();
                let col_name = col_name.clone();
                up_btn.connect_clicked(move |_| {
                    {
                        let mut sel = selected_cols.borrow_mut();
                        if let Some(pos) = sel.iter().position(|c| c == &col_name) {
                            if pos > 0 {
                                sel.swap(pos, pos - 1);
                            }
                        }
                    }
                    refresh_col_ui();
                });
            }

            {
                let selected_cols = selected_cols.clone();
                let refresh_col_ui = refresh_col_ui.clone();
                let col_name = col_name.clone();
                down_btn.connect_clicked(move |_| {
                    {
                        let mut sel = selected_cols.borrow_mut();
                        if let Some(pos) = sel.iter().position(|c| c == &col_name) {
                            if pos + 1 < sel.len() {
                                sel.swap(pos, pos + 1);
                            }
                        }
                    }
                    refresh_col_ui();
                });
            }

            col_widgets.borrow_mut().push(ColRowWidgets {
                col_name,
                check,
                order_lbl,
                up_btn,
                down_btn,
            });
        }

        // Update suggested name and sensitivity when Index Type changes
        {
            let refresh_col_ui = refresh_col_ui.clone();
            type_row.connect_selected_notify(move |_| {
                refresh_col_ui();
            });
        }

        refresh_col_ui();

        content_box.append(&cols_group);

        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_margin_top(4);

        let cancel_btn = gtk::Button::with_label("Cancel");
        btn_box.append(&cancel_btn);
        btn_box.append(&save_btn);
        content_box.append(&btn_box);

        let dialog_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(520)
            .child(&content_box)
            .build();

        toolbar_view.set_content(Some(&dialog_scroll));
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
            let selected_cols = selected_cols.clone();
            let old_idx = existing;
            save_btn.connect_clicked(move |_| {
                let cols = selected_cols.borrow().clone();
                if cols.is_empty() {
                    return;
                }
                let index_type = INDEX_TYPES
                    .get(type_row.selected() as usize)
                    .copied()
                    .unwrap_or("INDEX")
                    .to_string();
                let method_sel = INDEX_METHODS
                    .get(method_row.selected() as usize)
                    .copied()
                    .unwrap_or("Default");
                let method = if method_sel.eq_ignore_ascii_case("Default") {
                    None
                } else {
                    Some(method_sel.to_string())
                };
                let mut name = name_row.text().trim().to_string();
                if name.is_empty() {
                    name = Self::suggest_index_name(&table, &index_type, &cols);
                }

                let spec = IndexSpec {
                    name,
                    columns: cols,
                    index_type,
                    method,
                };
                let op = match &old_idx {
                    Some(prev) => IndexAlterOp::Modify {
                        old_name: prev.name.clone(),
                        old_is_primary: prev.is_primary,
                        spec,
                    },
                    None => IndexAlterOp::Add(spec),
                };
                let _ = cmd_tx.send_blocking(BackendCommand::AlterIndex {
                    database: db.clone(),
                    table: table.clone(),
                    op,
                });
                dialog.close();
            });
        }

        dialog.present(Some(parent));
    }

    fn confirm_drop_index(&self, parent: &gtk::Widget, idx: &IndexInfo) {
        let table = self.current_table.borrow().clone();
        let db = self.current_db.borrow().clone();
        let alert = adw::AlertDialog::builder()
            .heading(format!("Drop Index '{}'?", idx.name))
            .body(format!(
                "Are you sure you want to drop the {} index '{}' ({}) from table '{}'?",
                idx.index_type,
                idx.name,
                idx.columns.join(", "),
                table
            ))
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("drop", "Drop Index");
        alert.set_response_appearance("drop", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let cmd_tx = self.cmd_tx.clone();
        let idx_name = idx.name.clone();
        let is_primary = idx.is_primary;
        alert.connect_response(None, move |_, resp| {
            if resp == "drop" {
                let _ = cmd_tx.send_blocking(BackendCommand::AlterIndex {
                    database: db.clone(),
                    table: table.clone(),
                    op: IndexAlterOp::Drop {
                        name: idx_name.clone(),
                        is_primary,
                    },
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
    fn test_parse_and_format_column_data_types() {
        assert_eq!(
            StructureTab::parse_column_data_type("varchar(120)"),
            ("VARCHAR", Some("120".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("character varying(64)"),
            ("VARCHAR", Some("64".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("bpchar(36)"),
            ("CHAR", Some("36".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("char(10)"),
            ("CHAR", Some("10".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("decimal(12, 4)"),
            ("DECIMAL", Some("12, 4".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("numeric(10,2)"),
            ("NUMERIC", Some("10,2".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("numeric"),
            ("NUMERIC", Some(String::new()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("varbinary(255)"),
            ("VARBINARY", Some("255".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("binary(16)"),
            ("BINARY", Some("16".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("bit(8)"),
            ("BIT", Some("8".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("bit varying(64)"),
            ("VARBIT", Some("64".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("timestamp(3) with time zone"),
            ("TIMESTAMPTZ", Some("3".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("enum('active','pending')"),
            ("ENUM", Some("'active','pending'".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("set('read','write')"),
            ("SET", Some("'read','write'".to_string()))
        );
        assert_eq!(
            StructureTab::parse_column_data_type("int4"),
            ("INT", None)
        );

        assert_eq!(
            StructureTab::format_column_data_type("VARCHAR", "255"),
            Some("VARCHAR(255)".to_string())
        );
        assert_eq!(
            StructureTab::format_column_data_type("CHAR", "36"),
            Some("CHAR(36)".to_string())
        );
        assert_eq!(
            StructureTab::format_column_data_type("DECIMAL", "10,2"),
            Some("DECIMAL(10,2)".to_string())
        );
        assert_eq!(
            StructureTab::format_column_data_type("NUMERIC", ""),
            Some("NUMERIC".to_string())
        );
        assert_eq!(
            StructureTab::format_column_data_type("TIMESTAMP", ""),
            Some("TIMESTAMP".to_string())
        );
        assert_eq!(
            StructureTab::format_column_data_type("TIMESTAMP", "3"),
            Some("TIMESTAMP(3)".to_string())
        );
        assert_eq!(
            StructureTab::format_column_data_type("ENUM", "'a', 'b'"),
            Some("ENUM('a', 'b')".to_string())
        );
        assert_eq!(
            StructureTab::format_column_data_type("ENUM", ""),
            None
        );
        assert_eq!(
            StructureTab::format_column_data_type("VARCHAR", ""),
            None
        );
        assert_eq!(
            StructureTab::format_column_data_type("INT", ""),
            Some("INT".to_string())
        );
    }

    #[test]
    fn test_column_data_types_filtered_by_engine() {
        let mysql_types = StructureTab::column_data_types_for_engine(DbEngine::MySql);
        let pg_types = StructureTab::column_data_types_for_engine(DbEngine::Postgres);

        let pg_only = [
            "SERIAL",
            "BIGSERIAL",
            "DOUBLE PRECISION",
            "TIMESTAMPTZ",
            "UUID",
            "JSONB",
            "VARBIT",
            "BYTEA",
        ];
        for unsupported in pg_only {
            assert!(
                !mysql_types.contains(&unsupported),
                "MySQL dropdown must not list PostgreSQL-only type '{}'",
                unsupported
            );
            assert!(
                pg_types.contains(&unsupported),
                "PostgreSQL dropdown should list '{}'",
                unsupported
            );
        }

        let mysql_only = [
            "MEDIUMTEXT",
            "LONGTEXT",
            "TINYINT",
            "DOUBLE",
            "DATETIME",
            "ENUM",
            "SET",
            "BINARY",
            "VARBINARY",
            "BLOB",
        ];
        for unsupported in mysql_only {
            assert!(
                !pg_types.contains(&unsupported),
                "PostgreSQL dropdown must not list MySQL-only type '{}'",
                unsupported
            );
            assert!(
                mysql_types.contains(&unsupported),
                "MySQL dropdown should list '{}'",
                unsupported
            );
        }

        // Every listed type for both engines must produce a valid SQL type with its default parameter
        for &engine in &[DbEngine::MySql, DbEngine::Postgres] {
            for &ty in StructureTab::column_data_types_for_engine(engine) {
                let default_param = StructureTab::param_spec_for_type(ty)
                    .map(|s| s.default_value)
                    .unwrap_or("");
                let formatted = StructureTab::format_column_data_type(ty, default_param);
                assert!(
                    formatted.as_deref().is_some_and(|s| !s.is_empty()),
                    "Type '{}' for {:?} failed to format with default param '{}'",
                    ty,
                    engine,
                    default_param
                );
            }
        }
    }

    #[test]
    fn test_build_column_type_options_per_engine() {
        let (mysql_opts, mysql_idx, mysql_param) =
            StructureTab::build_column_type_options(DbEngine::MySql, None);
        assert_eq!(mysql_idx, 0);
        assert_eq!(mysql_param, None);
        assert_eq!(mysql_opts, MYSQL_COLUMN_DATA_TYPES);

        let (pg_opts, pg_idx, pg_param) =
            StructureTab::build_column_type_options(DbEngine::Postgres, None);
        assert_eq!(pg_idx, 0);
        assert_eq!(pg_param, None);
        assert_eq!(pg_opts, POSTGRES_COLUMN_DATA_TYPES);

        let existing_pg_col = ColumnInfo {
            ordinal: 1,
            name: "payload".to_string(),
            data_type: "jsonb".to_string(),
            is_nullable: true,
            is_primary_key: false,
            default_value: None,
            extra: String::new(),
        };
        let (pg_edit_opts, pg_edit_idx, _) =
            StructureTab::build_column_type_options(DbEngine::Postgres, Some(&existing_pg_col));
        assert_eq!(pg_edit_opts[pg_edit_idx], "JSONB");
    }
}
