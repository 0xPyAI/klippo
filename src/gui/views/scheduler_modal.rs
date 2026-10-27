//! The "Scheduler": the queues on the left; the selected queue's name, its
//! "Schedule" and "Files in the queue" tabs on the right; Start now, Stop,
//! Help, Apply and Close at the bottom.

use eframe::egui::{self, Color32, Context, Sense, Ui, Vec2};

use super::classic_ui::{button, list_view, right_buttons, tab_page, tab_rows};
use super::{dialog_window, fixed_width};
use crate::gui::icons::{self, PageIcon, SmallIcon};
use crate::gui::queues::{normalize_time, QueueConfig, MAIN_QUEUE};
use crate::gui::state::{KlippoApp, SchedulerTab};
use crate::gui::theme;

pub fn render_scheduler_modal(app: &mut KlippoApp, ctx: &Context) {
    if !app.show_scheduler_dialog {
        return;
    }
    if !app.settings.queues.iter().any(|q| q.id == app.scheduler_queue) {
        app.scheduler_queue = MAIN_QUEUE;
    }
    let mut is_open = true;
    let mut close = false;
    dialog_window("Scheduler", ctx, 592.0)
        .id(egui::Id::new("scheduler_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(592.0);
            ui.set_max_width(592.0);
            ui.horizontal_top(|ui| {
                queue_column(app, ui);
                // A vertical separator would stretch the dialog to the window height.
                let (line, _) = ui.allocate_exact_size(egui::vec2(9.0, 420.0), Sense::hover());
                ui.painter().vline(line.center().x, line.y_range(), egui::Stroke::new(1.0_f32, theme::current(ui.ctx()).border_color));
                ui.vertical(|ui| {
                    ui.set_width(410.0);
                    let qid = app.scheduler_queue;
                    let Some(idx) = app.settings.queues.iter().position(|q| q.id == qid) else { return };
                    ui.vertical_centered(|ui| ui.label(&app.settings.queues[idx].name));
                    tab_rows(ui, &[&[(SchedulerTab::Schedule, "Schedule"), (SchedulerTab::FilesQueue, "Files in the queue")]], &mut app.scheduler_active_tab);
                    tab_page(ui, 370.0, |ui| match app.scheduler_active_tab {
                        SchedulerTab::Schedule => schedule_tab(&mut app.settings.queues[idx], ui),
                        SchedulerTab::FilesQueue => files_tab(app, qid, idx, ui),
                    });
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(170.0);
                let qid = app.scheduler_queue;
                let running = app.is_queue_running(qid);
                if button(ui, !running, "Start now").clicked() {
                    app.save_settings_only();
                    app.start_queue(qid);
                }
                if button(ui, running, "Stop").clicked() {
                    app.stop_queue(qid);
                }
                if button(ui, true, "Help").clicked() {
                    if let Some(readme) = std::env::current_exe().ok().and_then(|exe| exe.ancestors().map(|a| a.join("README.md")).find(|p| p.exists())) {
                        let _ = open::that(readme);
                    }
                }
                if button(ui, true, "Apply").clicked() {
                    app.save_settings_only();
                    app.set_status("Scheduler settings saved");
                }
                if button(ui, true, "Close").clicked() {
                    close = true;
                }
            });
        });
    queue_name_dialog(app, ctx);
    if close || !is_open {
        app.save_settings_only();
        app.show_scheduler_dialog = false;
    }
}

/// The "Queues" list with New queue / Delete.
fn queue_column(app: &mut KlippoApp, ui: &mut Ui) {
    let look = theme::current(ui.ctx());
    ui.vertical(|ui| {
        ui.set_width(161.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
            icons::page(ui.painter(), rect, PageIcon::Scheduler);
            ui.add_space(8.0);
            ui.label("Queues");
        });
        egui::Frame::none().fill(look.list_bg).stroke(egui::Stroke::new(1.0_f32, look.border_color)).inner_margin(egui::Margin::same(3.0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(330.0);
            let queues: Vec<(u32, String)> = app.settings.queues.iter().map(|q| (q.id, q.name.clone())).collect();
            for (id, name) in queues {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 19.0), Sense::click());
                if app.scheduler_queue == id {
                    ui.painter().rect_filled(rect, 0.0, look.tree_selection);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 0.0, ui.visuals().widgets.hovered.bg_fill);
                }
                icons::small(ui.painter(), egui::Rect::from_min_size(rect.min + egui::vec2(2.0, 1.5), Vec2::splat(16.0)), SmallIcon::Queue);
                let running = if app.is_queue_running(id) { "  (running)" } else { "" };
                ui.painter().text(rect.min + egui::vec2(22.0, 9.5), egui::Align2::LEFT_CENTER, format!("{}{}", name, running), egui::FontId::proportional(crate::gui::theme::LIST_FONT), look.text_primary);
                if resp.clicked() {
                    app.scheduler_queue = id;
                }
                resp.context_menu(|ui| {
                    crate::gui::theme::menu_style(ui);
                    if ui.add_enabled(id != MAIN_QUEUE, egui::Button::new("Rename...")).clicked() {
                        app.scheduler_queue = id;
                        app.queue_name_edit = Some((Some(id), name.clone()));
                        ui.close_menu();
                    }
                });
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if button(ui, true, "New queue").clicked() {
                app.queue_name_edit = Some((None, String::new()));
            }
            let can_delete = app.scheduler_queue != MAIN_QUEUE;
            if button(ui, can_delete, "Delete").on_disabled_hover_text("The main queue cannot be deleted").clicked() {
                let id = app.scheduler_queue;
                app.delete_queue(id);
                app.scheduler_queue = MAIN_QUEUE;
            }
        });
    });
}

/// "Enter queue name".
fn queue_name_dialog(app: &mut KlippoApp, ctx: &Context) {
    let Some((target, mut name)) = app.queue_name_edit.take() else { return };
    let mut open = true;
    let mut done = false;
    let mut cancel = false;
    dialog_window("Enter queue name", ctx, 244.0)
        .id(egui::Id::new("queue_name_dialog"))
        .open(&mut open)
        .resizable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_width(244.0);
            let edit = ui.add(egui::TextEdit::singleline(&mut name).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY));
            edit.request_focus();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    cancel = button(ui, true, "Cancel").clicked();
                    done = button(ui, !name.trim().is_empty(), "OK").clicked();
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty() {
                done = true;
            }
        });
    if done {
        match target {
            Some(id) => {
                if let Some(q) = app.settings.queues.iter_mut().find(|q| q.id == id) {
                    q.name = name.trim().to_string();
                }
            }
            None => app.scheduler_queue = app.add_queue(name.trim()),
        }
    } else if open && !cancel {
        app.queue_name_edit = Some((target, name));
    }
}

fn time_field(ui: &mut Ui, value: &mut String) {
    let valid = normalize_time(value).is_some();
    let edit = egui::TextEdit::singleline(value).margin(crate::gui::theme::EDIT_MARGIN).desired_width(70.0).hint_text("HH:MM");
    let edit = if valid { edit } else { edit.text_color(Color32::from_rgb(220, 53, 69)) };
    ui.add(edit).on_hover_text("24-hour time, HH:MM");
}

fn schedule_tab(q: &mut QueueConfig, ui: &mut Ui) {
    // Older settings kept "every day" separately from the day boxes.
    if q.daily {
        q.days = [true; 7];
        q.daily = false;
    }
    ui.horizontal(|ui| {
        ui.radio_value(&mut q.repeat_enabled, false, "One-time downloading");
        ui.add_space(40.0);
        ui.radio_value(&mut q.repeat_enabled, true, "Periodic synchronization");
    });
    ui.separator();
    ui.checkbox(&mut q.start_on_startup, "Start download on Klippo startup");
    ui.separator();
    ui.horizontal(|ui| {
        ui.checkbox(&mut q.start_enabled, "Start download at");
        ui.add_space(30.0);
        ui.add_enabled_ui(q.start_enabled, |ui| time_field(ui, &mut q.start_time));
    });
    ui.add_enabled_ui(q.start_enabled, |ui| {
        if q.repeat_enabled {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label("Start again every");
                let mut hours = q.repeat_minutes / 60;
                let mut minutes = q.repeat_minutes % 60;
                ui.add(egui::DragValue::new(&mut hours).range(0..=168));
                ui.label("hours");
                ui.add(egui::DragValue::new(&mut minutes).range(0..=59));
                ui.label("min");
                q.repeat_minutes = (hours * 60 + minutes).max(1);
            });
        }
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.radio_value(&mut q.start_once, true, "Once at");
            ui.add_space(20.0);
            ui.add_enabled(q.start_once, egui::TextEdit::singleline(&mut q.start_date).margin(crate::gui::theme::EDIT_MARGIN).desired_width(150.0).hint_text("YYYY-MM-DD"));
        });
        ui.horizontal_top(|ui| {
            ui.add_space(20.0);
            ui.radio_value(&mut q.start_once, false, "Daily");
            ui.add_space(20.0);
            ui.add_enabled_ui(!q.start_once, |ui| {
                // Three columns, Sunday first; `days` is Monday first.
                egui::Grid::new("sched_days").num_columns(3).spacing([16.0, 2.0]).show(ui, |ui| {
                    let order = [[6, 2, 5], [0, 3, usize::MAX], [1, 4, usize::MAX]];
                    let names = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
                    for row in order {
                        for day in row {
                            if day == usize::MAX {
                                ui.label("");
                            } else {
                                ui.checkbox(&mut q.days[day], names[day]);
                            }
                        }
                        ui.end_row();
                    }
                });
            });
        });
    });
    ui.separator();
    ui.horizontal(|ui| {
        ui.checkbox(&mut q.stop_enabled, "Stop download at");
        ui.add_space(36.0);
        ui.add_enabled_ui(q.stop_enabled, |ui| time_field(ui, &mut q.stop_time));
    });
    ui.horizontal(|ui| {
        let mut retry = q.retries > 0;
        if ui.checkbox(&mut retry, "Number of retries for each file if downloading failed :").changed() {
            q.retries = if retry { 10 } else { 0 };
        }
        ui.add_enabled(retry, egui::DragValue::new(&mut q.retries).range(0..=50));
    });
    ui.separator();
    let mut open_file = !q.open_when_done.is_empty();
    ui.checkbox(&mut open_file, "Open the following file when done:");
    ui.horizontal(|ui| {
        fixed_width(ui, 332.0, |ui| ui.add(egui::TextEdit::singleline(&mut q.open_when_done).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY)));
        if ui.button("...").clicked() {
            if let Some(file) = rfd::FileDialog::new().pick_file() {
                q.open_when_done = file.to_string_lossy().to_string();
            }
        }
    });
    if !open_file {
        q.open_when_done.clear();
    }
    ui.checkbox(&mut q.hangup_when_done, "Hang up modem when done");
    ui.checkbox(&mut q.exit_when_done, "Exit Klippo when done");
}

fn files_tab(app: &mut KlippoApp, qid: u32, idx: usize, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label("Download");
        ui.add(egui::DragValue::new(&mut app.settings.queues[idx].simultaneous).range(1..=10));
        ui.label("files at the same time");
    });
    let ids: Vec<u64> = app.downloads.iter().filter(|d| d.queue == Some(qid)).map(|d| d.id).collect();
    let rows: Vec<[String; 4]> = ids
        .iter()
        .filter_map(|id| app.item(*id))
        .map(|d| [d.filename.clone(), d.size_string(), d.status_string(), d.eta_string()])
        .collect();
    let sel_id = egui::Id::new(("queue_files_sel", qid));
    let mut sel: Option<usize> = ui.data(|d| d.get_temp(sel_id)).flatten();
    ui.horizontal_top(|ui| {
        ui.allocate_ui(egui::vec2(ui.available_width() - 80.0, 300.0), |ui| {
            list_view(ui, "queue_files", &[("File Name", 130.0), ("Size", 60.0), ("Status", 60.0), ("Time left", 55.0)], rows.len(), 300.0, &mut sel, |ui, row, col| {
                ui.label(&rows[row][col]);
            });
        });
        ui.vertical(|ui| {
            let picked = sel.and_then(|i| ids.get(i).copied());
            if button(ui, picked.is_some() && sel != Some(0), "Move up").clicked() {
                if let Some(id) = picked {
                    app.move_in_queue(id, true);
                    sel = sel.map(|i| i - 1);
                }
            }
            if button(ui, picked.is_some() && sel.is_some_and(|i| i + 1 < ids.len()), "Move down").clicked() {
                if let Some(id) = picked {
                    app.move_in_queue(id, false);
                    sel = sel.map(|i| i + 1);
                }
            }
            if button(ui, picked.is_some(), "Remove").on_hover_text("Remove the file from this queue").clicked() {
                if let Some(id) = picked {
                    app.set_queue(&[id], None);
                    sel = None;
                }
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(sel_id, sel));
}
