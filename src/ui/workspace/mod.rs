pub mod content_tab;
pub mod query_tab;
pub mod relations_tab;
pub mod structure_tab;

use crate::db::models::BackendCommand;
use crate::ui::workspace::content_tab::ContentTab;
use crate::ui::workspace::query_tab::QueryTab;
use crate::ui::workspace::relations_tab::RelationsTab;
use crate::ui::workspace::structure_tab::StructureTab;
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
        let query_tab = QueryTab::new(cmd_tx, on_toast);

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
        }
    }
}

pub const MIN_COLUMN_WIDTH: i32 = 48;
const RESIZE_HANDLE_HIT_WIDTH: i32 = 10;

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

/// Wraps a row cell widget in a fixed-width cell container that clips overflow
/// so column widths in `ListBox` rows stay strictly aligned with resizable headers.
pub fn wrap_resizable_row_cell<W: gtk4::prelude::IsA<gtk4::Widget>>(
    child: &W,
    width: i32,
) -> gtk4::Box {
    use gtk4 as gtk;
    use gtk4::prelude::*;

    let cell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cell.set_width_request(width);
    cell.set_hexpand(false);
    cell.set_overflow(gtk::Overflow::Hidden);
    cell.add_css_class("table-row-cell");
    child.set_margin_start(8);
    child.set_margin_end(6);
    cell.append(child);
    cell
}
