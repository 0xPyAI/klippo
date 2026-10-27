//! The configuration dialog, rebuilt page by page from a classic download
//! manager: two rows of tabs, an icon and title on every page, OK and Help.

use eframe::egui::{self, Context, RichText, Ui};

use super::classic_ui::{button, check_group, group_box, grid_list_view, list_view, name_combo, note, page_header, right_buttons, tab_page, tab_rows};
use super::{dialog_window, fixed_width};
use crate::gui::icons::PageIcon;
use crate::gui::state::{AppSettings, Category, DuplicateAction, LinksPanel, KlippoApp, PanelSettings, ServerLimit, SiteLogin};
use crate::gui::system::{self, SoundEvent};
use crate::native_host::{self, BrowserTargets};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum OptionsTab {
    General,
    FileTypes,
    SaveTo,
    Downloads,
    Connection,
    ProxySocks,
    SiteLogins,
    DialUpVpn,
    Sounds,
}

/// Small dialogs opened from the pages (the "Edit...", "New", "Add browser..." buttons).
#[derive(Debug, Clone)]
pub enum OptionsSub {
    ExtensionIds(String),
    Addresses(String),
    ProgressDialog,
    Virus,
    ServerLimit { index: Option<usize>, server: String, number: usize },
    CategoryTypes { cat: Category, text: String },
    ProxyAdvanced,
    Keys,
    BrowserMenu,
    Panels(Box<PanelEdit>),
}

/// "Customize download panels in browsers": a copy of the settings until OK.
#[derive(Debug, Clone)]
pub struct PanelEdit {
    /// 0: web players, 1: selected links.
    pub tab: u8,
    pub value: PanelSettings,
    pub site: Option<usize>,
    pub inner: Option<PanelInner>,
}

/// The small dialogs of the panels dialog.
#[derive(Debug, Clone)]
pub enum PanelInner {
    AddType(String),
    MinSize(String),
    Exceptions(String),
    AddSite(String),
}

const PAGE_HEIGHT: f32 = 403.0;
/// The pages are 268 x 258 dialog units: 375 x 413 at 125 %, inside the tab frame.
const WIDTH: f32 = 387.0;

pub fn render_options_modal(app: &mut KlippoApp, ctx: &Context) {
    if !app.show_options_dialog {
        return;
    }
    let mut is_open = true;
    let mut ok = false;
    dialog_window("Klippo Configuration", ctx, WIDTH)
        .id(egui::Id::new("options_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.set_max_width(WIDTH);
            tab_rows(
                ui,
                &[
                    &[(OptionsTab::ProxySocks, "Proxy / Socks"), (OptionsTab::SiteLogins, "Sites Logins"), (OptionsTab::DialUpVpn, "Dial Up / VPN"), (OptionsTab::Sounds, "Sounds")],
                    &[(OptionsTab::General, "General"), (OptionsTab::FileTypes, "File types"), (OptionsTab::SaveTo, "Save to"), (OptionsTab::Downloads, "Downloads"), (OptionsTab::Connection, "Connection")],
                ],
                &mut app.options_active_tab,
            );
            tab_page(ui, PAGE_HEIGHT, |ui| match app.options_active_tab {
                OptionsTab::General => general_tab(app, ui),
                OptionsTab::FileTypes => file_types_tab(app, ui),
                OptionsTab::SaveTo => save_to_tab(app, ui),
                OptionsTab::Downloads => downloads_tab(app, ui),
                OptionsTab::Connection => connection_tab(app, ui),
                OptionsTab::ProxySocks => proxy_tab(app, ui),
                OptionsTab::SiteLogins => site_logins_tab(app, ui),
                OptionsTab::DialUpVpn => dialup_tab(app, ui),
                OptionsTab::Sounds => sounds_tab(app, ui),
            });
            ui.add_space(6.4);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    if button(ui, true, "Help").clicked() {
                        if let Some(readme) = std::env::current_exe().ok().and_then(|exe| exe.ancestors().map(|a| a.join("README.md")).find(|p| p.exists())) {
                            let _ = open::that(readme);
                        }
                    }
                    if button(ui, true, "OK").clicked() {
                        ok = true;
                    }
                });
            });
        });

    if ok {
        app.settings.sound_alerts = true;
        app.save_settings();
        app.apply_theme(ctx);
        app.settings_backup = None;
        app.show_options_dialog = false;
        app.options_sub = None;
    } else if !is_open {
        // The close box leaves the settings as they were.
        app.cancel_options(ctx);
        app.options_sub = None;
    }

    render_site_login_submodal(app, ctx);
    render_sub_dialog(app, ctx);
}

fn selection(ui: &Ui, key: &str) -> Option<usize> {
    ui.data(|d| d.get_temp::<Option<usize>>(egui::Id::new(("options_sel", key)))).flatten()
}

fn set_selection(ui: &Ui, key: &str, value: Option<usize>) {
    ui.data_mut(|d| d.insert_temp(egui::Id::new(("options_sel", key)), value));
}

/// A label on the left and a button on the right, like the classic "Edit..." rows.
fn label_button(ui: &mut Ui, text: &str, button_text: &str, enabled: bool, tip: Option<&str>) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        right_buttons(ui, |ui| {
            let r = button(ui, enabled, button_text);
            let r = match tip {
                Some(t) => r.on_disabled_hover_text(t).on_hover_text(t),
                None => r,
            };
            clicked = r.clicked();
            ui.label(text);
        });
    });
    clicked
}

fn general_tab(app: &mut KlippoApp, ui: &mut Ui) {
    page_header(ui, PageIcon::General, "Browser/System Integration", false);
    let installed = native_host::is_registered();
    let status = if installed { "Advanced browser integration is enabled" } else { "Browser integration is not installed" };
    if label_button(ui, status, if installed { "Restart" } else { "Install" }, true, Some("Registers Klippo with the selected browsers again")) {
        install_integration(app);
    }
    ui.add_space(6.0);
    ui.checkbox(&mut app.settings.launch_on_startup, "Launch Klippo on startup");
    ui.checkbox(&mut app.settings.monitor_clipboard, "Automatically start downloading of URLs placed to clipboard");

    let mut add_browser = false;
    let mut sub = None;
    group_box(ui, "Capture downloads from the following browsers:", |ui| {
        let s = &mut app.settings;
        let list: [(&mut bool, &str); 6] = [
            (&mut s.browser_brave, "Brave Browser"),
            (&mut s.browser_chrome, "Google Chrome"),
            (&mut s.browser_edge, "Microsoft Edge"),
            (&mut s.browser_firefox, "Mozilla Firefox"),
            (&mut s.browser_opera, "Opera"),
            (&mut s.browser_vivaldi, "Vivaldi"),
        ];
        let theme = crate::gui::theme::current(ui.ctx());
        egui::Frame::none().fill(theme.list_bg).stroke(egui::Stroke::new(1.0_f32, theme.border_color)).inner_margin(egui::Margin::same(4.0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(118.0).auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (value, name) in list {
                    ui.checkbox(value, name);
                }
            });
        });
        ui.horizontal(|ui| {
            right_buttons(ui, |ui| {
                add_browser = button(ui, true, "Add browser...").on_hover_text("Extension IDs of the Klippo browser extension").clicked();
            });
        });
        ui.separator();
        if label_button(ui, "Customize keys to prevent or force downloading with Klippo", "Keys...", true, None) {
            sub = Some(OptionsSub::Keys);
        }
    });
    if add_browser {
        app.options_sub = Some(OptionsSub::ExtensionIds(app.settings.extension_ids.join("\n")));
    }
    if sub.is_some() {
        app.options_sub = sub;
    }
    ui.add_space(4.0);
    if label_button(ui, "Customize Klippo menu items in context menu of browsers", "Edit...", true, None) {
        app.options_sub = Some(OptionsSub::BrowserMenu);
    }
    if label_button(ui, "Customize download panels in browsers", "Edit...", true, None) {
        app.options_sub = Some(OptionsSub::Panels(Box::new(PanelEdit { tab: 0, value: app.settings.panel.clone(), site: None, inner: None })));
    }
}

fn install_integration(app: &mut KlippoApp) {
    let targets = BrowserTargets {
        chromium: app.settings.browser_chrome || app.settings.browser_opera || app.settings.browser_vivaldi,
        edge: app.settings.browser_edge,
        brave: app.settings.browser_brave,
        firefox: app.settings.browser_firefox,
    };
    match native_host::register_native_host_for(&app.settings.extension_ids, targets) {
        Ok(report) => app.set_status(report),
        Err(e) => app.set_status(format!("Browser integration failed: {}", e)),
    }
}

fn file_types_tab(app: &mut KlippoApp, ui: &mut Ui) {
    page_header(ui, PageIcon::FileTypes, "Downloaded file types", true);
    let defaults = AppSettings::default();
    ui.label("Automatically start downloading the following file types:");
    ui.add(egui::TextEdit::multiline(&mut app.settings.file_types_extensions).desired_rows(4).desired_width(f32::INFINITY));
    ui.horizontal(|ui| {
        right_buttons(ui, |ui| {
            if button(ui, true, "Default").clicked() {
                app.settings.file_types_extensions = defaults.file_types_extensions.clone();
            }
        });
    });
    ui.checkbox(&mut app.settings.capture_all_types, "Take browser downloads of all file types (web pages stay in the browser)")
        .on_hover_text("Also files that the page makes (blob: and data: addresses) and downloads from forms. The list above is still used for links copied to the clipboard.");
    ui.checkbox(&mut app.settings.capture_fetched_files, "Also take files that a web page downloads itself (for file hosts like gofile)")
        .on_hover_text("Some sites fetch the file in the page and save it without a normal browser download. Turn this on to send those to Klippo. Leave it off if a web app, such as webmail, starts downloads you did not ask for.");
    ui.add_space(8.0);
    ui.label("Don't start downloading automatically from the following sites:");
    ui.add(egui::TextEdit::multiline(&mut app.settings.exceptions_servers).desired_rows(3).desired_width(f32::INFINITY));
    ui.horizontal(|ui| {
        ui.label("(separate names by spaces)");
        right_buttons(ui, |ui| {
            if button(ui, true, "Default").clicked() {
                app.settings.exceptions_servers = defaults.exceptions_servers.clone();
            }
        });
    });
    ui.add_space(8.0);
    ui.label("Don't start downloading automatically from the following addresses:");
    if button(ui, true, "Edit list ...").clicked() {
        app.options_sub = Some(OptionsSub::Addresses(app.settings.exception_addresses.join("\n")));
    }
}

fn save_to_tab(app: &mut KlippoApp, ui: &mut Ui) {
    page_header(ui, PageIcon::SaveTo, "Categories, file types, folders", true);
    let cat = app.options_save_to_category;
    let mut edit_types = false;
    let (mut new_category, mut delete_category) = (false, false);
    group_box(ui, "Save To...", |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label("Category");
                let combo = egui::ComboBox::from_id_salt("save_to_cat").width(270.0).selected_text(cat.name()).show_ui(ui, |ui| {
                    for c in Category::all() {
                        ui.selectable_value(&mut app.options_save_to_category, c, c.name());
                    }
                });
                name_combo(&combo, "Category", &app.options_save_to_category.name());
            });
            right_buttons(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    new_category = button(ui, true, "New").on_hover_text("Add a category with its own file types and folder").clicked();
                    delete_category = button(ui, cat.is_user(), "Delete").on_disabled_hover_text("The fixed categories cannot be deleted").clicked();
                    edit_types = button(ui, cat != Category::General, "Edit...").clicked();
                });
            });
        });
        ui.add_space(4.0);
        ui.label(format!("Automatically put in \"{}\" category the following file types:", cat.name()));
        let mut types = if cat == Category::General {
            "The file types that are not listed in any other category".to_string()
        } else {
            app.settings.category_types.get(&cat).cloned().unwrap_or_default()
        };
        ui.add_enabled(false, egui::TextEdit::singleline(&mut types).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY));
        ui.add_space(4.0);
        ui.label(format!("Default download directory for \"{}\" category", cat.name()));
        ui.horizontal(|ui| {
            let current = app.settings.category_dir(cat);
            let mut path = current.to_string_lossy().to_string();
            if fixed_width(ui, ui.available_width() - 90.0, |ui| ui.add(egui::TextEdit::singleline(&mut path).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY))).changed() {
                app.settings.category_dirs.insert(cat, path.into());
            }
            if button(ui, true, "Browse").clicked() {
                if let Some(folder) = rfd::FileDialog::new().set_directory(&current).pick_folder() {
                    app.settings.category_dirs.insert(cat, folder);
                }
            }
        });
        ui.checkbox(&mut app.settings.remember_last_path, format!("Change folder for \"{}\" category on last selected", cat.name()));
    });
    if edit_types {
        let text = app.settings.category_types.get(&cat).cloned().unwrap_or_default();
        app.options_sub = Some(OptionsSub::CategoryTypes { cat, text });
    }
    if new_category {
        app.open_new_category(crate::gui::views::category_dialog::CategoryFrom::Options);
    }
    if delete_category {
        let name = cat.name();
        app.delete_category(cat);
        app.set_status(format!("Deleted the category \"{}\"", name));
    }
    ui.add_space(4.0);
    ui.checkbox(&mut app.settings.set_file_time_from_server, "Set file creation date as provided by the server");
    group_box(ui, "Temporary directory", |ui| {
        ui.horizontal(|ui| {
            let mut temp = app.settings.temp_dir.to_string_lossy().to_string();
            if fixed_width(ui, ui.available_width() - 90.0, |ui| ui.add(egui::TextEdit::singleline(&mut temp).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY))).changed() {
                app.settings.temp_dir = temp.into();
            }
            if button(ui, true, "Browse").clicked() {
                if let Some(folder) = rfd::FileDialog::new().set_directory(&app.settings.temp_dir).pick_folder() {
                    app.settings.temp_dir = folder;
                }
            }
        });
        note(ui, "Temporary directory is required for storing the parts of video streams during download. If you have several physical drives on your computer, you should select different physical drives for temporary directory and \"Save To\" folders for faster assembling of downloaded files.");
    });
}

fn downloads_tab(app: &mut KlippoApp, ui: &mut Ui) {
    page_header(ui, PageIcon::Downloads, "Default download settings", true);
    if label_button(ui, "Customize \"Download progress\" dialog", "Edit...", true, None) {
        app.options_sub = Some(OptionsSub::ProgressDialog);
    }
    let mut virus = false;
    {
        let s = &mut app.settings;
        ui.checkbox(&mut s.show_start_dialog, "Show start download dialog");
        ui.add_enabled_ui(s.show_start_dialog, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.checkbox(&mut s.captured_to_queue, "Do not start downloading, only add files to the queue");
            });
        });
        ui.checkbox(&mut s.show_complete_window, "Show download complete dialog");
        ui.label("Note: These settings don't relate to queue processing");
        ui.separator();
        ui.checkbox(&mut s.start_while_dialog, "Start downloading immediately while displaying \"Download File Info\" dialog")
            .on_hover_text("Files from the browser or the clipboard start at once; Cancel removes them");
        ui.checkbox(&mut s.queue_panel_on_later, "Show queue selection panel on pressing \"Download Later\" button");
        ui.checkbox(&mut s.queue_panel_on_batch, "Show queue selection panel on closing batch download dialogs");
        ui.checkbox(&mut s.ignore_time_changes, "Ignore file modification time changes when resuming a download");
        ui.separator();
        ui.label("If a duplicate download link is added:");
        let combo = egui::ComboBox::from_id_salt("dup_action").width(ui.available_width() - 8.0).selected_text(s.duplicate_action.label()).show_ui(ui, |ui| {
            for action in [DuplicateAction::Ask, DuplicateAction::Numbered, DuplicateAction::Overwrite, DuplicateAction::ResumeExisting] {
                ui.selectable_value(&mut s.duplicate_action, action, action.label());
            }
        });
        name_combo(&combo, "If a duplicate download link is added", s.duplicate_action.label());
        ui.add_space(4.0);
        ui.label("User-Agent for manually added downloads:");
        fixed_width(ui, f32::INFINITY, |ui| {
            ui.add(egui::TextEdit::singleline(&mut s.manual_user_agent).margin(crate::gui::theme::EDIT_MARGIN).hint_text(crate::core::http::DEFAULT_USER_AGENT).desired_width(f32::INFINITY))
        });
        ui.separator();
        if label_button(ui, "Virus checking settings", "Edit...", true, None) {
            virus = true;
        }
    }
    if virus {
        app.options_sub = Some(OptionsSub::Virus);
    }
}

fn connection_tab(app: &mut KlippoApp, ui: &mut Ui) {
    page_header(ui, PageIcon::Connection, "Connections and Limits", true);
    let mut sub = None;
    group_box(ui, "Max. connections number", |ui| {
        ui.horizontal(|ui| {
            right_buttons(ui, |ui| {
                let combo = egui::ComboBox::from_id_salt("max_conns").width(130.0).selected_text(app.settings.max_connections.to_string()).show_ui(ui, |ui| {
                    for c in [1, 2, 4, 8, 16, 24, 32] {
                        ui.selectable_value(&mut app.settings.max_connections, c, c.to_string());
                    }
                });
                name_combo(&combo, "Default max. conn. number", &app.settings.max_connections.to_string());
                ui.label("Default max. conn. number");
            });
        });
        ui.separator();
        ui.label("Exceptions:");
        let mut sel = selection(ui, "server_limits");
        ui.horizontal(|ui| {
            let limits = app.settings.server_limits.clone();
            ui.allocate_ui(egui::vec2(ui.available_width() - 100.0, 150.0), |ui| {
                let double = grid_list_view(ui, "server_limits", &[("Server", 230.0), ("Number", 60.0)], limits.len(), 118.0, &mut sel, |ui, row, col| {
                    if col == 0 {
                        ui.label(&limits[row].server);
                    } else {
                        ui.label(limits[row].max_connections.to_string());
                    }
                });
                if let Some(i) = double {
                    sub = Some(OptionsSub::ServerLimit { index: Some(i), server: limits[i].server.clone(), number: limits[i].max_connections });
                }
            });
            ui.vertical(|ui| {
                if button(ui, true, "New").clicked() {
                    sub = Some(OptionsSub::ServerLimit { index: None, server: String::new(), number: 2 });
                }
                if button(ui, sel.is_some(), "Delete").clicked() {
                    if let Some(i) = sel.take() {
                        if i < app.settings.server_limits.len() {
                            app.settings.server_limits.remove(i);
                        }
                    }
                }
                if button(ui, sel.is_some(), "Edit").clicked() {
                    if let Some(l) = sel.and_then(|i| app.settings.server_limits.get(i)) {
                        sub = Some(OptionsSub::ServerLimit { index: sel, server: l.server.clone(), number: l.max_connections });
                    }
                }
            });
        });
        set_selection(ui, "server_limits", sel);
    });
    if sub.is_some() {
        app.options_sub = sub;
    }
    let s = &mut app.settings;
    check_group(ui, &mut s.download_limit_enabled, "Download limits", |ui| {
        ui.horizontal(|ui| {
            ui.add_space(40.0);
            ui.label("Download no more than");
            ui.add(egui::DragValue::new(&mut s.download_limit_mb).range(1..=10_000_000));
            ui.label("MBytes");
        });
        ui.horizontal(|ui| {
            ui.add_space(118.0);
            ui.label("every");
            ui.add(egui::DragValue::new(&mut s.download_limit_hours).range(1..=720));
            ui.label("hours");
        });
        ui.checkbox(&mut s.download_limit_warn, "Show warning before stopping downloads");
    });
}

/// The proxy mode as the radio buttons show it.
#[derive(PartialEq, Clone, Copy)]
enum ProxyMode {
    None,
    System,
    Script,
    Manual,
}

fn proxy_tab(app: &mut KlippoApp, ui: &mut Ui) {
    let mut get_system = false;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
        crate::gui::icons::page(ui.painter(), rect, PageIcon::Proxy);
        ui.add_space(60.0);
        ui.label(RichText::new("Proxy / socks configuration").size(crate::gui::theme::TITLE_FONT));
        right_buttons(ui, |ui| {
            get_system = button(ui, true, "Get System").on_hover_text("Copy the Windows proxy into the manual settings").clicked();
        });
    });
    if get_system {
        match system::windows_proxy() {
            Some((server, bypass)) => {
                if let Some(cfg) = crate::gui::state::system_proxy_config(&server, &bypass) {
                    if let Some(url) = cfg.http.or(cfg.https).and_then(|u| url::Url::parse(&u).ok()) {
                        app.settings.proxy_http_host = url.host_str().unwrap_or_default().to_string();
                        app.settings.proxy_http_port = url.port_or_known_default().unwrap_or(8080);
                        app.settings.proxy_http_enabled = true;
                        app.settings.proxy_use_system = false;
                    }
                    app.settings.proxy_exceptions = bypass;
                }
            }
            None => app.set_status("Windows has no proxy set"),
        }
    }
    let s = &mut app.settings;
    ui.checkbox(&mut s.browser_proxy_fallback, "Use proxy/socks from a browser's request if there are download errors for the downloads intercepted from the browser");
    ui.separator();
    let mut mode = if s.proxy_pac_enabled {
        ProxyMode::Script
    } else if s.proxy_use_system {
        ProxyMode::System
    } else if s.proxy_http_enabled || s.proxy_socks_enabled {
        ProxyMode::Manual
    } else {
        ProxyMode::None
    };
    ui.radio_value(&mut mode, ProxyMode::None, "No proxy/socks");
    ui.separator();
    ui.radio_value(&mut mode, ProxyMode::System, "Use system settings");
    ui.separator();
    ui.radio_value(&mut mode, ProxyMode::Script, "Use automatic configuration script")
        .on_hover_text("A PAC script (.pac); Windows works out the proxy for every address");
    ui.add_enabled_ui(mode == ProxyMode::Script, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label("Address");
            ui.add(egui::TextEdit::singleline(&mut s.proxy_pac_url).margin(crate::gui::theme::EDIT_MARGIN).hint_text("http://wpad/wpad.dat").desired_width(f32::INFINITY));
        });
    });
    ui.separator();
    ui.radio_value(&mut mode, ProxyMode::Manual, "Manual proxy/socks configuration");
    match mode {
        ProxyMode::None => {
            s.proxy_use_system = false;
            s.proxy_pac_enabled = false;
            s.proxy_http_enabled = false;
            s.proxy_socks_enabled = false;
        }
        ProxyMode::System => {
            s.proxy_use_system = true;
            s.proxy_pac_enabled = false;
        }
        ProxyMode::Script => {
            s.proxy_use_system = false;
            s.proxy_pac_enabled = true;
        }
        ProxyMode::Manual => {
            s.proxy_use_system = false;
            s.proxy_pac_enabled = false;
            if !s.proxy_socks_enabled {
                s.proxy_http_enabled = true;
            }
        }
    }
    let mut advanced = false;
    ui.add_enabled_ui(mode == ProxyMode::Manual, |ui| {
        egui::Grid::new("proxy_grid").num_columns(4).spacing([6.0, 2.0]).show(ui, |ui| {
            ui.label("Proxy server address");
            ui.label("Port");
            ui.label("UserName");
            ui.label("Password");
            ui.end_row();
            fixed_width(ui, 127.0, |ui| ui.add(egui::TextEdit::singleline(&mut s.proxy_http_host).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
            ui.add(egui::DragValue::new(&mut s.proxy_http_port).range(1..=65535));
            fixed_width(ui, 73.0, |ui| ui.add(egui::TextEdit::singleline(&mut s.proxy_http_user).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
            fixed_width(ui, 77.0, |ui| ui.add(egui::TextEdit::singleline(&mut s.proxy_http_pass).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(f32::INFINITY)));
            ui.end_row();
        });
        ui.label("Use this proxy for the following protocols:");
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.proxy_protocol_http, "http");
            ui.checkbox(&mut s.proxy_protocol_https, "https");
            ui.checkbox(&mut s.proxy_protocol_ftp, "ftp").on_hover_text("FTP goes through the SOCKS5 proxy (Advanced / Socks...)");
            right_buttons(ui, |ui| {
                advanced = button(ui, true, "Advanced / Socks...").clicked();
            });
        });
    });
    if advanced {
        app.options_sub = Some(OptionsSub::ProxyAdvanced);
    }
    ui.add_space((ui.available_height() - 30.0).max(0.0));
    ui.separator();
    ui.checkbox(&mut app.settings.ftp_pasv, "Use FTP in PASV mode");
}

fn site_logins_tab(app: &mut KlippoApp, ui: &mut Ui) {
    page_header(ui, PageIcon::SiteLogins, "User names and passwords for servers/sites", true);
    let mut sel = selection(ui, "site_logins");
    let logins = app.settings.site_logins.clone();
    let double = list_view(ui, "site_logins", &[("Site/path", 260.0), ("User", 80.0), ("Password", 80.0)], logins.len(), 273.0, &mut sel, |ui, row, col| {
        match col {
            0 => ui.label(&logins[row].url),
            1 => ui.label(&logins[row].username),
            _ => ui.label("••••••"),
        };
    });
    let edit = |app: &mut KlippoApp, i: usize| {
        let login = app.settings.site_logins[i].clone();
        app.site_login_url = login.url;
        app.site_login_user = login.username;
        app.site_login_pass = login.password;
        app.site_login_edit_idx = Some(i);
        app.show_site_login_modal = true;
    };
    if let Some(i) = double {
        edit(app, i);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        if button(ui, true, "New").clicked() {
            app.site_login_url.clear();
            app.site_login_user.clear();
            app.site_login_pass.clear();
            app.site_login_edit_idx = None;
            app.show_site_login_modal = true;
        }
        ui.add_space(20.0);
        if button(ui, sel.is_some(), "Edit").clicked() {
            if let Some(i) = sel {
                edit(app, i);
            }
        }
        ui.add_space(20.0);
        if button(ui, sel.is_some(), "Remove").clicked() {
            if let Some(i) = sel.take() {
                if i < app.settings.site_logins.len() {
                    app.settings.site_logins.remove(i);
                }
            }
        }
    });
    set_selection(ui, "site_logins", sel);
}

fn dialup_tab(app: &mut KlippoApp, ui: &mut Ui) {
    page_header(ui, PageIcon::DialUp, "Dial up / VPN settings", true);
    let s = &mut app.settings;
    ui.checkbox(&mut s.dialup_enabled, "Use Windows Dial Up / VPN Networking")
        .on_hover_text("Connects the chosen entry (rasdial) before downloads start");
    let entries_id = egui::Id::new("dialup_entries");
    let entries: Vec<String> = ui.data_mut(|d| d.get_temp_mut_or_insert_with(entries_id, system::dialup_entries).clone());
    let mut apply = false;
    group_box(ui, "Connection options", |ui| {
        ui.add_enabled_ui(s.dialup_enabled, |ui| {
            egui::Grid::new("dialup_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Connection:");
                let shown = if s.dialup_entry.is_empty() { "(choose a connection)".to_string() } else { s.dialup_entry.clone() };
                let combo = egui::ComboBox::from_id_salt("dialup_conn").width(240.0).selected_text(shown.clone()).show_ui(ui, |ui| {
                    if entries.is_empty() {
                        ui.label("No dial-up or VPN connections in Windows");
                    }
                    for e in &entries {
                        ui.selectable_value(&mut s.dialup_entry, e.clone(), e);
                    }
                });
                name_combo(&combo, "Connection", &shown);
                ui.end_row();
                ui.label("User name:");
                ui.add(egui::TextEdit::singleline(&mut s.dialup_user).margin(crate::gui::theme::EDIT_MARGIN).desired_width(200.0));
                ui.end_row();
                ui.label("Password:");
                ui.add(egui::TextEdit::singleline(&mut s.dialup_pass).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(200.0));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.dialup_save_pass, "Save password");
                ui.end_row();
            });
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    apply = button(ui, !s.dialup_entry.is_empty(), "Apply").on_hover_text("Connect now").clicked();
                })
            });
        });
    });
    if apply {
        system::ensure_dialup(s);
    }
    group_box(ui, "Redial options", |ui| {
        egui::Grid::new("redial_grid").num_columns(3).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Redial attempts (zero if endlessly):");
            ui.add(egui::DragValue::new(&mut app.settings.redial_attempts).range(0..=100));
            ui.label("times");
            ui.end_row();
            ui.label("Time between redial attempts:");
            ui.add(egui::DragValue::new(&mut app.settings.redial_wait_seconds).range(1..=600));
            ui.label("seconds");
            ui.end_row();
        });
    });
}

fn sounds_tab(app: &mut KlippoApp, ui: &mut Ui) {
    page_header(ui, PageIcon::Sounds, "Sound settings", true);
    let mut sel = selection(ui, "sounds");
    group_box(ui, "Select sounds for Klippo events", |ui| {
        let s = &mut app.settings;
        let paths = [s.sound_complete_path.clone(), s.sound_failed_path.clone(), s.sound_queue_start_path.clone(), s.sound_queue_done_path.clone()];
        let mut flags = [s.sound_complete, s.sound_failed, s.sound_queue_start, s.sound_queue_done];
        let names = ["Download complete", "Download failed", "Queue processing started", "Queue processing stopped/finished"];
        grid_list_view(ui, "sounds", &[("Event", 230.0), ("Sound file", 160.0)], 4, 244.0, &mut sel, |ui, row, col| {
            if col == 0 {
                ui.checkbox(&mut flags[row], names[row]);
            } else {
                let name = std::path::Path::new(&paths[row]).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                ui.label(name);
            }
        });
        [s.sound_complete, s.sound_failed, s.sound_queue_start, s.sound_queue_done] = flags;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(90.0);
            if button(ui, sel.is_some(), "Browse...").clicked() {
                if let Some(file) = rfd::FileDialog::new().add_filter("Wave sound", &["wav"]).pick_file() {
                    let path = file.to_string_lossy().to_string();
                    match sel {
                        Some(0) => s.sound_complete_path = path,
                        Some(1) => s.sound_failed_path = path,
                        Some(2) => s.sound_queue_start_path = path,
                        Some(3) => s.sound_queue_done_path = path,
                        _ => {}
                    }
                }
            }
            ui.add_space(70.0);
            if button(ui, sel.is_some(), "Play").clicked() {
                match sel {
                    Some(0) => system::play_sound(SoundEvent::DownloadComplete, &s.sound_complete_path),
                    Some(1) => system::play_sound(SoundEvent::DownloadFailed, &s.sound_failed_path),
                    Some(2) => system::play_sound(SoundEvent::QueueStarted, &s.sound_queue_start_path),
                    Some(3) => system::play_sound(SoundEvent::QueueComplete, &s.sound_queue_done_path),
                    _ => {}
                }
            }
        });
    });
    set_selection(ui, "sounds", sel);
}

fn render_sub_dialog(app: &mut KlippoApp, ctx: &Context) {
    let Some(mut sub) = app.options_sub.take() else { return };
    let title = match sub {
        OptionsSub::ExtensionIds(_) => "Add browser",
        OptionsSub::Addresses(_) => "The list of address exceptions",
        OptionsSub::ProgressDialog => "Customize \"Download progress\" dialog",
        OptionsSub::Virus => "Virus checking",
        OptionsSub::ServerLimit { .. } => "Max. connections number for a server",
        OptionsSub::CategoryTypes { .. } => "Edit category",
        OptionsSub::ProxyAdvanced => "Advanced proxy settings",
        OptionsSub::Keys => "Using special keys",
        OptionsSub::BrowserMenu => "Context menu of browsers",
        OptionsSub::Panels(_) => "Customize download panels",
    };
    let width = match sub {
        OptionsSub::Panels(_) => 515.0,
        OptionsSub::Addresses(_) => 377.0,
        OptionsSub::ProgressDialog | OptionsSub::ProxyAdvanced => 359.0,
        OptionsSub::ServerLimit { .. } => 293.0,
        _ => 420.0,
    };
    let mut open = true;
    let mut done = false;
    let mut cancel = false;
    dialog_window(title, ctx, width)
        .id(egui::Id::new("options_sub_dialog"))
        .open(&mut open)
        .resizable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_width(width);
            let s = &mut app.settings;
            match &mut sub {
                OptionsSub::Panels(edit) => panels_dialog(ui, edit),
                OptionsSub::ExtensionIds(text) => {
                    ui.label("IDs of the Klippo extension in your browsers (one per line):");
                    ui.add(egui::TextEdit::multiline(text).desired_rows(4).desired_width(f32::INFINITY));
                    note(ui, "Load the \"extension\" folder as an unpacked extension (chrome://extensions, Developer mode). The bundled extension ID is filled in for you.");
                }
                OptionsSub::Addresses(text) => {
                    ui.label("Don't start downloading automatically from these addresses (one per line, * is a wildcard):");
                    ui.add(egui::TextEdit::multiline(text).desired_rows(8).desired_width(f32::INFINITY));
                }
                OptionsSub::ProgressDialog => {
                    ui.checkbox(&mut s.show_progress_window, "Show download progress dialog");
                    ui.checkbox(&mut s.auto_close_progress, "Close the dialog when the download is complete");
                    ui.checkbox(&mut s.open_file_on_completion, "Open the file when the download is complete");
                }
                OptionsSub::Virus => {
                    ui.checkbox(&mut s.antivirus_scan, "Scan files with an antivirus program after the download");
                    ui.add_enabled_ui(s.antivirus_scan, |ui| {
                        ui.label("Antivirus program:");
                        ui.horizontal(|ui| {
                            fixed_width(ui, 320.0, |ui| ui.add(egui::TextEdit::singleline(&mut s.antivirus_path).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                            if button(ui, true, "Browse...").clicked() {
                                if let Some(file) = rfd::FileDialog::new().add_filter("Programs", &["exe", "bat", "cmd"]).pick_file() {
                                    s.antivirus_path = file.to_string_lossy().to_string();
                                }
                            }
                        });
                        ui.label("Command line parameters:");
                        ui.add(egui::TextEdit::singleline(&mut s.antivirus_args).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY));
                        note(ui, "\"%1\" is replaced with the path of the downloaded file.");
                    });
                }
                OptionsSub::ServerLimit { server, number, .. } => {
                    ui.label("Server (* is a wildcard, e.g. *.example.com):");
                    ui.add(egui::TextEdit::singleline(server).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        ui.label("Max. connections number:");
                        ui.add(egui::DragValue::new(number).range(1..=32));
                    });
                }
                OptionsSub::CategoryTypes { cat, text } => {
                    ui.label(format!("Automatically put in \"{}\" category the following file types:", cat.name()));
                    ui.add(egui::TextEdit::multiline(text).desired_rows(4).desired_width(f32::INFINITY));
                    if button(ui, true, "Default").clicked() {
                        *text = cat.default_types().to_string();
                    }
                }
                OptionsSub::Keys => {
                    ui.checkbox(&mut s.keys_prevent_enabled, "Use the following key(s) to prevent downloading with Klippo for any links:");
                    ui.add_enabled_ui(s.keys_prevent_enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            for (i, name) in ["Alt", "Shift", "Ctrl"].iter().enumerate() {
                                ui.checkbox(&mut s.keys_prevent[i], *name);
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.checkbox(&mut s.keys_force_enabled, "Use the following key(s) to force downloading with Klippo for any links:");
                    ui.add_enabled_ui(s.keys_force_enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            for (i, name) in ["Alt", "Shift", "Ctrl", "Ins"].iter().enumerate() {
                                ui.checkbox(&mut s.keys_force[i], *name);
                            }
                        });
                    });
                    ui.add_space(6.0);
                    note(ui, "Some sites open a page that starts the download after a few seconds. To force such a download to Klippo, clear the next check box.");
                    ui.checkbox(&mut s.keys_check_click, "Check for left mouse button clicked on a link along with the special key(s) pressed to force downloading the link");
                    ui.add_space(4.0);
                    ui.label("Note 2:");
                    ui.checkbox(&mut s.keys_skip_web, "While holding down a special key DO NOT take over the downloads which are web-pages, pictures, scripts and etc.");
                }
                OptionsSub::BrowserMenu => {
                    ui.label("Selected menu items will appear in browser context menu:");
                    let names = [
                        "Download with Klippo",
                        "Download last requested video with Klippo",
                        "Download videos with Klippo from 10 last requested",
                        "Download all links with Klippo",
                    ];
                    for (i, name) in names.iter().enumerate() {
                        ui.checkbox(&mut s.browser_menu[i], *name);
                    }
                    note(ui, "The Klippo browser extension reads these settings within a minute.");
                }
                OptionsSub::ProxyAdvanced => {
                    ui.checkbox(&mut s.proxy_https_enabled, "Use a different proxy server for https");
                    ui.add_enabled_ui(s.proxy_https_enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Address");
                            fixed_width(ui, 200.0, |ui| ui.add(egui::TextEdit::singleline(&mut s.proxy_https_host).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                            ui.label("Port");
                            ui.add(egui::DragValue::new(&mut s.proxy_https_port).range(1..=65535));
                        });
                    });
                    ui.add_space(6.0);
                    ui.label("Do not use proxy server for addresses beginning with:");
                    ui.add(egui::TextEdit::singleline(&mut s.proxy_exceptions).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY));
                    note(ui, "Separate names by ; (<local> means names without a dot)");
                    ui.add_space(6.0);
                    ui.checkbox(&mut s.proxy_socks_enabled, "Use socks (SOCKS5) instead of the http proxy");
                    ui.add_enabled_ui(s.proxy_socks_enabled, |ui| {
                        egui::Grid::new("socks_grid").num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
                            ui.label("Socks address");
                            fixed_width(ui, 125.0, |ui| ui.add(egui::TextEdit::singleline(&mut s.proxy_socks_host).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                            ui.label("Port");
                            ui.add(egui::DragValue::new(&mut s.proxy_socks_port).range(1..=65535));
                            ui.end_row();
                            ui.label("UserName");
                            fixed_width(ui, 125.0, |ui| ui.add(egui::TextEdit::singleline(&mut s.proxy_socks_user).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                            ui.label("Password");
                            fixed_width(ui, 80.0, |ui| ui.add(egui::TextEdit::singleline(&mut s.proxy_socks_pass).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(f32::INFINITY)));
                            ui.end_row();
                        });
                        note(ui, "Host names are resolved by the socks server (socks5h).");
                    });
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    if button(ui, true, "Cancel").clicked() {
                        cancel = true;
                    }
                    if button(ui, true, "OK").clicked() {
                        done = true;
                    }
                });
            });
        });
    if done {
        let s = &mut app.settings;
        match sub {
            OptionsSub::ExtensionIds(text) => {
                s.extension_ids = text.split([',', ' ', '\n', '\r']).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
            }
            OptionsSub::Addresses(text) => {
                s.exception_addresses = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            }
            OptionsSub::ServerLimit { index, server, number } => {
                let limit = ServerLimit { server: server.trim().to_string(), max_connections: number };
                if !limit.server.is_empty() {
                    match index {
                        Some(i) if i < s.server_limits.len() => s.server_limits[i] = limit,
                        _ => s.server_limits.push(limit),
                    }
                }
            }
            OptionsSub::CategoryTypes { cat, text } => {
                s.category_types.insert(cat, text.split_whitespace().map(|t| t.to_uppercase()).collect::<Vec<_>>().join(" "));
            }
            OptionsSub::Panels(edit) => s.panel = edit.value,
            OptionsSub::ProgressDialog | OptionsSub::Virus | OptionsSub::ProxyAdvanced | OptionsSub::Keys | OptionsSub::BrowserMenu => {}
        }
    } else if open && !cancel {
        if let OptionsSub::Panels(edit) = &mut sub {
            render_panel_inner(edit, ctx);
        }
        app.options_sub = Some(sub);
    }
}

/// Two pages: the panel on web players and the panel for selected links.
fn panels_dialog(ui: &mut Ui, edit: &mut PanelEdit) {
    tab_rows(ui, &[&[(0u8, "Web-players"), (1u8, "Selected links")]], &mut edit.tab);
    tab_page(ui, 392.0, |ui| {
        if edit.tab == 0 {
            web_player_panel(ui, edit);
        } else {
            links_panel(ui, edit);
        }
    });
}

fn web_player_panel(ui: &mut Ui, edit: &mut PanelEdit) {
    let theme = crate::gui::theme::current(ui.ctx());
    let PanelEdit { value: v, inner, .. } = edit;
    ui.label("When a page plays video or audio, Klippo can put a download panel on the player.");
    ui.separator();
    ui.label("Panel view:");
    panel_view(ui, &mut v.mini, "Download this video");
    ui.separator();
    ui.label("Show the download panel for these file types:");
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        egui::Frame::none().fill(theme.list_bg).stroke(egui::Stroke::new(1.0_f32, theme.border_color)).inner_margin(egui::Margin::same(2.0)).show(ui, |ui| {
            ui.set_width(233.0);
            ui.set_height(137.0);
            egui::ScrollArea::vertical().id_salt("panel_types").auto_shrink([false, false]).show(ui, |ui| {
                // One type per line (the row around the list is horizontal).
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for (name, on) in v.types.iter_mut() {
                        ui.checkbox(on, name.as_str());
                    }
                });
            });
        });
        ui.add_space(20.0);
        ui.vertical(|ui| {
            let wide = |ui: &mut Ui, text: &str| ui.add_sized([158.0, 22.4], egui::Button::new(text)).clicked();
            if wide(ui, "Add") {
                *inner = Some(PanelInner::AddType(String::new()));
            }
            if wide(ui, "Minimum size") {
                *inner = Some(PanelInner::MinSize(v.min_size_kb.to_string()));
            }
            if wide(ui, "Check All") {
                v.types.iter_mut().for_each(|t| t.1 = true);
            }
            if wide(ui, "Clear All") {
                v.types.iter_mut().for_each(|t| t.1 = false);
            }
            if wide(ui, "Exceptions") {
                *inner = Some(PanelInner::Exceptions(v.exceptions.clone()));
            }
        });
    });
    note(ui, &format!(
        "Note: the panel shows for files of the checked types that are larger than {} KB, and for streams (M3U8, MPD) while they play.",
        v.min_size_kb
    ));
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        ui.checkbox(&mut v.no_auto_capture, "Don't capture downloads from web-players automatically");
    });
    ui.separator();
    ui.checkbox(&mut v.protected, "Show the download panel also for protected content, which may not download");
}

fn links_panel(ui: &mut Ui, edit: &mut PanelEdit) {
    let theme = crate::gui::theme::current(ui.ctx());
    let PanelEdit { value: v, inner, site, .. } = edit;
    ui.label("Klippo can show Download panel on a web-page when you select a text that contains download links.");
    ui.separator();
    ui.label("Panel view:");
    panel_view(ui, &mut v.links_mini, "Download selected links");
    ui.separator();
    ui.radio_value(&mut v.links, LinksPanel::Any, "Show Download panel for any selected links");
    ui.radio_value(&mut v.links, LinksPanel::Never, "Don't show Download panel for selected links");
    ui.radio_value(&mut v.links, LinksPanel::Sites, "Show Download panel for selected links for the following sites only:");
    ui.add_enabled_ui(v.links == LinksPanel::Sites, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            egui::Frame::none().fill(theme.list_bg).stroke(egui::Stroke::new(1.0_f32, theme.border_color)).inner_margin(egui::Margin::same(2.0)).show(ui, |ui| {
                ui.set_width(297.0);
                ui.set_height(114.0);
                egui::ScrollArea::vertical().id_salt("panel_sites").auto_shrink([false, false]).show(ui, |ui| {
                    ui.vertical(|ui| {
                        for (i, name) in v.links_sites.iter().enumerate() {
                            if ui.selectable_label(*site == Some(i), name.as_str()).clicked() {
                                *site = Some(i);
                            }
                        }
                    });
                });
            });
            ui.add_space(14.0);
            ui.vertical(|ui| {
                if button(ui, true, "Add").clicked() {
                    *inner = Some(PanelInner::AddSite(String::new()));
                }
                let chosen = site.filter(|i| *i < v.links_sites.len());
                if button(ui, chosen.is_some(), "Delete").clicked() {
                    if let Some(i) = chosen {
                        v.links_sites.remove(i);
                        *site = None;
                    }
                }
            });
        });
    });
}

/// "Full mode" / "Mini mode" with a picture of each panel.
fn panel_view(ui: &mut Ui, mini: &mut bool, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        ui.vertical(|ui| {
            ui.set_width(110.0);
            ui.radio_value(mini, false, "Full mode");
            ui.add_space(10.0);
            ui.radio_value(mini, true, "Mini mode");
        });
        ui.vertical(|ui| {
            panel_picture(ui, Some(text));
            ui.add_space(6.0);
            panel_picture(ui, None);
        });
    });
}

/// The browser panel as it looks: a green arrow, the text and the list arrow.
fn panel_picture(ui: &mut Ui, text: Option<&str>) {
    let font = egui::FontId::proportional(crate::gui::theme::LIST_FONT);
    let text_w = text.map(|t| ui.fonts(|f| f.layout_no_wrap(t.to_string(), font.clone(), egui::Color32::BLACK).size().x)).unwrap_or(0.0);
    let w = if text.is_some() { 20.0 + text_w + 34.0 } else { 34.0 };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 20.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3.0, egui::Color32::from_rgb(234, 240, 249));
    p.rect_stroke(rect, 3.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(122, 156, 198)));
    let icon = egui::Rect::from_min_size(rect.left_top() + egui::vec2(4.0, 3.5), egui::Vec2::splat(13.0));
    p.rect_filled(icon, 2.5, egui::Color32::from_rgb(46, 155, 71));
    let c = icon.center();
    p.add(egui::Shape::convex_polygon(
        vec![c + egui::vec2(-1.5, -4.5), c + egui::vec2(1.5, -4.5), c + egui::vec2(1.5, 0.0), c + egui::vec2(4.0, 0.0), c + egui::vec2(0.0, 4.5), c + egui::vec2(-4.0, 0.0), c + egui::vec2(-1.5, 0.0)],
        egui::Color32::WHITE,
        egui::Stroke::NONE,
    ));
    let dark = egui::Color32::from_rgb(18, 55, 107);
    if let Some(t) = text {
        p.text(rect.left_center() + egui::vec2(22.0, 0.0), egui::Align2::LEFT_CENTER, t, font, dark);
    }
    let a = egui::pos2(rect.right() - (if text.is_some() { 22.0 } else { 9.0 }), rect.center().y);
    p.add(egui::Shape::convex_polygon(vec![a + egui::vec2(-3.5, -1.5), a + egui::vec2(3.5, -1.5), a + egui::vec2(0.0, 2.5)], dark, egui::Stroke::NONE));
    if text.is_some() {
        let x = rect.right() - 9.0;
        p.vline(x - 6.0, (rect.top() + 2.0)..=(rect.bottom() - 2.0), egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(183, 201, 224)));
        let s = egui::Stroke::new(1.2_f32, dark);
        p.line_segment([egui::pos2(x - 2.5, rect.center().y - 2.5), egui::pos2(x + 2.5, rect.center().y + 2.5)], s);
        p.line_segment([egui::pos2(x + 2.5, rect.center().y - 2.5), egui::pos2(x - 2.5, rect.center().y + 2.5)], s);
    }
}

/// Add file type, Minimum size, Download panel exceptions and Enter site name.
fn render_panel_inner(edit: &mut PanelEdit, ctx: &Context) {
    let Some(mut inner) = edit.inner.take() else { return };
    let title = match inner {
        PanelInner::AddType(_) => "Add file type",
        PanelInner::MinSize(_) => "Minimum size",
        PanelInner::Exceptions(_) => "Download panel exceptions",
        PanelInner::AddSite(_) => "Enter site name",
    };
    let width = match inner {
        PanelInner::AddType(_) | PanelInner::AddSite(_) => 200.0,
        _ => 382.0,
    };
    let mut open = true;
    let mut ok = false;
    let mut cancel = false;
    dialog_window(title, ctx, width)
        .id(egui::Id::new("panel_inner_dialog"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(width);
            match &mut inner {
                PanelInner::AddType(text) | PanelInner::AddSite(text) => {
                    let edit = ui.add(egui::TextEdit::singleline(text).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY));
                    if !edit.has_focus() && text.is_empty() {
                        edit.request_focus();
                    }
                }
                PanelInner::MinSize(text) => {
                    ui.label("Minimum size for media file");
                    ui.horizontal(|ui| {
                        ui.add_space(50.0);
                        ui.label("Size:");
                        fixed_width(ui, 77.0, |ui| ui.add(egui::TextEdit::singleline(text).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                        ui.label("KB");
                    });
                    note(ui, "Smaller media files get no download panel. Streams have no size, so their panel always shows.");
                }
                PanelInner::Exceptions(text) => {
                    ui.label("Don't show the download panel on these sites:");
                    ui.add(egui::TextEdit::multiline(text).desired_rows(5).desired_width(f32::INFINITY));
                    note(ui, "Separate names by spaces. You may use asterisk as a wildcard pattern");
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    if button(ui, true, "Cancel").clicked() {
                        cancel = true;
                    }
                    if button(ui, true, "OK").clicked() {
                        ok = true;
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !matches!(inner, PanelInner::Exceptions(_)) {
                ok = true;
            }
        });
    if ok {
        let v = &mut edit.value;
        match inner {
            PanelInner::AddType(text) => {
                let t = text.trim().trim_start_matches('.').to_uppercase();
                if !t.is_empty() && !t.contains(char::is_whitespace) && !v.types.iter().any(|(x, _)| *x == t) {
                    v.types.push((t, true));
                }
            }
            PanelInner::MinSize(text) => {
                if let Ok(kb) = text.trim().parse::<u64>() {
                    v.min_size_kb = kb;
                }
            }
            PanelInner::Exceptions(text) => v.exceptions = text.split_whitespace().collect::<Vec<_>>().join(" "),
            PanelInner::AddSite(text) => {
                let site = text.trim().to_lowercase();
                if !site.is_empty() && !v.links_sites.contains(&site) {
                    v.links_sites.push(site);
                }
            }
        }
    } else if open && !cancel {
        edit.inner = Some(inner);
    }
}

fn render_site_login_submodal(app: &mut KlippoApp, ctx: &Context) {
    if !app.show_site_login_modal {
        return;
    }
    let mut is_open = true;
    dialog_window("Site login", ctx, 380.0)
        .id(egui::Id::new("site_login_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.set_max_width(380.0);
            egui::Grid::new("site_login_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Site/path");
                fixed_width(ui, 280.0, |ui| ui.add(egui::TextEdit::singleline(&mut app.site_login_url).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                ui.end_row();
                ui.label("User");
                fixed_width(ui, 280.0, |ui| ui.add(egui::TextEdit::singleline(&mut app.site_login_user).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                ui.end_row();
                ui.label("Password");
                fixed_width(ui, 280.0, |ui| ui.add(egui::TextEdit::singleline(&mut app.site_login_pass).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(f32::INFINITY)));
                ui.end_row();
            });
            note(ui, "A host name (files.example.com), an address prefix or a pattern (*.example.com).");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    if button(ui, true, "Cancel").clicked() {
                        app.show_site_login_modal = false;
                    }
                    if button(ui, !app.site_login_url.trim().is_empty(), "OK").clicked() {
                        let login = SiteLogin {
                            url: app.site_login_url.trim().to_string(),
                            username: app.site_login_user.trim().to_string(),
                            password: app.site_login_pass.clone(),
                        };
                        match app.site_login_edit_idx.take() {
                            Some(idx) if idx < app.settings.site_logins.len() => app.settings.site_logins[idx] = login,
                            _ => app.settings.site_logins.push(login),
                        }
                        app.show_site_login_modal = false;
                    }
                });
            });
        });
    if !is_open {
        app.show_site_login_modal = false;
    }
}
