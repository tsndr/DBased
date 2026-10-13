use crate::config::bookmarks::{
    BookmarkStore, ConnectionBookmark, ConnectionProfile, DbEngine, SshAuthMethod, SshConfig,
};
use crate::config::secrets::SecretStore;
use crate::db::models::BackendCommand;
use adw::prelude::*;
use async_channel::Sender;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;
use uuid::Uuid;

#[derive(Clone)]
pub struct ConnectionPage {
    pub root: adw::NavigationSplitView,
    status_spinner: gtk::Spinner,
    status_label: gtk::Label,
}

impl ConnectionPage {
    pub fn new<F>(cmd_tx: Sender<BackendCommand>, on_toast: F) -> Self
    where
        F: Fn(&str) + Clone + 'static,
    {
        let store = Rc::new(BookmarkStore::new());
        let bookmarks: Rc<RefCell<Vec<ConnectionBookmark>>> =
            Rc::new(RefCell::new(store.load_all().unwrap_or_default()));
        let current_id: Rc<RefCell<String>> =
            Rc::new(RefCell::new(Uuid::new_v4().to_string()));
        let updating_ui = Rc::new(RefCell::new(false));

        // Sidebar: Saved Bookmarks
        let sidebar_toolbar = adw::ToolbarView::new();
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("DBased", "Bookmarks")));

        let new_bm_btn = gtk::Button::from_icon_name("list-add-symbolic");
        new_bm_btn.set_tooltip_text(Some("New Connection"));
        sidebar_header.pack_start(&new_bm_btn);

        // Quick Connect URL Popover button
        let url_popover = gtk::Popover::new();
        let pop_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        pop_box.set_margin_top(12);
        pop_box.set_margin_bottom(12);
        pop_box.set_margin_start(12);
        pop_box.set_margin_end(12);
        pop_box.set_width_request(340);

        let url_lbl = gtk::Label::new(Some("Quick Connect via URL"));
        url_lbl.add_css_class("heading");
        url_lbl.set_halign(gtk::Align::Start);
        let url_entry = gtk::Entry::new();
        url_entry.set_placeholder_text(Some("postgres://user:pass@host:5432/db"));
        let url_go_btn = gtk::Button::with_label("Connect Now");
        url_go_btn.add_css_class("suggested-action");

        pop_box.append(&url_lbl);
        pop_box.append(&url_entry);
        pop_box.append(&url_go_btn);
        url_popover.set_child(Some(&pop_box));

        let quick_url_btn = gtk::MenuButton::new();
        quick_url_btn.set_icon_name("insert-link-symbolic");
        quick_url_btn.set_tooltip_text(Some("Quick Connect via Connection URL"));
        quick_url_btn.set_popover(Some(&url_popover));
        sidebar_header.pack_end(&quick_url_btn);

        sidebar_toolbar.add_top_bar(&sidebar_header);

        let bookmark_list = gtk::ListBox::new();
        bookmark_list.add_css_class("navigation-sidebar");
        bookmark_list.set_selection_mode(gtk::SelectionMode::Single);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&bookmark_list)
            .build();
        sidebar_toolbar.set_content(Some(&sidebar_scroll));

        let sidebar_page = adw::NavigationPage::builder()
            .title("Saved Connections")
            .child(&sidebar_toolbar)
            .build();

        // Right Content: Connection Form
        let content_toolbar = adw::ToolbarView::new();
        let content_header = adw::HeaderBar::new();
        content_header.set_title_widget(Some(&adw::WindowTitle::new(
            "Connection Setup",
            "Direct or SSH Tunnel",
        )));
        content_toolbar.add_top_bar(&content_header);

        let prefs_page = adw::PreferencesPage::new();

        // 1. Database Connection Group
        let db_group = adw::PreferencesGroup::builder()
            .title("Database Connection")
            .description("Configure MySQL or PostgreSQL connection parameters")
            .build();

        let name_row = adw::EntryRow::builder()
            .title("Bookmark Name")
            .text("Local PostgreSQL")
            .build();

        let engine_model = gtk::StringList::new(&["PostgreSQL", "MySQL"]);
        let engine_row = adw::ComboRow::builder()
            .title("Database Engine")
            .model(&engine_model)
            .selected(0)
            .build();

        let host_row = adw::EntryRow::builder()
            .title("Host")
            .text("127.0.0.1")
            .build();

        let port_row = adw::EntryRow::builder()
            .title("Port")
            .text("5432")
            .build();

        let user_row = adw::EntryRow::builder()
            .title("Username")
            .text("postgres")
            .build();

        let pass_row = adw::PasswordEntryRow::builder()
            .title("Password")
            .build();

        let default_db_row = adw::EntryRow::builder()
            .title("Default Database")
            .text("postgres")
            .build();

        db_group.add(&name_row);
        db_group.add(&engine_row);
        db_group.add(&host_row);
        db_group.add(&port_row);
        db_group.add(&user_row);
        db_group.add(&pass_row);
        db_group.add(&default_db_row);
        prefs_page.add(&db_group);

        // 2. SSH Tunnel Group
        let ssh_group = adw::PreferencesGroup::builder()
            .title("SSH Tunnel")
            .description("Optional SSH bastion port forwarding")
            .build();

        let ssh_expander = adw::ExpanderRow::builder()
            .title("Use SSH Tunnel")
            .subtitle("Forward database port over an encrypted SSH session")
            .show_enable_switch(true)
            .enable_expansion(false)
            .build();

        let ssh_host_row = adw::EntryRow::builder()
            .title("SSH Host")
            .text("")
            .build();

        let ssh_port_row = adw::EntryRow::builder()
            .title("SSH Port")
            .text("22")
            .build();

        let ssh_user_row = adw::EntryRow::builder()
            .title("SSH Username")
            .text("")
            .build();

        let ssh_auth_model = gtk::StringList::new(&["Password", "Private Key"]);
        let ssh_auth_row = adw::ComboRow::builder()
            .title("Authentication Method")
            .model(&ssh_auth_model)
            .selected(0)
            .build();

        let ssh_key_row = adw::EntryRow::builder()
            .title("Private Key File")
            .text("~/.ssh/id_ed25519")
            .visible(false)
            .build();

        let browse_key_btn = gtk::Button::from_icon_name("document-open-symbolic");
        browse_key_btn.set_valign(gtk::Align::Center);
        browse_key_btn.set_tooltip_text(Some("Browse Private Key File"));
        ssh_key_row.add_suffix(&browse_key_btn);

        let ssh_secret_row = adw::PasswordEntryRow::builder()
            .title("SSH Password / Key Passphrase")
            .build();

        ssh_expander.add_row(&ssh_host_row);
        ssh_expander.add_row(&ssh_port_row);
        ssh_expander.add_row(&ssh_user_row);
        ssh_expander.add_row(&ssh_auth_row);
        ssh_expander.add_row(&ssh_key_row);
        ssh_expander.add_row(&ssh_secret_row);
        ssh_group.add(&ssh_expander);
        prefs_page.add(&ssh_group);

        // 3. Action Buttons Group
        let actions_group = adw::PreferencesGroup::new();
        let actions_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        actions_box.set_halign(gtk::Align::End);
        actions_box.set_margin_top(8);
        actions_box.set_margin_bottom(12);

        let status_spinner = gtk::Spinner::new();
        let status_label = gtk::Label::new(None);
        status_label.add_css_class("dim-label");
        status_label.set_hexpand(true);
        status_label.set_halign(gtk::Align::Start);

        let delete_btn = gtk::Button::with_label("Delete");
        delete_btn.add_css_class("destructive-action");

        let save_btn = gtk::Button::with_label("Save Bookmark");

        let test_btn = gtk::Button::with_label("Test Connection");

        let quick_connect_btn = gtk::Button::with_label("Quick Connect");
        quick_connect_btn.add_css_class("suggested-action");
        quick_connect_btn.add_css_class("pill");

        actions_box.append(&status_spinner);
        actions_box.append(&status_label);
        actions_box.append(&delete_btn);
        actions_box.append(&save_btn);
        actions_box.append(&test_btn);
        actions_box.append(&quick_connect_btn);
        actions_group.add(&actions_box);
        prefs_page.add(&actions_group);

        content_toolbar.set_content(Some(&prefs_page));

        let content_page = adw::NavigationPage::builder()
            .title("Connection Details")
            .child(&content_toolbar)
            .build();

        let split_view = adw::NavigationSplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content_page)
            .min_sidebar_width(240.0)
            .max_sidebar_width(320.0)
            .build();

        // Switch default port when engine changes
        {
            let port_row = port_row.clone();
            let user_row = user_row.clone();
            let default_db_row = default_db_row.clone();
            let updating_ui = updating_ui.clone();
            engine_row.connect_selected_notify(move |combo| {
                if *updating_ui.borrow() {
                    return;
                }
                if combo.selected() == 0 {
                    // PostgreSQL
                    if port_row.text() == "3306" {
                        port_row.set_text("5432");
                    }
                    if user_row.text() == "root" {
                        user_row.set_text("postgres");
                    }
                    if default_db_row.text().is_empty() {
                        default_db_row.set_text("postgres");
                    }
                } else {
                    // MySQL
                    if port_row.text() == "5432" {
                        port_row.set_text("3306");
                    }
                    if user_row.text() == "postgres" {
                        user_row.set_text("root");
                    }
                    if default_db_row.text() == "postgres" {
                        default_db_row.set_text("");
                    }
                }
            });
        }

        // Toggle SSH key path row based on SSH auth method
        {
            let ssh_key_row = ssh_key_row.clone();
            ssh_auth_row.connect_selected_notify(move |combo| {
                ssh_key_row.set_visible(combo.selected() == 1);
            });
        }

        // File chooser for SSH private key
        {
            let ssh_key_row = ssh_key_row.clone();
            browse_key_btn.connect_clicked(move |btn| {
                let dialog = gtk::FileDialog::builder()
                    .title("Select SSH Private Key")
                    .build();
                let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                let ssh_key_row = ssh_key_row.clone();
                dialog.open(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            ssh_key_row.set_text(&path.to_string_lossy());
                        }
                    }
                });
            });
        }

        // Helper closure to read form state into a ConnectionProfile
        let read_profile = {
            let current_id = current_id.clone();
            let name_row = name_row.clone();
            let engine_row = engine_row.clone();
            let host_row = host_row.clone();
            let port_row = port_row.clone();
            let user_row = user_row.clone();
            let pass_row = pass_row.clone();
            let default_db_row = default_db_row.clone();
            let ssh_expander = ssh_expander.clone();
            let ssh_host_row = ssh_host_row.clone();
            let ssh_port_row = ssh_port_row.clone();
            let ssh_user_row = ssh_user_row.clone();
            let ssh_auth_row = ssh_auth_row.clone();
            let ssh_key_row = ssh_key_row.clone();
            let ssh_secret_row = ssh_secret_row.clone();

            move || -> ConnectionProfile {
                let engine = if engine_row.selected() == 0 {
                    DbEngine::Postgres
                } else {
                    DbEngine::MySql
                };
                let port = port_row
                    .text()
                    .trim()
                    .parse::<u16>()
                    .unwrap_or_else(|_| engine.default_port());
                let ssh_port = ssh_port_row
                    .text()
                    .trim()
                    .parse::<u16>()
                    .unwrap_or(22);
                let auth_method = if ssh_auth_row.selected() == 0 {
                    SshAuthMethod::Password
                } else {
                    SshAuthMethod::PrivateKey {
                        key_path: ssh_key_row.text().trim().to_string(),
                    }
                };
                let name_txt = name_row.text().trim().to_string();
                let host_txt = host_row.text().trim().to_string();
                let bookmark = ConnectionBookmark {
                    id: current_id.borrow().clone(),
                    name: if name_txt.is_empty() {
                        format!("{} ({})", engine.display_name(), host_txt)
                    } else {
                        name_txt
                    },
                    engine,
                    host: if host_txt.is_empty() {
                        "127.0.0.1".to_string()
                    } else {
                        host_txt
                    },
                    port,
                    username: user_row.text().trim().to_string(),
                    default_database: default_db_row.text().trim().to_string(),
                    ssh: SshConfig {
                        enabled: ssh_expander.enables_expansion(),
                        host: ssh_host_row.text().trim().to_string(),
                        port: ssh_port,
                        username: ssh_user_row.text().trim().to_string(),
                        auth_method,
                    },
                };

                ConnectionProfile {
                    bookmark,
                    db_password: pass_row.text().to_string(),
                    ssh_password_or_passphrase: ssh_secret_row.text().to_string(),
                }
            }
        };

        // Helper to populate form from a bookmark
        let populate_form = {
            let current_id = current_id.clone();
            let updating_ui = updating_ui.clone();
            let name_row = name_row.clone();
            let engine_row = engine_row.clone();
            let host_row = host_row.clone();
            let port_row = port_row.clone();
            let user_row = user_row.clone();
            let pass_row = pass_row.clone();
            let default_db_row = default_db_row.clone();
            let ssh_expander = ssh_expander.clone();
            let ssh_host_row = ssh_host_row.clone();
            let ssh_port_row = ssh_port_row.clone();
            let ssh_user_row = ssh_user_row.clone();
            let ssh_auth_row = ssh_auth_row.clone();
            let ssh_key_row = ssh_key_row.clone();
            let ssh_secret_row = ssh_secret_row.clone();

            move |bm: &ConnectionBookmark| {
                *updating_ui.borrow_mut() = true;
                *current_id.borrow_mut() = bm.id.clone();
                name_row.set_text(&bm.name);
                engine_row.set_selected(match bm.engine {
                    DbEngine::Postgres => 0,
                    DbEngine::MySql => 1,
                });
                host_row.set_text(&bm.host);
                port_row.set_text(&bm.port.to_string());
                user_row.set_text(&bm.username);
                default_db_row.set_text(&bm.default_database);

                ssh_expander.set_enable_expansion(bm.ssh.enabled);
                ssh_expander_expand(&ssh_expander, bm.ssh.enabled);
                ssh_host_row.set_text(&bm.ssh.host);
                ssh_port_row.set_text(&bm.ssh.port.to_string());
                ssh_user_row.set_text(&bm.ssh.username);

                match &bm.ssh.auth_method {
                    SshAuthMethod::Password => {
                        ssh_auth_row.set_selected(0);
                        ssh_key_row.set_visible(false);
                    }
                    SshAuthMethod::PrivateKey { key_path } => {
                        ssh_auth_row.set_selected(1);
                        ssh_key_row.set_text(key_path);
                        ssh_key_row.set_visible(true);
                    }
                }
                *updating_ui.borrow_mut() = false;

                pass_row.set_text("");
                ssh_secret_row.set_text("");

                // Load saved secrets asynchronously from keyring / fallback
                let bm_id = bm.id.clone();
                let current_id_check = current_id.clone();
                let pass_row = pass_row.clone();
                let ssh_secret_row = ssh_secret_row.clone();
                gtk::glib::spawn_future_local(async move {
                    let secrets = SecretStore::load_secrets(&bm_id).await;
                    if *current_id_check.borrow() == bm_id {
                        pass_row.set_text(&secrets.db_password);
                        ssh_secret_row.set_text(&secrets.ssh_secret);
                    }
                });
            }
        };

        // Helper to connect directly to a bookmark (loading its secrets first)
        let connect_bookmark = {
            let cmd_tx = cmd_tx.clone();
            let status_spinner = status_spinner.clone();
            let status_label = status_label.clone();
            let populate_form = populate_form.clone();
            move |bm: ConnectionBookmark| {
                populate_form(&bm);
                status_spinner.start();
                status_label.set_text("Connecting...");
                let cmd_tx = cmd_tx.clone();
                gtk::glib::spawn_future_local(async move {
                    let secrets = SecretStore::load_secrets(&bm.id).await;
                    let profile = ConnectionProfile {
                        bookmark: bm,
                        db_password: secrets.db_password,
                        ssh_password_or_passphrase: secrets.ssh_secret,
                    };
                    let _ = cmd_tx.send_blocking(BackendCommand::Connect(profile));
                });
            }
        };

        // Helper to rebuild the bookmark list sidebar
        let rebuild_sidebar = {
            let bookmark_list = bookmark_list.clone();
            let bookmarks = bookmarks.clone();
            let connect_bookmark = connect_bookmark.clone();
            move || {
                while let Some(child) = bookmark_list.first_child() {
                    bookmark_list.remove(&child);
                }
                for bm in bookmarks.borrow().iter() {
                    let row = adw::ActionRow::builder()
                        .title(&bm.name)
                        .subtitle(format!(
                            "{} · {}:{}{}",
                            bm.engine.display_name(),
                            bm.host,
                            bm.port,
                            if bm.ssh.enabled { " (SSH)" } else { "" }
                        ))
                        .activatable(true)
                        .build();
                    let icon = gtk::Image::from_icon_name("network-server-symbolic");
                    row.add_prefix(&icon);

                    let connect_row_btn =
                        gtk::Button::from_icon_name("media-playback-start-symbolic");
                    connect_row_btn.add_css_class("flat");
                    connect_row_btn.set_valign(gtk::Align::Center);
                    connect_row_btn.set_tooltip_text(Some("Connect to Bookmark"));
                    {
                        let bm_clone = bm.clone();
                        let connect_bookmark = connect_bookmark.clone();
                        connect_row_btn.connect_clicked(move |_| {
                            connect_bookmark(bm_clone.clone());
                        });
                    }
                    row.add_suffix(&connect_row_btn);

                    // Double-click bookmark row to connect immediately
                    {
                        let dbl_click = gtk::GestureClick::new();
                        let bm_clone = bm.clone();
                        let connect_bookmark = connect_bookmark.clone();
                        dbl_click.connect_released(move |_, n_press, _, _| {
                            if n_press == 2 {
                                connect_bookmark(bm_clone.clone());
                            }
                        });
                        row.add_controller(dbl_click);
                    }

                    bookmark_list.append(&row);
                }
            }
        };

        rebuild_sidebar();
        if let Some(first) = bookmarks.borrow().first().cloned() {
            populate_form(&first);
            if let Some(first_row) = bookmark_list.row_at_index(0) {
                bookmark_list.select_row(Some(&first_row));
            }
        }

        // Clicking or activating a row in the bookmark sidebar
        {
            let bookmarks = bookmarks.clone();
            let populate_form = populate_form.clone();
            bookmark_list.connect_row_activated(move |_, row| {
                let idx = row.index();
                if idx >= 0 {
                    if let Some(bm) = bookmarks.borrow().get(idx as usize).cloned() {
                        populate_form(&bm);
                    }
                }
            });
        }

        // "+ New" bookmark button
        {
            let bookmark_list = bookmark_list.clone();
            let populate_form = populate_form.clone();
            let pass_row = pass_row.clone();
            let ssh_secret_row = ssh_secret_row.clone();
            new_bm_btn.connect_clicked(move |_| {
                bookmark_list.unselect_all();
                let fresh = ConnectionBookmark::default();
                populate_form(&fresh);
                pass_row.set_text("");
                ssh_secret_row.set_text("");
            });
        }

        // Save Bookmark button
        {
            let store = store.clone();
            let bookmarks = bookmarks.clone();
            let bookmark_list = bookmark_list.clone();
            let rebuild_sidebar = rebuild_sidebar.clone();
            let read_profile = read_profile.clone();
            let on_toast = on_toast.clone();
            save_btn.connect_clicked(move |_| {
                let profile = read_profile();
                let bm_id = profile.bookmark.id.clone();
                let db_pass = profile.db_password.clone();
                let ssh_sec = profile.ssh_password_or_passphrase.clone();
                match store.upsert(profile.bookmark.clone()) {
                    Ok(updated) => {
                        let sel_idx = updated.iter().position(|b| b.id == bm_id);
                        *bookmarks.borrow_mut() = updated;
                        rebuild_sidebar();
                        if let Some(idx) = sel_idx {
                            if let Some(row) = bookmark_list.row_at_index(idx as i32) {
                                bookmark_list.select_row(Some(&row));
                            }
                        }
                        gtk::glib::spawn_future_local(async move {
                            let _ = SecretStore::save_secrets(&bm_id, &db_pass, &ssh_sec).await;
                        });
                        on_toast(&format!("Saved bookmark '{}'", profile.bookmark.name));
                    }
                    Err(e) => {
                        on_toast(&format!("Failed to save bookmark: {}", e));
                    }
                }
            });
        }

        // Delete Bookmark button
        {
            let store = store.clone();
            let bookmarks = bookmarks.clone();
            let current_id = current_id.clone();
            let rebuild_sidebar = rebuild_sidebar.clone();
            let populate_form = populate_form.clone();
            let on_toast = on_toast.clone();
            delete_btn.connect_clicked(move |_| {
                let id_to_del = current_id.borrow().clone();
                if let Ok(updated) = store.delete(&id_to_del) {
                    *bookmarks.borrow_mut() = updated;
                    rebuild_sidebar();
                    gtk::glib::spawn_future_local(async move {
                        SecretStore::delete_secrets(&id_to_del).await;
                    });
                    if let Some(first) = bookmarks.borrow().first().cloned() {
                        populate_form(&first);
                    } else {
                        populate_form(&ConnectionBookmark::default());
                    }
                    on_toast("Bookmark deleted");
                }
            });
        }

        // Test Connection button
        {
            let cmd_tx = cmd_tx.clone();
            let read_profile = read_profile.clone();
            let status_spinner = status_spinner.clone();
            let status_label = status_label.clone();
            test_btn.connect_clicked(move |_| {
                let profile = read_profile();
                status_spinner.start();
                status_label.set_text("Testing connection...");
                let _ = cmd_tx.send_blocking(BackendCommand::TestConnection(profile));
            });
        }

        // Quick Connect button
        {
            let cmd_tx = cmd_tx.clone();
            let read_profile = read_profile.clone();
            let status_spinner = status_spinner.clone();
            let status_label = status_label.clone();
            quick_connect_btn.connect_clicked(move |_| {
                let profile = read_profile();
                status_spinner.start();
                status_label.set_text("Connecting...");
                let _ = cmd_tx.send_blocking(BackendCommand::Connect(profile));
            });
        }

        // Quick Connect via URL popover button
        {
            let cmd_tx = cmd_tx.clone();
            let url_entry = url_entry.clone();
            let url_popover = url_popover.clone();
            let status_spinner = status_spinner.clone();
            let status_label = status_label.clone();
            let on_toast = on_toast.clone();
            url_go_btn.connect_clicked(move |_| {
                let raw = url_entry.text();
                match ConnectionProfile::from_url(&raw) {
                    Ok(profile) => {
                        url_popover.popdown();
                        status_spinner.start();
                        status_label.set_text("Connecting via URL...");
                        let _ = cmd_tx.send_blocking(BackendCommand::Connect(profile));
                    }
                    Err(e) => {
                        on_toast(&format!("{:#}", e));
                    }
                }
            });
        }

        Self {
            root: split_view,
            status_spinner,
            status_label,
        }
    }

    pub fn set_busy(&self, busy: bool, message: &str) {
        if busy {
            self.status_spinner.start();
        } else {
            self.status_spinner.stop();
        }
        self.status_label.set_text(message);
    }
}

fn ssh_expander_expand(expander: &adw::ExpanderRow, expand: bool) {
    expander.set_expanded(expand);
}
