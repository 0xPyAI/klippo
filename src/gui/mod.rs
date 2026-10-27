pub mod theme;
pub mod icons;
pub mod state;
pub mod manager;
pub mod clipboard;
pub mod queues;
pub mod ipc;
pub mod system;
#[cfg(windows)]
pub mod tray;
#[cfg(windows)]
pub mod dnd;
pub mod views;

use std::time::Duration;
use eframe::egui::{self, Key, RichText};
use ipc::{IpcMessage, IpcServer};
use state::KlippoApp;
use views::menu_bar::render_menu_bar;
use views::toolbar::render_toolbar;
use views::category_sidebar::render_category_sidebar;
use views::downloads_table::render_downloads_table;
use views::status_bar::render_status_bar;
use views::add_url_modal::render_add_url_modal;
use views::progress_modal::render_progress_windows;
use views::complete_modal::render_complete_modal;
use views::options_modal::render_options_modal;
use views::scheduler_modal::render_scheduler_modal;
use views::speed_limiter_modal::render_speed_limiter_modal;
use views::file_properties_modal::render_file_properties_modal;
use views::batch_download_modal::render_batch_download_modal;
use views::about_modal::render_about_modal;
use views::dialogs::{
    render_delete_confirm, render_exit_confirm, render_export_dialog, render_grabber_dialog, render_limit_dialog,
    render_login_prompt,
};

impl eframe::App for KlippoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.frame_count == 0 {
            // A timed repaint does not wake an idle window that is in the
            // background, so a heartbeat keeps the queue and scheduler ticking.
            let heartbeat = ctx.clone();
            let _ = std::thread::Builder::new().name("klippo-heartbeat".to_string()).spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                heartbeat.request_repaint();
            });
            self.bridge.set_repaint_context(ctx);
            if let Some(ref ipc) = self.ipc {
                ipc.set_repaint_context(ctx);
            }
            // Accept links dragged from a browser onto the window.
            #[cfg(windows)]
            if let Some(hwnd) = self.hwnd {
                dnd::install(hwnd);
            }
            if self.start_minimized {
                if self.has_tray() && self.settings.close_to_tray {
                    self.hide_to_tray(ctx);
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
            }
        }
        if self.frame_count == 0 {
            self.start_queues_on_startup();
            self.open_requested_dialog();
        }
        self.frame_count += 1;
        if (ctx.zoom_factor() - self.settings.font_scale).abs() > 0.001 {
            ctx.set_zoom_factor(self.settings.font_scale);
        }
        self.sync_tray(ctx);
        self.sync_theme(ctx);
        self.last_minimized = ctx.input(|i| i.viewport().minimized) == Some(true);
        // Restored from outside (Alt+Tab, a tool): give back the taskbar button.
        let settled = self.tray_since.is_some_and(|t| t.elapsed() > Duration::from_secs(1));
        if self.in_tray && settled && ctx.input(|i| i.viewport().minimized) == Some(false) {
            if let Some(hwnd) = self.hwnd {
                system::restore_from_tray(hwnd);
            }
            self.in_tray = false;
        }
        if self.in_tray {
            if let Some(hwnd) = self.hwnd {
                system::keep_in_tray(hwnd);
            }
        }

        // Poll background events
        self.process_background_events();
        self.process_clipboard_events();
        self.process_dropped_urls();
        self.process_ipc_messages(ctx);
        self.next_pending_add();
        self.tick_add_probe();
        self.queue_tick();
        self.scheduler_tick();
        self.limits_tick();
        self.check_quit_when_done();
        self.flush_if_dirty();

        self.handle_tray(ctx);
        self.handle_close_request(ctx);
        self.handle_shortcuts(ctx);

        let title = match self.downloads.iter().filter(|d| d.status.is_active()).count() {
            0 => "Klippo - Download Manager".to_string(),
            n => format!("Klippo - {} active download{}", n, if n == 1 { "" } else { "s" }),
        };
        let title_id = egui::Id::new("klippo_window_title");
        if ctx.data(|d| d.get_temp::<String>(title_id)).as_deref() != Some(title.as_str()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            ctx.data_mut(|d| d.insert_temp(title_id, title));
        }

        let look = theme::current(ctx);

        // Menu strip, then the toolbar under a thin line.
        egui::TopBottomPanel::top("menu_panel")
            .frame(egui::Frame::none().fill(look.menu_bg).inner_margin(egui::Margin { left: 6.0, right: 6.0, top: 1.0, bottom: 1.0 }))
            .show_separator_line(false)
            .show(ctx, |ui| render_menu_bar(self, ui));
        if self.show_toolbar {
            egui::TopBottomPanel::top("toolbar_panel")
                .frame(
                    egui::Frame::none()
                        .fill(look.toolbar_bg)
                        .stroke(egui::Stroke::new(1.0_f32, look.menu_line))
                        .inner_margin(egui::Margin { left: 4.0, right: 4.0, top: 2.0, bottom: 2.0 }),
                )
                .show_separator_line(false)
                .show(ctx, |ui| render_toolbar(self, ui));
        }

        // There is no status bar by default; View > Status Bar adds one.
        if self.settings.show_status_bar {
            egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
                render_status_bar(self, ui);
            });
        }

        // Left Panel: Categories tree
        if self.show_sidebar {
            egui::SidePanel::left("left_panel")
                .resizable(true)
                .default_width(130.0)
                .width_range(100.0..=320.0)
                .frame(
                    egui::Frame::none()
                        .fill(look.sidebar_bg)
                        .stroke(egui::Stroke::new(1.0_f32, look.border_color))
                        .outer_margin(egui::Margin { left: 6.0, right: 0.0, top: 4.0, bottom: 4.0 }),
                )
                .show_separator_line(false)
                .show(ctx, |ui| {
                    render_category_sidebar(self, ui);
                });
        }

        // Central Panel: the downloads list
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(look.toolbar_bg).inner_margin(egui::Margin { left: 4.0, right: 6.0, top: 4.0, bottom: 4.0 }))
            .show(ctx, |ui| {
            egui::Frame::none()
                .fill(look.list_bg)
                .stroke(egui::Stroke::new(1.0_f32, look.border_color))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    render_downloads_table(self, ui);
                });
        });

        // Without the status bar, short messages show for a moment at the bottom.
        if !self.settings.show_status_bar {
            if let Some((ref msg, at)) = self.status_message {
                if at.elapsed() < Duration::from_secs(6) {
                    egui::Area::new(egui::Id::new("status_toast"))
                        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-14.0, -12.0))
                        .order(egui::Order::Foreground)
                        .interactable(false)
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.label(RichText::new(msg).size(12.0));
                            });
                        });
                }
            }
        }

        // Modals & Floating Windows
        render_add_url_modal(self, ctx);
        render_progress_windows(self, ctx);
        render_complete_modal(self, ctx);
        render_options_modal(self, ctx);
        render_scheduler_modal(self, ctx);
        render_speed_limiter_modal(self, ctx);
        render_file_properties_modal(self, ctx);
        render_batch_download_modal(self, ctx);
        render_about_modal(self, ctx);
        views::find_modal::render_find_dialog(self, ctx);
        views::find_modal::render_columns_dialog(self, ctx);
        render_grabber_dialog(self, ctx);
        views::site_grabber_modal::render_site_grabber(self, ctx);
        views::category_dialog::render_new_category_dialog(self, ctx);
        render_delete_confirm(self, ctx);
        render_exit_confirm(self, ctx);
        render_export_dialog(self, ctx);
        render_login_prompt(self, ctx);
        render_limit_dialog(self, ctx);

        if self.pending_exit {
            self.pending_exit = false;
            self.exit_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx.request_repaint();
        }

        // Background events wake the GUI; this only drives timers and animations.
        let any_active = self.downloads.iter().any(|d| d.status.is_active());
        ctx.request_repaint_after(if any_active {
            Duration::from_millis(250)
        } else {
            Duration::from_millis(1000)
        });
    }

    /// What shows where no panel paints, such as the margin around the
    /// Categories tree: the window's own background, not eframe's near-black.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        let look = if visuals.dark_mode { theme::IdmTheme::idm_dark() } else { theme::IdmTheme::idm_light() };
        look.toolbar_bg.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown_background();
    }
}

impl KlippoApp {
    /// KLIPPO_SHOW=<dialog>[:n] opens one window at start (for screenshots and checks):
    /// options:<tab 0-8>, scheduler, speed, add, dfi, batch, find, about, columns, progress, complete, properties,
    /// sitegrabber[:<start page>], newcategory.
    fn open_requested_dialog(&mut self) {
        use views::options_modal::OptionsTab;
        let Ok(what) = std::env::var("KLIPPO_SHOW") else { return };
        let (name, arg) = what.split_once(':').unwrap_or((what.as_str(), ""));
        let first = self.downloads.first().map(|d| d.id);
        match name {
            "options" => {
                self.open_options();
                let tabs = [
                    OptionsTab::General,
                    OptionsTab::FileTypes,
                    OptionsTab::SaveTo,
                    OptionsTab::Downloads,
                    OptionsTab::Connection,
                    OptionsTab::ProxySocks,
                    OptionsTab::SiteLogins,
                    OptionsTab::DialUpVpn,
                    OptionsTab::Sounds,
                ];
                self.options_active_tab = tabs[arg.parse::<usize>().unwrap_or(0).min(8)];
            }
            "panels" => {
                // Options > General > "Customize download panels in browsers" (panels:1 = selected links).
                self.open_options();
                self.options_sub = Some(views::options_modal::OptionsSub::Panels(Box::new(views::options_modal::PanelEdit {
                    tab: if arg == "1" { 1 } else { 0 },
                    value: self.settings.panel.clone(),
                    site: None,
                    inner: None,
                })));
            }
            "scheduler" => self.show_scheduler_dialog = true,
            "speed" => self.show_speed_limiter_dialog = true,
            "add" => self.open_add_url_dialog(),
            "dfi" => {
                self.open_add_url_dialog();
                self.add.url = if arg.is_empty() { "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf".to_string() } else { arg.to_string() };
                self.on_add_url_changed();
                self.add.step = state::AddStep::Info;
            }
            "batch" => self.batch.open = true,
            "find" => self.focus_search = true,
            "columns" => self.show_columns_dialog = true,
            "about" => self.show_about_dialog = true,
            "progress" => {
                if let Some(id) = first {
                    self.show_progress_window(id);
                }
            }
            "complete" => self.show_complete_dialog = first,
            "properties" => self.show_file_properties_dialog = first,
            "sitegrabber" => {
                self.site_grabber.start = arg.to_string();
                self.open_site_grabber();
            }
            "newcategory" => self.open_new_category(views::category_dialog::CategoryFrom::Tree),
            _ => {}
        }
    }

    fn has_tray(&self) -> bool {
        #[cfg(windows)]
        return self.tray.is_some();
        #[cfg(not(windows))]
        return false;
    }

    /// Creates or removes the tray icon when View > Tray icon changes.
    fn sync_tray(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            if self.settings.show_tray_icon && self.tray.is_none() {
                let icon = app_icon();
                self.tray = tray::Tray::new(ctx, icon.rgba, icon.width);
            } else if !self.settings.show_tray_icon && self.tray.is_some() {
                if self.in_tray {
                    self.show_main_window(ctx);
                }
                self.tray = None;
            }
        }
        #[cfg(not(windows))]
        let _ = ctx;
    }

    /// Restores the main window (also from the notification area) and focuses it.
    pub fn show_main_window(&mut self, ctx: &egui::Context) {
        if self.in_tray {
            if let Some(hwnd) = self.hwnd {
                system::restore_from_tray(hwnd);
            }
            self.in_tray = false;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        // Taking the keyboard from another program is an outside action (test log).
        if !system::test_log("front main window") {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        match self.hwnd {
            Some(hwnd) => {
                system::hide_to_tray(hwnd);
                self.in_tray = true;
                self.tray_since = Some(std::time::Instant::now());
            }
            None => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
        }
    }

    /// Tray clicks and the tray menu; also keeps the tooltip current.
    fn handle_tray(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            let mut show = false;
            let any_running = self.any_queue_running();
            let Some(ref mut tray) = self.tray else { return };
            let active: Vec<&state::DownloadItem> = self.downloads.iter().filter(|d| d.status.is_active()).collect();
            let speed: f64 = active.iter().map(|d| d.speed_bps).sum();
            let tooltip = if active.is_empty() {
                "Klippo".to_string()
            } else {
                format!("Klippo: {} active, {}/s", active.len(), state::format_bytes(speed as u64))
            };
            tray.set_tooltip(&tooltip);
            let actions = tray.poll(self.hwnd, any_running, self.settings.speed_limit_enabled);
            for action in actions {
                use tray::TrayAction;
                match action {
                    TrayAction::Show => show = true,
                    TrayAction::AddUrl => {
                        show = true;
                        self.open_add_url_dialog();
                    }
                    TrayAction::StartQueue => self.start_queue(state::MAIN_QUEUE),
                    TrayAction::StopQueue => self.stop_all_queues(),
                    TrayAction::ToggleLimiter => {
                        self.settings.speed_limit_enabled = !self.settings.speed_limit_enabled;
                        self.save_settings();
                    }
                    TrayAction::Exit => {
                        self.exit_requested = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ctx.request_repaint();
                    }
                }
            }
            if show {
                self.show_main_window(ctx);
            }
        }
        #[cfg(not(windows))]
        let _ = ctx;
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        // Klippo keeps running in the notification area when its window closes.
        if !self.exit_requested && self.settings.close_to_tray && self.has_tray() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_to_tray(ctx);
            return;
        }
        let active = self.downloads.iter().filter(|d| d.status.is_active()).count();
        if active > 0 && self.settings.confirm_exit && !self.exit_confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.show_main_window(ctx);
            self.show_exit_confirm = true;
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (ctrl_f, f3) = ctx.input(|i| (i.modifiers.command && i.key_pressed(Key::F), i.key_pressed(Key::F3)));
        if ctrl_f {
            self.focus_search = true;
        }
        // Text fields keep their own keys.
        if ctx.wants_keyboard_input() {
            return;
        }
        if f3 {
            self.find_next();
        }
        let (ctrl_n, ctrl_o, ctrl_s, ctrl_b, del, enter, ctrl_a, ctrl_c) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && i.key_pressed(Key::N),
                cmd && i.key_pressed(Key::O),
                cmd && i.key_pressed(Key::S),
                cmd && i.key_pressed(Key::B),
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::Enter),
                cmd && i.key_pressed(Key::A),
                cmd && i.key_pressed(Key::C),
            )
        });
        if ctrl_n {
            self.open_add_url_dialog();
        }
        if ctrl_o {
            self.open_options();
        }
        if ctrl_s {
            self.show_scheduler_dialog = true;
        }
        if ctrl_b {
            self.batch.open = true;
        }
        let modal_open = self.add.open || self.delete_confirm.is_some() || self.show_options_dialog;
        if modal_open {
            return;
        }
        if del {
            self.request_delete(self.selected_ids());
        }
        if ctrl_a {
            self.selection = self.visible_ids().into_iter().collect();
        }
        if ctrl_c {
            let links: Vec<String> = self.selected_ids().iter().filter_map(|id| self.item(*id)).map(|d| d.url.clone()).collect();
            if !links.is_empty() {
                self.copy_to_clipboard(&links.join("\r\n"));
            }
        }
        if enter {
            if let Some(item) = self.selected_download() {
                let id = item.id;
                if item.status == state::DownloadStatus::Completed {
                    crate::gui::system::open_file(&item.save_path);
                } else {
                    self.show_progress_window(id);
                }
            }
        }
    }
}

/// Green arrow icon for the window and taskbar.
fn app_icon() -> egui::IconData {
    let size = 64usize;
    let mut rgba = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32, y as f32);
            // Rounded square background.
            let r = 12.0;
            let cx = fx.clamp(r, size as f32 - 1.0 - r);
            let cy = fy.clamp(r, size as f32 - 1.0 - r);
            let inside = (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r;
            // Down arrow: shaft and head.
            let shaft = (26.0..=38.0).contains(&fx) && (10.0..=36.0).contains(&fy);
            let head = (32.0..=52.0).contains(&fy) && (fx - 32.0).abs() <= (52.0 - fy) * 0.95;
            let i = (y * size + x) * 4;
            if shaft || head {
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            } else if inside {
                let shade = (200.0 - fy * 1.2) as u8;
                rgba[i..i + 4].copy_from_slice(&[30, shade.max(120), 60, 255]);
            }
        }
    }
    egui::IconData { rgba, width: size as u32, height: size as u32 }
}

pub fn run_gui() -> Result<(), Box<dyn std::error::Error>> {
    run_gui_with_message(IpcMessage::default(), false)
}

pub fn run_gui_with_initial_url(
    initial_url: Option<String>,
    initial_path: Option<String>,
    initial_filename: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = IpcMessage {
        url: initial_url.unwrap_or_default(),
        dir: initial_path,
        filename: initial_filename,
        ..Default::default()
    };
    run_gui_with_message(msg, false)
}

/// Runs the main window. A second launch hands `msg` to the first one and returns.
pub fn run_gui_with_message(msg: IpcMessage, start_minimized: bool) -> Result<(), Box<dyn std::error::Error>> {
    if ipc::send_to_running_instance(&msg) {
        return Ok(());
    }
    // Started for a download (`/d <address>`): only its dialog shows.
    let start_minimized = start_minimized || !msg.url.trim().is_empty() || !msg.batch_urls.is_empty();
    let ipc_server = IpcServer::start();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Klippo - Download Manager")
            // The classic main window: 1137 x 576 pixels at 125 % with its frame.
            .with_inner_size([895.0, 423.0])
            .with_min_inner_size([600.0, 320.0])
            .with_icon(app_icon())
            .with_decorations(true),
        ..Default::default()
    };

    eframe::run_native(
        "Klippo",
        native_options,
        Box::new(move |cc| {
            let mut app = KlippoApp::new(start_minimized, ipc_server);
            {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = cc.window_handle() {
                    if let RawWindowHandle::Win32(h) = handle.as_raw() {
                        app.hwnd = Some(h.hwnd.get());
                    }
                }
            }
            #[cfg(windows)]
            if app.settings.show_tray_icon {
                let icon = app_icon();
                app.tray = tray::Tray::new(&cc.egui_ctx, icon.rgba, icon.width);
            }
            theme::install_fonts(&cc.egui_ctx);
            app.apply_theme(&cc.egui_ctx);
            app.handle_message(msg, None);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| format!("Failed to run GUI: {:?}", e).into())
}

