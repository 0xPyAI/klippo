//! Small confirmation dialogs plus the HLS grabber.

use std::time::Instant;

use eframe::egui::{self, Color32, Context, RichText};
use crate::gui::state::{
    format_bytes, Category, DownloadKind, DownloadStatus, ExportFormat, ExportScope, NewDownload, KlippoApp, StartMode,
};
use super::classic_ui::name_combo;
use super::{dialog_window, fixed_width};

/// "Confirm deletion of downloads".
pub fn render_delete_confirm(app: &mut KlippoApp, ctx: &Context) {
    let Some(ref mut confirm) = app.delete_confirm else { return };
    let count = confirm.ids.len();
    let any_completed = confirm
        .ids
        .iter()
        .any(|id| app.downloads.iter().any(|d| d.id == *id && d.status == DownloadStatus::Completed));
    let any_unfinished = confirm
        .ids
        .iter()
        .any(|id| app.downloads.iter().any(|d| d.id == *id && d.status != DownloadStatus::Completed));
    let first_name = app
        .downloads
        .iter()
        .find(|d| Some(&d.id) == confirm.ids.first())
        .map(|d| d.filename.clone())
        .unwrap_or_default();

    let mut answer: Option<bool> = None;
    let mut is_open = true;
    dialog_window("Confirm deletion of downloads", ctx, 373.0)
        .id(egui::Id::new("delete_confirm"))
        .open(&mut is_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(373.0);
            ui.set_max_width(373.0);
            // A different question shows when every selected file is unfinished.
            let what = if count == 1 { format!("\"{}\"", first_name) } else { format!("{} downloads", count) };
            let question = if any_unfinished && !any_completed {
                format!("{} {} not complete. Are you sure you want to delete {} from the list of downloads?",
                    what, if count == 1 { "is" } else { "are" }, if count == 1 { "it" } else { "them" })
            } else {
                format!("Are you sure you want to delete {} from the list of downloads?", what)
            };
            ui.add(egui::Label::new(RichText::new(question)).wrap());
            ui.add_space(6.0);
            if any_unfinished {
                ui.label(RichText::new("Partly downloaded data of unfinished files is removed.").size(crate::gui::theme::LIST_FONT));
            }
            if any_completed {
                ui.checkbox(&mut confirm.delete_files, "Delete completely downloaded files from your hard disk as well. Be careful!");
            }
            ui.checkbox(&mut confirm.dont_ask, "Don't show this dialog again");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("No").clicked() {
                        answer = Some(false);
                    }
                    if ui.button(RichText::new("Yes")).clicked() {
                        answer = Some(true);
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });

    if !is_open {
        answer = Some(false);
    }
    match answer {
        Some(true) => {
            let confirm = app.delete_confirm.take().unwrap();
            if confirm.dont_ask {
                app.settings.confirm_delete = false;
                app.save_settings_only();
            }
            app.delete_downloads(&confirm.ids, confirm.delete_files);
        }
        Some(false) => app.delete_confirm = None,
        None => {}
    }
}

/// Asked when the window closes while files are downloading.
pub fn render_exit_confirm(app: &mut KlippoApp, ctx: &Context) {
    if !app.show_exit_confirm {
        return;
    }
    let active = app.downloads.iter().filter(|d| d.status.is_active()).count();
    let mut answer: Option<bool> = None;
    let mut window_open = true;
    dialog_window("Exit Klippo", ctx, 380.0)
        .id(egui::Id::new("exit_confirm"))
        .open(&mut window_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.set_max_width(380.0);
            ui.label(RichText::new(format!("{} download(s) are in progress.", active)));
            ui.label("They stop now and you can resume them the next time.");
            ui.add_space(4.0);
            let mut ask = app.settings.confirm_exit;
            if ui.checkbox(&mut ask, "Ask me before exit").changed() {
                app.settings.confirm_exit = ask;
                app.save_settings_only();
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                    if ui.button(RichText::new("Exit")).clicked() {
                        answer = Some(true);
                    }
                });
            });
        });
    if !window_open {
        answer = Some(false);
    }
    match answer {
        Some(true) => {
            app.show_exit_confirm = false;
            app.exit_confirmed = true;
            app.pending_exit = true;
        }
        Some(false) => {
            app.show_exit_confirm = false;
            app.exit_requested = false;
        }
        None => {}
    }
}

/// "Export download list": which files, then where.
pub fn render_export_dialog(app: &mut KlippoApp, ctx: &Context) {
    let Some(ref mut dialog) = app.export_dialog else { return };
    let mut ok = false;
    let mut cancel = false;
    let selected = app.selection.len();
    let queues: Vec<(u32, String)> = app.settings.queues.iter().map(|q| (q.id, q.name.clone())).collect();
    let mut window_open = true;
    dialog_window("Export download list", ctx, 284.0)
        .id(egui::Id::new("export_dialog"))
        .open(&mut window_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(284.0);
            ui.set_max_width(284.0);
            let format = match dialog.format {
                ExportFormat::Ef2 => "Export file (.ef2): addresses with referrer, cookies and logins",
                ExportFormat::Text => "Text file: one address per line",
            };
            ui.label(RichText::new(format).size(crate::gui::theme::LIST_FONT));
            ui.add_space(4.0);
            let queue_scope = match dialog.scope {
                ExportScope::Queue(q) => q,
                _ => crate::gui::queues::MAIN_QUEUE,
            };
            ui.horizontal(|ui| {
                ui.radio_value(&mut dialog.scope, ExportScope::Queue(queue_scope), "Export download queue:");
                let shown = queues.iter().find(|(id, _)| *id == queue_scope).map(|(_, n)| n.clone()).unwrap_or_default();
                let combo = egui::ComboBox::from_id_salt("export_queue")
                    .selected_text(shown.clone())
                    .show_ui(ui, |ui| {
                        for (id, name) in &queues {
                            ui.selectable_value(&mut dialog.scope, ExportScope::Queue(*id), name);
                        }
                    });
                name_combo(&combo, "Download queue", &shown);
            });
            ui.add_enabled_ui(selected > 0, |ui| {
                ui.radio_value(&mut dialog.scope, ExportScope::Selected, format!("Export selected files ({})", selected));
            });
            ui.radio_value(&mut dialog.scope, ExportScope::All, "Export all files");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button(RichText::new("OK")).clicked() {
                        ok = true;
                    }
                });
            });
        });
    if ok {
        let dialog = app.export_dialog.take().unwrap();
        let (name, filter, ext) = match dialog.format {
            ExportFormat::Ef2 => ("Klippo links.ef2", "Export file", "ef2"),
            ExportFormat::Text => ("Klippo links.txt", "Text file", "txt"),
        };
        if let Some(path) = rfd::FileDialog::new().set_file_name(name).add_filter(filter, &[ext]).save_file() {
            app.export_list(&path, dialog.format, dialog.scope);
        }
    } else if cancel || !window_open {
        app.export_dialog = None;
    }
}

/// "Connecting to the site": the server asked for a login.
pub fn render_login_prompt(app: &mut KlippoApp, ctx: &Context) {
    let Some(ref mut prompt) = app.login_prompt else { return };
    let name = app.downloads.iter().find(|d| d.id == prompt.id).map(|d| d.filename.clone()).unwrap_or_default();
    let mut ok = false;
    let mut cancel = false;
    let mut window_open = true;
    dialog_window("Connecting to the site", ctx, 366.0)
        .id(egui::Id::new("login_prompt"))
        .open(&mut window_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(366.0);
            ui.set_max_width(366.0);
            ui.label(RichText::new(format!("The server needs a login to download \"{}\".", name)));
            ui.label("Type your user name and password.");
            if !prompt.realm.is_empty() {
                ui.label(format!("Realm: {}", prompt.realm));
            }
            ui.add_space(4.0);
            egui::Grid::new("login_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("User name:");
                ui.add(egui::TextEdit::singleline(&mut prompt.username).margin(crate::gui::theme::EDIT_MARGIN).desired_width(240.0));
                ui.end_row();
                ui.label("Password:");
                ui.add(egui::TextEdit::singleline(&mut prompt.password).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(240.0));
                ui.end_row();
            });
            ui.checkbox(&mut prompt.save, "Save this password in the site logins list");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.add_enabled(!prompt.username.is_empty(), egui::Button::new(RichText::new("OK"))).clicked() {
                        ok = true;
                    }
                });
            });
        });
    if ok {
        let prompt = app.login_prompt.take().unwrap();
        app.apply_login(prompt);
    } else if cancel || !window_open {
        app.login_prompt = None;
    }
}

/// "Download limits exceeded!"
pub fn render_limit_dialog(app: &mut KlippoApp, ctx: &Context) {
    let Some(ref dialog) = app.limit_dialog else { return };
    let (warn, downloaded) = (dialog.warn, dialog.downloaded);
    let hours = app.settings.download_limit_hours;
    let limit = app.settings.download_limit_mb;
    let mut stop = false;
    let mut ignore = false;
    let mut close = false;
    let mut window_open = true;
    dialog_window("Download limits exceeded!", ctx, 400.0)
        .id(egui::Id::new("limit_dialog"))
        .open(&mut window_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(400.0);
            ui.set_max_width(400.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "{} were downloaded in the last {} hours. The limit is {} MB every {} hours.",
                        format_bytes(downloaded),
                        hours,
                        limit,
                        hours
                    )),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            if warn {
                ui.label("Stop the downloads now, or ignore the limit until this period ends?");
            } else {
                ui.label("The downloads were stopped. They can start again when the period ends.");
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if warn {
                    if ui.button(RichText::new("Stop downloads")).clicked() {
                        stop = true;
                    }
                    if ui.button("Ignore the limit").clicked() {
                        ignore = true;
                    }
                } else if ui.button("OK").clicked() {
                    close = true;
                }
            });
        });
    if !window_open {
        // The close box does not stop anything.
        if warn {
            ignore = true;
        } else {
            close = true;
        }
    }
    if stop {
        app.stop_all_downloads();
        app.limit_dialog = None;
    } else if ignore {
        app.limit_ignored_until = Some(Instant::now() + std::time::Duration::from_secs(hours.max(1) as u64 * 3600));
        app.limit_dialog = None;
    } else if close {
        app.limit_dialog = None;
    }
}

/// Grabber for HLS (M3U8) streams.
pub fn render_grabber_dialog(app: &mut KlippoApp, ctx: &Context) {
    if !app.grabber.open {
        return;
    }
    if app.grabber.dir.as_os_str().is_empty() {
        app.grabber.dir = app.settings.category_dir(Category::Video);
    }
    let mut is_open = true;
    let mut submit: Option<StartMode> = None;
    dialog_window("Klippo Video Grabber (HLS / M3U8)", ctx, 520.0)
        .id(egui::Id::new("grabber_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui.set_max_width(520.0);
            ui.label("Address of the M3U8 playlist (master or media playlist):");
            let edit = fixed_width(ui, f32::INFINITY, |ui| {
                ui.add(egui::TextEdit::singleline(&mut app.grabber.url).margin(crate::gui::theme::EDIT_MARGIN).hint_text("https://example.com/video/master.m3u8").desired_width(f32::INFINITY))
            });
            if edit.changed() && app.grabber.filename.is_empty() {
                let stem = crate::core::http::filename_from_url(&app.grabber.url)
                    .and_then(|f| std::path::Path::new(&f).file_stem().map(|s| s.to_string_lossy().to_string()))
                    .unwrap_or_else(|| "video".to_string());
                app.grabber.filename = format!("{}.ts", stem);
            }
            ui.add_space(6.0);
            ui.label("Save as:");
            fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut app.grabber.filename).margin(crate::gui::theme::EDIT_MARGIN).hint_text("video.ts").desired_width(f32::INFINITY)));
            ui.horizontal(|ui| {
                let mut dir = app.grabber.dir.to_string_lossy().to_string();
                if fixed_width(ui, 420.0, |ui| ui.add(egui::TextEdit::singleline(&mut dir).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY))).changed() {
                    app.grabber.dir = std::path::PathBuf::from(dir);
                }
                if ui.button("Browse...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        app.grabber.dir = folder;
                    }
                }
            });
            ui.label(RichText::new("The best quality variant is chosen. Klippo decrypts AES-128 streams; DRM streams (SAMPLE-AES, FairPlay, Widevine) are not supported.").size(crate::gui::theme::LIST_FONT).color(Color32::from_rgb(120, 120, 120)));
            ui.add_space(8.0);
            let valid = app.grabber.url.trim().starts_with("http") && !app.grabber.filename.trim().is_empty();
            ui.horizontal(|ui| {
                if ui.add_enabled(valid, egui::Button::new(RichText::new("🎬 Grab & Download"))).clicked() {
                    submit = Some(StartMode::Now);
                }
                if ui.add_enabled(valid, egui::Button::new("Download Later")).clicked() {
                    submit = Some(StartMode::Queue(crate::gui::queues::MAIN_QUEUE));
                }
                if ui.button("Cancel").clicked() {
                    app.grabber.open = false;
                }
            });
        });

    if let Some(mode) = submit {
        let mut filename = crate::core::http::sanitize_filename(app.grabber.filename.trim());
        if std::path::Path::new(&filename).extension().is_none() {
            filename.push_str(".ts");
        }
        let new = NewDownload {
            url: app.grabber.url.trim().to_string(),
            filename,
            dir: app.grabber.dir.clone(),
            category: Category::Video,
            connections: app.settings.max_connections.min(16),
            kind: DownloadKind::Hls,
            ..Default::default()
        };
        let id = app.add_item(new, mode);
        if mode == StartMode::Now && app.settings.show_progress_window {
            app.show_progress_window(id);
        }
        app.grabber = Default::default();
    } else if !is_open {
        app.grabber.open = false;
    }
}
