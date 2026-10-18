use crate::config::bookmarks::DbEngine;
use crate::db::models::{
    BackendCommand, CreateUserSpec, DatabasePermission, DbPermissionSpec, DbUser,
    PermissionPreset, UpdateUserSpec,
};
use crate::ui::workspace::{
    CELL_EXTRA_PADDING, DEFAULT_DATA_COL_WIDTH, HEADER_EXTRA_PADDING, MIN_COLUMN_WIDTH,
    build_resizable_header_cell, build_spreadsheet_scrolled_window, clear_spreadsheet_grid,
    compute_auto_column_widths, create_spreadsheet_label_cell, measure_single_line_text,
};
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const DEFAULT_USERS_INDEX_COL_WIDTH: i32 = 48;
const DEFAULT_PERMS_INDEX_COL_WIDTH: i32 = 48;
const DEFAULT_USER_ACTIONS_COL_WIDTH: i32 = 104;
const DEFAULT_PERM_ACTIONS_COL_WIDTH: i32 = 84;

pub const USERS_HEADERS: &[&str] = &[
    "Username",
    "Host",
    "Preset",
    "Global Privileges",
    "Grant Option",
    "Actions",
];

pub const DB_PERMISSIONS_HEADERS: &[&str] = &[
    "Database",
    "Preset",
    "Privileges",
    "Grant Option",
    "Actions",
];

#[derive(Clone)]
pub struct UsersTab {
    pub root: gtk::Box,
    scroll: gtk::ScrolledWindow,
    pub(crate) grid: gtk::Grid,
    header_label: gtk::Label,
    perm_scroll: gtk::ScrolledWindow,
    pub(crate) perm_grid: gtk::Grid,
    perm_header_label: gtk::Label,
    add_perm_btn: gtk::Button,
    perm_refresh_btn: gtk::Button,
    current_engine: Rc<RefCell<DbEngine>>,
    available_databases: Rc<RefCell<Vec<String>>>,
    users: Rc<RefCell<Vec<DbUser>>>,
    selected_user: Rc<RefCell<Option<(String, Option<String>)>>>,
    db_permissions: Rc<RefCell<Vec<DatabasePermission>>>,
    col_widths: Rc<RefCell<HashMap<usize, i32>>>,
    natural_col_widths: Rc<RefCell<Vec<i32>>>,
    header_cells: Rc<RefCell<Vec<(usize, gtk::Box)>>>,
    last_viewport_width: Rc<RefCell<i32>>,
    index_col_width: Rc<RefCell<i32>>,
    perm_col_widths: Rc<RefCell<HashMap<usize, i32>>>,
    perm_natural_col_widths: Rc<RefCell<Vec<i32>>>,
    perm_header_cells: Rc<RefCell<Vec<(usize, gtk::Box)>>>,
    perm_last_viewport_width: Rc<RefCell<i32>>,
    perm_index_col_width: Rc<RefCell<i32>>,
    cmd_tx: Sender<BackendCommand>,
}

impl UsersTab {
    pub fn new(cmd_tx: Sender<BackendCommand>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_hexpand(true);

        let paned = gtk::Paned::new(gtk::Orientation::Vertical);
        paned.set_position(360);
        paned.set_vexpand(true);
        paned.set_hexpand(true);
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);

        // ==========================================
        // Top Pane: Users & Global Permissions
        // ==========================================
        let top_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        top_box.set_vexpand(true);
        top_box.set_hexpand(true);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.set_margin_top(8);
        bar.set_margin_bottom(8);
        bar.set_margin_start(12);
        bar.set_margin_end(12);

        let header_label = gtk::Label::new(Some("Users & Global Permissions"));
        header_label.add_css_class("heading");
        header_label.set_halign(gtk::Align::Start);
        header_label.set_hexpand(true);

        let refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_btn.set_tooltip_text(Some("Refresh Users"));

        let add_btn = gtk::Button::with_label("Add User");
        add_btn.add_css_class("suggested-action");

        bar.append(&header_label);
        bar.append(&refresh_btn);
        bar.append(&add_btn);

        top_box.append(&bar);
        top_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let grid = gtk::Grid::new();
        grid.add_css_class("spreadsheet-grid");
        grid.set_row_spacing(0);
        grid.set_column_spacing(0);
        grid.set_halign(gtk::Align::Start);
        grid.set_valign(gtk::Align::Start);

        let scroll = build_spreadsheet_scrolled_window(&grid);

        top_box.append(&scroll);

        // ==========================================
        // Bottom Pane: Per-Database Permissions
        // ==========================================
        let bottom_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bottom_box.set_vexpand(true);
        bottom_box.set_hexpand(true);

        let perm_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        perm_bar.set_margin_top(8);
        perm_bar.set_margin_bottom(8);
        perm_bar.set_margin_start(12);
        perm_bar.set_margin_end(12);

        let perm_header_label =
            gtk::Label::new(Some("Select a user to manage per-database permissions"));
        perm_header_label.add_css_class("heading");
        perm_header_label.set_halign(gtk::Align::Start);
        perm_header_label.set_hexpand(true);

        let perm_refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        perm_refresh_btn.set_tooltip_text(Some("Refresh Database Permissions"));
        perm_refresh_btn.set_sensitive(false);

        let add_perm_btn = gtk::Button::with_label("Grant Database Access");
        add_perm_btn.add_css_class("suggested-action");
        add_perm_btn.set_sensitive(false);

        perm_bar.append(&perm_header_label);
        perm_bar.append(&perm_refresh_btn);
        perm_bar.append(&add_perm_btn);

        bottom_box.append(&perm_bar);
        bottom_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let perm_grid = gtk::Grid::new();
        perm_grid.add_css_class("spreadsheet-grid");
        perm_grid.set_row_spacing(0);
        perm_grid.set_column_spacing(0);
        perm_grid.set_halign(gtk::Align::Start);
        perm_grid.set_valign(gtk::Align::Start);

        let perm_scroll = build_spreadsheet_scrolled_window(&perm_grid);

        bottom_box.append(&perm_scroll);

        paned.set_start_child(Some(&top_box));
        paned.set_end_child(Some(&bottom_box));
        root.append(&paned);

        let current_engine = Rc::new(RefCell::new(DbEngine::Postgres));
        let available_databases = Rc::new(RefCell::new(Vec::new()));
        let users = Rc::new(RefCell::new(Vec::new()));
        let selected_user = Rc::new(RefCell::new(None));
        let db_permissions = Rc::new(RefCell::new(Vec::new()));

        let tab = Self {
            root,
            scroll: scroll.clone(),
            grid,
            header_label,
            perm_scroll: perm_scroll.clone(),
            perm_grid,
            perm_header_label,
            add_perm_btn: add_perm_btn.clone(),
            perm_refresh_btn: perm_refresh_btn.clone(),
            current_engine,
            available_databases,
            users,
            selected_user: selected_user.clone(),
            db_permissions,
            col_widths: Rc::new(RefCell::new(HashMap::new())),
            natural_col_widths: Rc::new(RefCell::new(Vec::new())),
            header_cells: Rc::new(RefCell::new(Vec::new())),
            last_viewport_width: Rc::new(RefCell::new(0)),
            index_col_width: Rc::new(RefCell::new(DEFAULT_USERS_INDEX_COL_WIDTH)),
            perm_col_widths: Rc::new(RefCell::new(HashMap::new())),
            perm_natural_col_widths: Rc::new(RefCell::new(Vec::new())),
            perm_header_cells: Rc::new(RefCell::new(Vec::new())),
            perm_last_viewport_width: Rc::new(RefCell::new(0)),
            perm_index_col_width: Rc::new(RefCell::new(DEFAULT_PERMS_INDEX_COL_WIDTH)),
            cmd_tx: cmd_tx.clone(),
        };

        // Recompute auto column widths when viewport width changes
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
        {
            let tab_clone = tab.clone();
            perm_scroll.hadjustment().connect_page_size_notify(move |_| {
                let tab_for_idle = tab_clone.clone();
                gtk::glib::idle_add_local_once(move || {
                    tab_for_idle.reapply_perm_column_widths_if_viewport_changed();
                });
            });
        }
        {
            let tab_clone = tab.clone();
            perm_scroll.connect_map(move |_| {
                let tab_for_idle = tab_clone.clone();
                gtk::glib::idle_add_local_once(move || {
                    tab_for_idle.reapply_perm_column_widths_if_viewport_changed();
                });
            });
        }

        // Top Refresh button
        {
            let tab_clone = tab.clone();
            refresh_btn.connect_clicked(move |_| {
                tab_clone.request_refresh();
            });
        }

        // Bottom Refresh button
        {
            let cmd_tx = cmd_tx.clone();
            let selected_user = selected_user.clone();
            perm_refresh_btn.connect_clicked(move |_| {
                let sel_opt = selected_user.borrow().clone();
                if let Some((username, host)) = sel_opt {
                    let _ = cmd_tx.send_blocking(BackendCommand::SelectUserForPermissions {
                        username,
                        host,
                    });
                }
            });
        }

        // Add User button
        {
            let tab_clone = tab.clone();
            add_btn.connect_clicked(move |btn| {
                tab_clone.open_user_dialog(btn.upcast_ref::<gtk::Widget>(), None);
            });
        }

        // Grant Database Access button
        {
            let tab_clone = tab.clone();
            add_perm_btn.connect_clicked(move |btn| {
                tab_clone.open_db_permission_dialog(btn.upcast_ref::<gtk::Widget>(), None);
            });
        }

        tab.render_users();
        tab.render_db_permissions();

        tab
    }

    pub fn set_connection_context(&self, engine: DbEngine, databases: Vec<String>) {
        let engine_changed = *self.current_engine.borrow() != engine;
        if engine_changed {
            self.col_widths.borrow_mut().clear();
            self.perm_col_widths.borrow_mut().clear();
            self.scroll.hadjustment().set_value(0.0);
            self.scroll.vadjustment().set_value(0.0);
            self.perm_scroll.hadjustment().set_value(0.0);
            self.perm_scroll.vadjustment().set_value(0.0);
        }
        *self.current_engine.borrow_mut() = engine;
        *self.available_databases.borrow_mut() = databases;
    }

    pub fn set_databases(&self, databases: Vec<String>) {
        *self.available_databases.borrow_mut() = databases;
    }

    pub fn clear(&self) {
        self.available_databases.borrow_mut().clear();
        self.set_users_and_permissions(Vec::new(), None, Vec::new());
    }

    pub fn request_refresh(&self) {
        let selected_user = self.selected_user.borrow().clone();
        let _ = self
            .cmd_tx
            .send_blocking(BackendCommand::RefreshUsers { selected_user });
    }

    pub fn set_users_and_permissions(
        &self,
        users: Vec<DbUser>,
        selected_user: Option<(String, Option<String>)>,
        db_permissions: Vec<DatabasePermission>,
    ) {
        let user_changed = *self.selected_user.borrow() != selected_user;
        *self.users.borrow_mut() = users;
        *self.selected_user.borrow_mut() = selected_user;
        *self.db_permissions.borrow_mut() = db_permissions;
        if user_changed {
            self.perm_scroll.hadjustment().set_value(0.0);
            self.perm_scroll.vadjustment().set_value(0.0);
        }
        self.render_users();
        self.render_db_permissions();
    }

    pub fn set_user_db_permissions(
        &self,
        username: &str,
        host: Option<String>,
        db_permissions: Vec<DatabasePermission>,
    ) {
        let new_sel = Some((username.to_string(), host));
        let user_changed = *self.selected_user.borrow() != new_sel;
        *self.selected_user.borrow_mut() = new_sel;
        *self.db_permissions.borrow_mut() = db_permissions;

        if self.grid.child_at(0, 1).is_some() || self.users.borrow().is_empty() {
            self.update_selected_user_rows();
        } else {
            self.render_users();
        }

        if user_changed {
            self.perm_scroll.hadjustment().set_value(0.0);
            self.perm_scroll.vadjustment().set_value(0.0);
        }
        self.render_db_permissions();
    }

    fn update_selected_user_rows(&self) {
        let users = self.users.borrow().clone();
        let selected_user = self.selected_user.borrow().clone();
        let num_cols = USERS_HEADERS.len() as i32;

        for (row_idx, user) in users.iter().enumerate() {
            let grid_row = (row_idx + 1) as i32;
            let is_selected = selected_user
                .as_ref()
                .map(|(u, h)| u == &user.username && h == &user.host)
                .unwrap_or(false);

            for col in 0..=num_cols {
                if let Some(cell) = self.grid.child_at(col, grid_row) {
                    if is_selected {
                        cell.add_css_class("user-row-selected");
                    } else {
                        cell.remove_css_class("user-row-selected");
                    }
                    if col == 1 {
                        if let Some(u_lbl) = cell.first_child() {
                            if is_selected {
                                u_lbl.add_css_class("heading");
                            } else {
                                u_lbl.remove_css_class("heading");
                            }
                        }
                    } else if col == num_cols {
                        if let Some(perms_btn) = cell.first_child() {
                            if is_selected {
                                perms_btn.add_css_class("accent");
                            } else {
                                perms_btn.remove_css_class("accent");
                            }
                        }
                    }
                }
            }
        }
    }

    fn available_viewport_width(&self, scroll: &gtk::ScrolledWindow) -> i32 {
        let hadj_w = scroll.hadjustment().page_size().round() as i32;
        if hadj_w > 0 {
            return hadj_w;
        }
        let scroll_w = scroll.width();
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
        let viewport_w = self.available_viewport_width(&self.scroll);
        if viewport_w != *self.last_viewport_width.borrow() {
            self.apply_column_widths();
        }
    }

    fn apply_column_widths(&self) {
        let viewport_w = self.available_viewport_width(&self.scroll);
        *self.last_viewport_width.borrow_mut() = viewport_w;

        let header_cells = self.header_cells.borrow();
        let natural_widths = self.natural_col_widths.borrow();
        if header_cells.is_empty() || header_cells.len() != natural_widths.len() {
            return;
        }

        let actions_col_idx = USERS_HEADERS.len().saturating_sub(1);
        let manual_map = self.col_widths.borrow();
        let manual_overrides: Vec<Option<i32>> = header_cells
            .iter()
            .map(|(col_idx, _)| {
                if let Some(&w) = manual_map.get(col_idx) {
                    Some(w)
                } else if *col_idx == actions_col_idx {
                    Some(DEFAULT_USER_ACTIONS_COL_WIDTH)
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

    fn reapply_perm_column_widths_if_viewport_changed(&self) {
        let viewport_w = self.available_viewport_width(&self.perm_scroll);
        if viewport_w != *self.perm_last_viewport_width.borrow() {
            self.apply_perm_column_widths();
        }
    }

    fn apply_perm_column_widths(&self) {
        let viewport_w = self.available_viewport_width(&self.perm_scroll);
        *self.perm_last_viewport_width.borrow_mut() = viewport_w;

        let header_cells = self.perm_header_cells.borrow();
        let natural_widths = self.perm_natural_col_widths.borrow();
        if header_cells.is_empty() || header_cells.len() != natural_widths.len() {
            return;
        }

        let actions_col_idx = DB_PERMISSIONS_HEADERS.len().saturating_sub(1);
        let manual_map = self.perm_col_widths.borrow();
        let manual_overrides: Vec<Option<i32>> = header_cells
            .iter()
            .map(|(col_idx, _)| {
                if let Some(&w) = manual_map.get(col_idx) {
                    Some(w)
                } else if *col_idx == actions_col_idx {
                    Some(DEFAULT_PERM_ACTIONS_COL_WIDTH)
                } else {
                    None
                }
            })
            .collect();

        let idx_w = *self.perm_index_col_width.borrow();
        let total_avail = (viewport_w - idx_w - 2).max(0);
        let widths = compute_auto_column_widths(&natural_widths, &manual_overrides, total_avail);

        for ((_, hdr_cell), target_w) in header_cells.iter().zip(widths) {
            if hdr_cell.width_request() != target_w {
                hdr_cell.set_width_request(target_w);
            }
        }
    }

    fn render_users(&self) {
        let saved_h = self.scroll.hadjustment().value();
        let saved_v = self.scroll.vadjustment().value();

        let engine = *self.current_engine.borrow();
        let users = self.users.borrow().clone();
        let selected_user = self.selected_user.borrow().clone();

        self.header_label.set_text(&format!(
            "Users & Global Permissions — {} user(s)",
            users.len()
        ));

        clear_spreadsheet_grid(&self.grid);
        self.header_cells.borrow_mut().clear();
        self.natural_col_widths.borrow_mut().clear();

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
            Vec::with_capacity(USERS_HEADERS.len());

        for (col_idx, &title) in USERS_HEADERS.iter().enumerate() {
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
                .unwrap_or(if col_idx + 1 == USERS_HEADERS.len() {
                    DEFAULT_USER_ACTIONS_COL_WIDTH
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
        let mut widest_texts: Vec<String> = vec![String::new(); USERS_HEADERS.len()];

        for (row_idx, user) in users.into_iter().enumerate() {
            let grid_row = (row_idx + 1) as i32;
            let is_selected = selected_user
                .as_ref()
                .map(|(u, h)| u == &user.username && h == &user.host)
                .unwrap_or(false);

            let ord_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            ord_box.add_css_class("spreadsheet-row-num");
            if is_selected {
                ord_box.add_css_class("user-row-selected");
            }
            ord_box.set_overflow(gtk::Overflow::Hidden);

            let ord_lbl = gtk::Label::new(Some(&(row_idx + 1).to_string()));
            ord_lbl.add_css_class("dim-label");
            ord_lbl.add_css_class("caption");
            ord_lbl.add_css_class("monospace");
            ord_lbl.set_halign(gtk::Align::Center);
            ord_lbl.set_hexpand(true);
            ord_box.append(&ord_lbl);
            {
                let click = gtk::GestureClick::new();
                let tab_clone = self.clone();
                let cmd_tx = self.cmd_tx.clone();
                let uname = user.username.clone();
                let uhost = user.host.clone();
                click.connect_released(move |_, _, _, _| {
                    *tab_clone.selected_user.borrow_mut() = Some((uname.clone(), uhost.clone()));
                    tab_clone.update_selected_user_rows();
                    let _ = cmd_tx.send_blocking(BackendCommand::SelectUserForPermissions {
                        username: uname.clone(),
                        host: uhost.clone(),
                    });
                });
                ord_box.add_controller(click);
            }
            self.grid.attach(&ord_box, 0, grid_row, 1, 1);

            let user_classes: &[&str] = if is_selected { &["heading"] } else { &[] };
            let (username_cell, u_lbl) =
                create_spreadsheet_label_cell(&user.username, user_classes, Some(&user.username));
            if sample_lbl.is_none() {
                sample_lbl = Some(u_lbl);
            }

            let host_str = user.host.as_deref().unwrap_or("*");
            let (host_cell, _) =
                create_spreadsheet_label_cell(host_str, &["dim-label"], Some(host_str));

            let preset = PermissionPreset::infer_global_preset(engine, &user.global_privileges);
            let preset_str = preset.label();
            let preset_classes: &[&str] = match preset {
                PermissionPreset::FullAdmin => &["accent", "heading"],
                PermissionPreset::None => &["dim-label"],
                _ => &[],
            };
            let (preset_cell, _) =
                create_spreadsheet_label_cell(preset_str, preset_classes, Some(preset_str));

            let privs_str = if user.global_privileges.is_empty() {
                "NONE".to_string()
            } else {
                user.global_privileges.join(", ")
            };
            let privs_classes: &[&str] = if user.global_privileges.is_empty() {
                &["cell-null"]
            } else {
                &[]
            };
            let (privs_cell, _) =
                create_spreadsheet_label_cell(&privs_str, privs_classes, Some(&privs_str));

            let grant_str = if user.has_grant_option { "YES" } else { "NO" };
            let grant_classes: &[&str] = if user.has_grant_option {
                &["accent"]
            } else {
                &["dim-label"]
            };
            let (grant_cell, _) = create_spreadsheet_label_cell(grant_str, grant_classes, None);

            let row_texts = [
                user.username.as_str(),
                host_str,
                preset_str,
                privs_str.as_str(),
                grant_str,
            ];
            for (i, txt) in row_texts.iter().enumerate() {
                if txt.chars().count() > widest_texts[i].chars().count() {
                    widest_texts[i] = (*txt).to_string();
                }
            }

            let actions_cell = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            actions_cell.add_css_class("spreadsheet-cell");
            actions_cell.set_overflow(gtk::Overflow::Hidden);

            let perms_btn = gtk::Button::from_icon_name("security-high-symbolic");
            perms_btn.add_css_class("flat");
            perms_btn.add_css_class("cell-action-btn");
            if is_selected {
                perms_btn.add_css_class("accent");
            }
            perms_btn.set_valign(gtk::Align::Center);
            perms_btn.set_margin_start(6);
            perms_btn.set_focus_on_click(false);
            perms_btn.set_tooltip_text(Some("View / Manage Database Permissions"));

            let edit_btn = gtk::Button::from_icon_name("document-edit-symbolic");
            edit_btn.add_css_class("flat");
            edit_btn.add_css_class("cell-action-btn");
            edit_btn.set_valign(gtk::Align::Center);
            edit_btn.set_focus_on_click(false);
            edit_btn.set_tooltip_text(Some("Edit User & Global Permissions"));

            let del_btn = gtk::Button::from_icon_name("user-trash-symbolic");
            del_btn.add_css_class("flat");
            del_btn.add_css_class("error");
            del_btn.add_css_class("cell-action-btn");
            del_btn.set_valign(gtk::Align::Center);
            del_btn.set_focus_on_click(false);
            del_btn.set_tooltip_text(Some("Drop User"));

            actions_cell.append(&perms_btn);
            actions_cell.append(&edit_btn);
            actions_cell.append(&del_btn);

            let data_cells = [
                username_cell,
                host_cell,
                preset_cell,
                privs_cell,
                grant_cell,
                actions_cell,
            ];
            for (col_idx, cell) in data_cells.iter().enumerate() {
                if is_selected {
                    cell.add_css_class("user-row-selected");
                }
                if col_idx + 1 < data_cells.len() {
                    let click = gtk::GestureClick::new();
                    let tab_clone = self.clone();
                    let cmd_tx = self.cmd_tx.clone();
                    let uname = user.username.clone();
                    let uhost = user.host.clone();
                    click.connect_released(move |_, _, _, _| {
                        *tab_clone.selected_user.borrow_mut() =
                            Some((uname.clone(), uhost.clone()));
                        tab_clone.update_selected_user_rows();
                        let _ = cmd_tx.send_blocking(BackendCommand::SelectUserForPermissions {
                            username: uname.clone(),
                            host: uhost.clone(),
                        });
                    });
                    cell.add_controller(click);
                }
                self.grid.attach(cell, (col_idx + 1) as i32, grid_row, 1, 1);
            }

            {
                let tab_clone = self.clone();
                let cmd_tx = self.cmd_tx.clone();
                let uname = user.username.clone();
                let uhost = user.host.clone();
                perms_btn.connect_clicked(move |_| {
                    *tab_clone.selected_user.borrow_mut() = Some((uname.clone(), uhost.clone()));
                    tab_clone.update_selected_user_rows();
                    let _ = cmd_tx.send_blocking(BackendCommand::SelectUserForPermissions {
                        username: uname.clone(),
                        host: uhost.clone(),
                    });
                });
            }

            {
                let tab_clone = self.clone();
                let user_clone = user.clone();
                edit_btn.connect_clicked(move |btn| {
                    tab_clone.open_user_dialog(btn.upcast_ref::<gtk::Widget>(), Some(&user_clone));
                });
            }

            {
                let tab_clone = self.clone();
                let user_clone = user;
                del_btn.connect_clicked(move |btn| {
                    tab_clone.confirm_drop_user(btn.upcast_ref::<gtk::Widget>(), &user_clone);
                });
            }
        }

        let mut natural_widths = Vec::with_capacity(USERS_HEADERS.len());
        for (col_idx, (hdr_lbl, hdr_text)) in header_labels.iter().enumerate() {
            let hdr_w = measure_single_line_text(hdr_lbl, hdr_text) + HEADER_EXTRA_PADDING;
            if col_idx + 1 == USERS_HEADERS.len() {
                natural_widths.push(hdr_w.max(DEFAULT_USER_ACTIONS_COL_WIDTH));
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
        self.scroll.hadjustment().set_value(saved_h);
        self.scroll.vadjustment().set_value(saved_v);

        let tab_for_idle = self.clone();
        gtk::glib::idle_add_local_once(move || {
            tab_for_idle.reapply_column_widths_if_viewport_changed();
        });
    }

    fn render_db_permissions(&self) {
        let saved_h = self.perm_scroll.hadjustment().value();
        let saved_v = self.perm_scroll.vadjustment().value();

        let engine = *self.current_engine.borrow();
        let selected_user = self.selected_user.borrow().clone();
        let perms = self.db_permissions.borrow().clone();

        if let Some((ref u, ref h)) = selected_user {
            let identity = match h {
                Some(host) if !host.is_empty() => format!("'{}'@'{}'", u, host),
                _ => format!("'{}'", u),
            };
            self.perm_header_label.set_text(&format!(
                "Database Permissions for {} — {} database grant(s)",
                identity,
                perms.len()
            ));
            self.perm_refresh_btn.set_sensitive(true);
            self.add_perm_btn.set_sensitive(true);
        } else {
            self.perm_header_label
                .set_text("Select a user to manage per-database permissions");
            self.perm_refresh_btn.set_sensitive(false);
            self.add_perm_btn.set_sensitive(false);
        }

        clear_spreadsheet_grid(&self.perm_grid);
        self.perm_header_cells.borrow_mut().clear();
        self.perm_natural_col_widths.borrow_mut().clear();

        let corner_lbl = gtk::Label::new(Some("#"));
        corner_lbl.add_css_class("dim-label");
        corner_lbl.set_halign(gtk::Align::Fill);
        corner_lbl.set_xalign(0.5);
        corner_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        corner_lbl.set_max_width_chars(1);
        corner_lbl.set_margin_top(6);
        corner_lbl.set_margin_bottom(6);

        let idx_width = *self.perm_index_col_width.borrow();
        let idx_width_ref = self.perm_index_col_width.clone();
        let corner_cell = build_resizable_header_cell(
            &corner_lbl,
            idx_width,
            Some("spreadsheet-header-box"),
            move |new_w| {
                *idx_width_ref.borrow_mut() = new_w;
            },
        );
        self.perm_grid.attach(&corner_cell, 0, 0, 1, 1);

        let mut header_labels: Vec<(gtk::Label, &'static str)> =
            Vec::with_capacity(DB_PERMISSIONS_HEADERS.len());

        for (col_idx, &title) in DB_PERMISSIONS_HEADERS.iter().enumerate() {
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
                .perm_col_widths
                .borrow()
                .get(&col_idx)
                .copied()
                .unwrap_or(if col_idx + 1 == DB_PERMISSIONS_HEADERS.len() {
                    DEFAULT_PERM_ACTIONS_COL_WIDTH
                } else {
                    DEFAULT_DATA_COL_WIDTH
                });
            let col_widths_ref = self.perm_col_widths.clone();
            let hdr_cell = build_resizable_header_cell(
                &hdr_lbl,
                col_w,
                Some("spreadsheet-header-box"),
                move |new_w| {
                    col_widths_ref.borrow_mut().insert(col_idx, new_w);
                },
            );

            self.perm_grid
                .attach(&hdr_cell, (col_idx + 1) as i32, 0, 1, 1);
            header_labels.push((hdr_lbl, title));
            self.perm_header_cells.borrow_mut().push((col_idx, hdr_cell));
        }

        let mut sample_lbl: Option<gtk::Label> = None;
        let mut widest_texts: Vec<String> = vec![String::new(); DB_PERMISSIONS_HEADERS.len()];

        for (row_idx, perm) in perms.into_iter().enumerate() {
            let grid_row = (row_idx + 1) as i32;

            let ord_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            ord_box.add_css_class("spreadsheet-row-num");
            ord_box.set_overflow(gtk::Overflow::Hidden);

            let ord_lbl = gtk::Label::new(Some(&(row_idx + 1).to_string()));
            ord_lbl.add_css_class("dim-label");
            ord_lbl.add_css_class("caption");
            ord_lbl.add_css_class("monospace");
            ord_lbl.set_halign(gtk::Align::Center);
            ord_lbl.set_hexpand(true);
            ord_box.append(&ord_lbl);
            self.perm_grid.attach(&ord_box, 0, grid_row, 1, 1);

            let (db_cell, db_lbl) =
                create_spreadsheet_label_cell(&perm.database, &[], Some(&perm.database));
            if sample_lbl.is_none() {
                sample_lbl = Some(db_lbl);
            }

            let preset = PermissionPreset::infer_db_preset(engine, &perm.privileges);
            let preset_str = preset.label();
            let preset_classes: &[&str] = match preset {
                PermissionPreset::FullAdmin => &["accent", "heading"],
                _ => &[],
            };
            let (preset_cell, _) =
                create_spreadsheet_label_cell(preset_str, preset_classes, Some(preset_str));

            let privs_str = perm.privileges.join(", ");
            let (privs_cell, _) =
                create_spreadsheet_label_cell(&privs_str, &[], Some(&privs_str));

            let grant_str = if perm.has_grant_option { "YES" } else { "NO" };
            let grant_classes: &[&str] = if perm.has_grant_option {
                &["accent"]
            } else {
                &["dim-label"]
            };
            let (grant_cell, _) = create_spreadsheet_label_cell(grant_str, grant_classes, None);

            let row_texts = [
                perm.database.as_str(),
                preset_str,
                privs_str.as_str(),
                grant_str,
            ];
            for (i, txt) in row_texts.iter().enumerate() {
                if txt.chars().count() > widest_texts[i].chars().count() {
                    widest_texts[i] = (*txt).to_string();
                }
            }

            let actions_cell = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            actions_cell.add_css_class("spreadsheet-cell");
            actions_cell.set_overflow(gtk::Overflow::Hidden);

            let edit_btn = gtk::Button::from_icon_name("document-edit-symbolic");
            edit_btn.add_css_class("flat");
            edit_btn.add_css_class("cell-action-btn");
            edit_btn.set_valign(gtk::Align::Center);
            edit_btn.set_margin_start(6);
            edit_btn.set_focus_on_click(false);
            edit_btn.set_tooltip_text(Some("Edit Database Permissions"));

            let del_btn = gtk::Button::from_icon_name("user-trash-symbolic");
            del_btn.add_css_class("flat");
            del_btn.add_css_class("error");
            del_btn.add_css_class("cell-action-btn");
            del_btn.set_valign(gtk::Align::Center);
            del_btn.set_focus_on_click(false);
            del_btn.set_tooltip_text(Some("Revoke Database Permissions"));

            actions_cell.append(&edit_btn);
            actions_cell.append(&del_btn);

            let data_cells = [db_cell, preset_cell, privs_cell, grant_cell, actions_cell];
            for (col_idx, cell) in data_cells.iter().enumerate() {
                self.perm_grid
                    .attach(cell, (col_idx + 1) as i32, grid_row, 1, 1);
            }

            {
                let tab_clone = self.clone();
                let perm_clone = perm.clone();
                edit_btn.connect_clicked(move |btn| {
                    tab_clone.open_db_permission_dialog(
                        btn.upcast_ref::<gtk::Widget>(),
                        Some(&perm_clone),
                    );
                });
            }

            {
                let tab_clone = self.clone();
                let perm_clone = perm;
                del_btn.connect_clicked(move |btn| {
                    tab_clone
                        .confirm_revoke_db_permission(btn.upcast_ref::<gtk::Widget>(), &perm_clone);
                });
            }
        }

        let mut natural_widths = Vec::with_capacity(DB_PERMISSIONS_HEADERS.len());
        for (col_idx, (hdr_lbl, hdr_text)) in header_labels.iter().enumerate() {
            let hdr_w = measure_single_line_text(hdr_lbl, hdr_text) + HEADER_EXTRA_PADDING;
            if col_idx + 1 == DB_PERMISSIONS_HEADERS.len() {
                natural_widths.push(hdr_w.max(DEFAULT_PERM_ACTIONS_COL_WIDTH));
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

        *self.perm_natural_col_widths.borrow_mut() = natural_widths;
        self.apply_perm_column_widths();
        self.perm_scroll.hadjustment().set_value(saved_h);
        self.perm_scroll.vadjustment().set_value(saved_v);

        let tab_for_idle = self.clone();
        gtk::glib::idle_add_local_once(move || {
            tab_for_idle.reapply_perm_column_widths_if_viewport_changed();
        });
    }

    fn open_user_dialog(&self, parent: &gtk::Widget, existing: Option<&DbUser>) {
        let engine = *self.current_engine.borrow();
        let is_edit = existing.is_some();

        let dialog = adw::Dialog::builder()
            .title(if is_edit { "Edit User" } else { "Create User" })
            .content_width(520)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar_view.add_top_bar(&header);

        let content_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content_box.set_margin_top(12);
        content_box.set_margin_bottom(12);
        content_box.set_margin_start(12);
        content_box.set_margin_end(12);

        let account_group = adw::PreferencesGroup::builder()
            .title("Account")
            .build();

        let username_row = adw::EntryRow::builder()
            .title("Username")
            .text(existing.map(|u| u.username.as_str()).unwrap_or(""))
            .build();
        account_group.add(&username_row);

        let host_row = adw::EntryRow::builder()
            .title("Host")
            .text(
                existing
                    .and_then(|u| u.host.as_deref())
                    .unwrap_or("%"),
            )
            .visible(engine == DbEngine::MySql)
            .build();
        account_group.add(&host_row);

        let password_row = adw::PasswordEntryRow::builder()
            .title(if is_edit {
                "New Password (leave blank to keep current)"
            } else {
                "Password"
            })
            .build();
        account_group.add(&password_row);

        content_box.append(&account_group);

        let global_group = adw::PreferencesGroup::builder()
            .title("Global Permissions")
            .build();

        let preset_labels: Vec<&str> = PermissionPreset::ALL_GLOBAL
            .iter()
            .map(|p| p.label())
            .collect();
        let preset_model = gtk::StringList::new(&preset_labels);

        let initial_privs: Vec<String> = existing
            .map(|u| u.global_privileges.clone())
            .unwrap_or_else(|| {
                PermissionPreset::preset_global_privileges(engine, PermissionPreset::None)
            });
        let initial_preset = PermissionPreset::infer_global_preset(engine, &initial_privs);
        let initial_preset_idx = PermissionPreset::ALL_GLOBAL
            .iter()
            .position(|&p| p == initial_preset)
            .unwrap_or(0) as u32;

        let preset_row = adw::ComboRow::builder()
            .title("Permission Preset")
            .model(&preset_model)
            .selected(initial_preset_idx)
            .build();
        global_group.add(&preset_row);

        let grant_opt_row = adw::SwitchRow::builder()
            .title("With Grant Option")
            .subtitle("Allow this user to grant privileges to other users")
            .active(existing.map(|u| u.has_grant_option).unwrap_or(false))
            .build();
        global_group.add(&grant_opt_row);

        content_box.append(&global_group);

        let privs_group = adw::PreferencesGroup::builder()
            .title("Individual Global Privileges")
            .build();

        let priv_grid = gtk::Grid::new();
        priv_grid.set_column_spacing(16);
        priv_grid.set_row_spacing(6);
        priv_grid.set_margin_top(6);
        priv_grid.set_margin_bottom(6);
        priv_grid.set_margin_start(6);
        priv_grid.set_margin_end(6);

        let available_privs = PermissionPreset::global_privileges_for_engine(engine);
        let mut check_buttons: Vec<(String, gtk::CheckButton)> =
            Vec::with_capacity(available_privs.len());

        for (idx, &priv_name) in available_privs.iter().enumerate() {
            let chk = gtk::CheckButton::with_label(priv_name);
            let is_checked = initial_privs
                .iter()
                .any(|p| p.eq_ignore_ascii_case(priv_name));
            chk.set_active(is_checked);
            priv_grid.attach(&chk, (idx % 2) as i32, (idx / 2) as i32, 1, 1);
            check_buttons.push((priv_name.to_string(), chk));
        }

        privs_group.add(&priv_grid);
        content_box.append(&privs_group);

        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_margin_top(6);

        let cancel_btn = gtk::Button::with_label("Cancel");
        let save_btn = gtk::Button::with_label(if is_edit {
            "Save Changes"
        } else {
            "Create User"
        });
        save_btn.add_css_class("suggested-action");
        save_btn.set_sensitive(!username_row.text().trim().is_empty());

        btn_box.append(&cancel_btn);
        btn_box.append(&save_btn);
        content_box.append(&btn_box);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(620)
            .child(&content_box)
            .build();

        toolbar_view.set_content(Some(&scroll));
        dialog.set_child(Some(&toolbar_view));

        let suppress_sync = Rc::new(RefCell::new(false));
        let check_buttons_rc = Rc::new(check_buttons);

        // Preset selection -> update checkboxes
        {
            let suppress_sync = suppress_sync.clone();
            let check_buttons_rc = check_buttons_rc.clone();
            let grant_opt_row = grant_opt_row.clone();
            preset_row.connect_selected_notify(move |combo| {
                if *suppress_sync.borrow() {
                    return;
                }
                let Some(&preset) = PermissionPreset::ALL_GLOBAL.get(combo.selected() as usize)
                else {
                    return;
                };
                if preset == PermissionPreset::Custom {
                    return;
                }
                let target_privs = PermissionPreset::preset_global_privileges(engine, preset);
                *suppress_sync.borrow_mut() = true;
                for (name, chk) in check_buttons_rc.iter() {
                    let active = target_privs
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(name));
                    chk.set_active(active);
                }
                if preset == PermissionPreset::FullAdmin {
                    grant_opt_row.set_active(true);
                } else if preset == PermissionPreset::None || preset == PermissionPreset::ReadOnly {
                    grant_opt_row.set_active(false);
                }
                *suppress_sync.borrow_mut() = false;
            });
        }

        // Checkbox toggle -> update preset dropdown
        for (_, chk) in check_buttons_rc.iter() {
            let suppress_sync = suppress_sync.clone();
            let check_buttons_rc = check_buttons_rc.clone();
            let preset_row = preset_row.clone();
            chk.connect_toggled(move |_| {
                if *suppress_sync.borrow() {
                    return;
                }
                let active_privs: Vec<String> = check_buttons_rc
                    .iter()
                    .filter(|(_, c)| c.is_active())
                    .map(|(n, _)| n.clone())
                    .collect();
                let inferred = PermissionPreset::infer_global_preset(engine, &active_privs);
                if let Some(pos) = PermissionPreset::ALL_GLOBAL
                    .iter()
                    .position(|&p| p == inferred)
                {
                    *suppress_sync.borrow_mut() = true;
                    preset_row.set_selected(pos as u32);
                    *suppress_sync.borrow_mut() = false;
                }
            });
        }

        {
            let save_btn = save_btn.clone();
            username_row.connect_changed(move |entry| {
                save_btn.set_sensitive(!entry.text().trim().is_empty());
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
            let check_buttons_rc = check_buttons_rc.clone();
            let old_user_opt = existing.cloned();
            save_btn.connect_clicked(move |_| {
                let username = username_row.text().trim().to_string();
                if username.is_empty() {
                    return;
                }
                let host = if engine == DbEngine::MySql {
                    let h = host_row.text().trim().to_string();
                    Some(if h.is_empty() { "%".to_string() } else { h })
                } else {
                    None
                };
                let password = password_row.text().to_string();
                let global_privileges: Vec<String> = check_buttons_rc
                    .iter()
                    .filter(|(_, c)| c.is_active())
                    .map(|(n, _)| n.clone())
                    .collect();
                let with_grant_option = grant_opt_row.is_active();

                if let Some(ref old_user) = old_user_opt {
                    let spec = UpdateUserSpec {
                        old_username: old_user.username.clone(),
                        old_host: old_user.host.clone(),
                        new_username: username,
                        new_host: host,
                        new_password: if password.is_empty() {
                            None
                        } else {
                            Some(password)
                        },
                        global_privileges,
                        with_grant_option,
                    };
                    let _ = cmd_tx.send_blocking(BackendCommand::UpdateUser(spec));
                } else {
                    let spec = CreateUserSpec {
                        username,
                        host,
                        password,
                        global_privileges,
                        with_grant_option,
                    };
                    let _ = cmd_tx.send_blocking(BackendCommand::CreateUser(spec));
                }
                dialog.close();
            });
        }

        dialog.present(Some(parent));
    }

    fn confirm_drop_user(&self, parent: &gtk::Widget, user: &DbUser) {
        let identity = user.display_identity();
        let alert = adw::AlertDialog::builder()
            .heading(format!("Drop User {}?", identity))
            .body(format!(
                "Are you sure you want to permanently drop user {} and revoke all of their permissions?",
                identity
            ))
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("drop", "Drop User");
        alert.set_response_appearance("drop", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let cmd_tx = self.cmd_tx.clone();
        let username = user.username.clone();
        let host = user.host.clone();
        alert.connect_response(None, move |_, resp| {
            if resp == "drop" {
                let _ = cmd_tx.send_blocking(BackendCommand::DropUser {
                    username: username.clone(),
                    host: host.clone(),
                });
            }
        });

        alert.present(Some(parent));
    }

    fn open_db_permission_dialog(
        &self,
        parent: &gtk::Widget,
        existing: Option<&DatabasePermission>,
    ) {
        let Some((username, host)) = self.selected_user.borrow().clone() else {
            return;
        };
        let engine = *self.current_engine.borrow();
        let is_edit = existing.is_some();

        let dialog = adw::Dialog::builder()
            .title(if is_edit {
                "Edit Database Permissions"
            } else {
                "Grant Database Permissions"
            })
            .content_width(520)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar_view.add_top_bar(&header);

        let content_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content_box.set_margin_top(12);
        content_box.set_margin_bottom(12);
        content_box.set_margin_start(12);
        content_box.set_margin_end(12);

        let target_group = adw::PreferencesGroup::builder()
            .title("Database & Preset")
            .build();

        let mut db_list = self.available_databases.borrow().clone();
        if let Some(ex) = existing {
            if !db_list.contains(&ex.database) {
                db_list.insert(0, ex.database.clone());
            }
        }
        let db_refs: Vec<&str> = db_list.iter().map(|s| s.as_str()).collect();
        let db_model = gtk::StringList::new(&db_refs);
        let initial_db_idx = existing
            .and_then(|ex| db_list.iter().position(|d| d == &ex.database))
            .unwrap_or(0) as u32;

        let db_row = adw::ComboRow::builder()
            .title("Database")
            .model(&db_model)
            .selected(initial_db_idx)
            .sensitive(!is_edit && !db_list.is_empty())
            .build();
        target_group.add(&db_row);

        let preset_labels: Vec<&str> = PermissionPreset::ALL_DB
            .iter()
            .map(|p| p.label())
            .collect();
        let preset_model = gtk::StringList::new(&preset_labels);

        let initial_privs: Vec<String> = existing
            .map(|p| p.privileges.clone())
            .unwrap_or_else(|| {
                PermissionPreset::preset_db_privileges(engine, PermissionPreset::ReadWrite)
            });
        let initial_preset = PermissionPreset::infer_db_preset(engine, &initial_privs);
        let initial_preset_idx = PermissionPreset::ALL_DB
            .iter()
            .position(|&p| p == initial_preset)
            .unwrap_or(0) as u32;

        let preset_row = adw::ComboRow::builder()
            .title("Permission Preset")
            .model(&preset_model)
            .selected(initial_preset_idx)
            .build();
        target_group.add(&preset_row);

        let grant_opt_row = adw::SwitchRow::builder()
            .title("With Grant Option")
            .active(existing.map(|p| p.has_grant_option).unwrap_or(false))
            .build();
        target_group.add(&grant_opt_row);

        content_box.append(&target_group);

        let privs_group = adw::PreferencesGroup::builder()
            .title(match engine {
                DbEngine::MySql => "Database Privileges",
                DbEngine::Postgres => "Database & Public Schema Table Privileges",
            })
            .build();

        let priv_grid = gtk::Grid::new();
        priv_grid.set_column_spacing(16);
        priv_grid.set_row_spacing(6);
        priv_grid.set_margin_top(6);
        priv_grid.set_margin_bottom(6);
        priv_grid.set_margin_start(6);
        priv_grid.set_margin_end(6);

        let available_privs = PermissionPreset::db_privileges_for_engine(engine);
        let mut check_buttons: Vec<(String, gtk::CheckButton)> =
            Vec::with_capacity(available_privs.len());

        for (idx, &priv_name) in available_privs.iter().enumerate() {
            let chk = gtk::CheckButton::with_label(priv_name);
            let is_checked = initial_privs
                .iter()
                .any(|p| p.eq_ignore_ascii_case(priv_name));
            chk.set_active(is_checked);
            priv_grid.attach(&chk, (idx % 2) as i32, (idx / 2) as i32, 1, 1);
            check_buttons.push((priv_name.to_string(), chk));
        }

        privs_group.add(&priv_grid);
        content_box.append(&privs_group);

        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_margin_top(6);

        let cancel_btn = gtk::Button::with_label("Cancel");
        let save_btn = gtk::Button::with_label(if is_edit {
            "Save Permissions"
        } else {
            "Grant Permissions"
        });
        save_btn.add_css_class("suggested-action");
        save_btn.set_sensitive(!db_list.is_empty());

        btn_box.append(&cancel_btn);
        btn_box.append(&save_btn);
        content_box.append(&btn_box);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(620)
            .child(&content_box)
            .build();

        toolbar_view.set_content(Some(&scroll));
        dialog.set_child(Some(&toolbar_view));

        let suppress_sync = Rc::new(RefCell::new(false));
        let check_buttons_rc = Rc::new(check_buttons);

        // Preset selection -> update checkboxes
        {
            let suppress_sync = suppress_sync.clone();
            let check_buttons_rc = check_buttons_rc.clone();
            let save_btn = save_btn.clone();
            preset_row.connect_selected_notify(move |combo| {
                if *suppress_sync.borrow() {
                    return;
                }
                let Some(&preset) = PermissionPreset::ALL_DB.get(combo.selected() as usize) else {
                    return;
                };
                if preset == PermissionPreset::Custom {
                    return;
                }
                let target_privs = PermissionPreset::preset_db_privileges(engine, preset);
                *suppress_sync.borrow_mut() = true;
                for (name, chk) in check_buttons_rc.iter() {
                    let active = target_privs
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(name));
                    chk.set_active(active);
                }
                save_btn.set_sensitive(!target_privs.is_empty());
                *suppress_sync.borrow_mut() = false;
            });
        }

        // Checkbox toggle -> update preset dropdown & save button sensitivity
        for (_, chk) in check_buttons_rc.iter() {
            let suppress_sync = suppress_sync.clone();
            let check_buttons_rc = check_buttons_rc.clone();
            let preset_row = preset_row.clone();
            let save_btn = save_btn.clone();
            let has_dbs = !db_list.is_empty();
            chk.connect_toggled(move |_| {
                if *suppress_sync.borrow() {
                    return;
                }
                let active_privs: Vec<String> = check_buttons_rc
                    .iter()
                    .filter(|(_, c)| c.is_active())
                    .map(|(n, _)| n.clone())
                    .collect();
                save_btn.set_sensitive(has_dbs && !active_privs.is_empty());
                let inferred = PermissionPreset::infer_db_preset(engine, &active_privs);
                if let Some(pos) = PermissionPreset::ALL_DB
                    .iter()
                    .position(|&p| p == inferred)
                {
                    *suppress_sync.borrow_mut() = true;
                    preset_row.set_selected(pos as u32);
                    *suppress_sync.borrow_mut() = false;
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
            let check_buttons_rc = check_buttons_rc.clone();
            save_btn.connect_clicked(move |_| {
                let Some(db_str) = db_model.string(db_row.selected()).map(|s| s.to_string()) else {
                    return;
                };
                let privileges: Vec<String> = check_buttons_rc
                    .iter()
                    .filter(|(_, c)| c.is_active())
                    .map(|(n, _)| n.clone())
                    .collect();
                if privileges.is_empty() {
                    return;
                }
                let spec = DbPermissionSpec {
                    username: username.clone(),
                    host: host.clone(),
                    database: db_str,
                    privileges,
                    with_grant_option: grant_opt_row.is_active(),
                };
                let _ = cmd_tx.send_blocking(BackendCommand::SetDatabasePermissions(spec));
                dialog.close();
            });
        }

        dialog.present(Some(parent));
    }

    fn confirm_revoke_db_permission(&self, parent: &gtk::Widget, perm: &DatabasePermission) {
        let user_label = match &perm.host {
            Some(h) if !h.is_empty() => format!("'{}'@'{}'", perm.username, h),
            _ => format!("'{}'", perm.username),
        };
        let alert = adw::AlertDialog::builder()
            .heading(format!("Revoke Permissions on '{}'?", perm.database))
            .body(format!(
                "Are you sure you want to revoke all privileges on database '{}' from {}?",
                perm.database, user_label
            ))
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("revoke", "Revoke Permissions");
        alert.set_response_appearance("revoke", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let cmd_tx = self.cmd_tx.clone();
        let username = perm.username.clone();
        let host = perm.host.clone();
        let database = perm.database.clone();
        alert.connect_response(None, move |_, resp| {
            if resp == "revoke" {
                let _ = cmd_tx.send_blocking(BackendCommand::RevokeDatabasePermissions {
                    username: username.clone(),
                    host: host.clone(),
                    database: database.clone(),
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
    fn verify_live_gtk_users_grid_alignment() {
        crate::ui::test_util::run_gtk_test(|| {
            use crate::ui::workspace::WorkspaceContent;
            use adw::prelude::*;

            let (cmd_tx, _cmd_rx) = async_channel::unbounded();
            let workspace_content = WorkspaceContent::new(cmd_tx, |_| {});
            workspace_content.stack.set_visible_child_name("users");

            let users = vec![
                DbUser {
                    username: "root".into(),
                    host: Some("localhost".into()),
                    global_privileges: vec!["SELECT".into(), "INSERT".into(), "SUPER".into()],
                    has_grant_option: true,
                },
                DbUser {
                    username: "app_service".into(),
                    host: Some("%".into()),
                    global_privileges: Vec::new(),
                    has_grant_option: false,
                },
            ];

            let perms = vec![DatabasePermission {
                username: "app_service".into(),
                host: Some("%".into()),
                database: "commerce_prod".into(),
                privileges: vec!["SELECT".into(), "INSERT".into(), "UPDATE".into()],
                has_grant_option: false,
            }];

            workspace_content.users_tab.set_connection_context(
                DbEngine::MySql,
                vec!["commerce_prod".into(), "analytics".into()],
            );
            workspace_content.users_tab.set_users_and_permissions(
                users,
                Some(("app_service".into(), Some("%".into()))),
                perms.clone(),
            );

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

            let grid = &workspace_content.users_tab.grid;
            assert!(grid.has_css_class("spreadsheet-grid"));
            for col in 0..=USERS_HEADERS.len() as i32 {
                let hdr = grid.child_at(col, 0).expect("users header cell");
                assert!(hdr.has_css_class("spreadsheet-header-box"));
                let hdr_bounds = hdr.compute_bounds(grid).expect("hdr bounds");
                for row in 1..=2 {
                    let cell = grid.child_at(col, row).expect("users row cell");
                    let cell_bounds = cell.compute_bounds(grid).expect("cell bounds");
                    assert_eq!(hdr_bounds.x(), cell_bounds.x());
                    assert_eq!(hdr_bounds.width(), cell_bounds.width());
                }
            }
            let users_scroll_w = workspace_content.users_tab.scroll.width();
            assert!(
                grid.width() >= users_scroll_w - 2,
                "users grid width ({}) should fill its surrounding panel width ({})",
                grid.width(),
                users_scroll_w
            );

            let perm_grid = &workspace_content.users_tab.perm_grid;
            let perm_scroll_w = workspace_content.users_tab.perm_scroll.width();
            assert!(
                perm_grid.width() >= perm_scroll_w - 2,
                "permissions grid width ({}) should fill its surrounding panel width ({})",
                perm_grid.width(),
                perm_scroll_w
            );

            // Verify UsersTab preserves vertical scroll position when selecting a user row
            let many_users: Vec<DbUser> = (0..25)
                .map(|i| DbUser {
                    username: format!("user_{:02}", i),
                    host: Some("%".into()),
                    global_privileges: if i == 0 {
                        vec!["SELECT".into(), "INSERT".into(), "SUPER".into()]
                    } else {
                        vec!["SELECT".into()]
                    },
                    has_grant_option: i == 0,
                })
                .collect();
            workspace_content.users_tab.set_users_and_permissions(
                many_users.clone(),
                Some(("user_00".into(), Some("%".into()))),
                perms.clone(),
            );
            for _ in 0..30 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }

            let users_vadj = workspace_content.users_tab.scroll.vadjustment();
            users_vadj.set_value(280.0);
            for _ in 0..20 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(users_vadj.value().round() as i32, 280);

            // Select row 20 ("user_19") and verify vertical scroll stays at 280
            workspace_content.users_tab.set_user_db_permissions(
                "user_19",
                Some("%".into()),
                perms.clone(),
            );
            for _ in 0..40 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(users_vadj.value().round() as i32, 280);
            assert!(
                workspace_content
                    .users_tab
                    .grid
                    .child_at(1, 20)
                    .expect("row 20 username cell")
                    .has_css_class("user-row-selected")
            );
            assert!(
                !workspace_content
                    .users_tab
                    .grid
                    .child_at(1, 1)
                    .expect("row 1 username cell")
                    .has_css_class("user-row-selected")
            );

            // Refresh full users list and verify vertical scroll still stays at 280
            workspace_content.users_tab.set_users_and_permissions(
                many_users,
                Some(("user_19".into(), Some("%".into()))),
                perms,
            );
            for _ in 0..40 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(users_vadj.value().round() as i32, 280);

            // Verify ContentTab preserves horizontal scroll when clicking a far-right column header to sort
            workspace_content.stack.set_visible_child_name("content");
            let cols: Vec<crate::db::models::ColumnInfo> = (0..15)
                .map(|i| crate::db::models::ColumnInfo {
                    ordinal: (i + 1) as u32,
                    name: format!("column_header_number_{}", i),
                    data_type: "varchar(255)".into(),
                    is_nullable: true,
                    is_primary_key: i == 0,
                    default_value: None,
                    extra: String::new(),
                })
                .collect();
            let rows: Vec<crate::db::models::TableRowData> = (0..5)
                .map(|r| crate::db::models::TableRowData {
                    pk: Some(crate::db::models::PrimaryKeyValue {
                        columns: vec![("column_header_number_0".into(), r.to_string())],
                    }),
                    values: (0..15)
                        .map(|c| crate::db::models::DbValue::Text(format!("val_{}_{}", r, c)))
                        .collect(),
                })
                .collect();
            let page = crate::db::models::TablePage {
                database: "commerce_prod".into(),
                table: "orders".into(),
                columns: cols,
                rows,
                total_rows: 5,
                page_offset: 0,
                page_limit: 100,
            };
            workspace_content.content_tab.set_page(page.clone());
            for _ in 0..30 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }

            let content_paned = workspace_content
                .content_tab
                .root
                .first_child()
                .and_then(|w| w.next_sibling())
                .and_then(|w| w.next_sibling())
                .and_then(|w| w.next_sibling())
                .and_then(|w| w.downcast::<gtk::Paned>().ok())
                .expect("content_paned");
            let content_scroll = content_paned
                .start_child()
                .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
                .expect("content_scroll");
            let content_viewport = content_scroll
                .child()
                .and_then(|w| w.downcast::<gtk::Viewport>().ok())
                .expect("content_viewport");
            let content_grid = content_viewport
                .child()
                .and_then(|w| w.downcast::<gtk::Grid>().ok())
                .expect("content_grid");

            let hadj = content_scroll.hadjustment();
            hadj.set_value(800.0);
            for _ in 0..20 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(hadj.value().round() as i32, 800);

            if let Some(hdr_box_14) = content_grid.child_at(15, 0) {
                if let Some(hdr_btn_14) = hdr_box_14
                    .first_child()
                    .and_then(|w| w.downcast::<gtk::Button>().ok())
                {
                    hdr_btn_14.emit_clicked();
                }
            }
            for _ in 0..10 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(hadj.value().round() as i32, 800);

            workspace_content.content_tab.set_page(page);
            for _ in 0..60 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(hadj.value().round() as i32, 800);

            win.close();
            while ctx.iteration(false) {}
        });
    }
}

