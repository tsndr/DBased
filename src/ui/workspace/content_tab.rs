use crate::db::driver::escape_sql_literal;
use crate::db::models::{
    BackendCommand, ColumnInfo, ContentQuery, DbValue, ForeignKeyInfo, PrimaryKeyValue, RowUpdate,
    SortDirection, TablePage,
};
use crate::ui::workspace::{MIN_COLUMN_WIDTH, build_resizable_header_cell};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::rc::Rc;

const DEFAULT_INDEX_COL_WIDTH: i32 = 64;
const DEFAULT_DATA_COL_WIDTH: i32 = 160;
const HEADER_EXTRA_PADDING: i32 = 34;
const CELL_EXTRA_PADDING: i32 = 28;
const FK_BTN_EXTRA_WIDTH: i32 = 30;
const MAX_MEASURE_CHARS: usize = 1000;
const MOUSE_BUTTON_BACK: u32 = 8;
const MOUSE_BUTTON_FORWARD: u32 = 9;

#[derive(Debug, Clone)]
struct NavigationEntry {
    database: String,
    table: String,
    query: ContentQuery,
}

#[derive(Clone)]
pub struct ContentTab {
    pub root: gtk::Box,
    scroll: gtk::ScrolledWindow,
    grid: gtk::Grid,
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
    col_widths: Rc<RefCell<HashMap<String, i32>>>,
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

        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&grid)
            .build();
        root.append(&scroll);

        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Bottom Pagination Bar
        let bottom_bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        bottom_bar.set_margin_top(6);
        bottom_bar.set_margin_bottom(6);
        bottom_bar.set_margin_start(12);
        bottom_bar.set_margin_end(12);

        let status_label = gtk::Label::new(Some("No table selected"));
        status_label.add_css_class("dim-label");
        status_label.set_halign(gtk::Align::Start);
        status_label.set_hexpand(true);

        let hint_label = gtk::Label::new(Some(
            "Click cell to edit · Drag column borders to resize · Ctrl+S to commit",
        ));
        hint_label.add_css_class("caption");
        hint_label.add_css_class("dim-label");

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

        bottom_bar.append(&status_label);
        bottom_bar.append(&hint_label);
        bottom_bar.append(&page_size_lbl);
        bottom_bar.append(&page_size_dd);
        bottom_bar.append(&prev_page_btn);
        bottom_bar.append(&next_page_btn);
        root.append(&bottom_bar);

        let tab = Self {
            root: root.clone(),
            scroll: scroll.clone(),
            grid,
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
        self.nav_back_stack.borrow_mut().clear();
        self.nav_forward_stack.borrow_mut().clear();
        *self.nav_in_progress.borrow_mut() = false;
        *self.current_page.borrow_mut() = None;
        self.update_nav_buttons();
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
        }

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
        let total_avail = (viewport_w - idx_w).max(0);
        let widths = compute_auto_column_widths(&natural_widths, &manual_overrides, total_avail);

        for ((_, hdr_cell), target_w) in header_cells.iter().zip(widths.into_iter()) {
            if hdr_cell.width_request() != target_w {
                hdr_cell.set_width_request(target_w);
            }
        }
    }

    fn measure_single_line_text<W: IsA<gtk::Widget>>(widget: &W, text: &str) -> i32 {
        if text.is_empty() {
            return 0;
        }
        let truncated: String;
        let sample = if text.chars().count() > MAX_MEASURE_CHARS {
            truncated = text.chars().take(MAX_MEASURE_CHARS).collect();
            truncated.as_str()
        } else {
            text
        };
        let layout = widget.create_pango_layout(Some(sample));
        layout.set_single_paragraph_mode(true);
        layout.pixel_size().0
    }

    fn render_grid(&self) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
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
            check.set_sensitive(has_pk && row_data.pk.is_some());
            let num_lbl = gtk::Label::new(Some(&(page.page_offset as usize + row_idx + 1).to_string()));
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

                let (cell_box, entry, ref_btn_opt) = self.create_spreadsheet_cell(
                    &page.database,
                    &current_val,
                    has_pk && row_data.pk.is_some(),
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

                let tab_clone = self.clone();
                let col_name = col_info.name.clone();
                let orig = original_val.clone();
                let cell_box_ref = cell_box.clone();
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

                self.grid
                    .attach(&cell_box, (col_idx + 1) as i32, grid_row, 1, 1);
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
            let hdr_w = Self::measure_single_line_text(hdr_lbl, btn_label) + HEADER_EXTRA_PADDING;

            let has_fk = relations.iter().any(|fk| fk.column_name == col_info.name);
            let mut widest_val: Option<&DbValue> = None;
            let mut max_score: usize = 0;

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
                    widest_val = Some(val);
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
                    widest_val = Some(val);
                }
            }

            let cell_w = if let Some(val) = widest_val {
                let txt = val.display_text();
                let text_px = if let Some(ref entry_widget) = sample_entry {
                    Self::measure_single_line_text(entry_widget, txt)
                } else {
                    Self::measure_single_line_text(hdr_lbl, txt)
                };
                let fk_extra = if has_fk && !val.is_null() && !txt.is_empty() {
                    FK_BTN_EXTRA_WIDTH
                } else {
                    0
                };
                text_px + CELL_EXTRA_PADDING + fk_extra
            } else {
                0
            };

            natural_widths.push(hdr_w.max(cell_w).max(MIN_COLUMN_WIDTH));
        }
        drop(dirty);

        *self.natural_col_widths.borrow_mut() = natural_widths;
        self.apply_column_widths();

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

    fn create_spreadsheet_cell(
        &self,
        database: &str,
        val: &DbValue,
        editable: bool,
        fk: Option<&ForeignKeyInfo>,
    ) -> (gtk::Box, gtk::Entry, Option<gtk::Button>) {
        let cell_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cell_box.add_css_class("spreadsheet-cell");
        cell_box.set_overflow(gtk::Overflow::Hidden);

        let entry = gtk::Entry::new();
        entry.set_has_frame(false);
        entry.add_css_class("spreadsheet-cell-entry");
        entry.add_css_class("monospace");
        entry.set_editable(editable);
        entry.set_hexpand(true);
        entry.set_width_chars(1);
        entry.set_max_width_chars(1);
        entry.set_text(val.display_text());
        if val.is_null() {
            entry.add_css_class("cell-null");
        }

        // When user clicks or focuses the cell, immediately select its full text content
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
        cell_box.append(&entry);

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

        (cell_box, entry, ref_btn_opt)
    }

    fn append_draft_row(&self) {
        let page_opt = self.current_page.borrow().clone();
        let Some(page) = page_opt else {
            return;
        };

        let mut initial_map = HashMap::new();
        for col in &page.columns {
            if let Some(def) = &col.default_value {
                initial_map.insert(col.name.clone(), DbValue::Text(def.clone()));
            } else {
                initial_map.insert(col.name.clone(), DbValue::Null);
            }
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
                .unwrap_or(DbValue::Null);
            let fk_opt = relations
                .iter()
                .find(|fk| fk.column_name == col_info.name)
                .cloned();
            let has_fk = fk_opt.is_some();
            let (cell_box, entry, ref_btn_opt) =
                self.create_spreadsheet_cell(&database, &val, true, fk_opt.as_ref());
            cell_box.add_css_class("cell-dirty");

            let tab_clone = self.clone();
            let col_name = col_info.name.clone();
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
            let cell_w = Self::measure_single_line_text(&entry, txt) + CELL_EXTRA_PADDING + fk_extra;
            if let Some(nat_w) = self.natural_col_widths.borrow_mut().get_mut(col_idx) {
                *nat_w = (*nat_w).max(cell_w);
            }

            if col_idx == 0 {
                let e_clone = entry.clone();
                gtk::glib::idle_add_local_once(move || {
                    e_clone.grab_focus();
                    e_clone.select_region(0, -1);
                });
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

        // Group dirty cells by row_idx
        let mut by_row: HashMap<usize, Vec<(String, DbValue)>> = HashMap::new();
        for ((row_idx, col_name), val) in dirty {
            by_row.entry(row_idx).or_default().push((col_name, val));
        }

        let mut updates = Vec::new();
        for (row_idx, changes) in by_row {
            if let Some(row_data) = page.rows.get(row_idx) {
                if let Some(pk) = &row_data.pk {
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

fn compute_auto_column_widths(
    natural_widths: &[i32],
    manual_overrides: &[Option<i32>],
    total_avail: i32,
) -> Vec<i32> {
    let num_cols = natural_widths.len();
    let mut widths = Vec::with_capacity(num_cols);
    let mut is_manual = Vec::with_capacity(num_cols);

    for i in 0..num_cols {
        if let Some(Some(w)) = manual_overrides.get(i) {
            widths.push((*w).max(MIN_COLUMN_WIDTH));
            is_manual.push(true);
        } else {
            let nat_w = natural_widths[i].max(MIN_COLUMN_WIDTH);
            widths.push(nat_w.min(DEFAULT_DATA_COL_WIDTH));
            is_manual.push(false);
        }
    }

    let current_sum: i32 = widths.iter().sum();
    let mut remaining = (total_avail.max(0) - current_sum).max(0);

    // Pass 1: Water-fill columns whose full content is wider than DEFAULT_DATA_COL_WIDTH
    // up to their natural full-content width if available width allows for it.
    while remaining > 0 {
        let needy: Vec<usize> = (0..num_cols)
            .filter(|&i| !is_manual[i] && widths[i] < natural_widths[i])
            .collect();
        if needy.is_empty() {
            break;
        }
        let count = needy.len() as i32;
        let min_deficit = needy
            .iter()
            .map(|&i| natural_widths[i] - widths[i])
            .min()
            .unwrap_or(0);
        if min_deficit <= 0 {
            break;
        }
        if min_deficit * count <= remaining {
            for &i in &needy {
                widths[i] += min_deficit;
                remaining -= min_deficit;
            }
        } else {
            let share = remaining / count;
            let extra = remaining % count;
            for (idx, &i) in needy.iter().enumerate() {
                widths[i] += share + if (idx as i32) < extra { 1 } else { 0 };
            }
            remaining = 0;
            break;
        }
    }

    // Pass 2: If space still remains after all columns show their full content,
    // expand narrow columns up to DEFAULT_DATA_COL_WIDTH so they don't look cramped.
    while remaining > 0 {
        let narrow: Vec<usize> = (0..num_cols)
            .filter(|&i| !is_manual[i] && widths[i] < DEFAULT_DATA_COL_WIDTH)
            .collect();
        if narrow.is_empty() {
            break;
        }
        let count = narrow.len() as i32;
        let min_deficit = narrow
            .iter()
            .map(|&i| DEFAULT_DATA_COL_WIDTH - widths[i])
            .min()
            .unwrap_or(0);
        if min_deficit <= 0 {
            break;
        }
        if min_deficit * count <= remaining {
            for &i in &narrow {
                widths[i] += min_deficit;
                remaining -= min_deficit;
            }
        } else {
            let share = remaining / count;
            let extra = remaining % count;
            for (idx, &i) in narrow.iter().enumerate() {
                widths[i] += share + if (idx as i32) < extra { 1 } else { 0 };
            }
            break;
        }
    }

    widths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_columns_show_full_content_when_available_width_allows() {
        let natural = vec![80, 220, 340];
        let manual = vec![None, None, None];
        let widths = compute_auto_column_widths(&natural, &manual, 900);
        // Plenty of width: wide columns expand to their full natural widths (220, 340)
        // and the narrow column expands back to DEFAULT_DATA_COL_WIDTH (160).
        assert_eq!(widths, vec![160, 220, 340]);
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
        assert_eq!(widths[1], 120);
        assert_eq!(widths[2], 400);
    }
}

