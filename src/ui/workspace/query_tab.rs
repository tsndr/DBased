use crate::db::models::{BackendCommand, DbValue, RawQueryOutcome, RawQueryResultSet};
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::fs::{self, File};
use std::io::Write;
use std::rc::Rc;

#[derive(Clone)]
pub struct QueryTab {
    pub root: gtk::Paned,
    buffer: sourceview5::Buffer,
    view: sourceview5::View,
    results_grid: gtk::Grid,
    results_status: gtk::Label,
    export_menu_btn: gtk::MenuButton,
    current_db: Rc<RefCell<String>>,
    last_result: Rc<RefCell<Option<RawQueryResultSet>>>,
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
            results_grid,
            results_status,
            export_menu_btn: export_menu_btn.clone(),
            current_db: Rc::new(RefCell::new(String::new())),
            last_result: Rc::new(RefCell::new(None)),
            cmd_tx,
        };

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

    pub fn show_outcome(&self, res: Result<RawQueryOutcome, String>) {
        while let Some(child) = self.results_grid.first_child() {
            self.results_grid.remove(&child);
        }

        match res {
            Ok(RawQueryOutcome::Rows(result_set)) => {
                self.results_status.set_text(&format!(
                    "Query Results — {} row(s) returned in {} ms",
                    result_set.rows.len(),
                    result_set.execution_time_ms
                ));
                self.export_menu_btn
                    .set_sensitive(!result_set.columns.is_empty());

                // Header row
                let corner = gtk::Label::new(Some("#"));
                corner.add_css_class("spreadsheet-header-cell");
                corner.add_css_class("dim-label");
                corner.set_width_request(48);
                self.results_grid.attach(&corner, 0, 0, 1, 1);

                for (col_idx, col_name) in result_set.columns.iter().enumerate() {
                    let hdr = gtk::Label::new(Some(col_name));
                    hdr.add_css_class("spreadsheet-header-cell");
                    hdr.add_css_class("heading");
                    hdr.set_halign(gtk::Align::Start);
                    hdr.set_width_request(150);
                    self.results_grid.attach(&hdr, (col_idx + 1) as i32, 0, 1, 1);
                }

                for (row_idx, row_vals) in result_set.rows.iter().enumerate() {
                    let grid_row = (row_idx + 1) as i32;
                    let num_lbl = gtk::Label::new(Some(&(row_idx + 1).to_string()));
                    num_lbl.add_css_class("spreadsheet-row-num");
                    num_lbl.add_css_class("monospace");
                    num_lbl.add_css_class("caption");
                    num_lbl.add_css_class("dim-label");
                    self.results_grid.attach(&num_lbl, 0, grid_row, 1, 1);

                    for (col_idx, val) in row_vals.iter().enumerate() {
                        let cell = gtk::Entry::new();
                        cell.set_has_frame(false);
                        cell.set_editable(false);
                        cell.add_css_class("spreadsheet-cell");
                        cell.add_css_class("monospace");
                        cell.set_width_chars(16);
                        cell.set_text(val.display_text());
                        if val.is_null() {
                            cell.add_css_class("cell-null");
                        }
                        self.results_grid
                            .attach(&cell, (col_idx + 1) as i32, grid_row, 1, 1);
                    }
                }

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
