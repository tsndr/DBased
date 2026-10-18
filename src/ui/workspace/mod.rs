pub mod content_tab;
pub mod query_tab;
pub mod relations_tab;
pub mod structure_tab;
pub mod users_tab;

use crate::db::models::BackendCommand;
use crate::ui::workspace::content_tab::ContentTab;
use crate::ui::workspace::query_tab::QueryTab;
use crate::ui::workspace::relations_tab::RelationsTab;
use crate::ui::workspace::structure_tab::StructureTab;
use crate::ui::workspace::users_tab::UsersTab;
use async_channel::Sender;
use libadwaita as adw;

#[derive(Clone)]
pub struct WorkspaceContent {
    pub page: adw::NavigationPage,
    pub stack: adw::ViewStack,
    pub structure_tab: StructureTab,
    pub content_tab: ContentTab,
    pub relations_tab: RelationsTab,
    pub query_tab: QueryTab,
    pub users_tab: UsersTab,
}

impl WorkspaceContent {
    pub fn new<F>(cmd_tx: Sender<BackendCommand>, on_toast: F) -> Self
    where
        F: Fn(&str) + Clone + 'static,
    {
        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let stack = adw::ViewStack::new();

        let structure_tab = StructureTab::new(cmd_tx.clone());
        let content_tab = ContentTab::new(cmd_tx.clone(), on_toast.clone());
        let relations_tab = RelationsTab::new(cmd_tx.clone());
        let query_tab = QueryTab::new(cmd_tx.clone(), on_toast);
        let users_tab = UsersTab::new(cmd_tx);

        stack.add_titled_with_icon(
            &structure_tab.root,
            Some("structure"),
            "Structure",
            "view-list-symbolic",
        );
        stack.add_titled_with_icon(
            &content_tab.root,
            Some("content"),
            "Content",
            "x-office-spreadsheet-symbolic",
        );
        stack.add_titled_with_icon(
            &relations_tab.root,
            Some("relations"),
            "Relations",
            "network-workgroup-symbolic",
        );
        stack.add_titled_with_icon(
            &query_tab.root,
            Some("query"),
            "Query",
            "utilities-terminal-symbolic",
        );
        stack.add_titled_with_icon(
            &users_tab.root,
            Some("users"),
            "Users",
            "system-users-symbolic",
        );

        // Default to Content tab when browsing tables
        stack.set_visible_child_name("content");

        let switcher = adw::ViewSwitcher::builder()
            .stack(&stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        header.set_title_widget(Some(&switcher));

        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&stack));

        let page = adw::NavigationPage::builder()
            .title("Table Workspace")
            .child(&toolbar_view)
            .build();

        Self {
            page,
            stack,
            structure_tab,
            content_tab,
            relations_tab,
            query_tab,
            users_tab,
        }
    }

    pub fn clear_table_tabs(&self) {
        self.content_tab.clear_navigation_history();
        self.structure_tab.clear();
        self.relations_tab.clear_table();
    }

    pub fn clear_all(&self) {
        self.clear_table_tabs();
        self.relations_tab.clear();
        self.query_tab.clear();
        self.users_tab.clear();
    }
}

pub const MIN_COLUMN_WIDTH: i32 = 48;
pub const DEFAULT_DATA_COL_WIDTH: i32 = 160;
pub const MAX_LONG_TEXT_AUTO_COL_WIDTH: i32 = 320;
pub const HEADER_EXTRA_PADDING: i32 = 34;
pub const CELL_EXTRA_PADDING: i32 = 28;
pub const MAX_MEASURE_CHARS: usize = 1000;
const RESIZE_HANDLE_HIT_WIDTH: i32 = 10;

pub fn measure_single_line_text<W: gtk4::prelude::IsA<gtk4::Widget>>(
    widget: &W,
    text: &str,
) -> i32 {
    use gtk4::prelude::*;
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

pub fn compute_auto_column_widths(
    natural_widths: &[i32],
    manual_overrides: &[Option<i32>],
    total_avail: i32,
) -> Vec<i32> {
    let num_cols = natural_widths.len();
    let mut widths = Vec::with_capacity(num_cols);
    let mut is_manual = Vec::with_capacity(num_cols);

    for (i, &nat_width) in natural_widths.iter().enumerate() {
        if let Some(Some(w)) = manual_overrides.get(i) {
            widths.push((*w).max(MIN_COLUMN_WIDTH));
            is_manual.push(true);
        } else {
            let nat_w = nat_width.max(MIN_COLUMN_WIDTH);
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
            remaining = 0;
            break;
        }
    }

    // Pass 3: If space still remains after all columns have reached at least
    // max(natural_width, DEFAULT_DATA_COL_WIDTH), distribute the remaining space
    // evenly across non-manual columns (or all columns if all have manual overrides)
    // so the table always fills the full width of its surrounding panel.
    if remaining > 0 && num_cols > 0 {
        let mut expandable: Vec<usize> = (0..num_cols).filter(|&i| !is_manual[i]).collect();
        if expandable.is_empty() {
            expandable = (0..num_cols).collect();
        }
        let count = expandable.len() as i32;
        let share = remaining / count;
        let extra = remaining % count;
        for (idx, &i) in expandable.iter().enumerate() {
            widths[i] += share + if (idx as i32) < extra { 1 } else { 0 };
        }
    }

    widths
}

pub fn format_long_text_preview(val: &crate::db::models::DbValue) -> String {
    use crate::db::models::DbValue;
    match val {
        DbValue::Null => "NULL".to_string(),
        DbValue::Text(s) => {
            let mut out = String::with_capacity(s.len().min(MAX_MEASURE_CHARS));
            let mut chars = s.chars().peekable();
            let mut count = 0;
            while let Some(c) = chars.next() {
                if count >= MAX_MEASURE_CHARS {
                    out.push('…');
                    break;
                }
                if c == '\r' {
                    if chars.peek() == Some(&'\n') {
                        let _ = chars.next();
                    }
                    out.push_str(" ↵ ");
                } else if c == '\n' {
                    out.push_str(" ↵ ");
                } else {
                    out.push(c);
                }
                count += 1;
            }
            out
        }
    }
}

/// Wraps a header content widget inside a horizontally-resizable header cell box
/// with a right-edge resize handle that supports smooth mouse dragging.
pub fn build_resizable_header_cell<W, F>(
    content: &W,
    initial_width: i32,
    css_class: Option<&str>,
    on_resize: F,
) -> gtk4::Box
where
    W: gtk4::prelude::IsA<gtk4::Widget>,
    F: Fn(i32) + 'static,
{
    use gtk4 as gtk;
    use gtk4::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    let cell_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cell_box.set_width_request(initial_width);
    cell_box.set_hexpand(false);
    cell_box.set_overflow(gtk::Overflow::Hidden);
    if let Some(cls) = css_class {
        cell_box.add_css_class(cls);
    }

    content.set_hexpand(true);
    cell_box.append(content);

    let handle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    handle.set_width_request(7);
    handle.set_halign(gtk::Align::End);
    handle.set_vexpand(true);
    handle.add_css_class("col-resize-handle");
    handle.set_cursor_from_name(Some("col-resize"));
    handle.set_tooltip_text(Some("Drag to resize column"));
    cell_box.append(&handle);

    let is_resizing = Rc::new(RefCell::new(false));
    let start_width = Rc::new(RefCell::new(initial_width));
    let panel_fill_state: Rc<RefCell<(i32, i32, Option<(gtk::Box, i32)>)>> =
        Rc::new(RefCell::new((0, 0, None)));

    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);

    {
        let cell_box_ref = cell_box.clone();
        let handle_ref = handle.clone();
        let is_resizing = is_resizing.clone();
        let start_width = start_width.clone();
        let panel_fill_state = panel_fill_state.clone();
        drag.connect_drag_begin(move |gesture, start_x, _start_y| {
            let alloc_w = cell_box_ref
                .width()
                .max(cell_box_ref.width_request())
                .max(MIN_COLUMN_WIDTH);
            if start_x >= (alloc_w - RESIZE_HANDLE_HIT_WIDTH) as f64 {
                *is_resizing.borrow_mut() = true;
                *start_width.borrow_mut() = alloc_w;

                let mut vp_w = 0;
                let mut rest_w = 0;
                let mut filler: Option<(gtk::Box, i32)> = None;
                if let Some(grid) = cell_box_ref
                    .parent()
                    .and_then(|p| p.downcast::<gtk::Grid>().ok())
                {
                    if let Some(vp) = grid.parent() {
                        vp_w = (vp.width() - 2).max(0);
                    }
                    let mut row0_cells: Vec<gtk::Box> = Vec::new();
                    let mut c = 0;
                    while let Some(w) = grid.child_at(c, 0) {
                        if let Ok(b) = w.downcast::<gtk::Box>() {
                            row0_cells.push(b);
                        }
                        c += 1;
                    }
                    if let Some(this_idx) = row0_cells.iter().position(|b| b == &cell_box_ref) {
                        let filler_idx = ((this_idx + 1)..row0_cells.len())
                            .rev()
                            .find(|&i| {
                                row0_cells[i]
                                    .first_child()
                                    .and_then(|ch| ch.downcast::<gtk::Label>().ok())
                                    .is_none_or(|lbl| lbl.text() != "Actions")
                            })
                            .or_else(|| {
                                if this_idx + 1 < row0_cells.len() {
                                    Some(row0_cells.len() - 1)
                                } else {
                                    None
                                }
                            });
                        for (i, b) in row0_cells.iter().enumerate() {
                            if i == this_idx {
                                continue;
                            }
                            let bw = b.width().max(b.width_request()).max(MIN_COLUMN_WIDTH);
                            if Some(i) == filler_idx {
                                filler = Some((b.clone(), bw));
                            } else {
                                rest_w += bw;
                            }
                        }
                    }
                }
                *panel_fill_state.borrow_mut() = (vp_w, rest_w, filler);

                handle_ref.add_css_class("resizing");
                gesture.set_state(gtk::EventSequenceState::Claimed);
            } else {
                *is_resizing.borrow_mut() = false;
                gesture.set_state(gtk::EventSequenceState::Denied);
            }
        });
    }

    {
        let cell_box_ref = cell_box.clone();
        let is_resizing = is_resizing.clone();
        let start_width = start_width.clone();
        let panel_fill_state = panel_fill_state.clone();
        drag.connect_drag_update(move |_, offset_x, _offset_y| {
            if *is_resizing.borrow() {
                let (vp_w, rest_w, ref filler) = *panel_fill_state.borrow();
                let mut new_w =
                    (*start_width.borrow() + offset_x.round() as i32).max(MIN_COLUMN_WIDTH);
                if let Some((ref filler_box, filler_start_w)) = *filler {
                    let target_filler_w = (vp_w - rest_w - new_w).max(filler_start_w);
                    filler_box.set_width_request(target_filler_w);
                } else if vp_w > 0 {
                    new_w = new_w.max(vp_w - rest_w);
                }
                cell_box_ref.set_width_request(new_w);
                on_resize(new_w);
            }
        });
    }

    {
        let handle_ref = handle.clone();
        let is_resizing = is_resizing.clone();
        drag.connect_drag_end(move |_, _, _| {
            *is_resizing.borrow_mut() = false;
            handle_ref.remove_css_class("resizing");
        });
    }

    cell_box.add_controller(drag);
    cell_box
}

/// Creates a read-only spreadsheet cell containing a monospace label with optional CSS classes
/// and tooltip, styled consistently with the Content and Query spreadsheet grids.
pub fn create_spreadsheet_label_cell(
    text: &str,
    extra_classes: &[&str],
    tooltip: Option<&str>,
) -> (gtk4::Box, gtk4::Label) {
    use gtk4 as gtk;
    use gtk4::prelude::*;

    let cell_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cell_box.add_css_class("spreadsheet-cell");
    cell_box.set_overflow(gtk::Overflow::Hidden);

    let lbl = gtk::Label::new(Some(text));
    lbl.add_css_class("monospace");
    for cls in extra_classes {
        lbl.add_css_class(cls);
    }
    lbl.set_halign(gtk::Align::Fill);
    lbl.set_valign(gtk::Align::Center);
    lbl.set_xalign(0.0);
    lbl.set_hexpand(true);
    lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
    lbl.set_max_width_chars(1);
    lbl.set_margin_start(8);
    lbl.set_margin_end(8);
    lbl.set_margin_top(4);
    lbl.set_margin_bottom(4);
    if let Some(tt) = tooltip {
        if !tt.is_empty() {
            lbl.set_tooltip_text(Some(tt));
        }
    }

    cell_box.append(&lbl);
    (cell_box, lbl)
}

/// Ensures header cells (row 0 with `.spreadsheet-header-box`) are positioned at the end
/// of `grid`'s child list so they render above scrolled data rows and receive pointer events first.
pub fn raise_spreadsheet_headers(grid: &gtk4::Grid) {
    use gtk4::prelude::*;

    let needs_raise = grid
        .last_child()
        .is_some_and(|c| !c.has_css_class("spreadsheet-header-box"));
    if needs_raise {
        let mut col = 0;
        while let Some(hdr) = grid.child_at(col, 0) {
            if hdr.has_css_class("spreadsheet-header-box") {
                hdr.insert_before(grid, None::<&gtk4::Widget>);
            }
            col += 1;
        }
    }
}

/// Creates a `gtk::ScrolledWindow` wrapping a spreadsheet `gtk::Grid` with `scroll-to-focus`
/// disabled on its `gtk::Viewport`, preventing GTK4 focus changes or grid rebuilds from
/// unexpectedly resetting horizontal or vertical scroll positions, and keeping the header row
/// sticky at the top when scrolling vertically.
pub fn build_spreadsheet_scrolled_window(grid: &gtk4::Grid) -> gtk4::ScrolledWindow {
    use gtk4 as gtk;
    use gtk4::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_STICKY_GRID_ID: AtomicUsize = AtomicUsize::new(1);
    let grid_id = NEXT_STICKY_GRID_ID.fetch_add(1, Ordering::Relaxed);
    let sticky_class = format!("sticky-spreadsheet-grid-{}", grid_id);
    grid.add_css_class(&sticky_class);

    let css_provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }

    let viewport = gtk::Viewport::builder()
        .scroll_to_focus(false)
        .child(grid)
        .build();

    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&viewport)
        .build();

    let update_sticky = {
        let grid_weak = grid.downgrade();
        let css_provider = css_provider.clone();
        let sticky_class = sticky_class.clone();
        let last_y = std::cell::Cell::new(0i32);
        move |vadj: &gtk::Adjustment| {
            let Some(grid) = grid_weak.upgrade() else {
                return;
            };
            raise_spreadsheet_headers(&grid);
            // Match GtkViewport's integer child allocation (`child_allocation.y = -vadj.value()`)
            // so the sticky header never drifts by 1-2px on fractional scroll offsets.
            let y = vadj.value().max(0.0) as i32;
            if y != last_y.get() {
                last_y.set(y);
                if y > 0 {
                    css_provider.load_from_string(&format!(
                        ".{} > .spreadsheet-header-box {{ transform: translateY({}px); box-shadow: 0 -2px 0 mix(@window_bg_color, @headerbar_bg_color, 0.65); }}",
                        sticky_class, y
                    ));
                } else {
                    css_provider.load_from_string("");
                }
            }
        }
    };

    let vadj = scroll.vadjustment();
    let update_for_val = update_sticky.clone();
    vadj.connect_value_changed(move |adj| {
        update_for_val(adj);
    });
    let update_for_upper = update_sticky.clone();
    vadj.connect_upper_notify(move |adj| {
        update_for_upper(adj);
    });
    vadj.connect_page_size_notify(move |adj| {
        update_sticky(adj);
    });

    scroll
}

/// Clears all children from a spreadsheet `gtk::Grid`, first unsetting window focus if the
/// currently focused widget is inside `grid` so GTK4's window focus repair does not arbitrarily
/// focus row 1 / column 1 after the grid is rebuilt.
pub fn clear_spreadsheet_grid(grid: &gtk4::Grid) {
    use gtk4::prelude::*;

    if let Some(root) = grid.root() {
        if let Some(focused) = root.focus() {
            if focused == *grid || focused.is_ancestor(grid) {
                gtk4::prelude::RootExt::set_focus(&root, None::<&gtk4::Widget>);
            }
        }
    }
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }

    let grid_weak = grid.downgrade();
    gtk4::glib::idle_add_local_once(move || {
        if let Some(grid) = grid_weak.upgrade() {
            raise_spreadsheet_headers(&grid);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{ColumnInfo, DbValue, PrimaryKeyValue, TablePage, TableRowData};
    use adw::prelude::*;
    use gtk4 as gtk;

    #[test]
    fn verify_live_gtk_sticky_header_on_scroll() {
        crate::ui::test_util::run_gtk_test(|| {
            let (cmd_tx, _cmd_rx) = async_channel::unbounded();
            let workspace_content = WorkspaceContent::new(cmd_tx, |_| {});
            workspace_content.stack.set_visible_child_name("content");

            let columns = vec![
                ColumnInfo {
                    ordinal: 1,
                    name: "id".into(),
                    data_type: "bigint".into(),
                    is_nullable: false,
                    is_primary_key: true,
                    default_value: None,
                    extra: String::new(),
                },
                ColumnInfo {
                    ordinal: 2,
                    name: "email".into(),
                    data_type: "varchar(255)".into(),
                    is_nullable: false,
                    is_primary_key: false,
                    default_value: None,
                    extra: String::new(),
                },
            ];

            let mut rows = Vec::new();
            for i in 1..=60 {
                rows.push(TableRowData {
                    pk: Some(PrimaryKeyValue {
                        columns: vec![("id".into(), i.to_string())],
                    }),
                    values: vec![
                        DbValue::Text(i.to_string()),
                        DbValue::Text(format!("user{}@example.com", i)),
                    ],
                });
            }

            workspace_content.content_tab.set_page(TablePage {
                database: "test_db".into(),
                table: "users".into(),
                columns,
                rows,
                total_rows: 60,
                page_offset: 0,
                page_limit: 100,
            });

            let win = adw::Window::builder()
                .title("DBased Sticky Header Test")
                .default_width(900)
                .default_height(400)
                .content(&workspace_content.page)
                .build();
            win.present();

            let ctx = gtk::glib::MainContext::default();
            for _ in 0..40 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            let grid = workspace_content
                .content_tab
                .root
                .observe_children()
                .into_iter()
                .flatten()
                .find_map(|obj| obj.downcast::<gtk::Paned>().ok())
                .and_then(|p| p.start_child())
                .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
                .expect("content scrolled window");

            let vadj = grid.vadjustment();
            assert!(
                vadj.upper() > vadj.page_size() + 200.0,
                "Expected scrollable content: upper={} page_size={}",
                vadj.upper(),
                vadj.page_size()
            );

            let inner_grid = grid
                .child()
                .and_then(|vp| vp.downcast::<gtk::Viewport>().ok())
                .and_then(|vp| vp.child())
                .and_then(|g| g.downcast::<gtk::Grid>().ok())
                .expect("inner spreadsheet grid");

            let hdr_col1 = inner_grid.child_at(1, 0).expect("header col 1");
            let hdr_bounds_before = hdr_col1.compute_bounds(&grid).expect("bounds before");
            assert_eq!(
                hdr_bounds_before.y(),
                0.0,
                "Header should start flush at y=0 of scroll window, got y={}",
                hdr_bounds_before.y()
            );

            // Test fractional scroll offsets (e.g. step_increment = page_size * 0.1 in Structure view)
            // to ensure the header never develops a 1-2px gap above it.
            for test_v in [10.1, 10.4, 10.5, 10.6, 10.9, 33.8, 67.6, 101.4, 180.3, 180.7] {
                vadj.set_value(test_v);
                for _ in 0..5 {
                    while ctx.iteration(false) {}
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let hdr_b = hdr_col1.compute_bounds(&grid).expect("hdr bounds");
                assert_eq!(
                    hdr_b.y(),
                    0.0,
                    "Header must stay flush at y=0.0 for fractional vadj={}, got y={}",
                    test_v,
                    hdr_b.y()
                );
            }

            // Scroll down by 180px
            vadj.set_value(180.0);
            for _ in 0..30 {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            let hdr_bounds_after = hdr_col1.compute_bounds(&grid).expect("bounds after");
            let row1_col1 = inner_grid.child_at(1, 1).expect("row 1 col 1");
            let row1_bounds_after = row1_col1.compute_bounds(&grid).expect("row1 bounds after");
            println!(
                "After scroll (180px): hdr_bounds_in_scroll = {:?}, row1_bounds_in_scroll = {:?}",
                hdr_bounds_after, row1_bounds_after
            );

            assert!(
                hdr_bounds_after.y().abs() <= 2.0,
                "Sticky header must remain at y=0 in ScrolledWindow after scrolling down, got y={}",
                hdr_bounds_after.y()
            );
            assert!(
                row1_bounds_after.y() < -100.0,
                "Row 1 should have scrolled above viewport, got y={}",
                row1_bounds_after.y()
            );

            // Verify pointer picking at the header position hits the header cell (or its child button), not a data row underneath!
            let pick_x = (hdr_bounds_after.x() + hdr_bounds_after.width() * 0.5) as f64;
            let pick_y = (hdr_bounds_after.y() + hdr_bounds_after.height() * 0.5) as f64;
            let picked = grid
                .pick(pick_x, pick_y, gtk::PickFlags::DEFAULT)
                .expect("picked widget");
            println!("Picked widget at ({}, {}): {:?}", pick_x, pick_y, picked);
            assert!(
                picked == hdr_col1 || picked.is_ancestor(&hdr_col1),
                "Expected picked widget at top of scrolled table to be inside header cell, got {:?}",
                picked
            );

            win.close();
            while ctx.iteration(false) {}
        });
    }
}

