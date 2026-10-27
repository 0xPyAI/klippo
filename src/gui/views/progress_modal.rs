use eframe::egui::{self, Color32, Context, Sense, Stroke, Vec2};

use super::dialog_window_offset;
use super::classic_ui::{button, list_view, note, right_buttons, tab_page, tab_rows};
use crate::gui::state::{format_bytes, DownloadKind, DownloadStatus, KlippoApp};

enum ProgressAction {
    Pause(u64),
    Resume(u64),
    Cancel(u64),
    Hide(u64),
    SetLimit(u64, u64),
    /// "Remember the speed limit for this server".
    Remember(u64, bool),
}

/// One "Download progress" window per open download.
pub fn render_progress_windows(app: &mut KlippoApp, ctx: &Context) {
    let ids = app.progress_windows.clone();
    let mut actions: Vec<ProgressAction> = Vec::new();

    for (index, id) in ids.into_iter().enumerate() {
        let Some(item) = app.item(id) else {
            app.hide_progress_window(id);
            continue;
        };
        let frac = item.progress_fraction();
        let title = if item.total_size.is_some() {
            format!("{:.0}% {}", frac * 100.0, item.filename)
        } else {
            item.filename.clone()
        };

        let mut is_open = true;
        let limit_key = egui::Id::new(("progress_limit", id));
        // "<< Hide details" folds the limiter and the connection list away.
        let details_key = egui::Id::new(("progress_details", id));
        let mut details: bool = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(details_key, || true));
        let mut limit_kbps: u64 = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(limit_key, || {
            if item.speed_limit_kbps > 0 { item.speed_limit_kbps } else { 512 }
        }));
        let mut limit_on = item.speed_limit_kbps > 0;
        let server = crate::gui::state::server_of(&item.url);
        let mut remember = server.as_ref().is_some_and(|s| app.settings.server_speed_limits.contains_key(s));

        let tab_key = egui::Id::new(("progress_tab", id));
        let mut tab: u8 = ctx.data(|d| d.get_temp(tab_key)).unwrap_or(0);
        let mut show_complete = app.settings.show_complete_window;
        dialog_window_offset(title, ctx, index, 478.0)
            // The title changes every frame; a fixed id keeps position and size.
            .id(egui::Id::new(("progress_window", id)))
            .open(&mut is_open)
            .resizable(false)
            .default_width(478.0)
            .minimizable(true)
            .bring_to_front(app.progress_front == Some(id))
            .show(ctx, |ui| {
                ui.set_width(478.0);
                tab_rows(ui, &[&[(0u8, "Download status"), (1u8, "Speed Limiter"), (2u8, "Options on completion")]], &mut tab);
                tab_page(ui, 140.0, |ui| match tab {
                    0 => {
                        ui.add(egui::Label::new(&item.url).truncate()).on_hover_text(&item.url);
                        ui.add_space(4.0);
                        egui::Grid::new(("idm_dl_info_grid", id)).num_columns(2).spacing([24.0, 2.0]).show(ui, |ui| {
                            let status_text = match (item.status, &item.error_message) {
                                (DownloadStatus::Failed, Some(e)) => format!("Error: {}", e),
                                (DownloadStatus::Downloading, _) => "Receiving data...".to_string(),
                                (s, _) => s.display_text().to_string(),
                            };
                            ui.label("Status");
                            ui.add(egui::Label::new(status_text).truncate());
                            ui.end_row();
                            ui.label("File size");
                            ui.label(item.size_string());
                            ui.end_row();
                            ui.label("Downloaded");
                            if item.total_size.is_some() {
                                ui.label(format!("{} ( {:.2} % )", format_bytes(item.downloaded_bytes), frac * 100.0));
                            } else {
                                ui.label(format_bytes(item.downloaded_bytes));
                            }
                            ui.end_row();
                            ui.label("Transfer rate");
                            ui.label(item.speed_string());
                            ui.end_row();
                            ui.label("Time left");
                            ui.label(item.eta_string());
                            ui.end_row();
                            ui.label("Resume capability");
                            ui.label(if item.resumable || item.kind != DownloadKind::Http { "Yes" } else { "No" });
                            ui.end_row();
                        });
                    }
                    1 => {
                        let enabled = item.kind == DownloadKind::Http;
                        ui.add_enabled_ui(enabled, |ui| {
                            let toggled = ui.checkbox(&mut limit_on, "Use speed limiter").changed();
                            let mut changed = false;
                            ui.horizontal(|ui| {
                                ui.add_space(20.0);
                                ui.label("Maximum download speed");
                                changed = ui.add_enabled(limit_on, egui::DragValue::new(&mut limit_kbps).speed(10).range(1..=1_000_000)).changed();
                                ui.label("KBytes/sec");
                            });
                            if toggled || (changed && limit_on) {
                                actions.push(ProgressAction::SetLimit(id, if limit_on { limit_kbps } else { 0 }));
                            }
                            let tip = format!("New downloads from {} get this limit", server.as_deref().unwrap_or("this server"));
                            if ui.add_enabled(limit_on && server.is_some(), egui::Checkbox::new(&mut remember, "Remember the speed limit for this server"))
                                .on_hover_text(tip)
                                .on_disabled_hover_text("Turn on the speed limiter first")
                                .changed()
                            {
                                actions.push(ProgressAction::Remember(id, remember));
                            }
                        });
                        if !enabled {
                            note(ui, "A stream download is limited by the global Speed Limiter (Downloads > Speed Limiter).");
                        }
                    }
                    _ => {
                        ui.checkbox(&mut show_complete, "Show download complete dialog");
                        ui.add_enabled(false, egui::Checkbox::new(&mut false, "Hang up modem when done"));
                        ui.add_enabled(false, egui::Checkbox::new(&mut false, "Exit Klippo when done"));
                        note(ui, "To hang up or exit after several files, put them in a queue (Scheduler).");
                    }
                });
                ui.add_space(6.0);

                // Windows' progress bar.
                let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
                let look = crate::gui::theme::current(ui.ctx());
                ui.painter().rect_filled(bar_rect, 0.0, look.progress_bg);
                ui.painter().rect_stroke(bar_rect, 0.0, Stroke::new(1.0_f32, look.border_color));
                if frac > 0.0 {
                    let fill_rect = egui::Rect::from_min_size(bar_rect.min, Vec2::new(bar_rect.width() * frac, bar_rect.height()));
                    let fill_color = match item.status {
                        DownloadStatus::Paused | DownloadStatus::Cancelled => Color32::from_rgb(218, 203, 38),
                        DownloadStatus::Failed => Color32::from_rgb(218, 38, 38),
                        _ => look.progress_fill,
                    };
                    ui.painter().rect_filled(fill_rect.shrink(1.0), 0.0, fill_color);
                }
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    let label = if details { "<< Hide details" } else { "Show details >>" };
                    if button(ui, true, label).clicked() {
                        details = !details;
                    }
                    right_buttons(ui, |ui| {
                        if button(ui, item.status != DownloadStatus::Completed, "Cancel").on_hover_text("Stop the download (it can be resumed later)").clicked() {
                            actions.push(ProgressAction::Cancel(id));
                        }
                        if item.status.is_active() {
                            if button(ui, true, "Pause").clicked() {
                                actions.push(ProgressAction::Pause(id));
                            }
                        } else if button(ui, item.status.can_start(), "Resume").clicked() {
                            actions.push(ProgressAction::Resume(id));
                        }
                    });
                });

                if details {
                    ui.add_space(6.0);
                    ui.label("Start positions and download progress by connections");
                    if item.kind == DownloadKind::Http {
                        segment_map(ui, item);
                        ui.add_space(3.0);
                        let segments = item.segments.clone();
                        let mut sel = None;
                        list_view(ui, "segments", &[("N.", 40.0), ("Downloaded", 120.0), ("Info", 200.0)], segments.len(), 126.0, &mut sel, |ui, row, col| {
                            let seg = &segments[row];
                            match col {
                                0 => ui.label((row + 1).to_string()),
                                1 => ui.label(format_bytes(seg.downloaded_bytes())),
                                _ => ui.label(if seg.is_completed {
                                    "Complete"
                                } else if seg.is_active {
                                    "Receiving data..."
                                } else {
                                    "Connecting..."
                                }),
                            };
                        });
                    } else if item.kind == DownloadKind::Youtube {
                        note(ui, "YouTube: the video and audio streams are downloaded together and joined into one file at the end.");
                    } else {
                        note(ui, "Stream: the media parts are downloaded in parallel and joined at the end.");
                    }
                }
            });
        ctx.data_mut(|d| d.insert_temp(tab_key, tab));
        if show_complete != app.settings.show_complete_window {
            app.settings.show_complete_window = show_complete;
            app.save_settings_only();
        }

        ctx.data_mut(|d| {
            d.insert_temp(limit_key, limit_kbps);
            d.insert_temp(details_key, details);
        });
        if !is_open {
            actions.push(ProgressAction::Hide(id));
        }
    }

    app.progress_front = None;
    for action in actions {
        match action {
            ProgressAction::Pause(id) => app.pause_download(id),
            ProgressAction::Resume(id) => app.resume_download(id),
            ProgressAction::Cancel(id) => {
                app.pause_download(id);
                app.hide_progress_window(id);
            }
            ProgressAction::Hide(id) => app.hide_progress_window(id),
            ProgressAction::SetLimit(id, kbps) => app.set_item_speed_limit(id, kbps),
            ProgressAction::Remember(id, on) => app.remember_server_limit(id, on),
        }
    }
}

/// A thin bar that shows every connection's range and progress at once.
fn segment_map(ui: &mut egui::Ui, item: &crate::gui::state::DownloadItem) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 12.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(215, 220, 228));
    let Some(total) = item.total_size.filter(|t| *t > 0) else { return };
    let scale = rect.width() / total as f32;
    for seg in &item.segments {
        if seg.is_unbounded() {
            continue;
        }
        let x0 = rect.left() + seg.start as f32 * scale;
        let done = seg.downloaded_bytes() as f32 * scale;
        let fill = egui::Rect::from_min_size(egui::pos2(x0, rect.top()), Vec2::new(done.max(0.0), rect.height()));
        let color = if seg.is_completed { Color32::from_rgb(46, 175, 80) } else { Color32::from_rgb(33, 150, 243) };
        ui.painter().rect_filled(fill, 0.0, color);
        ui.painter().line_segment(
            [egui::pos2(x0, rect.top()), egui::pos2(x0, rect.bottom())],
            Stroke::new(1.0_f32, Color32::from_rgb(90, 90, 90)),
        );
    }
}
