//! "Batch download" (an address with a * wildcard) and "Add batch download
//! from clipboard" (a list of addresses), then the queue selection panel.

use eframe::egui::{self, Context};

use super::classic_ui::{button, check_group, group_box, name_combo, note, right_buttons};
use super::{dialog_window, fixed_width};
use crate::core::http;
use crate::gui::state::{is_hls_url, BatchMode, Category, DownloadKind, NewDownload, KlippoApp, StartMode};

const INTRO: &str = "Add a series of numbered files in one step (for example img001.jpg to img100.jpg): \
write the address with an asterisk (*) where the number or letter changes.";

pub fn render_batch_download_modal(app: &mut KlippoApp, ctx: &Context) {
    if !app.batch.open {
        return;
    }
    let mut is_open = true;
    let mut ok = false;
    let title = match app.batch.mode {
        BatchMode::List if app.batch.from_browser => "Download all links with Klippo",
        BatchMode::List => "Batch download from clipboard",
        _ => "Batch download",
    };
    let urls = app.batch.urls();
    let to_front = std::mem::take(&mut app.batch.to_front);
    dialog_window(title, ctx, 426.0)
        .id(egui::Id::new("batch_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .minimizable(app.batch.from_browser)
        .bring_to_front(to_front)
        .show(ctx, |ui| {
            ui.set_width(426.0);
            ui.set_max_width(426.0);
            if app.batch.choosing_queue {
                queue_panel(app, ui, urls.len());
                return;
            }
            match app.batch.mode {
                BatchMode::Pattern => pattern_page(app, ui, &urls),
                BatchMode::List => {
                    ui.label("Addresses (one per line):");
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut app.batch.urls_text).desired_rows(10).desired_width(f32::INFINITY));
                    });
                    note(ui, &format!("{} addresses", urls.len()));
                    check_group(ui, &mut app.batch.auth_enabled, "Use authorization", |ui| auth_row((&mut app.batch.username, &mut app.batch.password), ui));
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    if button(ui, true, "Cancel").clicked() {
                        app.batch.open = false;
                    }
                    ok = button(ui, !urls.is_empty(), "OK").clicked();
                });
            });
        });
    if ok {
        if app.settings.queue_panel_on_batch {
            app.batch.choosing_queue = true;
        } else {
            add_all(app, crate::gui::queues::MAIN_QUEUE, false);
        }
    }
    if !is_open {
        app.batch.open = false;
        app.batch.choosing_queue = false;
    }
}

fn auth_row((user, pass): (&mut String, &mut String), ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        ui.label("Login");
        fixed_width(ui, 110.0, |ui| ui.add(egui::TextEdit::singleline(user).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
        ui.add_space(20.0);
        ui.label("Password");
        fixed_width(ui, 110.0, |ui| ui.add(egui::TextEdit::singleline(pass).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(f32::INFINITY)));
    });
}

fn pattern_page(app: &mut KlippoApp, ui: &mut egui::Ui, urls: &[String]) {
    ui.label(INTRO);
    ui.horizontal(|ui| {
        ui.label("For example:");
        ui.label("http://www.server.com/pictures/img*.jpg");
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Address");
        fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut app.batch.pattern).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
    });
    group_box(ui, "Replace asterisk to", |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut app.batch.letters, false, "Numbers");
            ui.add_space(60.0);
            ui.radio_value(&mut app.batch.letters, true, "Letters");
        });
        ui.horizontal(|ui| {
            ui.label("From:");
            spin(ui, &mut app.batch.from, app.batch.letters);
            ui.add_space(30.0);
            ui.label("To:");
            spin(ui, &mut app.batch.to, app.batch.letters);
            ui.add_enabled_ui(!app.batch.letters, |ui| {
                right_buttons(ui, |ui| {
                    ui.add(egui::DragValue::new(&mut app.batch.wildcard_size).range(1..=10));
                    ui.label("Wildcard size");
                });
            });
        });
    });
    check_group(ui, &mut app.batch.auth_enabled, "Use authorization", |ui| auth_row((&mut app.batch.username, &mut app.batch.password), ui));
    ui.add_space(6.0);
    let shown = |i: usize| urls.get(i).cloned().unwrap_or_default();
    egui::Grid::new("batch_preview").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        for (label, mut value) in [("First file:", shown(0)), ("Second file:", shown(1))] {
            ui.label(label);
            fixed_width(ui, 330.0, |ui| ui.add_enabled(false, egui::TextEdit::singleline(&mut value).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
            ui.end_row();
        }
        ui.label("   ...");
        ui.label("");
        ui.end_row();
        ui.label("Last file:");
        let mut last = if urls.len() > 2 { urls.last().cloned().unwrap_or_default() } else { String::new() };
        fixed_width(ui, 330.0, |ui| ui.add_enabled(false, egui::TextEdit::singleline(&mut last).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
        ui.end_row();
    });
}

/// A number or letter field with up / down arrows, like the Windows spin control.
fn spin(ui: &mut egui::Ui, value: &mut String, letters: bool) {
    ui.add(egui::TextEdit::singleline(value).margin(crate::gui::theme::EDIT_MARGIN).desired_width(50.0));
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let step = |v: &mut String, up: bool| {
            if letters {
                if let Some(c) = v.trim().chars().next().filter(|c| c.is_ascii_alphabetic()) {
                    let n = if up { (c as u8).saturating_add(1) } else { (c as u8).saturating_sub(1) } as char;
                    if n.is_ascii_alphabetic() {
                        *v = n.to_string();
                    }
                }
            } else if let Ok(n) = v.trim().parse::<u64>() {
                *v = if up { n + 1 } else { n.saturating_sub(1) }.to_string();
            }
        };
        if ui.small_button("▲").clicked() {
            step(value, true);
        }
        if ui.small_button("▼").clicked() {
            step(value, false);
        }
    });
}

/// The queue selection panel after a batch.
fn queue_panel(app: &mut KlippoApp, ui: &mut egui::Ui, count: usize) {
    ui.label(format!("Add {} files to the following queue:", count));
    let queues: Vec<(u32, String)> = app.settings.queues.iter().map(|q| (q.id, q.name.clone())).collect();
    let combo = egui::ComboBox::from_id_salt("batch_queue").width(260.0).selected_text(app.queue_name(app.batch.queue)).show_ui(ui, |ui| {
        for (id, name) in queues {
            ui.selectable_value(&mut app.batch.queue, id, name);
        }
    });
    name_combo(&combo, "Queue", &app.queue_name(app.batch.queue));
    ui.checkbox(&mut app.batch.start_queue, "Start queue processing");
    ui.add_space(8.0);
    let mut done = None;
    ui.horizontal(|ui| {
        right_buttons(ui, |ui| {
            if button(ui, true, "Cancel").clicked() {
                app.batch.choosing_queue = false;
            }
            if button(ui, true, "OK").clicked() {
                done = Some((app.batch.queue, app.batch.start_queue));
            }
        });
    });
    if let Some((queue, start)) = done {
        add_all(app, queue, start);
    }
}

fn add_all(app: &mut KlippoApp, queue: u32, start: bool) {
    let urls = app.batch.urls();
    for url in &urls {
        let filename = http::filename_from_url(url).unwrap_or_else(|| "download".to_string());
        let kind = if is_hls_url(url) { DownloadKind::Hls } else { DownloadKind::Http };
        let category = Category::from_filename(&filename);
        let new = NewDownload {
            url: url.clone(),
            category,
            filename,
            filename_auto: true,
            dir: app.settings.category_dir(category),
            connections: app.settings.max_connections,
            kind,
            username: app.batch.auth_enabled.then(|| app.batch.username.clone()),
            password: app.batch.auth_enabled.then(|| app.batch.password.clone()),
            ..Default::default()
        };
        // Batch files always go through a queue.
        app.add_item(new, StartMode::Queue(queue));
    }
    if start {
        app.start_queue(queue);
    }
    app.set_status(format!("Added {} files to \"{}\"", urls.len(), app.queue_name(queue)));
    app.batch.urls_text.clear();
    app.batch.choosing_queue = false;
    app.batch.open = false;
}
