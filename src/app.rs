use crate::ui::window::build_main_window;
use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

const APP_ID: &str = "io.github.tsndr.dbased";

const CUSTOM_CSS: &str = r#"
.spreadsheet-grid {
    border-top: 1px solid alpha(@borders, 0.7);
    border-left: 1px solid alpha(@borders, 0.7);
}

.spreadsheet-header-box {
    border-bottom: 2px solid alpha(@borders, 0.9);
    background: alpha(@headerbar_bg_color, 0.65);
}

.spreadsheet-header-btn {
    border-radius: 0;
    border: none;
    background: transparent;
    padding: 6px 6px 6px 10px;
    font-weight: 600;
}

.spreadsheet-header-cell {
    border-right: 1px solid alpha(@borders, 0.8);
    border-bottom: 2px solid alpha(@borders, 0.9);
    background: alpha(@headerbar_bg_color, 0.65);
    padding: 6px 10px;
}

.col-resize-handle {
    min-width: 7px;
    background: transparent;
    border-right: 1px solid alpha(@borders, 0.65);
    transition: background 120ms ease, border-color 120ms ease;
}

.col-resize-handle:hover,
.col-resize-handle.resizing {
    background: alpha(@accent_bg_color, 0.35);
    border-right: 2px solid @accent_color;
}

.resizable-boxed-list > row {
    padding: 0;
}

.structure-drag-handle {
    opacity: 0.4;
    transition: opacity 120ms ease;
}

row:hover .structure-drag-handle {
    opacity: 0.9;
}

.structure-row-dragging {
    opacity: 0.45;
}

.structure-drop-above {
    border-top: 2px solid @accent_color;
    background: alpha(@accent_bg_color, 0.12);
}

.structure-drop-below {
    border-bottom: 2px solid @accent_color;
    background: alpha(@accent_bg_color, 0.12);
}

.spreadsheet-row-num {
    border-right: 1px solid alpha(@borders, 0.7);
    border-bottom: 1px solid alpha(@borders, 0.5);
    background: alpha(@headerbar_bg_color, 0.35);
    padding: 4px 8px;
}

.spreadsheet-cell {
    border-radius: 0;
    border-right: 1px solid alpha(@borders, 0.6);
    border-bottom: 1px solid alpha(@borders, 0.5);
    padding: 4px 8px;
    min-height: 28px;
    background: transparent;
}

box.spreadsheet-cell {
    padding: 0;
}

.spreadsheet-cell:focus-within,
.spreadsheet-cell.cell-panel-active {
    outline: 2px solid @accent_color;
    outline-offset: -2px;
    background: alpha(@accent_bg_color, 0.14);
}

.spreadsheet-cell-entry {
    border-radius: 0;
    border: none;
    box-shadow: none;
    outline: none;
    padding: 4px 8px;
    min-height: 28px;
    background: transparent;
}

.cell-ref-btn {
    min-height: 22px;
    min-width: 22px;
    padding: 2px;
    margin: 2px 4px 2px 0;
    border-radius: 4px;
    opacity: 0.65;
}

.cell-ref-btn:hover {
    opacity: 1.0;
    color: @accent_color;
    background: alpha(@accent_bg_color, 0.2);
}

.cell-dirty {
    background: alpha(@warning_bg_color, 0.22);
    border-bottom: 2px solid @warning_color;
}

.cell-null {
    color: alpha(@window_fg_color, 0.45);
    font-style: italic;
}

.long-text-panel {
    background: @window_bg_color;
    border-left: 1px solid alpha(@borders, 0.8);
}

.long-text-panel-header {
    background: alpha(@headerbar_bg_color, 0.5);
}

.long-text-panel-footer {
    background: alpha(@headerbar_bg_color, 0.35);
    min-height: 24px;
}

.vim-status-bar {
    background: alpha(@headerbar_bg_color, 0.5);
    min-height: 24px;
}
"#;

pub fn run_app() -> gtk::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_startup(|_| {
        sourceview5::init();
        gtk::Window::set_default_icon_name(APP_ID);
        load_css();
    });

    app.connect_activate(|app| {
        let win = build_main_window(app);
        win.present();
    });

    app.run()
}

pub(crate) fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CUSTOM_CSS);

    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
