//! The Site Grabber window: a project template and the start page, how to
//! explore the site, then the files found, which go to a download queue.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Color32, Context, RichText};

use super::classic_ui::{button, check_group, group_box, list_view, name_combo, note, right_buttons};
use super::{dialog_window, fixed_width};
use crate::core::http;
use crate::core::site_grabber::{self, FoundFile, GrabProgress, GrabSettings, GrabTemplate};
use crate::gui::queues::MAIN_QUEUE;
use crate::gui::state::{is_hls_url, server_of, Category, DownloadKind, KlippoApp, NewDownload, StartMode};

const WIDTH: f32 = 516.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrabStep {
    #[default]
    Start,
    Explore,
    Files,
}

pub struct SiteGrabberDialog {
    pub open: bool,
    pub step: GrabStep,
    pub template: GrabTemplate,
    pub start: String,
    pub types: String,
    /// Empty: each file goes to the folder of its category.
    pub dir: String,
    pub depth: u32,
    pub same_server: bool,
    pub same_folder: bool,
    pub other_server_files: bool,
    pub max_pages: usize,
    pub auth_enabled: bool,
    pub username: String,
    pub password: String,
    /// The grabber at work (or done).
    pub progress: Option<Arc<GrabProgress>>,
    pub files: Vec<FoundFile>,
    pub chosen: Vec<bool>,
    pub selected_row: Option<usize>,
    pub error: Option<String>,
    pub queue: u32,
    pub start_queue: bool,
}

impl Default for SiteGrabberDialog {
    fn default() -> Self {
        let defaults = GrabSettings::default();
        Self {
            open: false,
            step: GrabStep::Start,
            template: GrabTemplate::Images,
            start: String::new(),
            types: GrabTemplate::Images.types().to_string(),
            dir: String::new(),
            depth: defaults.depth,
            same_server: defaults.same_server,
            same_folder: defaults.same_folder,
            other_server_files: defaults.other_server_files,
            max_pages: defaults.max_pages,
            auth_enabled: false,
            username: String::new(),
            password: String::new(),
            progress: None,
            files: Vec::new(),
            chosen: Vec::new(),
            selected_row: None,
            error: None,
            queue: MAIN_QUEUE,
            start_queue: true,
        }
    }
}

impl SiteGrabberDialog {
    fn running(&self) -> bool {
        self.progress.as_ref().is_some_and(|p| !p.finished.load(Ordering::SeqCst))
    }

    fn stop(&self) {
        if let Some(ref p) = self.progress {
            p.stop.store(true, Ordering::SeqCst);
        }
    }
}

impl KlippoApp {
    /// Tasks > Run site grabber and the Grabber button.
    pub fn open_site_grabber(&mut self) {
        if !self.site_grabber.open {
            let start = std::mem::take(&mut self.site_grabber.start);
            self.site_grabber = SiteGrabberDialog { open: true, start, ..Default::default() };
        }
    }

    fn start_grabbing(&mut self) {
        let g = &self.site_grabber;
        let start = g.start.trim().to_string();
        let login = if g.auth_enabled && !g.username.is_empty() {
            Some((g.username.clone(), g.password.clone()))
        } else {
            self.settings.site_logins.iter().find(|l| l.matches(&start)).map(|l| (l.username.clone(), l.password.clone()))
        };
        let settings = GrabSettings {
            start: start.clone(),
            depth: g.depth,
            same_server: g.same_server,
            same_folder: g.same_folder,
            other_server_files: g.other_server_files,
            max_pages: g.max_pages,
            file_types: site_grabber::parse_types(&g.types),
            login,
        };
        let request = self.page_request_options(&start);
        let progress = Arc::new(GrabProgress::default());
        let shared = progress.clone();
        let spawned = std::thread::Builder::new().name("klippo-site-grabber".to_string()).spawn(move || {
            match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt.block_on(site_grabber::explore(settings, request, shared)),
                Err(e) => {
                    if let Ok(mut error) = shared.error.lock() {
                        *error = Some(e.to_string());
                    }
                    shared.finished.store(true, Ordering::SeqCst);
                }
            }
        });
        let g = &mut self.site_grabber;
        g.files.clear();
        g.chosen.clear();
        g.selected_row = None;
        g.error = spawned.err().map(|e| e.to_string());
        g.progress = g.error.is_none().then_some(progress);
        g.step = GrabStep::Files;
    }

    /// Adds the chosen files to the queue; returns how many were added.
    fn add_grabbed_files(&mut self) -> usize {
        let g = &self.site_grabber;
        let start_server = server_of(&g.start);
        let login = (g.auth_enabled && !g.username.is_empty()).then(|| (g.username.clone(), g.password.clone()));
        let dir = PathBuf::from(g.dir.trim());
        let (queue, start) = (g.queue, g.start_queue);
        let files: Vec<FoundFile> = g.files.iter().zip(&g.chosen).filter(|(_, on)| **on).map(|(f, _)| f.clone()).collect();
        let mut added = 0;
        for f in files {
            if self.downloads.iter().any(|d| d.url == f.url) {
                continue;
            }
            let filename = http::filename_from_url(&f.url).unwrap_or_else(|| "download".to_string());
            let category = Category::from_filename(&filename);
            // The login goes to the start page's server only.
            let login = login.clone().filter(|_| server_of(&f.url) == start_server);
            let new = NewDownload {
                kind: if is_hls_url(&f.url) { DownloadKind::Hls } else { DownloadKind::Http },
                dir: if dir.as_os_str().is_empty() { self.settings.category_dir(category) } else { dir.clone() },
                url: f.url,
                filename,
                filename_auto: true,
                category,
                connections: self.settings.max_connections,
                referer: Some(f.page),
                username: login.as_ref().map(|l| l.0.clone()),
                password: login.map(|l| l.1),
                ..Default::default()
            };
            self.add_item(new, StartMode::Queue(queue));
            added += 1;
        }
        if start && added > 0 {
            self.start_queue(queue);
        }
        added
    }
}

pub fn render_site_grabber(app: &mut KlippoApp, ctx: &Context) {
    if !app.site_grabber.open {
        return;
    }
    // The grabber's results come in while it works.
    if let Some(progress) = app.site_grabber.progress.clone() {
        let found = progress.files.lock().map(|f| f.clone()).unwrap_or_default();
        let g = &mut app.site_grabber;
        if found.len() > g.files.len() {
            g.chosen.resize(found.len(), true);
            g.files = found;
        }
        if progress.finished.load(Ordering::SeqCst) {
            if g.error.is_none() {
                g.error = progress.error.lock().ok().and_then(|e| e.clone());
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    let mut is_open = true;
    let mut close = false;
    let mut explore = false;
    let mut download = false;
    let step_text = match app.site_grabber.step {
        GrabStep::Start => "Step 1 of 3: the start page and the files to get",
        GrabStep::Explore => "Step 2 of 3: how to explore the site",
        GrabStep::Files => "Step 3 of 3: the files found",
    };
    dialog_window("Site Grabber", ctx, WIDTH)
        .id(egui::Id::new("site_grabber_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .minimizable(true)
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.set_max_width(WIDTH);
            ui.label(step_text);
            ui.add_space(4.0);
            let g = &mut app.site_grabber;
            match g.step {
                GrabStep::Start => start_page(g, ui),
                GrabStep::Explore => explore_page(g, ui),
                GrabStep::Files => files_page(g, &queues_of(&app.settings.queues), ui),
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    let g = &mut app.site_grabber;
                    let running = g.running();
                    if button(ui, true, if g.step == GrabStep::Files && !running { "Close" } else { "Cancel" }).clicked() {
                        close = true;
                    }
                    match g.step {
                        GrabStep::Start => {
                            let ready = url::Url::parse(g.start.trim()).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
                            if button(ui, ready, "Next >").on_disabled_hover_text("Type the address of the start page (http:// or https://)").clicked() {
                                g.step = GrabStep::Explore;
                            }
                        }
                        GrabStep::Explore => {
                            if button(ui, true, "Explore").on_hover_text("Read the pages and find the files").clicked() {
                                explore = true;
                            }
                            if button(ui, true, "< Back").clicked() {
                                g.step = GrabStep::Start;
                            }
                        }
                        GrabStep::Files => {
                            if running {
                                if button(ui, true, "Stop").on_hover_text("Stop exploring; the files found so far stay").clicked() {
                                    g.stop();
                                }
                            } else {
                                let any = g.chosen.iter().any(|c| *c);
                                if button(ui, any, "Download").on_hover_text("Add the chosen files to the queue").clicked() {
                                    download = true;
                                }
                                if button(ui, true, "< Back").clicked() {
                                    g.step = GrabStep::Explore;
                                }
                            }
                        }
                    }
                });
            });
        });

    if explore {
        app.start_grabbing();
    }
    if download {
        let added = app.add_grabbed_files();
        let queue = app.queue_name(app.site_grabber.queue);
        app.set_status(format!("Site Grabber: added {} files to \"{}\"", added, queue));
        close = true;
    }
    if close || !is_open {
        app.site_grabber.stop();
        app.site_grabber.open = false;
        app.site_grabber.progress = None;
    }
}

fn queues_of(queues: &[crate::gui::queues::QueueConfig]) -> Vec<(u32, String)> {
    queues.iter().map(|q| (q.id, q.name.clone())).collect()
}

fn start_page(g: &mut SiteGrabberDialog, ui: &mut egui::Ui) {
    ui.label("Project template");
    let combo = egui::ComboBox::from_id_salt("grabber_template").width(WIDTH - 6.0).selected_text(g.template.label()).show_ui(ui, |ui| {
        for t in GrabTemplate::ALL {
            if ui.selectable_label(g.template == t, t.label()).clicked() {
                g.template = t;
                if t != GrabTemplate::Custom {
                    g.types = t.types().to_string();
                }
            }
        }
    });
    name_combo(&combo, "Project template", g.template.label());
    ui.add_space(4.0);
    ui.label("Start page (the address of the web page to explore)");
    fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut g.start).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
    note(ui, "For example: https://www.example.com/gallery/");
    ui.add_space(4.0);
    ui.label("File types to get");
    let types = fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut g.types).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
    if types.changed() {
        g.template = GrabTemplate::Custom;
    }
    note(ui, if g.template == GrabTemplate::AllFiles {
        "Every linked file that is not a web page, a style sheet or a script"
    } else {
        "Separate the types with spaces; * matches any letters, for example MP*"
    });
    ui.add_space(4.0);
    ui.label("Save the files to");
    ui.horizontal(|ui| {
        fixed_width(ui, ui.available_width() - 78.0, |ui| {
            ui.add(egui::TextEdit::singleline(&mut g.dir).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY))
        });
        if button(ui, true, "Browse").clicked() {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                g.dir = folder.to_string_lossy().to_string();
            }
        }
    });
    note(ui, "Empty: each file goes to the folder of its category (Options > Save to)");
}

fn explore_page(g: &mut SiteGrabberDialog, ui: &mut egui::Ui) {
    group_box(ui, "Explore", |ui| {
        ui.horizontal(|ui| {
            ui.label("Follow links up to");
            ui.add(egui::DragValue::new(&mut g.depth).range(0..=10));
            ui.label("levels from the start page");
        });
        ui.checkbox(&mut g.same_server, "Explore only the pages on the start page's server");
        ui.checkbox(&mut g.same_folder, "Explore only the pages in the start page's folder and below it");
        ui.checkbox(&mut g.other_server_files, "Get the files from other servers too");
        ui.horizontal(|ui| {
            ui.label("Stop after");
            ui.add(egui::DragValue::new(&mut g.max_pages).range(1..=5000));
            ui.label("pages");
        });
    });
    check_group(ui, &mut g.auth_enabled, "The site requires a login", |ui| {
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.label("Login");
            fixed_width(ui, 140.0, |ui| ui.add(egui::TextEdit::singleline(&mut g.username).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
            ui.add_space(20.0);
            ui.label("Password");
            fixed_width(ui, 140.0, |ui| ui.add(egui::TextEdit::singleline(&mut g.password).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(f32::INFINITY)));
        });
    });
    note(ui, "The pages are read one after the other. A login goes only to the start page's server.");
}

fn files_page(g: &mut SiteGrabberDialog, queues: &[(u32, String)], ui: &mut egui::Ui) {
    let progress = g.progress.clone();
    let running = g.running();
    let (done, found) = progress.as_ref().map_or((0, 0), |p| (p.pages_done.load(Ordering::Relaxed), p.pages_found.load(Ordering::Relaxed)));
    ui.horizontal(|ui| {
        if running {
            ui.spinner();
        }
        ui.label(format!("Pages explored: {} of {}    Files found: {}", done, found, g.files.len()));
    });
    if running {
        let current = progress.as_ref().and_then(|p| p.current.lock().ok().map(|c| c.clone())).unwrap_or_default();
        fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::Label::new(RichText::new(current).size(crate::gui::theme::LIST_FONT)).truncate()));
    } else if let Some(ref e) = g.error {
        ui.label(RichText::new(e).color(Color32::from_rgb(220, 53, 69)));
    } else if g.files.is_empty() {
        note(ui, "No files of these types were found. Go back to explore more levels or to change the types.");
    }
    let files = g.files.clone();
    let chosen = &mut g.chosen;
    list_view(ui, "grabbed_files", &[("", 24.0), ("File", 190.0), ("Type", 44.0), ("Server", 120.0)], files.len(), 180.0, &mut g.selected_row, |ui, row, col| {
        let f = &files[row];
        let url = url::Url::parse(&f.url).ok();
        match col {
            0 => {
                ui.checkbox(&mut chosen[row], "");
            }
            1 => {
                let name = http::filename_from_url(&f.url).unwrap_or_else(|| f.url.clone());
                ui.add(egui::Label::new(name).truncate()).on_hover_text(&f.url);
            }
            2 => {
                ui.label(url.as_ref().map(|u| site_grabber::url_type(u).to_uppercase()).unwrap_or_default());
            }
            _ => {
                ui.add(egui::Label::new(url.as_ref().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()).truncate());
            }
        }
    });
    ui.horizontal(|ui| {
        if button(ui, !files.is_empty(), "Select all").clicked() {
            g.chosen.iter_mut().for_each(|c| *c = true);
        }
        if button(ui, !files.is_empty(), "Select none").clicked() {
            g.chosen.iter_mut().for_each(|c| *c = false);
        }
        ui.label(format!("{} chosen", g.chosen.iter().filter(|c| **c).count()));
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Add the files to the queue");
        let name = queues.iter().find(|(id, _)| *id == g.queue).map(|(_, n)| n.clone()).unwrap_or_default();
        let combo = egui::ComboBox::from_id_salt("grabber_queue").width(180.0).selected_text(name.clone()).show_ui(ui, |ui| {
            for (id, n) in queues {
                ui.selectable_value(&mut g.queue, *id, n);
            }
        });
        name_combo(&combo, "Queue", &name);
    });
    ui.checkbox(&mut g.start_queue, "Start queue processing");
}
