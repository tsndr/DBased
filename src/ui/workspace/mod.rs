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
            break;
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

    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);

    {
        let cell_box_ref = cell_box.clone();
        let handle_ref = handle.clone();
        let is_resizing = is_resizing.clone();
        let start_width = start_width.clone();
        drag.connect_drag_begin(move |gesture, start_x, _start_y| {
            let alloc_w = cell_box_ref
                .width()
                .max(cell_box_ref.width_request())
                .max(MIN_COLUMN_WIDTH);
            if start_x >= (alloc_w - RESIZE_HANDLE_HIT_WIDTH) as f64 {
                *is_resizing.borrow_mut() = true;
                *start_width.borrow_mut() = alloc_w;
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
        drag.connect_drag_update(move |_, offset_x, _offset_y| {
            if *is_resizing.borrow() {
                let new_w = (*start_width.borrow() + offset_x.round() as i32).max(MIN_COLUMN_WIDTH);
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

/// Creates a `gtk::ScrolledWindow` wrapping a spreadsheet `gtk::Grid` with `scroll-to-focus`
/// disabled on its `gtk::Viewport`, preventing GTK4 focus changes or grid rebuilds from
/// unexpectedly resetting horizontal or vertical scroll positions.
pub fn build_spreadsheet_scrolled_window(grid: &gtk4::Grid) -> gtk4::ScrolledWindow {
    use gtk4 as gtk;

    let viewport = gtk::Viewport::builder()
        .scroll_to_focus(false)
        .child(grid)
        .build();

    gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&viewport)
        .build()
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
}

