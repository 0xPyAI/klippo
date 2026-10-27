//! The two-step add: "Enter new address to download" (address and login),
//! then "Download File Info" (category, save-as path, description, size).

use std::path::PathBuf;

use eframe::egui::{self, Color32, Context, PopupCloseBehavior, RichText, Sense, Vec2};

use super::classic_ui::{button, check_group, name_combo, note, right_buttons};
use super::{dialog_window, fixed_width};
use crate::gui::icons::{self, SmallIcon};
use crate::gui::state::{format_bytes, AddStep, Category, DownloadKind, DuplicateAction, KlippoApp, ProbeState, StartMode};

pub fn render_add_url_modal(app: &mut KlippoApp, ctx: &Context) {
    if !app.add.open {
        return;
    }
    match app.add.step {
        AddStep::Address => address_step(app, ctx),
        AddStep::Info => info_step(app, ctx),
    }
}

fn url_ok(url: &str) -> bool {
    url::Url::parse(url.trim()).map(|u| matches!(u.scheme(), "http" | "https" | "ftp")).unwrap_or(false)
}

/// "Enter new address to download": the address with OK / Cancel, and an optional login.
fn address_step(app: &mut KlippoApp, ctx: &Context) {
    let mut is_open = true;
    let mut ok = false;
    let mut cancel = false;
    dialog_window("Enter new address to download", ctx, 506.0)
        .id(egui::Id::new("add_url_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .bring_to_front(app.add.to_front)
        .show(ctx, |ui| {
            ui.set_width(506.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(422.0);
                    ui.horizontal(|ui| {
                        ui.label("Address");
                        let edit = fixed_width(ui, f32::INFINITY, |ui| {
                            ui.add(egui::TextEdit::singleline(&mut app.add.url).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY))
                        });
                        if edit.changed() {
                            app.on_add_url_changed();
                        }
                        if !app.add.focused {
                            edit.request_focus();
                            app.add.focused = true;
                        }
                    });
                    check_group(ui, &mut app.add.auth_enabled, "Use authorization", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Login");
                            fixed_width(ui, 110.0, |ui| ui.add(egui::TextEdit::singleline(&mut app.add.username).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                            ui.add_space(12.0);
                            ui.label("Password");
                            fixed_width(ui, 110.0, |ui| ui.add(egui::TextEdit::singleline(&mut app.add.password).margin(crate::gui::theme::EDIT_MARGIN).password(true).desired_width(f32::INFINITY)));
                        });
                    });
                });
                ui.vertical(|ui| {
                    ok = button(ui, url_ok(&app.add.url), "OK").clicked();
                    cancel = button(ui, true, "Cancel").clicked();
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && url_ok(&app.add.url) {
                ok = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
    app.add.to_front = false;
    if ok {
        app.add.step = AddStep::Info;
    } else if cancel || !is_open {
        app.close_add_dialog();
    }
}

/// "Download File Info".
fn info_step(app: &mut KlippoApp, ctx: &Context) {
    let mut is_open = true;
    let mut submit: Option<StartMode> = None;
    let mut cancel = false;
    let to_front = std::mem::take(&mut app.add.to_front);
    dialog_window("Download File Info", ctx, 516.0)
        .id(egui::Id::new("download_file_info"))
        .open(&mut is_open)
        .resizable(false)
        .minimizable(true)
        .bring_to_front(to_front)
        .show(ctx, |ui| {
            ui.set_width(516.0);
            ui.set_max_width(516.0);
            let label_w = 70.0;
            ui.horizontal(|ui| {
                ui.add_sized([label_w, 20.0], egui::Label::new("URL"));
                let edit = fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut app.add.url).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                if edit.changed() {
                    app.on_add_url_changed();
                }
            });
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(410.0);
                    ui.horizontal(|ui| {
                        ui.add_sized([label_w, 20.0], egui::Label::new("Category"));
                        let combo = egui::ComboBox::from_id_salt("dfi_category").width(200.0).selected_text(app.add.category.name()).show_ui(ui, |ui| {
                            for c in Category::all() {
                                if ui.selectable_label(app.add.category == c, c.name()).clicked() {
                                    app.add.category = c;
                                    app.add.dir_edited = false;
                                    app.update_add_dir_for_category();
                                }
                            }
                        });
                        name_combo(&combo, "Category", &app.add.category.name());
                        let plus = ui.add(egui::Button::new("+").min_size(Vec2::new(26.0, 22.0))).on_hover_text("Add a new category");
                        plus.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Add new category"));
                        if plus.clicked() {
                            app.open_new_category(crate::gui::views::category_dialog::CategoryFrom::AddDialog);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_sized([label_w, 20.0], egui::Label::new("Save As"));
                        // The folder and the file name show as one path.
                        let mut full = app.add.dir.join(&app.add.filename).to_string_lossy().to_string();
                        let edit = fixed_width(ui, 292.0, |ui| ui.add(egui::TextEdit::singleline(&mut full).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
                        if edit.changed() {
                            set_save_as(app, PathBuf::from(full));
                        }
                        if ui.add(egui::Button::new("...").min_size(Vec2::new(30.0, 22.0))).on_hover_text("Choose where to save the file").clicked() {
                            if let Some(path) = rfd::FileDialog::new().set_directory(&app.add.dir).set_file_name(&app.add.filename).save_file() {
                                set_save_as(app, path);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(label_w + 6.0);
                        ui.checkbox(&mut app.add.remember_path, format!("Remember this path for \"{}\" category", app.add.category.name()));
                    });
                });
                // The file's icon and size, at the right.
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                    icons::small(ui.painter(), rect, SmallIcon::Category(app.add.category));
                    let size = match (&app.add.youtube, &app.add.probe) {
                        (Some(yt), _) => yt.total_size().map(format_bytes),
                        (None, ProbeState::Done(info)) => info.total_size.map(format_bytes),
                        (None, ProbeState::Pending { .. }) => Some("...".to_string()),
                        _ => None,
                    };
                    ui.label(size.unwrap_or_else(|| "Unknown".to_string()));
                });
            });
            ui.horizontal(|ui| {
                ui.add_sized([label_w, 20.0], egui::Label::new("Description"));
                fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut app.add.description).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
            });

            notes(app, ui);

            ui.add_space(8.0);
            let valid = url_ok(&app.add.url) && !app.add.filename.trim().is_empty();
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    if button(ui, true, "Cancel").clicked() {
                        cancel = true;
                    }
                    let later = button(ui, valid, "Download Later").on_hover_text("Add the file to a queue for downloading later");
                    let popup = ui.make_persistent_id("download_later_queues");
                    if later.clicked() {
                        if app.settings.queue_panel_on_later && app.settings.queues.len() > 1 {
                            ui.memory_mut(|m| m.toggle_popup(popup));
                        } else {
                            submit = Some(StartMode::Queue(app.add.queue));
                        }
                    }
                    // The queue selection panel.
                    egui::popup_below_widget(ui, popup, &later, PopupCloseBehavior::CloseOnClickOutside, |ui| {
                        ui.set_min_width(170.0);
                        ui.label("Add to queue:");
                        let queues: Vec<(u32, String)> = app.settings.queues.iter().map(|q| (q.id, q.name.clone())).collect();
                        for (qid, name) in queues {
                            if ui.button(name).clicked() {
                                app.add.queue = qid;
                                submit = Some(StartMode::Queue(qid));
                            }
                        }
                    });
                    if button(ui, valid, "Start Download").clicked() {
                        submit = Some(StartMode::Now);
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid && submit.is_none() {
                submit = Some(StartMode::Now);
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
    if let Some(mode) = submit {
        app.submit_add_dialog(mode);
    } else if cancel || !is_open {
        app.close_add_dialog();
    }
}

fn set_save_as(app: &mut KlippoApp, path: PathBuf) {
    if let Some(name) = path.file_name() {
        app.add.filename = name.to_string_lossy().to_string();
        app.add.filename_edited = true;
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        app.add.dir = dir.to_path_buf();
        app.add.dir_edited = true;
    }
}

/// Server problems, stream kinds and duplicates, under the fields.
fn notes(app: &mut KlippoApp, ui: &mut egui::Ui) {
    if let ProbeState::Failed { error, .. } = &app.add.probe {
        ui.label(RichText::new(error).size(crate::gui::theme::LIST_FONT).color(Color32::from_rgb(200, 40, 40)));
    }
    if app.add.audio_url.is_some() {
        note(ui, "Video with a separate audio stream: both are downloaded and joined into one file.");
    } else if app.add.kind == DownloadKind::Hls && app.add.youtube.is_none() {
        note(ui, "Video stream: the parts are downloaded and joined into one file.");
    }
    if let Some(ref yt) = app.add.youtube {
        let what = if yt.live {
            crate::core::hls::LIVE_STREAM
        } else if yt.video.is_some() {
            "YouTube video: the video and audio streams are downloaded and joined into one file."
        } else {
            "YouTube audio only (M4A)."
        };
        note(ui, what);
    }
    if app.add.referer.is_some() || app.add.cookies.is_some() {
        note(ui, "Sent by the browser (cookies and referrer are used)");
    }
    // "Duplicate download link".
    if let Some(name) = app.add_dialog_duplicate().map(|d| d.filename.clone()) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(format!("This file already exists in your download list (\"{}\"). Choose what to do:", name));
            for action in [DuplicateAction::Numbered, DuplicateAction::Overwrite, DuplicateAction::ResumeExisting] {
                ui.radio_value(&mut app.add.dup_action, action, action.label());
            }
            ui.checkbox(&mut app.add.dup_remember, "Remember my selection and don't show this again");
        });
    }
}
