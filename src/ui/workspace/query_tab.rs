use crate::db::models::{BackendCommand, DbValue, RawQueryOutcome, RawQueryResultSet};
use crate::ui::workspace::{
    CELL_EXTRA_PADDING, DEFAULT_DATA_COL_WIDTH, HEADER_EXTRA_PADDING,
    MAX_LONG_TEXT_AUTO_COL_WIDTH, MAX_MEASURE_CHARS, MIN_COLUMN_WIDTH,
    build_resizable_header_cell, compute_auto_column_widths, format_long_text_preview,
    measure_single_line_text,
};
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::rc::Rc;

const DEFAULT_INDEX_COL_WIDTH: i32 = 48;

#[derive(Clone)]
pub struct QueryTab {
    pub root: gtk::Paned,
    buffer: sourceview5::Buffer,
    view: sourceview5::View,
    results_scroll: gtk::ScrolledWindow,
    results_grid: gtk::Grid,
    results_status: gtk::Label,
    export_menu_btn: gtk::MenuButton,
    current_db: Rc<RefCell<String>>,
    last_result: Rc<RefCell<Option<RawQueryResultSet>>>,
    col_widths: Rc<RefCell<HashMap<usize, i32>>>,
    natural_col_widths: Rc<RefCell<Vec<i32>>>,
    header_cells: Rc<RefCell<Vec<(usize, gtk::Box)>>>,
    last_viewport_width: Rc<RefCell<i32>>,
    index_col_width: Rc<RefCell<i32>>,
    cmd_tx: Sender<BackendCommand>,
}

impl QueryTab {
    pub fn new<F>(cmd_tx: Sender<BackendCommand>, on_toast: F) -> Self
    where
        F: Fn(&str) + Clone + 'static,
    {
        let paned = gtk::Paned::new(gtk::Orientation::Vertical);
        paned.set_position(320);
        paned.set_vexpand(true);
        paned.set_hexpand(true);

        // Top Pane: Editor Toolbar + SourceView + Vim Status Bar
        let top_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let editor_toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        editor_toolbar.set_margin_top(8);
        editor_toolbar.set_margin_bottom(8);
        editor_toolbar.set_margin_start(12);
        editor_toolbar.set_margin_end(12);

        let title_lbl = gtk::Label::new(Some("SQL Query Editor (Vim Mode — :r to run, :w to save)"));
        title_lbl.add_css_class("heading");
        title_lbl.set_halign(gtk::Align::Start);
        title_lbl.set_hexpand(true);

        let open_btn = gtk::Button::from_icon_name("document-open-symbolic");
        open_btn.set_tooltip_text(Some("Open .sql File"));

        let save_btn = gtk::Button::from_icon_name("document-save-symbolic");
        save_btn.set_label("Save (:w)");
        save_btn.set_tooltip_text(Some("Save SQL Query to File (:w)"));

        let run_btn = gtk::Button::from_icon_name("media-playback-start-symbolic");
        run_btn.set_label("Run (:r)");
        run_btn.add_css_class("suggested-action");
        run_btn.set_tooltip_text(Some("Execute SQL Query (:r or Ctrl+Enter)"));

        editor_toolbar.append(&title_lbl);
        editor_toolbar.append(&open_btn);
        editor_toolbar.append(&save_btn);
        editor_toolbar.append(&run_btn);

        top_box.append(&editor_toolbar);
        top_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Configure GtkSourceBuffer & GtkSourceView with SQL syntax highlighting
        let buffer = sourceview5::Buffer::new(None);
        let lang_mgr = sourceview5::LanguageManager::default();
        if let Some(sql_lang) = lang_mgr.language("sql") {
            buffer.set_language(Some(&sql_lang));
        }
        buffer.set_highlight_syntax(true);
        buffer.set_text("-- Write SQL here. In Vim normal mode: ':r' runs query, ':w' saves .sql file\nSELECT 1 AS status;\n");

        Self::apply_editor_style_scheme(&buffer);
        let style_mgr = adw::StyleManager::default();
        let buffer_for_theme = buffer.clone();
        style_mgr.connect_dark_notify(move |_| {
            Self::apply_editor_style_scheme(&buffer_for_theme);
        });

        let view = sourceview5::View::with_buffer(&buffer);
        view.set_show_line_numbers(true);
        view.set_highlight_current_line(true);
        view.set_auto_indent(true);
        view.set_indent_on_tab(true);
        view.set_tab_width(2);
        view.set_insert_spaces_instead_of_tabs(true);
        view.set_monospace(true);
        view.set_top_margin(10);
        view.set_bottom_margin(10);
        view.set_left_margin(12);
        view.set_right_margin(12);
        view.set_vexpand(true);
        view.set_hexpand(true);

        // Attach GtkSourceVimIMContext
        let vim_im = sourceview5::VimIMContext::new();
        vim_im.set_client_widget(Some(&view));
        let vim_key_ctrl = gtk::EventControllerKey::new();
        vim_key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        vim_key_ctrl.set_im_context(Some(&vim_im));
        view.add_controller(vim_key_ctrl);

        let editor_scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .child(&view)
            .build();
        top_box.append(&editor_scroll);

        // Vim Status Bar
        top_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let vim_status_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        vim_status_bar.add_css_class("vim-status-bar");
        vim_status_bar.set_margin_top(4);
        vim_status_bar.set_margin_bottom(4);
        vim_status_bar.set_margin_start(12);
        vim_status_bar.set_margin_end(12);

        let vim_mode_lbl = gtk::Label::new(Some("-- NORMAL --"));
        vim_mode_lbl.add_css_class("monospace");
        vim_mode_lbl.add_css_class("heading");
        vim_mode_lbl.set_halign(gtk::Align::Start);
        vim_mode_lbl.set_hexpand(true);

        let vim_cmd_lbl = gtk::Label::new(None);
        vim_cmd_lbl.add_css_class("monospace");
        vim_cmd_lbl.add_css_class("dim-label");

        let cursor_pos_lbl = gtk::Label::new(Some("Ln 1, Col 1"));
        cursor_pos_lbl.add_css_class("monospace");
        cursor_pos_lbl.add_css_class("caption");
        cursor_pos_lbl.add_css_class("dim-label");

        vim_status_bar.append(&vim_mode_lbl);
        vim_status_bar.append(&vim_cmd_lbl);
        vim_status_bar.append(&cursor_pos_lbl);
        top_box.append(&vim_status_bar);

        // Bottom Pane: Results Panel
        let bottom_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let results_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        results_bar.set_margin_top(6);
        results_bar.set_margin_bottom(6);
        results_bar.set_margin_start(12);
        results_bar.set_margin_end(12);

        let results_status = gtk::Label::new(Some("Query Results"));
        results_status.add_css_class("heading");
        results_status.set_halign(gtk::Align::Start);
        results_status.set_hexpand(true);

        let export_popover = gtk::Popover::new();
        let exp_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        exp_box.set_margin_top(8);
        exp_box.set_margin_bottom(8);
        exp_box.set_margin_start(8);
        exp_box.set_margin_end(8);
        let export_csv_btn = gtk::Button::with_label("Export Results to CSV...");
        export_csv_btn.add_css_class("flat");
        let export_json_btn = gtk::Button::with_label("Export Results to JSON...");
        export_json_btn.add_css_class("flat");
        exp_box.append(&export_csv_btn);
        exp_box.append(&export_json_btn);
        export_popover.set_child(Some(&exp_box));

        let export_menu_btn = gtk::MenuButton::new();
        export_menu_btn.set_icon_name("document-save-symbolic");
        export_menu_btn.set_tooltip_text(Some("Export Query Results"));
        export_menu_btn.set_popover(Some(&export_popover));
        export_menu_btn.set_sensitive(false);

        results_bar.append(&results_status);
        results_bar.append(&export_menu_btn);

        bottom_box.append(&results_bar);
        bottom_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let results_grid = gtk::Grid::new();
        results_grid.add_css_class("spreadsheet-grid");
        results_grid.set_row_spacing(0);
        results_grid.set_column_spacing(0);
        results_grid.set_halign(gtk::Align::Start);
        results_grid.set_valign(gtk::Align::Start);

        let results_scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&results_grid)
            .build();
        bottom_box.append(&results_scroll);

        paned.set_start_child(Some(&top_box));
        paned.set_end_child(Some(&bottom_box));

        let tab = Self {
            root: paned,
            buffer: buffer.clone(),
            view: view.clone(),
            results_scroll: results_scroll.clone(),
            results_grid,
            results_status,
            export_menu_btn: export_menu_btn.clone(),
            current_db: Rc::new(RefCell::new(String::new())),
            last_result: Rc::new(RefCell::new(None)),
            col_widths: Rc::new(RefCell::new(HashMap::new())),
            natural_col_widths: Rc::new(RefCell::new(Vec::new())),
            header_cells: Rc::new(RefCell::new(Vec::new())),
            last_viewport_width: Rc::new(RefCell::new(0)),
            index_col_width: Rc::new(RefCell::new(DEFAULT_INDEX_COL_WIDTH)),
            cmd_tx,
        };

        // Recompute auto column widths when the results viewport width changes
        {
            let tab_clone = tab.clone();
            results_scroll
                .hadjustment()
                .connect_page_size_notify(move |_| {
                    let tab_for_idle = tab_clone.clone();
                    gtk::glib::idle_add_local_once(move || {
                        tab_for_idle.reapply_column_widths_if_viewport_changed();
                    });
                });
        }
        {
            let tab_clone = tab.clone();
            results_scroll.connect_map(move |_| {
                let tab_for_idle = tab_clone.clone();
                gtk::glib::idle_add_local_once(move || {
                    tab_for_idle.reapply_column_widths_if_viewport_changed();
                });
            });
        }

        // Update Vim mode & command bar labels
        {
            let vim_mode_lbl = vim_mode_lbl.clone();
            vim_im.connect_command_bar_text_notify(move |vim| {
                let text = vim.command_bar_text();
                if text.is_empty() {
                    vim_mode_lbl.set_text("-- NORMAL --");
                } else {
                    vim_mode_lbl.set_text(&text);
                }
            });
        }
        {
            let vim_cmd_lbl = vim_cmd_lbl.clone();
            vim_im.connect_command_text_notify(move |vim| {
                vim_cmd_lbl.set_text(&vim.command_text());
            });
        }

        // Update cursor line / column indicator
        {
            let cursor_pos_lbl = cursor_pos_lbl.clone();
            buffer.connect_cursor_position_notify(move |buf| {
                let iter = buf.iter_at_offset(buf.cursor_position());
                cursor_pos_lbl.set_text(&format!(
                    "Ln {}, Col {}",
                    iter.line() + 1,
                    iter.line_offset() + 1
                ));
            });
        }

        // Vim ':w' signal -> open Save SQL file dialog
        let save_triggered_in_cmd = Rc::new(RefCell::new(false));
        {
            let tab_clone = tab.clone();
            let on_toast = on_toast.clone();
            let save_flag = save_triggered_in_cmd.clone();
            vim_im.connect_write(move |_, view_widget, _path| {
                if *save_flag.borrow() {
                    *save_flag.borrow_mut() = false;
                    return;
                }
                tab_clone.prompt_save_sql_file(view_widget.upcast_ref::<gtk::Widget>(), on_toast.clone());
            });
        }

        // Vim Ex commands: ':r' to run query, ':w' to open file save dialog
        {
            let tab_clone = tab.clone();
            let on_toast = on_toast.clone();
            let save_flag = save_triggered_in_cmd.clone();
            vim_im.connect_execute_command(move |_, cmd| {
                let normalized = cmd.trim().trim_start_matches(':').trim();
                if normalized == "r" || normalized == "run" {
                    tab_clone.execute_current_query();
                    return true;
                }
                if normalized == "w" || normalized == "write" || normalized.starts_with("w ") {
                    *save_flag.borrow_mut() = true;
                    tab_clone.prompt_save_sql_file(
                        tab_clone.view.upcast_ref::<gtk::Widget>(),
                        on_toast.clone(),
                    );
                    return true;
                }
                false
            });
        }

        // Ctrl+Enter shortcut to run query
        {
            let key_ctrl = gtk::EventControllerKey::new();
            key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
            let tab_clone = tab.clone();
            key_ctrl.connect_key_pressed(move |_, keyval, _, state| {
                let is_ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                if is_ctrl
                    && (keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::KP_Enter)
                {
                    tab_clone.execute_current_query();
                    return gtk::glib::Propagation::Stop;
                }
                gtk::glib::Propagation::Proceed
            });
            view.add_controller(key_ctrl);
        }

        // Toolbar Run button
        {
            let tab_clone = tab.clone();
            run_btn.connect_clicked(move |_| {
                tab_clone.execute_current_query();
            });
        }

        // Toolbar Save (:w) button
        {
            let tab_clone = tab.clone();
            let on_toast = on_toast.clone();
            save_btn.connect_clicked(move |btn| {
                tab_clone.prompt_save_sql_file(btn.upcast_ref::<gtk::Widget>(), on_toast.clone());
            });
        }

        // Toolbar Open .sql button
        {
            let tab_clone = tab.clone();
            let on_toast = on_toast.clone();
            open_btn.connect_clicked(move |btn| {
                tab_clone.prompt_open_sql_file(btn.upcast_ref::<gtk::Widget>(), on_toast.clone());
            });
        }

        // Export results CSV / JSON
        {
            let tab_clone = tab.clone();
            let export_popover = export_popover.clone();
            let on_toast = on_toast.clone();
            export_csv_btn.connect_clicked(move |btn| {
                export_popover.popdown();
                tab_clone.export_results(btn.upcast_ref::<gtk::Widget>(), true, on_toast.clone());
            });
        }
        {
            let tab_clone = tab.clone();
            let export_popover = export_popover.clone();
            let on_toast = on_toast.clone();
            export_json_btn.connect_clicked(move |btn| {
                export_popover.popdown();
                tab_clone.export_results(btn.upcast_ref::<gtk::Widget>(), false, on_toast.clone());
            });
        }

        tab
    }

    fn apply_editor_style_scheme(buffer: &sourceview5::Buffer) {
        let style_mgr = adw::StyleManager::default();
        let scheme_mgr = sourceview5::StyleSchemeManager::default();
        let candidates = if style_mgr.is_dark() {
            ["Adwaita-dark", "classic-dark", "oblivion"]
        } else {
            ["Adwaita", "classic", "tango"]
        };
        for name in candidates {
            if let Some(scheme) = scheme_mgr.scheme(name) {
                buffer.set_style_scheme(Some(&scheme));
                break;
            }
        }
    }

    pub fn set_database(&self, db_name: &str) {
        *self.current_db.borrow_mut() = db_name.to_string();
    }

    pub fn execute_current_query(&self) {
        let sql = if let Some((start, end)) = self.buffer.selection_bounds() {
            let sel = self.buffer.text(&start, &end, false).to_string();
            if sel.trim().is_empty() {
                let (s, e) = self.buffer.bounds();
                self.buffer.text(&s, &e, false).to_string()
            } else {
                sel
            }
        } else {
            let (s, e) = self.buffer.bounds();
            self.buffer.text(&s, &e, false).to_string()
        };

        if sql.trim().is_empty() {
            return;
        }

        self.results_status.set_text("Executing query...");
        let db = self.current_db.borrow().clone();
        let _ = self.cmd_tx.send_blocking(BackendCommand::ExecuteRawQuery {
            database: db,
            sql,
        });
    }

    pub fn prompt_save_sql_file<F>(&self, parent: &gtk::Widget, on_toast: F)
    where
        F: Fn(&str) + Clone + 'static,
    {
        let (start, end) = self.buffer.bounds();
        let sql_content = self.buffer.text(&start, &end, false).to_string();

        let dialog = gtk::FileDialog::builder()
            .title("Save SQL Query")
            .initial_name("query.sql")
            .build();

        let window = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        dialog.save(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    match fs::write(&path, sql_content.as_bytes()) {
                        Ok(()) => on_toast(&format!("Saved query to {}", path.display())),
                        Err(e) => on_toast(&format!("Failed to save file: {}", e)),
                    }
                }
            }
        });
    }

    fn prompt_open_sql_file<F>(&self, parent: &gtk::Widget, on_toast: F)
    where
        F: Fn(&str) + Clone + 'static,
    {
        let dialog = gtk::FileDialog::builder()
            .title("Open SQL File")
            .build();
        let window = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let buffer = self.buffer.clone();
        dialog.open(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    match fs::read_to_string(&path) {
                        Ok(content) => {
                            buffer.set_text(&content);
                            on_toast(&format!("Loaded {}", path.display()));
                        }
                        Err(e) => on_toast(&format!("Failed to read file: {}", e)),
                    }
                }
            }
        });
    }

    fn available_viewport_width(&self) -> i32 {
        let hadj_w = self.results_scroll.hadjustment().page_size().round() as i32;
        if hadj_w > 0 {
            return hadj_w;
        }
        let scroll_w = self.results_scroll.width();
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

        let manual_map = self.col_widths.borrow();
        let manual_overrides: Vec<Option<i32>> = header_cells
            .iter()
            .map(|(col_idx, _)| manual_map.get(col_idx).copied())
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

    pub fn show_outcome(&self, res: Result<RawQueryOutcome, String>) {
        while let Some(child) = self.results_grid.first_child() {
            self.results_grid.remove(&child);
        }
        self.header_cells.borrow_mut().clear();
        self.natural_col_widths.borrow_mut().clear();

        match res {
            Ok(RawQueryOutcome::Rows(result_set)) => {
                let cols_changed = self
                    .last_result
                    .borrow()
                    .as_ref()
                    .map(|prev| prev.columns != result_set.columns)
                    .unwrap_or(false);
                if cols_changed {
                    self.col_widths.borrow_mut().clear();
                }

                self.results_status.set_text(&format!(
                    "Query Results — {} row(s) returned in {} ms",
                    result_set.rows.len(),
                    result_set.execution_time_ms
                ));
                self.export_menu_btn
                    .set_sensitive(!result_set.columns.is_empty());

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
                self.results_grid.attach(&corner_cell, 0, 0, 1, 1);

                let mut header_labels: Vec<(gtk::Label, String)> =
                    Vec::with_capacity(result_set.columns.len());

                for (col_idx, col_name) in result_set.columns.iter().enumerate() {
                    let hdr_lbl = gtk::Label::new(Some(col_name));
                    hdr_lbl.add_css_class("heading");
                    hdr_lbl.set_halign(gtk::Align::Fill);
                    hdr_lbl.set_xalign(0.0);
                    hdr_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    hdr_lbl.set_max_width_chars(1);
                    hdr_lbl.set_margin_top(6);
                    hdr_lbl.set_margin_bottom(6);
                    hdr_lbl.set_margin_start(10);
                    hdr_lbl.set_margin_end(6);
                    hdr_lbl.set_tooltip_text(Some(col_name));

                    let col_w = self
                        .col_widths
                        .borrow()
                        .get(&col_idx)
                        .copied()
                        .unwrap_or(DEFAULT_DATA_COL_WIDTH);
                    let col_widths_ref = self.col_widths.clone();
                    let hdr_cell = build_resizable_header_cell(
                        &hdr_lbl,
                        col_w,
                        Some("spreadsheet-header-box"),
                        move |new_w| {
                            col_widths_ref.borrow_mut().insert(col_idx, new_w);
                        },
                    );

                    self.results_grid
                        .attach(&hdr_cell, (col_idx + 1) as i32, 0, 1, 1);
                    header_labels.push((hdr_lbl, col_name.clone()));
                    self.header_cells.borrow_mut().push((col_idx, hdr_cell));
                }

                let mut sample_entry: Option<gtk::Entry> = None;

                for (row_idx, row_vals) in result_set.rows.iter().enumerate() {
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
                    self.results_grid.attach(&num_lbl, 0, grid_row, 1, 1);

                    for col_idx in 0..result_set.columns.len() {
                        let val = row_vals.get(col_idx).unwrap_or(&DbValue::Null);

                        let cell_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                        cell_box.add_css_class("spreadsheet-cell");
                        cell_box.set_overflow(gtk::Overflow::Hidden);

                        let cell = gtk::Entry::new();
                        cell.set_has_frame(false);
                        cell.set_editable(false);
                        cell.add_css_class("spreadsheet-cell-entry");
                        cell.add_css_class("monospace");
                        cell.set_hexpand(true);
                        cell.set_width_chars(1);
                        cell.set_max_width_chars(1);
                        cell.set_text(&format_long_text_preview(val));
                        if val.is_null() {
                            cell.add_css_class("cell-null");
                        }

                        let click = gtk::GestureClick::new();
                        let entry_weak = cell.downgrade();
                        click.connect_released(move |_, n_press, _, _| {
                            if n_press == 1
                                && let Some(e) = entry_weak.upgrade()
                            {
                                let e_clone = e.clone();
                                gtk::glib::idle_add_local_once(move || {
                                    e_clone.grab_focus();
                                    e_clone.select_region(0, -1);
                                });
                            }
                        });
                        cell.add_controller(click);

                        if sample_entry.is_none() {
                            sample_entry = Some(cell.clone());
                        }

                        cell_box.append(&cell);
                        self.results_grid
                            .attach(&cell_box, (col_idx + 1) as i32, grid_row, 1, 1);
                    }
                }

                // Compute natural full-content width for each column
                let mut natural_widths = Vec::with_capacity(result_set.columns.len());
                for (col_idx, (hdr_lbl, hdr_text)) in header_labels.iter().enumerate() {
                    let hdr_w =
                        measure_single_line_text(hdr_lbl, hdr_text) + HEADER_EXTRA_PADDING;

                    let mut widest_val: Option<&DbValue> = None;
                    let mut max_score: usize = 0;

                    for row_vals in &result_set.rows {
                        let val = row_vals.get(col_idx).unwrap_or(&DbValue::Null);
                        let txt = val.display_text();
                        let score: usize = txt
                            .chars()
                            .take(MAX_MEASURE_CHARS)
                            .map(|c| if c.is_ascii() { 1 } else { 2 })
                            .sum();
                        if widest_val.is_none() || score > max_score {
                            max_score = score;
                            widest_val = Some(val);
                        }
                    }

                    let cell_w = if let Some(val) = widest_val {
                        let preview = format_long_text_preview(val);
                        let text_px = if let Some(ref entry_widget) = sample_entry {
                            measure_single_line_text(entry_widget, &preview)
                        } else {
                            measure_single_line_text(hdr_lbl, &preview)
                        };
                        let raw_cell_w = text_px + CELL_EXTRA_PADDING;
                        let is_long = matches!(
                            val,
                            DbValue::Text(s) if s.contains('\n') || s.contains('\r') || s.chars().count() > 255
                        );
                        if is_long {
                            raw_cell_w.min(MAX_LONG_TEXT_AUTO_COL_WIDTH)
                        } else {
                            raw_cell_w
                        }
                    } else {
                        0
                    };

                    natural_widths.push(hdr_w.max(cell_w).max(MIN_COLUMN_WIDTH));
                }

                *self.natural_col_widths.borrow_mut() = natural_widths;
                self.apply_column_widths();

                let tab_for_idle = self.clone();
                gtk::glib::idle_add_local_once(move || {
                    tab_for_idle.reapply_column_widths_if_viewport_changed();
                });

                *self.last_result.borrow_mut() = Some(result_set);
            }
            Ok(RawQueryOutcome::Executed {
                rows_affected,
                execution_time_ms,
                message,
            }) => {
                *self.last_result.borrow_mut() = None;
                self.export_menu_btn.set_sensitive(false);
                self.results_status.set_text(&format!(
                    "Executed — {} row(s) affected ({} ms)",
                    rows_affected, execution_time_ms
                ));

                let msg_lbl = gtk::Label::new(Some(&message));
                msg_lbl.add_css_class("success");
                msg_lbl.set_margin_top(20);
                msg_lbl.set_margin_start(20);
                self.results_grid.attach(&msg_lbl, 0, 0, 1, 1);
            }
            Err(err) => {
                *self.last_result.borrow_mut() = None;
                self.export_menu_btn.set_sensitive(false);
                self.results_status.set_text("Query Error");

                let err_lbl = gtk::Label::new(Some(&err));
                err_lbl.add_css_class("error");
                err_lbl.add_css_class("monospace");
                err_lbl.set_wrap(true);
                err_lbl.set_selectable(true);
                err_lbl.set_margin_top(16);
                err_lbl.set_margin_start(16);
                err_lbl.set_margin_end(16);
                self.results_grid.attach(&err_lbl, 0, 0, 1, 1);
            }
        }
    }

    fn export_results<F>(&self, parent: &gtk::Widget, as_csv: bool, on_toast: F)
    where
        F: Fn(&str) + Clone + 'static,
    {
        let res_opt = self.last_result.borrow().clone();
        let Some(res_set) = res_opt else {
            return;
        };

        let ext = if as_csv { "csv" } else { "json" };
        let dialog = gtk::FileDialog::builder()
            .title("Export Query Results")
            .initial_name(format!("query_results.{}", ext))
            .build();

        let window = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        dialog.save(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    let write_res = if as_csv {
                        export_resultset_csv(&res_set, &path)
                    } else {
                        export_resultset_json(&res_set, &path)
                    };
                    match write_res {
                        Ok(()) => on_toast(&format!("Exported results to {}", path.display())),
                        Err(e) => on_toast(&format!("Export failed: {}", e)),
                    }
                }
            }
        });
    }

    #[cfg(test)]
    pub fn set_sql_text(&self, sql: &str) {
        self.buffer.set_text(sql);
        let end = self.buffer.end_iter();
        self.buffer.place_cursor(&end);
    }
}

fn export_resultset_csv(rs: &RawQueryResultSet, path: &std::path::Path) -> anyhow::Result<()> {
    let mut wtr = csv::Writer::from_path(path)?;
    wtr.write_record(&rs.columns)?;
    for row in &rs.rows {
        let rec: Vec<&str> = row
            .iter()
            .map(|v| match v {
                DbValue::Null => "",
                DbValue::Text(s) => s.as_str(),
            })
            .collect();
        wtr.write_record(&rec)?;
    }
    wtr.flush()?;
    Ok(())
}

fn export_resultset_json(rs: &RawQueryResultSet, path: &std::path::Path) -> anyhow::Result<()> {
    let mut out = Vec::with_capacity(rs.rows.len());
    for row in &rs.rows {
        let mut obj = serde_json::Map::new();
        for (idx, col) in rs.columns.iter().enumerate() {
            let val = match row.get(idx) {
                Some(DbValue::Text(s)) => serde_json::Value::String(s.clone()),
                _ => serde_json::Value::Null,
            };
            obj.insert(col.clone(), val);
        }
        out.push(serde_json::Value::Object(obj));
    }
    let formatted = serde_json::to_string_pretty(&out)?;
    let mut f = File::create(path)?;
    f.write_all(formatted.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_results_header_and_column_widths_match() {
        // Verify that auto column widths and manual overrides work identically for QueryTab
        let natural = vec![120, 180, 90, 210];
        let manual = vec![None, Some(240), None, None];
        let widths = compute_auto_column_widths(&natural, &manual, 800);
        assert_eq!(widths.len(), 4);
        assert_eq!(widths[1], 240);
        assert!(widths.iter().all(|&w| w >= MIN_COLUMN_WIDTH));
    }

    #[test]
    #[ignore]
    fn verify_live_gtk_query_results_column_alignment() {
        use crate::db::models::TableInfo;
        use crate::ui::sidebar::WorkspaceSidebar;
        use crate::ui::workspace::WorkspaceContent;
        use adw::prelude::*;

        let _ = adw::init();
        sourceview5::init();
        crate::app::load_css();
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_icon_theme_name(Some("Adwaita"));
        }

        let (cmd_tx, _cmd_rx) = async_channel::unbounded();
        let sidebar = WorkspaceSidebar::new(cmd_tx.clone());
        let workspace_content = WorkspaceContent::new(cmd_tx, |_| {});

        let tables = vec![
            TableInfo { name: "audit_logs".into(), is_view: false },
            TableInfo { name: "coupons".into(), is_view: false },
            TableInfo { name: "customers".into(), is_view: false },
            TableInfo { name: "invoices".into(), is_view: false },
            TableInfo { name: "order_items".into(), is_view: false },
            TableInfo { name: "orders".into(), is_view: false },
            TableInfo { name: "payments".into(), is_view: false },
            TableInfo { name: "products".into(), is_view: false },
            TableInfo { name: "shipments".into(), is_view: false },
            TableInfo { name: "daily_revenue_summary".into(), is_view: true },
            TableInfo { name: "vip_customer_ltv".into(), is_view: true },
        ];
        sidebar.set_connection_state(
            "Production Store (SSH)",
            crate::config::bookmarks::DbEngine::Postgres,
            "PostgreSQL (via SSH)",
            &["commerce_prod".to_string()],
            Some("commerce_prod"),
            tables,
        );
        sidebar.set_selected_table("orders");

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar.page)
            .content(&workspace_content.page)
            .min_sidebar_width(240.0)
            .max_sidebar_width(320.0)
            .build();

        workspace_content.stack.set_visible_child_name("query");
        workspace_content.query_tab.set_sql_text(
            "-- Top customers by lifetime order volume (Vim mode: ':r' to run, ':w' to save)\n\
             SELECT\n  \
               c.email,\n  \
               c.full_name,\n  \
               c.country,\n  \
               COUNT(o.id) AS total_orders,\n  \
               SUM(o.total_eur) AS lifetime_revenue_eur,\n  \
               MAX(o.created_at) AS last_order_at\n\
             FROM customers c\n\
             JOIN orders o ON o.customer_id = c.id\n\
             WHERE o.status IN ('shipped', 'delivered')\n\
             GROUP BY c.id, c.email, c.full_name, c.country\n\
             ORDER BY lifetime_revenue_eur DESC\n\
             LIMIT 6;\n",
        );

        let rs = RawQueryResultSet {
            columns: vec![
                "email".into(),
                "full_name".into(),
                "country".into(),
                "total_orders".into(),
                "lifetime_revenue_eur".into(),
                "last_order_at".into(),
            ],
            rows: vec![
                vec![
                    DbValue::Text("elena.rostova@zurich-labs.ch".into()),
                    DbValue::Text("Elena Rostova".into()),
                    DbValue::Text("CH".into()),
                    DbValue::Text("19".into()),
                    DbValue::Text("14,820.50".into()),
                    DbValue::Text("2026-10-14 10:12:04".into()),
                ],
                vec![
                    DbValue::Text("clara.oswald@tardis.co.uk".into()),
                    DbValue::Text("Clara Oswald".into()),
                    DbValue::Text("DE".into()),
                    DbValue::Text("14".into()),
                    DbValue::Text("6,340.90".into()),
                    DbValue::Text("2026-10-14 10:25:19".into()),
                ],
                vec![
                    DbValue::Text("hannah.nielsen@windenergy.dk".into()),
                    DbValue::Text("Hannah Nielsen".into()),
                    DbValue::Text("AT".into()),
                    DbValue::Text("11".into()),
                    DbValue::Text("4,915.20".into()),
                    DbValue::Text("2026-10-14 11:03:41".into()),
                ],
                vec![
                    DbValue::Text("marcus.vance@cloudscale.io".into()),
                    DbValue::Text("Marcus Vance".into()),
                    DbValue::Text("DE".into()),
                    DbValue::Text("9".into()),
                    DbValue::Text("3,180.00".into()),
                    DbValue::Text("2026-10-12 16:40:12".into()),
                ],
                vec![
                    DbValue::Text("jonas.kahnwald@winden-power.de".into()),
                    DbValue::Text("Jonas Kahnwald".into()),
                    DbValue::Text("DE".into()),
                    DbValue::Text("7".into()),
                    DbValue::Text("2,410.75".into()),
                    DbValue::Text("2026-10-10 14:05:55".into()),
                ],
                vec![
                    DbValue::Text("lukas.weber@main-logistics.de".into()),
                    DbValue::Text("Lukas Weber".into()),
                    DbValue::Text("DE".into()),
                    DbValue::Text("5".into()),
                    DbValue::Text("1,690.40".into()),
                    DbValue::Text("2026-10-08 09:18:30".into()),
                ],
            ],
            execution_time_ms: 4,
        };
        workspace_content
            .query_tab
            .show_outcome(Ok(RawQueryOutcome::Rows(rs)));

        let win = adw::Window::builder()
            .title("DBased")
            .default_width(1440)
            .default_height(920)
            .content(&split)
            .build();
        win.present();

        let ctx = gtk::glib::MainContext::default();
        for _ in 0..80 {
            while ctx.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let grid = &workspace_content.query_tab.results_grid;
        for col in 0..=6 {
            let hdr = grid.child_at(col, 0).expect("header widget");
            let hdr_bounds = hdr.compute_bounds(grid).expect("hdr bounds");
            assert!(
                hdr_bounds.width() >= MIN_COLUMN_WIDTH as f32,
                "col {} hdr_w={}",
                col,
                hdr_bounds.width()
            );
            for row in 1..=6 {
                let cell = grid.child_at(col, row).expect("cell widget");
                let cell_bounds = cell.compute_bounds(grid).expect("cell bounds");
                assert_eq!(
                    hdr_bounds.x(),
                    cell_bounds.x(),
                    "Column {} header x ({}) != row {} cell x ({})",
                    col,
                    hdr_bounds.x(),
                    row,
                    cell_bounds.x()
                );
                assert_eq!(
                    hdr_bounds.width(),
                    cell_bounds.width(),
                    "Column {} header width ({}) != row {} cell width ({})",
                    col,
                    hdr_bounds.width(),
                    row,
                    cell_bounds.width()
                );
            }
            println!(
                "Column {}: hdr_x={} hdr_w={} == cell_w={}",
                col,
                hdr_bounds.x(),
                hdr_bounds.width(),
                hdr_bounds.width()
            );
        }

        win.close();
    }
}
