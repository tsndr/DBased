use crate::db::driver::escape_sql_literal;
use crate::db::models::{
    BackendCommand, ColumnInfo, ContentQuery, DbValue, ForeignKeyInfo, PrimaryKeyValue, RowUpdate,
    SortDirection, TablePage,
};
use crate::ui::workspace::{
    CELL_EXTRA_PADDING, DEFAULT_DATA_COL_WIDTH, HEADER_EXTRA_PADDING,
    MAX_LONG_TEXT_AUTO_COL_WIDTH, MAX_MEASURE_CHARS, MIN_COLUMN_WIDTH,
    build_resizable_header_cell, build_spreadsheet_scrolled_window, clear_spreadsheet_grid,
    compute_auto_column_widths, format_long_text_preview, measure_single_line_text,
};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::rc::Rc;

const DEFAULT_INDEX_COL_WIDTH: i32 = 64;
const DEFAULT_LONG_TEXT_PANEL_WIDTH: i32 = 380;
const FK_BTN_EXTRA_WIDTH: i32 = 30;
const ENUM_DROPDOWN_EXTRA_WIDTH: i32 = 28;
const MOUSE_BUTTON_BACK: u32 = 8;
const MOUSE_BUTTON_FORWARD: u32 = 9;

#[derive(Debug, Clone)]
struct NavigationEntry {
    database: String,
    table: String,
    query: ContentQuery,
}

#[derive(Clone)]
enum LongTextTargetKind {
    Existing {
        row_idx: usize,
        original_val: DbValue,
        editable: bool,
    },
    Draft {
        draft_idx: usize,
        initial_val: DbValue,
    },
}

#[derive(Clone)]
struct LongTextPanelTarget {
    kind: LongTextTargetKind,
    col_info: ColumnInfo,
    cell_box: gtk::Box,
    cell_entry: gtk::Entry,
    ref_btn_opt: Option<gtk::Button>,
    fk_opt: Option<ForeignKeyInfo>,
}

impl LongTextPanelTarget {
    fn is_editable(&self) -> bool {
        match &self.kind {
            LongTextTargetKind::Existing { editable, .. } => *editable,
            LongTextTargetKind::Draft { .. } => true,
        }
    }

    fn is_draft(&self) -> bool {
        matches!(self.kind, LongTextTargetKind::Draft { .. })
    }

    fn baseline_value(&self) -> &DbValue {
        match &self.kind {
            LongTextTargetKind::Existing { original_val, .. } => original_val,
            LongTextTargetKind::Draft { initial_val, .. } => initial_val,
        }
    }
}

#[derive(Clone)]
pub struct ContentTab {
    pub root: gtk::Box,
    pub(crate) content_paned: gtk::Paned,
    scroll: gtk::ScrolledWindow,
    pub(crate) grid: gtk::Grid,
    editor_panel: gtk::Box,
    panel_title_lbl: gtk::Label,
    panel_subtitle_lbl: gtk::Label,
    panel_status_lbl: gtk::Label,
    panel_null_btn: gtk::Button,
    panel_revert_btn: gtk::Button,
    pub(crate) panel_buffer: sourceview5::Buffer,
    panel_view: sourceview5::View,
    panel_target: Rc<RefCell<Option<LongTextPanelTarget>>>,
    panel_syncing: Rc<RefCell<bool>>,
    panel_initial_pos_set: Rc<RefCell<bool>>,
    nav_back_btn: gtk::Button,
    nav_forward_btn: gtk::Button,
    search_entry: gtk::SearchEntry,
    status_label: gtk::Label,
    dirty_badge_btn: gtk::Button,
    prev_page_btn: gtk::Button,
    next_page_btn: gtk::Button,
    info_banner: adw::Banner,
    current_page: Rc<RefCell<Option<TablePage>>>,
    relations: Rc<RefCell<Vec<ForeignKeyInfo>>>,
    query_state: Rc<RefCell<ContentQuery>>,
    nav_back_stack: Rc<RefCell<Vec<NavigationEntry>>>,
    nav_forward_stack: Rc<RefCell<Vec<NavigationEntry>>>,
    nav_in_progress: Rc<RefCell<bool>>,
    /// Maps (row_idx, column_name) -> edited DbValue for existing database rows
    dirty_cells: Rc<RefCell<HashMap<(usize, String), DbValue>>>,
    /// Maps (new_row_idx, column_name) -> edited DbValue for newly added draft rows
    draft_rows: Rc<RefCell<Vec<HashMap<String, DbValue>>>>,
    /// Checked row indices for deletion
    selected_rows: Rc<RefCell<Vec<(usize, gtk::CheckButton)>>>,
    /// User-resized widths per column name
    pub(crate) col_widths: Rc<RefCell<HashMap<String, i32>>>,
    /// Natural full-content widths per column index for the currently rendered grid
    natural_col_widths: Rc<RefCell<Vec<i32>>>,
    /// Header cell widgets per column for dynamic width updates
    header_cells: Rc<RefCell<Vec<(String, gtk::Box)>>>,
    /// Last viewport width used to compute auto column widths
    last_viewport_width: Rc<RefCell<i32>>,
    /// User-resized width for the '#' index column
    index_col_width: Rc<RefCell<i32>>,
    cmd_tx: Sender<BackendCommand>,
}

impl ContentTab {
    pub fn new<F>(cmd_tx: Sender<BackendCommand>, on_toast: F) -> Self
    where
        F: Fn(&str) + Clone + 'static,
    {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_hexpand(true);

        // Top toolbar
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        toolbar.set_margin_top(8);
        toolbar.set_margin_bottom(8);
        toolbar.set_margin_start(12);
        toolbar.set_margin_end(12);

        let nav_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        nav_box.add_css_class("linked");

        let nav_back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
        nav_back_btn.set_tooltip_text(Some("Navigate Back (Mouse Back / Alt+Left)"));
        nav_back_btn.set_sensitive(false);

        let nav_forward_btn = gtk::Button::from_icon_name("go-next-symbolic");
        nav_forward_btn.set_tooltip_text(Some("Navigate Forward (Mouse Forward / Alt+Right)"));
        nav_forward_btn.set_sensitive(false);

        nav_box.append(&nav_back_btn);
        nav_box.append(&nav_forward_btn);

        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("Search rows or filter (e.g. WHERE id > 10)..."));
        search_entry.set_hexpand(true);

        let add_row_btn = gtk::Button::from_icon_name("list-add-symbolic");
        add_row_btn.set_label("Add Row");
        add_row_btn.set_tooltip_text(Some("Append a new row to the table"));

        let delete_row_btn = gtk::Button::from_icon_name("user-trash-symbolic");
        delete_row_btn.add_css_class("destructive-action");
        delete_row_btn.set_tooltip_text(Some("Delete Checked Row(s)"));

        let dirty_badge_btn = gtk::Button::with_label("Commit (Ctrl+S)");
        dirty_badge_btn.add_css_class("suggested-action");
        dirty_badge_btn.set_visible(false);
        dirty_badge_btn.set_tooltip_text(Some("Commit edited cells to database (Ctrl+S)"));

        // Export popover (CSV / JSON)
        let export_popover = gtk::Popover::new();
        let exp_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        exp_box.set_margin_top(8);
        exp_box.set_margin_bottom(8);
        exp_box.set_margin_start(8);
        exp_box.set_margin_end(8);
        let export_csv_btn = gtk::Button::with_label("Export to CSV...");
        export_csv_btn.add_css_class("flat");
        let export_json_btn = gtk::Button::with_label("Export to JSON...");
        export_json_btn.add_css_class("flat");
        exp_box.append(&export_csv_btn);
        exp_box.append(&export_json_btn);
        export_popover.set_child(Some(&exp_box));

        let export_menu_btn = gtk::MenuButton::new();
        export_menu_btn.set_icon_name("document-save-symbolic");
        export_menu_btn.set_tooltip_text(Some("Export Table Content (CSV / JSON)"));
        export_menu_btn.set_popover(Some(&export_popover));

        let refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_btn.set_tooltip_text(Some("Refresh Table Content"));

        toolbar.append(&nav_box);
        toolbar.append(&search_entry);
        toolbar.append(&dirty_badge_btn);
        toolbar.append(&add_row_btn);
        toolbar.append(&delete_row_btn);
        toolbar.append(&export_menu_btn);
        toolbar.append(&refresh_btn);

        root.append(&toolbar);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let info_banner = adw::Banner::new(
            "This table has no Primary Key; inline cell editing and row deletion are disabled.",
        );
        info_banner.set_revealed(false);
        root.append(&info_banner);

        // Spreadsheet Grid inside ScrolledWindow
        let grid = gtk::Grid::new();
        grid.add_css_class("spreadsheet-grid");
        grid.set_row_spacing(0);
        grid.set_column_spacing(0);
        grid.set_halign(gtk::Align::Start);
        grid.set_valign(gtk::Align::Start);

        let scroll = build_spreadsheet_scrolled_window(&grid);

        // Separate Multiline Editor Panel for Long Text Columns
        let editor_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        editor_panel.add_css_class("long-text-panel");
        editor_panel.set_width_request(300);
        editor_panel.set_vexpand(true);
        editor_panel.set_visible(false);

        let panel_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        panel_header.add_css_class("long-text-panel-header");
        panel_header.set_margin_top(0);
        panel_header.set_margin_bottom(0);

        let panel_header_inner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        panel_header_inner.set_margin_top(8);
        panel_header_inner.set_margin_bottom(8);
        panel_header_inner.set_margin_start(12);
        panel_header_inner.set_margin_end(8);
        panel_header_inner.set_hexpand(true);

        let panel_title_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        panel_title_box.set_hexpand(true);
        panel_title_box.set_valign(gtk::Align::Center);

        let panel_title_lbl = gtk::Label::new(Some("Column"));
        panel_title_lbl.add_css_class("heading");
        panel_title_lbl.add_css_class("monospace");
        panel_title_lbl.set_halign(gtk::Align::Start);
        panel_title_lbl.set_xalign(0.0);
        panel_title_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let panel_subtitle_lbl = gtk::Label::new(Some("TEXT"));
        panel_subtitle_lbl.add_css_class("caption");
        panel_subtitle_lbl.add_css_class("dim-label");
        panel_subtitle_lbl.set_halign(gtk::Align::Start);
        panel_subtitle_lbl.set_xalign(0.0);
        panel_subtitle_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);

        panel_title_box.append(&panel_title_lbl);
        panel_title_box.append(&panel_subtitle_lbl);

        let panel_null_btn = gtk::Button::with_label("Set NULL");
        panel_null_btn.add_css_class("flat");
        panel_null_btn.set_valign(gtk::Align::Center);
        panel_null_btn.set_tooltip_text(Some("Set cell value to SQL NULL"));

        let panel_revert_btn = gtk::Button::from_icon_name("edit-undo-symbolic");
        panel_revert_btn.add_css_class("flat");
        panel_revert_btn.set_valign(gtk::Align::Center);
        panel_revert_btn.set_tooltip_text(Some("Revert cell to original value"));

        let panel_close_btn = gtk::Button::from_icon_name("window-close-symbolic");
        panel_close_btn.add_css_class("flat");
        panel_close_btn.set_valign(gtk::Align::Center);
        panel_close_btn.set_tooltip_text(Some("Close multiline editor (:q)"));

        panel_header_inner.append(&panel_title_box);
        panel_header_inner.append(&panel_null_btn);
        panel_header_inner.append(&panel_revert_btn);
        panel_header_inner.append(&panel_close_btn);
        panel_header.append(&panel_header_inner);

        editor_panel.append(&panel_header);
        editor_panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let panel_buffer = sourceview5::Buffer::new(None);
        Self::apply_panel_style_scheme(&panel_buffer);
        {
            let style_mgr = adw::StyleManager::default();
            let buf_for_theme = panel_buffer.clone();
            style_mgr.connect_dark_notify(move |_| {
                Self::apply_panel_style_scheme(&buf_for_theme);
            });
        }

        let panel_view = sourceview5::View::with_buffer(&panel_buffer);
        panel_view.set_show_line_numbers(true);
        panel_view.set_highlight_current_line(true);
        panel_view.set_auto_indent(true);
        panel_view.set_indent_on_tab(true);
        panel_view.set_wrap_mode(gtk::WrapMode::WordChar);
        panel_view.set_monospace(true);
        panel_view.set_tab_width(2);
        panel_view.set_insert_spaces_instead_of_tabs(true);
        panel_view.set_top_margin(10);
        panel_view.set_bottom_margin(10);
        panel_view.set_left_margin(12);
        panel_view.set_right_margin(12);
        panel_view.set_vexpand(true);
        panel_view.set_hexpand(true);

        // Attach GtkSourceVimIMContext for Vim inputs in the multiline panel
        let panel_vim_im = sourceview5::VimIMContext::new();
        panel_vim_im.set_client_widget(Some(&panel_view));
        let panel_vim_key_ctrl = gtk::EventControllerKey::new();
        panel_vim_key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        panel_vim_key_ctrl.set_im_context(Some(&panel_vim_im));
        panel_view.add_controller(panel_vim_key_ctrl);

        let panel_scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&panel_view)
            .build();
        editor_panel.append(&panel_scroll);

        editor_panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let panel_vim_bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        panel_vim_bar.add_css_class("vim-status-bar");
        panel_vim_bar.set_margin_top(4);
        panel_vim_bar.set_margin_bottom(4);
        panel_vim_bar.set_margin_start(12);
        panel_vim_bar.set_margin_end(12);

        let panel_vim_mode_lbl = gtk::Label::new(Some("-- NORMAL --"));
        panel_vim_mode_lbl.add_css_class("monospace");
        panel_vim_mode_lbl.add_css_class("heading");
        panel_vim_mode_lbl.set_halign(gtk::Align::Start);
        panel_vim_mode_lbl.set_hexpand(true);

        let panel_vim_cmd_lbl = gtk::Label::new(None);
        panel_vim_cmd_lbl.add_css_class("monospace");
        panel_vim_cmd_lbl.add_css_class("dim-label");

        let panel_cursor_lbl = gtk::Label::new(Some("Ln 1, Col 1"));
        panel_cursor_lbl.add_css_class("monospace");
        panel_cursor_lbl.add_css_class("caption");
        panel_cursor_lbl.add_css_class("dim-label");

        panel_vim_bar.append(&panel_vim_mode_lbl);
        panel_vim_bar.append(&panel_vim_cmd_lbl);
        panel_vim_bar.append(&panel_cursor_lbl);
        editor_panel.append(&panel_vim_bar);

        editor_panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let panel_footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        panel_footer.add_css_class("long-text-panel-footer");
        panel_footer.set_margin_top(4);
        panel_footer.set_margin_bottom(4);
        panel_footer.set_margin_start(12);
        panel_footer.set_margin_end(12);

        let panel_status_lbl = gtk::Label::new(Some("0 lines · 0 chars"));
        panel_status_lbl.add_css_class("monospace");
        panel_status_lbl.add_css_class("caption");
        panel_status_lbl.add_css_class("dim-label");
        panel_status_lbl.set_halign(gtk::Align::Start);
        panel_status_lbl.set_hexpand(true);

        let panel_hint_lbl = gtk::Label::new(Some(":w commit · :q close"));
        panel_hint_lbl.add_css_class("caption");
        panel_hint_lbl.add_css_class("dim-label");

        panel_footer.append(&panel_status_lbl);
        panel_footer.append(&panel_hint_lbl);
        editor_panel.append(&panel_footer);

        let content_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        content_paned.set_vexpand(true);
        content_paned.set_hexpand(true);
        content_paned.set_start_child(Some(&scroll));
        content_paned.set_resize_start_child(true);
        content_paned.set_shrink_start_child(false);
        content_paned.set_end_child(Some(&editor_panel));
        content_paned.set_resize_end_child(false);
        content_paned.set_shrink_end_child(false);
        root.append(&content_paned);

        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Bottom Pagination Bar
        let bottom_bar = gtk::CenterBox::new();
        bottom_bar.set_margin_top(6);
        bottom_bar.set_margin_bottom(6);
        bottom_bar.set_margin_start(12);
        bottom_bar.set_margin_end(12);

        let status_label = gtk::Label::new(Some("No table selected"));
        status_label.add_css_class("dim-label");
        status_label.set_halign(gtk::Align::Start);

        let hint_label = gtk::Label::new(Some("Click cell to edit · Ctrl+S to commit"));
        hint_label.add_css_class("caption");
        hint_label.add_css_class("dim-label");
        hint_label.set_halign(gtk::Align::Center);

        let page_controls = gtk::Box::new(gtk::Orientation::Horizontal, 10);

        let page_size_lbl = gtk::Label::new(Some("Rows per page:"));
        page_size_lbl.add_css_class("caption");
        page_size_lbl.add_css_class("dim-label");

        let page_size_model = gtk::StringList::new(&["100", "500", "1000"]);
        let page_size_dd = gtk::DropDown::new(Some(page_size_model), gtk::Expression::NONE);
        page_size_dd.set_selected(0);

        let prev_page_btn = gtk::Button::from_icon_name("go-previous-symbolic");
        prev_page_btn.set_tooltip_text(Some("Previous Page"));
        prev_page_btn.set_sensitive(false);

        let next_page_btn = gtk::Button::from_icon_name("go-next-symbolic");
        next_page_btn.set_tooltip_text(Some("Next Page"));
        next_page_btn.set_sensitive(false);

        page_controls.append(&page_size_lbl);
        page_controls.append(&page_size_dd);
        page_controls.append(&prev_page_btn);
        page_controls.append(&next_page_btn);

        bottom_bar.set_start_widget(Some(&status_label));
        bottom_bar.set_center_widget(Some(&hint_label));
        bottom_bar.set_end_widget(Some(&page_controls));
        root.append(&bottom_bar);

        let tab = Self {
            root: root.clone(),
            content_paned: content_paned.clone(),
            scroll: scroll.clone(),
            grid,
            editor_panel: editor_panel.clone(),
            panel_title_lbl,
            panel_subtitle_lbl,
            panel_status_lbl,
            panel_null_btn: panel_null_btn.clone(),
            panel_revert_btn: panel_revert_btn.clone(),
            panel_buffer: panel_buffer.clone(),
            panel_view: panel_view.clone(),
            panel_target: Rc::new(RefCell::new(None)),
            panel_syncing: Rc::new(RefCell::new(false)),
            panel_initial_pos_set: Rc::new(RefCell::new(false)),
            nav_back_btn: nav_back_btn.clone(),
            nav_forward_btn: nav_forward_btn.clone(),
            search_entry: search_entry.clone(),
            status_label,
            dirty_badge_btn: dirty_badge_btn.clone(),
            prev_page_btn: prev_page_btn.clone(),
            next_page_btn: next_page_btn.clone(),
            info_banner,
            current_page: Rc::new(RefCell::new(None)),
            relations: Rc::new(RefCell::new(Vec::new())),
            query_state: Rc::new(RefCell::new(ContentQuery::default())),
            nav_back_stack: Rc::new(RefCell::new(Vec::new())),
            nav_forward_stack: Rc::new(RefCell::new(Vec::new())),
            nav_in_progress: Rc::new(RefCell::new(false)),
            dirty_cells: Rc::new(RefCell::new(HashMap::new())),
            draft_rows: Rc::new(RefCell::new(Vec::new())),
            selected_rows: Rc::new(RefCell::new(Vec::new())),
            col_widths: Rc::new(RefCell::new(HashMap::new())),
            natural_col_widths: Rc::new(RefCell::new(Vec::new())),
            header_cells: Rc::new(RefCell::new(Vec::new())),
            last_viewport_width: Rc::new(RefCell::new(0)),
            index_col_width: Rc::new(RefCell::new(DEFAULT_INDEX_COL_WIDTH)),
            cmd_tx: cmd_tx.clone(),
        };

        // Long text panel signals: Vim mode/command/cursor indicators, buffer change, Set NULL, Revert, Close
        {
            let panel_vim_mode_lbl = panel_vim_mode_lbl.clone();
            panel_vim_im.connect_command_bar_text_notify(move |vim| {
                let text = vim.command_bar_text();
                if text.is_empty() {
                    panel_vim_mode_lbl.set_text("-- NORMAL --");
                } else {
                    panel_vim_mode_lbl.set_text(&text);
                }
            });
        }
        {
            let panel_vim_cmd_lbl = panel_vim_cmd_lbl.clone();
            panel_vim_im.connect_command_text_notify(move |vim| {
                panel_vim_cmd_lbl.set_text(&vim.command_text());
            });
        }
        {
            let panel_cursor_lbl = panel_cursor_lbl.clone();
            panel_buffer.connect_cursor_position_notify(move |buf| {
                let iter = buf.iter_at_offset(buf.cursor_position());
                panel_cursor_lbl.set_text(&format!(
                    "Ln {}, Col {}",
                    iter.line() + 1,
                    iter.line_offset() + 1
                ));
            });
        }
        {
            let tab_clone = tab.clone();
            panel_vim_im.connect_write(move |_, _, _| {
                tab_clone.commit_pending_changes();
            });
        }
        {
            let tab_clone = tab.clone();
            panel_vim_im.connect_execute_command(move |_, cmd| {
                let normalized = cmd.trim().trim_start_matches(':').trim();
                match normalized {
                    "w" | "write" => {
                        tab_clone.commit_pending_changes();
                        true
                    }
                    "q" | "quit" => {
                        tab_clone.close_long_text_panel();
                        true
                    }
                    "wq" | "x" => {
                        tab_clone.commit_pending_changes();
                        tab_clone.close_long_text_panel();
                        true
                    }
                    "q!" => {
                        tab_clone.revert_active_panel_cell();
                        tab_clone.close_long_text_panel();
                        true
                    }
                    "e!" => {
                        tab_clone.revert_active_panel_cell();
                        true
                    }
                    _ => false,
                }
            });
        }
        {
            let tab_clone = tab.clone();
            panel_buffer.connect_changed(move |buf| {
                if *tab_clone.panel_syncing.borrow() {
                    return;
                }
                let text = buf
                    .text(&buf.start_iter(), &buf.end_iter(), true)
                    .to_string();
                tab_clone.apply_panel_value(DbValue::Text(text));
            });
        }
        {
            let tab_clone = tab.clone();
            panel_null_btn.connect_clicked(move |_| {
                *tab_clone.panel_syncing.borrow_mut() = true;
                tab_clone.panel_buffer.set_text("");
                *tab_clone.panel_syncing.borrow_mut() = false;
                tab_clone.apply_panel_value(DbValue::Null);
                tab_clone.panel_view.grab_focus();
            });
        }
        {
            let tab_clone = tab.clone();
            panel_revert_btn.connect_clicked(move |_| {
                tab_clone.revert_active_panel_cell();
                tab_clone.panel_view.grab_focus();
            });
        }
        {
            let tab_clone = tab.clone();
            panel_close_btn.connect_clicked(move |_| {
                tab_clone.close_long_text_panel();
            });
        }

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

        // Ctrl+S & Back/Forward keyboard shortcut controller (Capture phase)
        {
            let key_ctrl = gtk::EventControllerKey::new();
            key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
            let tab_clone = tab.clone();
            key_ctrl.connect_key_pressed(move |_, keyval, _keycode, state| {
                let is_ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                let is_alt = state.contains(gtk::gdk::ModifierType::ALT_MASK);
                if is_ctrl && (keyval == gtk::gdk::Key::s || keyval == gtk::gdk::Key::S) {
                    tab_clone.commit_pending_changes();
                    return gtk::glib::Propagation::Stop;
                }
                if keyval == gtk::gdk::Key::Back || (is_alt && keyval == gtk::gdk::Key::Left) {
                    if tab_clone.navigate_back() {
                        return gtk::glib::Propagation::Stop;
                    }
                }
                if keyval == gtk::gdk::Key::Forward || (is_alt && keyval == gtk::gdk::Key::Right) {
                    if tab_clone.navigate_forward() {
                        return gtk::glib::Propagation::Stop;
                    }
                }
                gtk::glib::Propagation::Proceed
            });
            root.add_controller(key_ctrl);
        }

        // Mouse Back (button 8) and Forward (button 9) gesture controller (Capture phase)
        {
            let mouse_nav = gtk::GestureClick::new();
            mouse_nav.set_button(0);
            mouse_nav.set_propagation_phase(gtk::PropagationPhase::Capture);
            let tab_clone = tab.clone();
            mouse_nav.connect_pressed(move |gesture, _n_press, _x, _y| {
                match gesture.current_button() {
                    MOUSE_BUTTON_BACK => {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        tab_clone.navigate_back();
                    }
                    MOUSE_BUTTON_FORWARD => {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        tab_clone.navigate_forward();
                    }
                    _ => {
                        gesture.set_state(gtk::EventSequenceState::Denied);
                    }
                }
            });
            root.add_controller(mouse_nav);
        }

        // Toolbar Back / Forward button clicks
        {
            let tab_clone = tab.clone();
            nav_back_btn.connect_clicked(move |_| {
                tab_clone.navigate_back();
            });
        }
        {
            let tab_clone = tab.clone();
            nav_forward_btn.connect_clicked(move |_| {
                tab_clone.navigate_forward();
            });
        }

        // Commit button click
        {
            let tab_clone = tab.clone();
            dirty_badge_btn.connect_clicked(move |_| {
                tab_clone.commit_pending_changes();
            });
        }

        // Search entry activate / changed
        {
            let tab_clone = tab.clone();
            search_entry.connect_activate(move |entry| {
                {
                    let mut q = tab_clone.query_state.borrow_mut();
                    q.search_text = entry.text().to_string();
                    q.offset = 0;
                }
                tab_clone.request_fetch();
            });
        }
        {
            let tab_clone = tab.clone();
            search_entry.connect_search_changed(move |entry| {
                if entry.text().is_empty() && !tab_clone.query_state.borrow().search_text.is_empty()
                {
                    {
                        let mut q = tab_clone.query_state.borrow_mut();
                        q.search_text.clear();
                        q.offset = 0;
                    }
                    tab_clone.request_fetch();
                }
            });
        }

        // Refresh button
        {
            let tab_clone = tab.clone();
            refresh_btn.connect_clicked(move |_| {
                tab_clone.request_fetch();
            });
        }

        // Add Row button
        {
            let tab_clone = tab.clone();
            add_row_btn.connect_clicked(move |_| {
                tab_clone.append_draft_row();
            });
        }

        // Delete Selected Rows button
        {
            let tab_clone = tab.clone();
            delete_row_btn.connect_clicked(move |btn| {
                tab_clone.delete_checked_rows(btn.upcast_ref::<gtk::Widget>());
            });
        }

        // Page size dropdown
        {
            let tab_clone = tab.clone();
            page_size_dd.connect_selected_notify(move |dd| {
                let new_limit = match dd.selected() {
                    0 => 100,
                    1 => 500,
                    _ => 1000,
                };
                {
                    let mut q = tab_clone.query_state.borrow_mut();
                    if q.limit != new_limit {
                        q.limit = new_limit;
                        q.offset = 0;
                    } else {
                        return;
                    }
                }
                tab_clone.request_fetch();
            });
        }

        // Previous / Next page buttons
        {
            let tab_clone = tab.clone();
            prev_page_btn.connect_clicked(move |_| {
                {
                    let mut q = tab_clone.query_state.borrow_mut();
                    q.offset = q.offset.saturating_sub(q.limit);
                }
                tab_clone.request_fetch();
            });
        }
        {
            let tab_clone = tab.clone();
            next_page_btn.connect_clicked(move |_| {
                {
                    let mut q = tab_clone.query_state.borrow_mut();
                    q.offset += q.limit;
                }
                tab_clone.request_fetch();
            });
        }

        // Export CSV & JSON
        {
            let tab_clone = tab.clone();
            let export_popover = export_popover.clone();
            let on_toast = on_toast.clone();
            export_csv_btn.connect_clicked(move |btn| {
                export_popover.popdown();
                tab_clone.export_data(btn.upcast_ref::<gtk::Widget>(), true, on_toast.clone());
            });
        }
        {
            let tab_clone = tab.clone();
            let export_popover = export_popover.clone();
            let on_toast = on_toast.clone();
            export_json_btn.connect_clicked(move |btn| {
                export_popover.popdown();
                tab_clone.export_data(btn.upcast_ref::<gtk::Widget>(), false, on_toast.clone());
            });
        }

        tab
    }

    pub fn current_query(&self) -> ContentQuery {
        self.query_state.borrow().clone()
    }

    #[cfg(test)]
    pub(crate) fn current_table_context(&self) -> Option<(String, String)> {
        self.current_page
            .borrow()
            .as_ref()
            .map(|p| (p.database.clone(), p.table.clone()))
    }

    pub(crate) fn is_navigation_in_progress(&self) -> bool {
        *self.nav_in_progress.borrow()
    }

    pub fn set_query_for_table(&self, database: &str, query: ContentQuery) {
        if *self.nav_in_progress.borrow() {
            *self.nav_in_progress.borrow_mut() = false;
        } else if let Some(prev_page) = self.current_page.borrow().clone() {
            if prev_page.database == database {
                self.nav_back_stack.borrow_mut().push(NavigationEntry {
                    database: prev_page.database,
                    table: prev_page.table,
                    query: self.current_query(),
                });
                self.nav_forward_stack.borrow_mut().clear();
            } else {
                self.nav_back_stack.borrow_mut().clear();
                self.nav_forward_stack.borrow_mut().clear();
            }
        }
        self.update_nav_buttons();

        let search_text = query.search_text.clone();
        *self.query_state.borrow_mut() = query;
        self.search_entry.set_text(&search_text);
    }

    pub fn clear_navigation_history(&self) {
        self.close_long_text_panel();
        clear_spreadsheet_grid(&self.grid);
        self.nav_back_stack.borrow_mut().clear();
        self.nav_forward_stack.borrow_mut().clear();
        *self.nav_in_progress.borrow_mut() = false;
        *self.current_page.borrow_mut() = None;
        self.relations.borrow_mut().clear();
        self.dirty_cells.borrow_mut().clear();
        self.draft_rows.borrow_mut().clear();
        self.selected_rows.borrow_mut().clear();
        self.header_cells.borrow_mut().clear();
        self.natural_col_widths.borrow_mut().clear();
        self.col_widths.borrow_mut().clear();
        *self.query_state.borrow_mut() = ContentQuery::default();
        self.search_entry.set_text("");
        self.info_banner.set_revealed(false);
        self.status_label.set_text("No table selected");
        self.prev_page_btn.set_sensitive(false);
        self.next_page_btn.set_sensitive(false);
        self.update_dirty_badge();
        self.update_nav_buttons();
    }

    fn apply_panel_style_scheme(buffer: &sourceview5::Buffer) {
        let style_mgr = adw::StyleManager::default();
        let scheme_mgr = sourceview5::StyleSchemeManager::default();
        let preferred = if style_mgr.is_dark() {
            ["Adwaita-dark", "classic-dark", "oblivion"]
        } else {
            ["Adwaita", "classic", "tango"]
        };
        for name in preferred {
            if let Some(scheme) = scheme_mgr.scheme(name) {
                buffer.set_style_scheme(Some(&scheme));
                break;
            }
        }
    }

    fn apply_panel_syntax_language(buffer: &sourceview5::Buffer, data_type: &str) {
        let dt = data_type.trim().to_ascii_lowercase();
        let lang_id = if dt == "json" || dt == "jsonb" {
            Some("json")
        } else if dt == "xml" {
            Some("xml")
        } else {
            None
        };
        let lang_mgr = sourceview5::LanguageManager::default();
        if let Some(id) = lang_id.and_then(|id| lang_mgr.language(id)) {
            buffer.set_language(Some(&id));
            buffer.set_highlight_syntax(true);
        } else {
            buffer.set_language(None);
            buffer.set_highlight_syntax(false);
        }
    }

    fn update_panel_status_label(&self, val: &DbValue) {
        match val {
            DbValue::Null => {
                self.panel_status_lbl
                    .set_text("NULL — press i to insert");
            }
            DbValue::Text(s) => {
                let chars = s.chars().count();
                let lines = if s.is_empty() {
                    1
                } else {
                    s.split('\n').count()
                };
                self.panel_status_lbl
                    .set_text(&format!("{} line(s) · {} char(s)", lines, chars));
            }
        }
    }

    fn open_long_text_panel(&self, target: LongTextPanelTarget) {
        if let Some(prev) = self.panel_target.borrow().as_ref() {
            prev.cell_box.remove_css_class("cell-panel-active");
        }
        target.cell_box.add_css_class("cell-panel-active");

        let current_val = match &target.kind {
            LongTextTargetKind::Existing {
                row_idx,
                original_val,
                ..
            } => self
                .dirty_cells
                .borrow()
                .get(&(*row_idx, target.col_info.name.clone()))
                .cloned()
                .unwrap_or_else(|| original_val.clone()),
            LongTextTargetKind::Draft {
                draft_idx,
                initial_val,
            } => self
                .draft_rows
                .borrow()
                .get(*draft_idx)
                .and_then(|m| m.get(&target.col_info.name))
                .cloned()
                .unwrap_or_else(|| initial_val.clone()),
        };

        let editable = target.is_editable();
        let can_set_null =
            editable && (target.col_info.is_nullable || target.is_draft());

        let subtitle = match &target.kind {
            LongTextTargetKind::Existing { row_idx, .. } => {
                let offset = self
                    .current_page
                    .borrow()
                    .as_ref()
                    .map(|p| p.page_offset as usize)
                    .unwrap_or(0);
                let ro_suffix = if editable { "" } else { " (Read-only)" };
                format!(
                    "{} · Row #{}{}",
                    target.col_info.data_type,
                    offset + row_idx + 1,
                    ro_suffix
                )
            }
            LongTextTargetKind::Draft { draft_idx, .. } => {
                format!("{} · NEW Row #{}", target.col_info.data_type, draft_idx + 1)
            }
        };

        self.panel_title_lbl.set_text(&target.col_info.name);
        self.panel_subtitle_lbl.set_text(&subtitle);
        Self::apply_panel_syntax_language(&self.panel_buffer, &target.col_info.data_type);

        *self.panel_syncing.borrow_mut() = true;
        match &current_val {
            DbValue::Null => {
                self.panel_buffer.set_text("");
                self.panel_view.add_css_class("cell-null");
            }
            DbValue::Text(s) => {
                self.panel_buffer.set_text(s);
                self.panel_view.remove_css_class("cell-null");
            }
        }
        *self.panel_syncing.borrow_mut() = false;

        self.panel_view.set_editable(editable);
        self.panel_null_btn
            .set_sensitive(can_set_null && !current_val.is_null());
        self.panel_revert_btn
            .set_sensitive(editable && &current_val != target.baseline_value());
        self.update_panel_status_label(&current_val);

        *self.panel_target.borrow_mut() = Some(target);

        let was_visible = self.editor_panel.is_visible();
        if !was_visible {
            if !*self.panel_initial_pos_set.borrow() {
                let total_w = self.content_paned.width().max(self.root.width());
                if total_w > DEFAULT_LONG_TEXT_PANEL_WIDTH + 200 {
                    self.content_paned
                        .set_position(total_w - DEFAULT_LONG_TEXT_PANEL_WIDTH);
                    *self.panel_initial_pos_set.borrow_mut() = true;
                }
            }
            self.editor_panel.set_visible(true);
            let tab_for_idle = self.clone();
            gtk::glib::idle_add_local_once(move || {
                tab_for_idle.reapply_column_widths_if_viewport_changed();
            });
        }

        let view_clone = self.panel_view.clone();
        gtk::glib::idle_add_local_once(move || {
            view_clone.grab_focus();
        });
    }

    pub fn close_long_text_panel(&self) {
        let prev_opt = self.panel_target.borrow_mut().take();
        if let Some(prev) = prev_opt {
            prev.cell_box.remove_css_class("cell-panel-active");
            prev.cell_entry.grab_focus();
        }
        if self.editor_panel.is_visible() {
            self.editor_panel.set_visible(false);
            let tab_for_idle = self.clone();
            gtk::glib::idle_add_local_once(move || {
                tab_for_idle.reapply_column_widths_if_viewport_changed();
            });
        }
    }

    fn revert_active_panel_cell(&self) {
        let baseline = {
            let target_ref = self.panel_target.borrow();
            target_ref.as_ref().map(|t| t.baseline_value().clone())
        };
        if let Some(baseline_val) = baseline {
            *self.panel_syncing.borrow_mut() = true;
            match &baseline_val {
                DbValue::Null => self.panel_buffer.set_text(""),
                DbValue::Text(s) => self.panel_buffer.set_text(s),
            }
            *self.panel_syncing.borrow_mut() = false;
            self.apply_panel_value(baseline_val);
        }
    }

    fn apply_panel_value(&self, new_val: DbValue) {
        let target_opt = self.panel_target.borrow().clone();
        let Some(target) = target_opt else {
            return;
        };
        if !target.is_editable() {
            return;
        }

        if new_val.is_null() {
            self.panel_view.add_css_class("cell-null");
            target.cell_entry.add_css_class("cell-null");
        } else {
            self.panel_view.remove_css_class("cell-null");
            target.cell_entry.remove_css_class("cell-null");
        }
        target
            .cell_entry
            .set_text(&format_long_text_preview(&new_val));

        if let (Some(btn), Some(fk)) = (&target.ref_btn_opt, &target.fk_opt) {
            Self::update_ref_button_state(btn, fk, &new_val);
        }

        match &target.kind {
            LongTextTargetKind::Existing {
                row_idx,
                original_val,
                ..
            } => {
                if &new_val != original_val {
                    target.cell_box.add_css_class("cell-dirty");
                    self.dirty_cells
                        .borrow_mut()
                        .insert((*row_idx, target.col_info.name.clone()), new_val.clone());
                } else {
                    target.cell_box.remove_css_class("cell-dirty");
                    self.dirty_cells
                        .borrow_mut()
                        .remove(&(*row_idx, target.col_info.name.clone()));
                }
                self.update_dirty_badge();
            }
            LongTextTargetKind::Draft { draft_idx, .. } => {
                if let Some(row_map) = self.draft_rows.borrow_mut().get_mut(*draft_idx) {
                    row_map.insert(target.col_info.name.clone(), new_val.clone());
                }
            }
        }

        let can_set_null = target.is_editable()
            && (target.col_info.is_nullable || target.is_draft());
        self.panel_null_btn
            .set_sensitive(can_set_null && !new_val.is_null());
        self.panel_revert_btn
            .set_sensitive(&new_val != target.baseline_value());
        self.update_panel_status_label(&new_val);
    }

    fn attach_long_text_cell_handlers(
        &self,
        target: LongTextPanelTarget,
        edit_btn_opt: Option<gtk::Button>,
    ) {
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let tab_for_click = self.clone();
        let target_for_click = target.clone();
        click.connect_released(move |gesture, n_press, _, _| {
            if n_press >= 1 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                tab_for_click.open_long_text_panel(target_for_click.clone());
            }
        });
        target.cell_entry.add_controller(click);

        let key_ctrl = gtk::EventControllerKey::new();
        let tab_for_key = self.clone();
        let target_for_key = target.clone();
        key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Return
                || keyval == gtk::gdk::Key::KP_Enter
                || keyval == gtk::gdk::Key::space
            {
                tab_for_key.open_long_text_panel(target_for_key.clone());
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        target.cell_entry.add_controller(key_ctrl);

        if let Some(edit_btn) = edit_btn_opt {
            let tab_for_btn = self.clone();
            let target_for_btn = target;
            edit_btn.connect_clicked(move |_| {
                tab_for_btn.open_long_text_panel(target_for_btn.clone());
            });
        }
    }

    fn update_nav_buttons(&self) {
        let back_stack = self.nav_back_stack.borrow();
        let fwd_stack = self.nav_forward_stack.borrow();

        if let Some(last) = back_stack.last() {
            self.nav_back_btn.set_sensitive(true);
            self.nav_back_btn.set_tooltip_text(Some(&format!(
                "Back to '{}' (Mouse Back / Alt+Left)",
                last.table
            )));
        } else {
            self.nav_back_btn.set_sensitive(false);
            self.nav_back_btn
                .set_tooltip_text(Some("Navigate Back (Mouse Back / Alt+Left)"));
        }

        if let Some(next) = fwd_stack.last() {
            self.nav_forward_btn.set_sensitive(true);
            self.nav_forward_btn.set_tooltip_text(Some(&format!(
                "Forward to '{}' (Mouse Forward / Alt+Right)",
                next.table
            )));
        } else {
            self.nav_forward_btn.set_sensitive(false);
            self.nav_forward_btn
                .set_tooltip_text(Some("Navigate Forward (Mouse Forward / Alt+Right)"));
        }
    }

    pub fn navigate_back(&self) -> bool {
        let prev_opt = self.nav_back_stack.borrow_mut().pop();
        let Some(prev) = prev_opt else {
            return false;
        };

        if let Some(curr_page) = self.current_page.borrow().clone() {
            self.nav_forward_stack.borrow_mut().push(NavigationEntry {
                database: curr_page.database,
                table: curr_page.table,
                query: self.current_query(),
            });
        }
        *self.nav_in_progress.borrow_mut() = true;
        self.update_nav_buttons();

        let _ = self.cmd_tx.send_blocking(BackendCommand::SelectTable {
            database: prev.database,
            table: prev.table,
            content_query: prev.query,
        });
        true
    }

    pub fn navigate_forward(&self) -> bool {
        let next_opt = self.nav_forward_stack.borrow_mut().pop();
        let Some(next) = next_opt else {
            return false;
        };

        if let Some(curr_page) = self.current_page.borrow().clone() {
            self.nav_back_stack.borrow_mut().push(NavigationEntry {
                database: curr_page.database,
                table: curr_page.table,
                query: self.current_query(),
            });
        }
        *self.nav_in_progress.borrow_mut() = true;
        self.update_nav_buttons();

        let _ = self.cmd_tx.send_blocking(BackendCommand::SelectTable {
            database: next.database,
            table: next.table,
            content_query: next.query,
        });
        true
    }

    pub fn set_relations(&self, relations: Vec<ForeignKeyInfo>) {
        *self.relations.borrow_mut() = relations;
    }

    pub fn refresh_grid(&self) {
        self.render_grid();
    }

    pub fn request_fetch(&self) {
        let page_opt = self.current_page.borrow().clone();
        if let Some(page) = page_opt {
            let query = self.query_state.borrow().clone();
            let _ = self.cmd_tx.send_blocking(BackendCommand::FetchTableContent {
                database: page.database,
                table: page.table,
                query,
            });
        }
    }

    fn follow_reference(
        &self,
        database: &str,
        target_table: &str,
        target_col: &str,
        val: &DbValue,
    ) {
        if val.is_null() {
            return;
        }
        if let Some(curr_page) = self.current_page.borrow().clone() {
            self.nav_back_stack.borrow_mut().push(NavigationEntry {
                database: curr_page.database,
                table: curr_page.table,
                query: self.current_query(),
            });
            self.nav_forward_stack.borrow_mut().clear();
            *self.nav_in_progress.borrow_mut() = true;
            self.update_nav_buttons();
        }
        let escaped_val = escape_sql_literal(val);
        let filter = format!("WHERE {} = {}", target_col, escaped_val);
        let limit = self.query_state.borrow().limit;
        let query = ContentQuery {
            search_text: filter,
            sort_column: None,
            sort_direction: SortDirection::Asc,
            limit,
            offset: 0,
        };
        let _ = self.cmd_tx.send_blocking(BackendCommand::SelectTable {
            database: database.to_string(),
            table: target_table.to_string(),
            content_query: query,
        });
    }

    pub fn set_page(&self, page: TablePage) {
        let table_changed = self
            .current_page
            .borrow()
            .as_ref()
            .map(|p| (&p.database, &p.table) != (&page.database, &page.table))
            .unwrap_or(false);
        if table_changed {
            self.col_widths.borrow_mut().clear();
            self.scroll.hadjustment().set_value(0.0);
            self.scroll.vadjustment().set_value(0.0);
        }

        self.close_long_text_panel();
        clear_spreadsheet_grid(&self.grid);
        self.dirty_cells.borrow_mut().clear();
        self.draft_rows.borrow_mut().clear();
        self.selected_rows.borrow_mut().clear();
        self.update_dirty_badge();

        let has_pk = page.columns.iter().any(|c| c.is_primary_key);
        self.info_banner.set_revealed(!page.columns.is_empty() && !has_pk);

        let start_row = if page.total_rows == 0 {
            0
        } else {
            page.page_offset as u64 + 1
        };
        let end_row = (page.page_offset as u64 + page.rows.len() as u64).min(page.total_rows);
        self.status_label.set_text(&format!(
            "Showing {}–{} of {} row(s)",
            start_row, end_row, page.total_rows
        ));

        self.prev_page_btn.set_sensitive(page.page_offset > 0);
        self.next_page_btn
            .set_sensitive((page.page_offset as u64 + page.page_limit as u64) < page.total_rows);

        *self.current_page.borrow_mut() = Some(page);
        self.render_grid();
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
        if let Some(win) = self.root.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
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
            .map(|(col_name, _)| manual_map.get(col_name).copied())
            .collect();

        let idx_w = *self.index_col_width.borrow();
        let total_avail = (viewport_w - idx_w - 2).max(0);
        let widths = compute_auto_column_widths(&natural_widths, &manual_overrides, total_avail);

        for ((_, hdr_cell), target_w) in header_cells.iter().zip(widths.into_iter()) {
            if hdr_cell.width_request() != target_w {
                hdr_cell.set_width_request(target_w);
            }
        }
    }

    fn render_grid(&self) {
        let saved_h = self.scroll.hadjustment().value();
        let saved_v = self.scroll.vadjustment().value();

        self.close_long_text_panel();
        clear_spreadsheet_grid(&self.grid);
        self.selected_rows.borrow_mut().clear();
        self.header_cells.borrow_mut().clear();
        self.natural_col_widths.borrow_mut().clear();

        let page_opt = self.current_page.borrow().clone();
        let Some(page) = page_opt else {
            return;
        };

        let q = self.query_state.borrow().clone();
        let relations = self.relations.borrow().clone();
        let has_pk = page.columns.iter().any(|c| c.is_primary_key);

        // Header Row (grid row 0)
        let corner_lbl = gtk::Label::new(Some("#"));
        corner_lbl.add_css_class("dim-label");
        corner_lbl.set_halign(gtk::Align::Fill);
        corner_lbl.set_xalign(0.5);
        corner_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        corner_lbl.set_max_width_chars(1);

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

        let mut header_labels: Vec<(gtk::Label, String)> = Vec::with_capacity(page.columns.len());

        for (col_idx, col_info) in page.columns.iter().enumerate() {
            let sort_indicator = if q.sort_column.as_deref() == Some(col_info.name.as_str()) {
                match q.sort_direction {
                    SortDirection::Asc => " ▲",
                    SortDirection::Desc => " ▼",
                }
            } else {
                ""
            };
            let fk_opt = relations
                .iter()
                .find(|fk| fk.column_name == col_info.name);
            let pk_prefix = if col_info.is_primary_key { "🔑 " } else { "" };
            let btn_label = format!("{}{}{}", pk_prefix, col_info.name, sort_indicator);

            let hdr_lbl = gtk::Label::new(Some(&btn_label));
            hdr_lbl.set_halign(gtk::Align::Fill);
            hdr_lbl.set_xalign(0.0);
            hdr_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            hdr_lbl.set_max_width_chars(1);

            let hdr_btn = gtk::Button::new();
            hdr_btn.set_child(Some(&hdr_lbl));
            hdr_btn.add_css_class("spreadsheet-header-btn");
            hdr_btn.set_focus_on_click(false);
            let tooltip = if let Some(fk) = fk_opt {
                format!(
                    "{} ({}) → {}.{} — Click to sort",
                    col_info.name, col_info.data_type, fk.referenced_table, fk.referenced_column
                )
            } else {
                format!("{} ({}) — Click to sort", col_info.name, col_info.data_type)
            };
            hdr_btn.set_tooltip_text(Some(&tooltip));

            let tab_clone = self.clone();
            let col_name = col_info.name.clone();
            hdr_btn.connect_clicked(move |_| {
                {
                    let mut qs = tab_clone.query_state.borrow_mut();
                    if qs.sort_column.as_deref() == Some(col_name.as_str()) {
                        qs.sort_direction = match qs.sort_direction {
                            SortDirection::Asc => SortDirection::Desc,
                            SortDirection::Desc => SortDirection::Asc,
                        };
                    } else {
                        qs.sort_column = Some(col_name.clone());
                        qs.sort_direction = SortDirection::Asc;
                    }
                    qs.offset = 0;
                }
                tab_clone.request_fetch();
            });

            let col_w = self
                .col_widths
                .borrow()
                .get(&col_info.name)
                .copied()
                .unwrap_or(DEFAULT_DATA_COL_WIDTH);
            let col_widths_ref = self.col_widths.clone();
            let col_name_for_resize = col_info.name.clone();
            let hdr_cell = build_resizable_header_cell(
                &hdr_btn,
                col_w,
                Some("spreadsheet-header-box"),
                move |new_w| {
                    col_widths_ref
                        .borrow_mut()
                        .insert(col_name_for_resize.clone(), new_w);
                },
            );

            self.grid.attach(&hdr_cell, (col_idx + 1) as i32, 0, 1, 1);
            header_labels.push((hdr_lbl, btn_label));
            self.header_cells
                .borrow_mut()
                .push((col_info.name.clone(), hdr_cell));
        }

        let mut sample_entry: Option<gtk::Entry> = None;

        // Existing Data Rows
        for (row_idx, row_data) in page.rows.iter().enumerate() {
            let grid_row = (row_idx + 1) as i32;

            let row_selector_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            row_selector_box.add_css_class("spreadsheet-row-num");
            row_selector_box.set_overflow(gtk::Overflow::Hidden);

            let check = gtk::CheckButton::new();
            let row_editable = has_pk && row_data.pk.is_some();
            check.set_sensitive(row_editable);
            check.set_focus_on_click(false);
            let num_lbl =
                gtk::Label::new(Some(&(page.page_offset as usize + row_idx + 1).to_string()));
            num_lbl.add_css_class("monospace");
            num_lbl.add_css_class("caption");
            num_lbl.add_css_class("dim-label");
            num_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            num_lbl.set_max_width_chars(1);
            num_lbl.set_hexpand(true);
            num_lbl.set_xalign(0.0);

            row_selector_box.append(&check);
            row_selector_box.append(&num_lbl);
            self.grid.attach(&row_selector_box, 0, grid_row, 1, 1);
            self.selected_rows.borrow_mut().push((row_idx, check));

            for (col_idx, col_info) in page.columns.iter().enumerate() {
                let original_val = row_data
                    .values
                    .get(col_idx)
                    .cloned()
                    .unwrap_or(DbValue::Null);

                let current_val = self
                    .dirty_cells
                    .borrow()
                    .get(&(row_idx, col_info.name.clone()))
                    .cloned()
                    .unwrap_or_else(|| original_val.clone());

                let fk_opt = relations
                    .iter()
                    .find(|fk| fk.column_name == col_info.name)
                    .cloned();

                if let Some(enum_vals) = col_info.enum_values() {
                    let (cell_box, dropdown, options_rc, ref_btn_opt) = self
                        .create_enum_spreadsheet_cell(
                            &page.database,
                            &enum_vals,
                            col_info.is_nullable,
                            &current_val,
                            row_editable,
                            fk_opt.as_ref(),
                        );
                    if self
                        .dirty_cells
                        .borrow()
                        .contains_key(&(row_idx, col_info.name.clone()))
                    {
                        cell_box.add_css_class("cell-dirty");
                    }

                    let tab_clone = self.clone();
                    let col_name = col_info.name.clone();
                    let orig = original_val.clone();
                    let cell_box_ref = cell_box.clone();
                    dropdown.connect_selected_notify(move |dd| {
                        let new_val = options_rc
                            .get(dd.selected() as usize)
                            .cloned()
                            .unwrap_or(DbValue::Null);
                        if let (Some(btn), Some(fk)) = (&ref_btn_opt, &fk_opt) {
                            Self::update_ref_button_state(btn, fk, &new_val);
                        }
                        if new_val.is_null() {
                            dd.add_css_class("cell-null");
                        } else {
                            dd.remove_css_class("cell-null");
                        }
                        if new_val != orig {
                            cell_box_ref.add_css_class("cell-dirty");
                            tab_clone
                                .dirty_cells
                                .borrow_mut()
                                .insert((row_idx, col_name.clone()), new_val);
                        } else {
                            cell_box_ref.remove_css_class("cell-dirty");
                            tab_clone
                                .dirty_cells
                                .borrow_mut()
                                .remove(&(row_idx, col_name.clone()));
                        }
                        tab_clone.update_dirty_badge();
                    });

                    self.grid
                        .attach(&cell_box, (col_idx + 1) as i32, grid_row, 1, 1);
                } else {
                    let is_long_text = col_info.is_long_text();

                    let (cell_box, entry, ref_btn_opt, long_text_btn_opt) = self
                        .create_spreadsheet_cell(
                            &page.database,
                            &current_val,
                            row_editable,
                            is_long_text,
                            fk_opt.as_ref(),
                        );
                    if sample_entry.is_none() {
                        sample_entry = Some(entry.clone());
                    }
                    if self
                        .dirty_cells
                        .borrow()
                        .contains_key(&(row_idx, col_info.name.clone()))
                    {
                        cell_box.add_css_class("cell-dirty");
                    }

                    if is_long_text {
                        let target = LongTextPanelTarget {
                            kind: LongTextTargetKind::Existing {
                                row_idx,
                                original_val,
                                editable: row_editable,
                            },
                            col_info: col_info.clone(),
                            cell_box: cell_box.clone(),
                            cell_entry: entry.clone(),
                            ref_btn_opt,
                            fk_opt,
                        };
                        self.attach_long_text_cell_handlers(target, long_text_btn_opt);
                    } else {
                        let tab_clone = self.clone();
                        let col_name = col_info.name.clone();
                        let orig = original_val.clone();
                        let cell_box_ref = cell_box.clone();
                        if row_editable {
                            attach_blur_null_handler(&entry, col_info.is_nullable);
                        }
                        entry.connect_changed(move |e| {
                            let new_val = DbValue::from_editor_str(&e.text());
                            if let (Some(btn), Some(fk)) = (&ref_btn_opt, &fk_opt) {
                                Self::update_ref_button_state(btn, fk, &new_val);
                            }
                            if new_val != orig {
                                cell_box_ref.add_css_class("cell-dirty");
                                if new_val.is_null() {
                                    e.add_css_class("cell-null");
                                } else {
                                    e.remove_css_class("cell-null");
                                }
                                tab_clone
                                    .dirty_cells
                                    .borrow_mut()
                                    .insert((row_idx, col_name.clone()), new_val);
                            } else {
                                cell_box_ref.remove_css_class("cell-dirty");
                                if orig.is_null() {
                                    e.add_css_class("cell-null");
                                } else {
                                    e.remove_css_class("cell-null");
                                }
                                tab_clone
                                    .dirty_cells
                                    .borrow_mut()
                                    .remove(&(row_idx, col_name.clone()));
                            }
                            tab_clone.update_dirty_badge();
                        });
                    }

                    self.grid
                        .attach(&cell_box, (col_idx + 1) as i32, grid_row, 1, 1);
                }
            }
        }

        // Draft (newly added) rows at bottom
        let drafts = self.draft_rows.borrow().clone();
        for (draft_idx, draft_map) in drafts.iter().enumerate() {
            let grid_row = (page.rows.len() + draft_idx + 1) as i32;
            self.attach_draft_row_widgets(grid_row, draft_idx, &page.columns, draft_map);
        }

        // Compute natural full-content width for each column
        let dirty = self.dirty_cells.borrow();
        let mut natural_widths = Vec::with_capacity(page.columns.len());

        for (col_idx, col_info) in page.columns.iter().enumerate() {
            let (hdr_lbl, btn_label) = &header_labels[col_idx];
            let hdr_w = measure_single_line_text(hdr_lbl, btn_label) + HEADER_EXTRA_PADDING;

            let has_fk = relations.iter().any(|fk| fk.column_name == col_info.name);
            let enum_vals_opt = col_info.enum_values();
            let is_enum = enum_vals_opt.is_some();
            let is_long_text = !is_enum && col_info.is_long_text();
            let mut widest_val: Option<DbValue> = None;
            let mut max_score: usize = 0;

            if let Some(ref enum_vals) = enum_vals_opt {
                for ev in enum_vals {
                    let score: usize = ev
                        .chars()
                        .take(MAX_MEASURE_CHARS)
                        .map(|c| if c.is_ascii() { 1 } else { 2 })
                        .sum();
                    if widest_val.is_none() || score > max_score {
                        max_score = score;
                        widest_val = Some(DbValue::Text(ev.clone()));
                    }
                }
                if col_info.is_nullable && (widest_val.is_none() || 4 > max_score) {
                    max_score = 4;
                    widest_val = Some(DbValue::Null);
                }
            }

            for (row_idx, row_data) in page.rows.iter().enumerate() {
                let val = dirty
                    .get(&(row_idx, col_info.name.clone()))
                    .or_else(|| row_data.values.get(col_idx))
                    .unwrap_or(&DbValue::Null);
                let txt = val.display_text();
                let char_score: usize = txt
                    .chars()
                    .take(MAX_MEASURE_CHARS)
                    .map(|c| if c.is_ascii() { 1 } else { 2 })
                    .sum();
                let fk_bonus = if has_fk && !val.is_null() && !txt.is_empty() {
                    4
                } else {
                    0
                };
                let score = char_score + fk_bonus;
                if widest_val.is_none() || score > max_score {
                    max_score = score;
                    widest_val = Some(val.clone());
                }
            }

            for draft_map in &drafts {
                let val = draft_map.get(&col_info.name).unwrap_or(&DbValue::Null);
                let txt = val.display_text();
                let char_score: usize = txt
                    .chars()
                    .take(MAX_MEASURE_CHARS)
                    .map(|c| if c.is_ascii() { 1 } else { 2 })
                    .sum();
                let fk_bonus = if has_fk && !val.is_null() && !txt.is_empty() {
                    4
                } else {
                    0
                };
                let score = char_score + fk_bonus;
                if widest_val.is_none() || score > max_score {
                    max_score = score;
                    widest_val = Some(val.clone());
                }
            }

            let cell_w = if let Some(ref val) = widest_val {
                let preview_cow;
                let txt = if is_long_text {
                    preview_cow = format_long_text_preview(val);
                    preview_cow.as_str()
                } else {
                    val.display_text()
                };
                let text_px = if let Some(ref entry_widget) = sample_entry {
                    measure_single_line_text(entry_widget, txt)
                } else {
                    measure_single_line_text(hdr_lbl, txt)
                };
                let fk_extra = if has_fk && !val.is_null() && !txt.is_empty() {
                    FK_BTN_EXTRA_WIDTH
                } else {
                    0
                };
                let long_text_extra = if is_long_text { FK_BTN_EXTRA_WIDTH } else { 0 };
                let enum_extra = if is_enum { ENUM_DROPDOWN_EXTRA_WIDTH } else { 0 };
                let raw_cell_w =
                    text_px + CELL_EXTRA_PADDING + fk_extra + long_text_extra + enum_extra;
                if is_long_text {
                    raw_cell_w.min(MAX_LONG_TEXT_AUTO_COL_WIDTH)
                } else {
                    raw_cell_w
                }
            } else {
                0
            };

            natural_widths.push(hdr_w.max(cell_w).max(MIN_COLUMN_WIDTH));
        }
        drop(dirty);

        *self.natural_col_widths.borrow_mut() = natural_widths;
        self.apply_column_widths();
        self.scroll.hadjustment().set_value(saved_h);
        self.scroll.vadjustment().set_value(saved_v);

        let tab_for_idle = self.clone();
        gtk::glib::idle_add_local_once(move || {
            tab_for_idle.reapply_column_widths_if_viewport_changed();
        });
    }

    fn update_ref_button_state(btn: &gtk::Button, fk: &ForeignKeyInfo, val: &DbValue) {
        let has_target = !val.is_null() && !val.display_text().is_empty();
        btn.set_visible(has_target);
        if has_target {
            btn.set_tooltip_text(Some(&format!(
                "Follow reference → {}.{} = {}",
                fk.referenced_table,
                fk.referenced_column,
                val.display_text()
            )));
        }
    }

    fn create_enum_spreadsheet_cell(
        &self,
        database: &str,
        enum_values: &[String],
        is_nullable: bool,
        val: &DbValue,
        editable: bool,
        fk: Option<&ForeignKeyInfo>,
    ) -> (
        gtk::Box,
        gtk::DropDown,
        Rc<Vec<DbValue>>,
        Option<gtk::Button>,
    ) {
        let cell_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cell_box.add_css_class("spreadsheet-cell");
        cell_box.set_overflow(gtk::Overflow::Hidden);

        let (options, selected_idx) =
            build_enum_dropdown_options(enum_values, is_nullable, val);
        let options_rc = Rc::new(options);

        let labels: Vec<&str> = options_rc.iter().map(|v| v.display_text()).collect();
        let model = gtk::StringList::new(&labels);
        let dropdown = gtk::DropDown::new(Some(model), gtk::Expression::NONE);
        dropdown.add_css_class("spreadsheet-cell-dropdown");
        dropdown.set_hexpand(true);
        dropdown.set_valign(gtk::Align::Fill);
        dropdown.set_sensitive(editable);

        let btn_factory = gtk::SignalListItemFactory::new();
        btn_factory.connect_setup(|_, obj| {
            if let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() {
                let lbl = gtk::Label::new(None);
                lbl.add_css_class("monospace");
                lbl.set_halign(gtk::Align::Fill);
                lbl.set_xalign(0.0);
                lbl.set_hexpand(true);
                lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                lbl.set_max_width_chars(1);
                list_item.set_child(Some(&lbl));
            }
        });
        btn_factory.connect_bind(|_, obj| {
            if let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() {
                if let (Some(lbl), Some(str_obj)) = (
                    list_item
                        .child()
                        .and_then(|c| c.downcast::<gtk::Label>().ok()),
                    list_item
                        .item()
                        .and_then(|i| i.downcast::<gtk::StringObject>().ok()),
                ) {
                    lbl.set_text(&str_obj.string());
                }
            }
        });
        dropdown.set_factory(Some(&btn_factory));

        let list_factory = gtk::SignalListItemFactory::new();
        let options_for_list = options_rc.clone();
        list_factory.connect_setup(|_, obj| {
            if let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() {
                let lbl = gtk::Label::new(None);
                lbl.add_css_class("monospace");
                lbl.set_halign(gtk::Align::Start);
                lbl.set_xalign(0.0);
                list_item.set_child(Some(&lbl));
            }
        });
        list_factory.connect_bind(move |_, obj| {
            if let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() {
                if let (Some(lbl), Some(str_obj)) = (
                    list_item
                        .child()
                        .and_then(|c| c.downcast::<gtk::Label>().ok()),
                    list_item
                        .item()
                        .and_then(|i| i.downcast::<gtk::StringObject>().ok()),
                ) {
                    lbl.set_text(&str_obj.string());
                    let is_null_item = options_for_list
                        .get(list_item.position() as usize)
                        .is_some_and(|v| v.is_null());
                    if is_null_item {
                        lbl.add_css_class("cell-null");
                    } else {
                        lbl.remove_css_class("cell-null");
                    }
                }
            }
        });
        dropdown.set_list_factory(Some(&list_factory));

        dropdown.set_selected(selected_idx as u32);
        if options_rc.get(selected_idx).is_some_and(|v| v.is_null()) {
            dropdown.add_css_class("cell-null");
        }

        cell_box.append(&dropdown);

        let ref_btn_opt = if let Some(fk_info) = fk {
            let btn = gtk::Button::from_icon_name("go-next-symbolic");
            btn.add_css_class("flat");
            btn.add_css_class("cell-ref-btn");
            btn.set_valign(gtk::Align::Center);
            btn.set_focus_on_click(false);
            Self::update_ref_button_state(&btn, fk_info, val);

            let tab_clone = self.clone();
            let dropdown_ref = dropdown.clone();
            let options_for_fk = options_rc.clone();
            let db_name = database.to_string();
            let target_table = fk_info.referenced_table.clone();
            let target_col = fk_info.referenced_column.clone();
            btn.connect_clicked(move |_| {
                let current_val = options_for_fk
                    .get(dropdown_ref.selected() as usize)
                    .cloned()
                    .unwrap_or(DbValue::Null);
                tab_clone.follow_reference(&db_name, &target_table, &target_col, &current_val);
            });

            cell_box.append(&btn);
            Some(btn)
        } else {
            None
        };

        (cell_box, dropdown, options_rc, ref_btn_opt)
    }

    fn create_spreadsheet_cell(
        &self,
        database: &str,
        val: &DbValue,
        editable: bool,
        is_long_text: bool,
        fk: Option<&ForeignKeyInfo>,
    ) -> (gtk::Box, gtk::Entry, Option<gtk::Button>, Option<gtk::Button>) {
        let cell_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cell_box.add_css_class("spreadsheet-cell");
        cell_box.set_overflow(gtk::Overflow::Hidden);

        let entry = gtk::Entry::new();
        entry.set_has_frame(false);
        entry.add_css_class("spreadsheet-cell-entry");
        entry.add_css_class("monospace");
        entry.set_editable(editable && !is_long_text);
        entry.set_hexpand(true);
        entry.set_width_chars(1);
        entry.set_max_width_chars(1);
        if is_long_text {
            entry.set_text(&format_long_text_preview(val));
            entry.set_tooltip_text(Some("Click to open multiline editor panel"));
        } else {
            entry.set_text(val.display_text());
        }
        if val.is_null() {
            entry.add_css_class("cell-null");
        }

        if !is_long_text {
            // When user clicks or focuses a short cell, immediately select its full text content
            // so typing overwrites it just like an Excel / spreadsheet cell!
            let click = gtk::GestureClick::new();
            let entry_weak = entry.downgrade();
            click.connect_released(move |_, n_press, _, _| {
                if n_press == 1 {
                    if let Some(e) = entry_weak.upgrade() {
                        let e_clone = e.clone();
                        gtk::glib::idle_add_local_once(move || {
                            e_clone.grab_focus();
                            e_clone.select_region(0, -1);
                        });
                    }
                }
            });
            entry.add_controller(click);
        }
        cell_box.append(&entry);

        let long_text_btn_opt = if is_long_text {
            let edit_btn = gtk::Button::from_icon_name("document-edit-symbolic");
            edit_btn.add_css_class("flat");
            edit_btn.add_css_class("cell-ref-btn");
            edit_btn.set_valign(gtk::Align::Center);
            edit_btn.set_focus_on_click(false);
            edit_btn.set_tooltip_text(Some("Open multiline editor panel"));
            cell_box.append(&edit_btn);
            Some(edit_btn)
        } else {
            None
        };

        let ref_btn_opt = if let Some(fk_info) = fk {
            let btn = gtk::Button::from_icon_name("go-next-symbolic");
            btn.add_css_class("flat");
            btn.add_css_class("cell-ref-btn");
            btn.set_valign(gtk::Align::Center);
            btn.set_focus_on_click(false);
            Self::update_ref_button_state(&btn, fk_info, val);

            let tab_clone = self.clone();
            let entry_ref = entry.clone();
            let db_name = database.to_string();
            let target_table = fk_info.referenced_table.clone();
            let target_col = fk_info.referenced_column.clone();
            btn.connect_clicked(move |_| {
                let current_val = DbValue::from_editor_str(&entry_ref.text());
                tab_clone.follow_reference(&db_name, &target_table, &target_col, &current_val);
            });

            cell_box.append(&btn);
            Some(btn)
        } else {
            None
        };

        (cell_box, entry, ref_btn_opt, long_text_btn_opt)
    }

    fn append_draft_row(&self) {
        let page_opt = self.current_page.borrow().clone();
        let Some(page) = page_opt else {
            return;
        };

        let mut initial_map = HashMap::new();
        for col in &page.columns {
            initial_map.insert(col.name.clone(), initial_draft_value_for_column(col));
        }

        let draft_idx = {
            let mut drafts = self.draft_rows.borrow_mut();
            let idx = drafts.len();
            drafts.push(initial_map.clone());
            idx
        };

        let grid_row = (page.rows.len() + draft_idx + 1) as i32;
        self.attach_draft_row_widgets(grid_row, draft_idx, &page.columns, &initial_map);
        self.apply_column_widths();
        self.update_dirty_badge();

        let scroll_for_idle = self.scroll.clone();
        gtk::glib::idle_add_local_once(move || {
            scroll_for_idle.hadjustment().set_value(0.0);
            let vadj = scroll_for_idle.vadjustment();
            let bottom = (vadj.upper() - vadj.page_size()).max(vadj.lower());
            vadj.set_value(bottom);
        });
    }

    fn attach_draft_row_widgets(
        &self,
        grid_row: i32,
        draft_idx: usize,
        columns: &[ColumnInfo],
        draft_map: &HashMap<String, DbValue>,
    ) {
        let database = self
            .current_page
            .borrow()
            .as_ref()
            .map(|p| p.database.clone())
            .unwrap_or_default();
        let relations = self.relations.borrow().clone();

        let badge = gtk::Label::new(Some("NEW"));
        badge.add_css_class("accent");
        badge.add_css_class("caption-heading");
        badge.add_css_class("spreadsheet-row-num");
        badge.set_ellipsize(gtk::pango::EllipsizeMode::End);
        badge.set_max_width_chars(1);
        self.grid.attach(&badge, 0, grid_row, 1, 1);

        for (col_idx, col_info) in columns.iter().enumerate() {
            let val = draft_map
                .get(&col_info.name)
                .cloned()
                .unwrap_or_else(|| initial_draft_value_for_column(col_info));
            let fk_opt = relations
                .iter()
                .find(|fk| fk.column_name == col_info.name)
                .cloned();
            let has_fk = fk_opt.is_some();

            if let Some(enum_vals) = col_info.enum_values() {
                let (cell_box, dropdown, options_rc, ref_btn_opt) = self
                    .create_enum_spreadsheet_cell(
                        &database,
                        &enum_vals,
                        col_info.is_nullable,
                        &val,
                        true,
                        fk_opt.as_ref(),
                    );
                cell_box.add_css_class("cell-dirty");

                if let Some(selected_val) = options_rc.get(dropdown.selected() as usize) {
                    if let Some(row_map) = self.draft_rows.borrow_mut().get_mut(draft_idx) {
                        row_map.insert(col_info.name.clone(), selected_val.clone());
                    }
                }

                let tab_clone = self.clone();
                let col_name = col_info.name.clone();
                dropdown.connect_selected_notify(move |dd| {
                    let new_val = options_rc
                        .get(dd.selected() as usize)
                        .cloned()
                        .unwrap_or(DbValue::Null);
                    if let (Some(btn), Some(fk)) = (&ref_btn_opt, &fk_opt) {
                        Self::update_ref_button_state(btn, fk, &new_val);
                    }
                    if new_val.is_null() {
                        dd.add_css_class("cell-null");
                    } else {
                        dd.remove_css_class("cell-null");
                    }
                    if let Some(row_map) = tab_clone.draft_rows.borrow_mut().get_mut(draft_idx) {
                        row_map.insert(col_name.clone(), new_val);
                    }
                });

                self.grid
                    .attach(&cell_box, (col_idx + 1) as i32, grid_row, 1, 1);

                let txt = val.display_text();
                let fk_extra = if has_fk && !val.is_null() && !txt.is_empty() {
                    FK_BTN_EXTRA_WIDTH
                } else {
                    0
                };
                let cell_w = measure_single_line_text(&cell_box, txt)
                    + CELL_EXTRA_PADDING
                    + fk_extra
                    + ENUM_DROPDOWN_EXTRA_WIDTH;
                if let Some(nat_w) = self.natural_col_widths.borrow_mut().get_mut(col_idx) {
                    *nat_w = (*nat_w).max(cell_w);
                }

                if col_idx == 0 {
                    let dd_clone = dropdown.clone();
                    gtk::glib::idle_add_local_once(move || {
                        dd_clone.grab_focus();
                    });
                }
            } else {
                let is_long_text = col_info.is_long_text();
                let (cell_box, entry, ref_btn_opt, long_text_btn_opt) = self
                    .create_spreadsheet_cell(
                        &database,
                        &val,
                        true,
                        is_long_text,
                        fk_opt.as_ref(),
                    );
                cell_box.add_css_class("cell-dirty");

                if is_long_text {
                    let initial_val = initial_draft_value_for_column(col_info);
                    let target = LongTextPanelTarget {
                        kind: LongTextTargetKind::Draft {
                            draft_idx,
                            initial_val,
                        },
                        col_info: col_info.clone(),
                        cell_box: cell_box.clone(),
                        cell_entry: entry.clone(),
                        ref_btn_opt,
                        fk_opt,
                    };
                    self.attach_long_text_cell_handlers(target, long_text_btn_opt);
                } else {
                    let tab_clone = self.clone();
                    let col_name = col_info.name.clone();
                    attach_blur_null_handler(&entry, col_info.is_nullable);
                    entry.connect_changed(move |e| {
                        let new_val = DbValue::from_editor_str(&e.text());
                        if let (Some(btn), Some(fk)) = (&ref_btn_opt, &fk_opt) {
                            Self::update_ref_button_state(btn, fk, &new_val);
                        }
                        if new_val.is_null() {
                            e.add_css_class("cell-null");
                        } else {
                            e.remove_css_class("cell-null");
                        }
                        if let Some(row_map) = tab_clone.draft_rows.borrow_mut().get_mut(draft_idx)
                        {
                            row_map.insert(col_name.clone(), new_val);
                        }
                    });
                }

                self.grid
                    .attach(&cell_box, (col_idx + 1) as i32, grid_row, 1, 1);

                let preview_cow;
                let txt = if is_long_text {
                    preview_cow = format_long_text_preview(&val);
                    preview_cow.as_str()
                } else {
                    val.display_text()
                };
                let fk_extra = if has_fk && !val.is_null() && !txt.is_empty() {
                    FK_BTN_EXTRA_WIDTH
                } else {
                    0
                };
                let long_text_extra = if is_long_text { FK_BTN_EXTRA_WIDTH } else { 0 };
                let raw_cell_w = measure_single_line_text(&entry, txt)
                    + CELL_EXTRA_PADDING
                    + fk_extra
                    + long_text_extra;
                let cell_w = if is_long_text {
                    raw_cell_w.min(MAX_LONG_TEXT_AUTO_COL_WIDTH)
                } else {
                    raw_cell_w
                };
                if let Some(nat_w) = self.natural_col_widths.borrow_mut().get_mut(col_idx) {
                    *nat_w = (*nat_w).max(cell_w);
                }

                if col_idx == 0 {
                    let e_clone = entry.clone();
                    gtk::glib::idle_add_local_once(move || {
                        e_clone.grab_focus();
                        if !is_long_text {
                            e_clone.select_region(0, -1);
                        }
                    });
                }
            }
        }
    }

    fn update_dirty_badge(&self) {
        let dirty_count = self.dirty_cells.borrow().len();
        let draft_count = self.draft_rows.borrow().len();
        let total = dirty_count + draft_count;
        if total > 0 {
            self.dirty_badge_btn
                .set_label(&format!("Commit {} Change(s) (Ctrl+S)", total));
            self.dirty_badge_btn.set_visible(true);
        } else {
            self.dirty_badge_btn.set_visible(false);
        }
    }

    pub fn commit_pending_changes(&self) {
        let page_opt = self.current_page.borrow().clone();
        let Some(page) = page_opt else {
            return;
        };

        let dirty = self.dirty_cells.borrow().clone();
        let drafts = self.draft_rows.borrow().clone();
        if dirty.is_empty() && drafts.is_empty() {
            return;
        }

        let (updates, inserts) = build_pending_content_changes(&page, &dirty, &drafts);

        let query_for_refresh = self.query_state.borrow().clone();
        let _ = self
            .cmd_tx
            .send_blocking(BackendCommand::CommitContentChanges {
                database: page.database,
                table: page.table,
                updates,
                inserts,
                query_for_refresh,
            });
    }

    fn delete_checked_rows(&self, parent: &gtk::Widget) {
        let page_opt = self.current_page.borrow().clone();
        let Some(page) = page_opt else {
            return;
        };

        let mut pks_to_delete: Vec<PrimaryKeyValue> = Vec::new();
        for (row_idx, check) in self.selected_rows.borrow().iter() {
            if check.is_active() {
                if let Some(row_data) = page.rows.get(*row_idx) {
                    if let Some(pk) = &row_data.pk {
                        pks_to_delete.push(pk.clone());
                    }
                }
            }
        }

        if pks_to_delete.is_empty() {
            return;
        }

        let count = pks_to_delete.len();
        let alert = adw::AlertDialog::builder()
            .heading(format!("Delete {} Selected Row(s)?", count))
            .body(format!(
                "This will permanently delete {} row(s) from '{}'.",
                count, page.table
            ))
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("delete", "Delete Rows");
        alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let cmd_tx = self.cmd_tx.clone();
        let db = page.database.clone();
        let tbl = page.table.clone();
        let query_for_refresh = self.query_state.borrow().clone();
        alert.connect_response(None, move |_, resp| {
            if resp == "delete" {
                let _ = cmd_tx.send_blocking(BackendCommand::DeleteRows {
                    database: db.clone(),
                    table: tbl.clone(),
                    pks: pks_to_delete.clone(),
                    query_for_refresh: query_for_refresh.clone(),
                });
            }
        });

        alert.present(Some(parent));
    }

    fn export_data<F>(&self, parent: &gtk::Widget, as_csv: bool, on_toast: F)
    where
        F: Fn(&str) + Clone + 'static,
    {
        let page_opt = self.current_page.borrow().clone();
        let Some(page) = page_opt else {
            on_toast("No table data loaded to export");
            return;
        };

        let ext = if as_csv { "csv" } else { "json" };
        let dialog = gtk::FileDialog::builder()
            .title(if as_csv {
                "Export Table Content to CSV"
            } else {
                "Export Table Content to JSON"
            })
            .initial_name(format!("{}.{}", page.table, ext))
            .build();

        let window = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        dialog.save(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    let write_res = if as_csv {
                        export_page_to_csv(&page, &path)
                    } else {
                        export_page_to_json(&page, &path)
                    };
                    match write_res {
                        Ok(()) => on_toast(&format!("Exported to {}", path.display())),
                        Err(e) => on_toast(&format!("Export failed: {}", e)),
                    }
                }
            }
        });
    }
}

/// When a cell entry loses focus with empty text, a nullable column reverts to SQL NULL.
/// Returns the replacement entry text, or `None` if the text should stay as-is.
pub fn blur_normalized_text(text: &str, is_nullable: bool) -> Option<&'static str> {
    if is_nullable && text.is_empty() {
        Some("NULL")
    } else {
        None
    }
}

/// Attach a focus-leave handler that reverts an emptied nullable cell to NULL.
/// Setting the entry text re-triggers its `changed` handler, which records the value.
fn attach_blur_null_handler(entry: &gtk::Entry, is_nullable: bool) {
    let focus = gtk::EventControllerFocus::new();
    let entry_weak = entry.downgrade();
    focus.connect_leave(move |_| {
        if let Some(e) = entry_weak.upgrade()
            && let Some(txt) = blur_normalized_text(&e.text(), is_nullable)
        {
            e.set_text(txt);
        }
    });
    entry.add_controller(focus);
}

pub fn build_enum_dropdown_options(
    enum_values: &[String],
    is_nullable: bool,
    current_val: &DbValue,
) -> (Vec<DbValue>, usize) {
    let mut options: Vec<DbValue> = enum_values
        .iter()
        .map(|v| DbValue::Text(v.clone()))
        .collect();

    if is_nullable {
        options.push(DbValue::Null);
    }

    let selected_idx = if let Some(pos) = options.iter().position(|v| v == current_val) {
        pos
    } else {
        match current_val {
            DbValue::Text(s) => {
                if let Some(pos) = options.iter().position(|v| match v {
                    DbValue::Text(opt_s) => opt_s.eq_ignore_ascii_case(s),
                    DbValue::Null => false,
                }) {
                    pos
                } else {
                    options.push(DbValue::Text(s.clone()));
                    options.len() - 1
                }
            }
            DbValue::Null => 0,
        }
    };

    (options, selected_idx)
}

pub fn initial_draft_value_for_column(col: &ColumnInfo) -> DbValue {
    if let Some(def) = &col.default_value {
        let cleaned_def = if col.enum_values().is_some() {
            def.split("::")
                .next()
                .unwrap_or(def)
                .trim()
                .trim_matches('\'')
                .to_string()
        } else {
            def.clone()
        };
        if col.is_nullable && cleaned_def.eq_ignore_ascii_case("NULL") {
            DbValue::Null
        } else {
            DbValue::Text(cleaned_def)
        }
    } else if !col.is_nullable {
        if let Some(first_enum) = col
            .enum_values()
            .and_then(|vals| vals.into_iter().next())
        {
            DbValue::Text(first_enum)
        } else {
            DbValue::Null
        }
    } else {
        DbValue::Null
    }
}

pub fn build_pending_content_changes(
    page: &TablePage,
    dirty: &HashMap<(usize, String), DbValue>,
    drafts: &[HashMap<String, DbValue>],
) -> (Vec<RowUpdate>, Vec<Vec<(String, DbValue)>>) {
    let mut by_row: std::collections::BTreeMap<usize, Vec<(String, DbValue)>> =
        std::collections::BTreeMap::new();
    for ((row_idx, col_name), val) in dirty {
        by_row
            .entry(*row_idx)
            .or_default()
            .push((col_name.clone(), val.clone()));
    }

    let mut updates = Vec::new();
    for (row_idx, mut changes) in by_row {
        if let Some(row_data) = page.rows.get(row_idx) {
            if let Some(pk) = &row_data.pk {
                changes.sort_by(|a, b| a.0.cmp(&b.0));
                updates.push(RowUpdate {
                    pk: pk.clone(),
                    changes,
                });
            }
        }
    }

    let mut inserts = Vec::new();
    for draft_map in drafts {
        let mut row_vals = Vec::new();
        for col in &page.columns {
            if let Some(v) = draft_map.get(&col.name) {
                // Omit auto-increment NULL primary keys so the DB generates them
                if col.is_primary_key && v.is_null() {
                    continue;
                }
                row_vals.push((col.name.clone(), v.clone()));
            }
        }
        inserts.push(row_vals);
    }

    (updates, inserts)
}

fn export_page_to_csv(page: &TablePage, path: &std::path::Path) -> anyhow::Result<()> {
    let mut wtr = csv::Writer::from_path(path)?;
    let headers: Vec<&str> = page.columns.iter().map(|c| c.name.as_str()).collect();
    wtr.write_record(&headers)?;
    for row in &page.rows {
        let record: Vec<&str> = row
            .values
            .iter()
            .map(|v| match v {
                DbValue::Null => "",
                DbValue::Text(s) => s.as_str(),
            })
            .collect();
        wtr.write_record(&record)?;
    }
    wtr.flush()?;
    Ok(())
}

fn export_page_to_json(page: &TablePage, path: &std::path::Path) -> anyhow::Result<()> {
    let mut json_rows = Vec::with_capacity(page.rows.len());
    for row in &page.rows {
        let mut obj = serde_json::Map::new();
        for (idx, col) in page.columns.iter().enumerate() {
            let val = match row.values.get(idx) {
                Some(DbValue::Text(s)) => serde_json::Value::String(s.clone()),
                _ => serde_json::Value::Null,
            };
            obj.insert(col.name.clone(), val);
        }
        json_rows.push(serde_json::Value::Object(obj));
    }
    let formatted = serde_json::to_string_pretty(&json_rows)?;
    let mut f = File::create(path)?;
    f.write_all(formatted.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{TableRowData, is_long_text_data_type, parse_enum_data_type};

    #[test]
    fn test_columns_show_full_content_when_available_width_allows() {
        let natural = vec![80, 220, 340];
        let manual = vec![None, None, None];
        let widths = compute_auto_column_widths(&natural, &manual, 900);
        // Plenty of width: wide columns expand to their full natural widths (220, 340),
        // the narrow column expands to DEFAULT_DATA_COL_WIDTH (160), and the remaining
        // 180px is distributed evenly (+60px each) so the table fills all 900px.
        assert_eq!(widths, vec![220, 280, 400]);
        assert_eq!(widths.iter().sum::<i32>(), 900);
    }

    #[test]
    fn test_water_fill_prioritizes_full_content_over_padding_narrow_columns() {
        let natural = vec![80, 240, 480];
        let manual = vec![None, None, None];
        // Total natural is 800, available is 800: all 3 columns show 100% of their content
        let widths = compute_auto_column_widths(&natural, &manual, 800);
        assert_eq!(widths, vec![80, 240, 480]);

        // Available is 700: col 0 (80) and col 1 (240) fit their full content,
        // and col 2 gets the remaining 380px.
        let widths_constrained = compute_auto_column_widths(&natural, &manual, 700);
        assert_eq!(widths_constrained, vec![80, 240, 380]);
    }

    #[test]
    fn test_manual_column_width_override_is_preserved() {
        let natural = vec![80, 300, 400];
        let manual = vec![None, Some(120), None];
        let widths = compute_auto_column_widths(&natural, &manual, 700);
        assert_eq!(widths, vec![170, 120, 410]);
        assert_eq!(widths.iter().sum::<i32>(), 700);
    }

    #[test]
    fn test_columns_always_fill_surrounding_panel_width() {
        // Indexes-style table: 5 auto data columns + 1 fixed Actions column (84px) in a 1100px panel
        let natural = vec![246, 92, 202, 86, 86, 84];
        let manual = vec![None, None, None, None, None, Some(84)];
        let widths = compute_auto_column_widths(&natural, &manual, 1100);
        assert_eq!(widths[5], 84);
        assert_eq!(widths.iter().sum::<i32>(), 1100);
    }

    #[test]
    fn test_is_long_text_data_type_detection() {
        assert!(is_long_text_data_type("text"));
        assert!(is_long_text_data_type("TEXT"));
        assert!(is_long_text_data_type("tinytext"));
        assert!(is_long_text_data_type("mediumtext"));
        assert!(is_long_text_data_type("LONGTEXT"));
        assert!(is_long_text_data_type("citext"));
        assert!(is_long_text_data_type("clob"));
        assert!(is_long_text_data_type("json"));
        assert!(is_long_text_data_type("jsonb"));
        assert!(is_long_text_data_type("xml"));
        assert!(is_long_text_data_type("varchar"));
        assert!(is_long_text_data_type("varchar(500)"));
        assert!(is_long_text_data_type("VARCHAR(MAX)"));

        assert!(!is_long_text_data_type("int"));
        assert!(!is_long_text_data_type("bigint(20)"));
        assert!(!is_long_text_data_type("varchar(255)"));
        assert!(!is_long_text_data_type("varchar(64)"));
        assert!(!is_long_text_data_type("boolean"));
        assert!(!is_long_text_data_type("timestamp"));
        assert!(!is_long_text_data_type("enum('plain_text','rich_text')"));
    }

    #[test]
    fn test_format_long_text_preview_renders_single_line() {
        assert_eq!(format_long_text_preview(&DbValue::Null), "NULL");
        assert_eq!(
            format_long_text_preview(&DbValue::Text("first\nsecond\r\nthird".to_string())),
            "first ↵ second ↵ third"
        );
    }

    #[test]
    fn test_parse_enum_data_type_and_dropdown_options() {
        assert_eq!(
            parse_enum_data_type("enum('active','pending','archived')"),
            Some(vec![
                "active".to_string(),
                "pending".to_string(),
                "archived".to_string()
            ])
        );
        assert_eq!(
            parse_enum_data_type("ENUM('it''s', 'a, b', '')"),
            Some(vec![
                "it's".to_string(),
                "a, b".to_string(),
                "".to_string()
            ])
        );
        assert_eq!(parse_enum_data_type("varchar(255)"), None);
        assert_eq!(parse_enum_data_type("set('a','b')"), None);

        let enum_vals = vec!["active".to_string(), "pending".to_string()];

        // Non-nullable enum: only enum values, no NULL option
        let (non_null_opts, non_null_idx) = build_enum_dropdown_options(
            &enum_vals,
            false,
            &DbValue::Text("pending".to_string()),
        );
        assert_eq!(
            non_null_opts,
            vec![
                DbValue::Text("active".to_string()),
                DbValue::Text("pending".to_string()),
            ]
        );
        assert_eq!(non_null_idx, 1);

        // Nullable enum: includes NULL option and selects it when value is Null
        let (null_opts, null_idx) =
            build_enum_dropdown_options(&enum_vals, true, &DbValue::Null);
        assert_eq!(
            null_opts,
            vec![
                DbValue::Text("active".to_string()),
                DbValue::Text("pending".to_string()),
                DbValue::Null,
            ]
        );
        assert_eq!(null_idx, 2);
    }

    #[test]
    fn test_initial_draft_value_for_enum_columns() {
        let non_nullable_enum = ColumnInfo {
            ordinal: 1,
            name: "status".to_string(),
            data_type: "enum('active','pending')".to_string(),
            is_nullable: false,
            is_primary_key: false,
            default_value: None,
            extra: String::new(),
        };
        assert_eq!(
            initial_draft_value_for_column(&non_nullable_enum),
            DbValue::Text("active".to_string())
        );

        let nullable_enum = ColumnInfo {
            is_nullable: true,
            ..non_nullable_enum.clone()
        };
        assert_eq!(initial_draft_value_for_column(&nullable_enum), DbValue::Null);

        let pg_enum_with_default = ColumnInfo {
            ordinal: 2,
            name: "role".to_string(),
            data_type: "user_role".to_string(),
            is_nullable: false,
            is_primary_key: false,
            default_value: Some("'member'::user_role".to_string()),
            extra: "enum('admin','member','guest')".to_string(),
        };
        assert_eq!(
            pg_enum_with_default.enum_values(),
            Some(vec![
                "admin".to_string(),
                "member".to_string(),
                "guest".to_string()
            ])
        );
        assert_eq!(
            initial_draft_value_for_column(&pg_enum_with_default),
            DbValue::Text("member".to_string())
        );
    }

    #[test]
    fn test_build_pending_content_changes_groups_updates_and_omits_null_pk_on_drafts() {
        let page = TablePage {
            database: "shop".to_string(),
            table: "orders".to_string(),
            columns: vec![
                ColumnInfo {
                    ordinal: 1,
                    name: "id".to_string(),
                    data_type: "int".to_string(),
                    is_nullable: false,
                    is_primary_key: true,
                    default_value: None,
                    extra: "auto_increment".to_string(),
                },
                ColumnInfo {
                    ordinal: 2,
                    name: "status".to_string(),
                    data_type: "varchar(32)".to_string(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 3,
                    name: "notes".to_string(),
                    data_type: "text".to_string(),
                    is_nullable: true,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
            ],
            rows: vec![
                TableRowData {
                    pk: Some(PrimaryKeyValue {
                        columns: vec![("id".to_string(), "10".to_string())],
                    }),
                    values: vec![
                        DbValue::Text("10".to_string()),
                        DbValue::Text("pending".to_string()),
                        DbValue::Null,
                    ],
                },
                TableRowData {
                    // Row without PK (e.g., view or keyless table) must be skipped
                    pk: None,
                    values: vec![
                        DbValue::Text("11".to_string()),
                        DbValue::Text("pending".to_string()),
                        DbValue::Null,
                    ],
                },
            ],
            total_rows: 2,
            page_limit: 100,
            page_offset: 0,
        };

        let mut dirty = HashMap::new();
        dirty.insert(
            (0, "status".to_string()),
            DbValue::Text("shipped".to_string()),
        );
        dirty.insert((0, "notes".to_string()), DbValue::Null);
        dirty.insert(
            (1, "status".to_string()),
            DbValue::Text("ignored_no_pk".to_string()),
        );

        let mut draft1 = HashMap::new();
        // NULL primary key must be omitted so AUTO_INCREMENT / SERIAL generates it
        draft1.insert("id".to_string(), DbValue::Null);
        draft1.insert("status".to_string(), DbValue::Text("new".to_string()));
        draft1.insert("notes".to_string(), DbValue::Null);

        let mut draft2 = HashMap::new();
        // Explicit primary key must be preserved
        draft2.insert("id".to_string(), DbValue::Text("99".to_string()));
        draft2.insert("status".to_string(), DbValue::Text("manual".to_string()));

        let (updates, inserts) =
            build_pending_content_changes(&page, &dirty, &[draft1, draft2]);

        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates[0].pk.columns,
            vec![("id".to_string(), "10".to_string())]
        );
        assert_eq!(
            updates[0].changes,
            vec![
                ("notes".to_string(), DbValue::Null),
                ("status".to_string(), DbValue::Text("shipped".to_string())),
            ]
        );

        assert_eq!(
            inserts,
            vec![
                vec![
                    ("status".to_string(), DbValue::Text("new".to_string())),
                    ("notes".to_string(), DbValue::Null),
                ],
                vec![
                    ("id".to_string(), DbValue::Text("99".to_string())),
                    ("status".to_string(), DbValue::Text("manual".to_string())),
                ],
            ]
        );
    }

    #[test]
    fn test_export_page_to_csv_and_json() {
        let page = TablePage {
            database: "shop".to_string(),
            table: "items".to_string(),
            columns: vec![
                ColumnInfo {
                    ordinal: 1,
                    name: "id".to_string(),
                    data_type: "int".to_string(),
                    is_nullable: false,
                    is_primary_key: true,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 2,
                    name: "label".to_string(),
                    data_type: "text".to_string(),
                    is_nullable: true,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
            ],
            rows: vec![
                TableRowData {
                    pk: None,
                    values: vec![
                        DbValue::Text("1".to_string()),
                        DbValue::Text("a, \"quoted\" item".to_string()),
                    ],
                },
                TableRowData {
                    pk: None,
                    values: vec![DbValue::Text("2".to_string()), DbValue::Null],
                },
            ],
            total_rows: 2,
            page_limit: 100,
            page_offset: 0,
        };

        let tmp_dir =
            std::env::temp_dir().join(format!("dbased_page_export_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let csv_path = tmp_dir.join("items.csv");
        let json_path = tmp_dir.join("items.json");

        export_page_to_csv(&page, &csv_path).unwrap();
        let csv_str = std::fs::read_to_string(&csv_path).unwrap();
        assert_eq!(csv_str, "id,label\n1,\"a, \"\"quoted\"\" item\"\n2,\n");

        export_page_to_json(&page, &json_path).unwrap();
        let json_val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(
            json_val,
            serde_json::json!([
                {"id": "1", "label": "a, \"quoted\" item"},
                {"id": "2", "label": null}
            ])
        );

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn empty_nullable_cell_becomes_null_on_blur() {
        assert_eq!(blur_normalized_text("", true), Some("NULL"));
    }

    #[test]
    fn empty_non_nullable_cell_is_untouched_on_blur() {
        assert_eq!(blur_normalized_text("", false), None);
    }

    #[test]
    fn non_empty_nullable_cell_is_untouched_on_blur() {
        assert_eq!(blur_normalized_text("abc", true), None);
        assert_eq!(blur_normalized_text(" ", true), None);
    }

    #[test]
    fn test_set_page_does_not_leak_blurred_nullable_cell_into_new_page_dirty_cells() {
        crate::ui::test_util::run_gtk_test(|| {
            let (cmd_tx, _cmd_rx) = async_channel::unbounded();
            let tab = ContentTab::new(cmd_tx, |_| {});

            let win = adw::Window::builder()
                .default_width(600)
                .default_height(300)
                .content(&tab.root)
                .build();
            win.present();

            let columns = vec![
                ColumnInfo {
                    ordinal: 1,
                    name: "id".to_string(),
                    data_type: "int".to_string(),
                    is_nullable: false,
                    is_primary_key: true,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 2,
                    name: "nickname".to_string(),
                    data_type: "varchar(64)".to_string(),
                    is_nullable: true,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
            ];

            tab.set_page(TablePage {
                database: "app".to_string(),
                table: "users".to_string(),
                columns: columns.clone(),
                rows: vec![TableRowData {
                    pk: Some(PrimaryKeyValue {
                        columns: vec![("id".to_string(), "1".to_string())],
                    }),
                    values: vec![
                        DbValue::Text("1".to_string()),
                        DbValue::Text("alice".to_string()),
                    ],
                }],
                total_rows: 1,
                page_limit: 100,
                page_offset: 0,
            });

            let ctx = gtk::glib::MainContext::default();
            while ctx.iteration(false) {}

            // Focus the nullable "nickname" cell at column 2, row 1 and clear its text to ""
            let cell_box = tab.grid.child_at(2, 1).expect("nickname cell box");
            let entry = cell_box
                .first_child()
                .and_then(|w| w.downcast::<gtk::Entry>().ok())
                .expect("nickname entry");
            gtk::prelude::RootExt::set_focus(&win, Some(&entry));
            while ctx.iteration(false) {}
            entry.set_text("");
            while ctx.iteration(false) {}

            assert!(!tab.dirty_cells.borrow().is_empty());

            // Now load a new page while the cleared nullable cell still holds focus
            tab.set_page(TablePage {
                database: "app".to_string(),
                table: "users".to_string(),
                columns,
                rows: vec![TableRowData {
                    pk: Some(PrimaryKeyValue {
                        columns: vec![("id".to_string(), "2".to_string())],
                    }),
                    values: vec![
                        DbValue::Text("2".to_string()),
                        DbValue::Text("bob".to_string()),
                    ],
                }],
                total_rows: 1,
                page_limit: 100,
                page_offset: 100,
            });
            while ctx.iteration(false) {}

            assert!(
                tab.dirty_cells.borrow().is_empty(),
                "dirty_cells must be empty after loading a new page, got: {:?}",
                *tab.dirty_cells.borrow()
            );
            let new_cell_box = tab.grid.child_at(2, 1).expect("new nickname cell box");
            let new_entry = new_cell_box
                .first_child()
                .and_then(|w| w.downcast::<gtk::Entry>().ok())
                .expect("new nickname entry");
            assert_eq!(new_entry.text().as_str(), "bob");

            win.close();
        });
    }
}
