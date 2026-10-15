use crate::ui::window::build_main_window;
use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

const APP_ID: &str = "dev.tsndr.dbased";
const APP_ICON_SVG: &str = include_str!("../data/dev.tsndr.dbased.svg");

const CUSTOM_CSS: &str = r#"
.spreadsheet-grid {
    border-left: 1px solid alpha(@borders, 0.7);
}

.spreadsheet-header-box {
    border-top: 1px solid alpha(@borders, 0.7);
    border-bottom: 2px solid alpha(@borders, 0.9);
    background: mix(@window_bg_color, @headerbar_bg_color, 0.65);
}

.spreadsheet-header-btn {
    border-radius: 0;
    border: none;
    background: transparent;
    padding: 6px 6px 6px 10px;
    font-weight: 600;
}

.spreadsheet-header-cell {
    border-top: 1px solid alpha(@borders, 0.7);
    border-right: 1px solid alpha(@borders, 0.8);
    border-bottom: 2px solid alpha(@borders, 0.9);
    background: mix(@window_bg_color, @headerbar_bg_color, 0.65);
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

.structure-drag-handle {
    opacity: 0.4;
    transition: opacity 120ms ease;
}

.spreadsheet-row-num:hover .structure-drag-handle,
.structure-drag-handle:hover {
    opacity: 0.9;
}

.cell-action-btn {
    min-height: 20px;
    min-width: 18px;
    padding: 1px 2px;
    margin: 2px 1px;
    border-radius: 4px;
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

.user-row-selected {
    background: alpha(@accent_bg_color, 0.12);
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

.spreadsheet-cell-dropdown {
    min-height: 28px;
}

.spreadsheet-cell-dropdown > button {
    border-radius: 0;
    border: none;
    box-shadow: none;
    outline: none;
    padding: 2px 8px;
    min-height: 28px;
    background: transparent;
    font-weight: normal;
}

.spreadsheet-cell-dropdown > button:hover,
.spreadsheet-cell-dropdown > button:active,
.spreadsheet-cell-dropdown > button:checked {
    background: alpha(@window_fg_color, 0.05);
}

.spreadsheet-cell-dropdown.cell-null > button label {
    color: alpha(@window_fg_color, 0.45);
    font-style: italic;
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
        register_app_icon();
        gtk::Window::set_default_icon_name(APP_ID);
        load_css();
    });

    app.connect_activate(|app| {
        let win = build_main_window(app);
        win.present();
    });

    app.run()
}

fn register_app_icon() {
    let mut custom_search_dirs = Vec::new();

    if let Some(base_dirs) = directories::BaseDirs::new() {
        let user_icons_dir = base_dirs.data_local_dir().join("icons");
        let user_svg_path = user_icons_dir
            .join("hicolor")
            .join("scalable")
            .join("apps")
            .join(format!("{APP_ID}.svg"));

        let needs_write = match std::fs::read_to_string(&user_svg_path) {
            Ok(existing) => existing != APP_ICON_SVG,
            Err(_) => true,
        };

        if needs_write {
            if let Some(parent) = user_svg_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&user_svg_path, APP_ICON_SVG).is_ok() {
                let cache_file = user_icons_dir.join("hicolor").join("icon-theme.cache");
                let _ = std::fs::remove_file(cache_file);
            }
        }

        custom_search_dirs.push(user_icons_dir);
    }

    if let Some(display) = gtk::gdk::Display::default() {
        let icon_theme = gtk::IconTheme::for_display(&display);
        let existing_paths = icon_theme.search_path();
        let mut new_paths: Vec<&std::path::Path> =
            custom_search_dirs.iter().map(|p| p.as_path()).collect();
        for p in &existing_paths {
            if !custom_search_dirs.iter().any(|c| c == p) {
                new_paths.push(p.as_path());
            }
        }
        icon_theme.set_search_path(&new_paths);
    }
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
