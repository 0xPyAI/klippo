use eframe::egui::{self, Ui};
use crate::gui::ipc::IpcMessage;
use crate::gui::state::{BatchMode, DownloadStatus, ExportDialog, ExportFormat, ExportScope, KlippoApp, SortColumn};
use crate::gui::system;

pub fn render_menu_bar(app: &mut KlippoApp, ui: &mut Ui) {
    egui::menu::bar(ui, |ui| {
        crate::gui::theme::menu_style(ui);
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.menu_button("Tasks", |ui| {
            crate::gui::theme::menu_style(ui);
            if ui.add(egui::Button::new("Add new download...").shortcut_text("Ctrl+N")).clicked() {
                app.open_add_url_dialog();
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Add batch download...").shortcut_text("Ctrl+B")).clicked() {
                app.batch.open = true;
                app.batch.mode = BatchMode::Pattern;
                ui.close_menu();
            }
            if ui.button("Add batch download from clipboard").clicked() {
                if let Ok(mut clip) = arboard::Clipboard::new() {
                    if let Ok(text) = clip.get_text() {
                        app.batch.urls_text = text;
                    }
                }
                app.batch.mode = BatchMode::List;
                app.batch.from_browser = false;
                app.batch.open = true;
                ui.close_menu();
            }
            if ui.button("Grab video stream (HLS / M3U8)...").clicked() {
                app.grabber.open = true;
                ui.close_menu();
            }
            if ui.button("Run site grabber").clicked() {
                app.open_site_grabber();
                ui.close_menu();
            }
            ui.separator();
            ui.menu_button("Export", |ui| {
                crate::gui::theme::menu_style(ui);
                let scope = if app.selection.len() > 1 { ExportScope::Selected } else { ExportScope::All };
                if ui.button("To export file (.ef2)...").clicked() {
                    app.export_dialog = Some(ExportDialog { format: ExportFormat::Ef2, scope });
                    ui.close_menu();
                }
                if ui.button("To text file...").clicked() {
                    app.export_dialog = Some(ExportDialog { format: ExportFormat::Text, scope });
                    ui.close_menu();
                }
            });
            ui.menu_button("Import", |ui| {
                crate::gui::theme::menu_style(ui);
                if ui.button("From export file (.ef2)...").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Export file", &["ef2"]).pick_file() {
                        app.import_list(&path);
                    }
                    ui.close_menu();
                }
                if ui.button("From text file...").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Text file", &["txt"]).pick_file() {
                        app.import_list(&path);
                    }
                    ui.close_menu();
                }
            });
            ui.separator();
            if ui.button("Exit").clicked() {
                app.exit_requested = true;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                ui.close_menu();
            }
        });

        ui.menu_button("File", |ui| {
            crate::gui::theme::menu_style(ui);
            let selected = app.selected_ids();
            let any = !selected.is_empty();
            let one = app.selected_download().cloned();
            let can_start = selected.iter().filter_map(|id| app.item(*id)).any(|d| d.status.can_start());
            let can_stop = selected.iter().filter_map(|id| app.item(*id)).any(|d| d.status.is_active());
            let completed = one.as_ref().is_some_and(|d| d.status == DownloadStatus::Completed);

            if ui.add_enabled(can_start, egui::Button::new("Download now / Resume")).clicked() {
                app.resume_selected();
                ui.close_menu();
            }
            if ui.add_enabled(can_stop, egui::Button::new("Stop download")).clicked() {
                app.pause_selected();
                ui.close_menu();
            }
            if ui.add_enabled(one.is_some(), egui::Button::new("Redownload")).clicked() {
                if let Some(ref item) = one {
                    app.redownload(item.id);
                }
                ui.close_menu();
            }
            ui.separator();
            if ui.add_enabled(completed, egui::Button::new("Open")).clicked() {
                if let Some(ref item) = one {
                    crate::gui::system::open_file(&item.save_path);
                }
                ui.close_menu();
            }
            if ui.add_enabled(completed, egui::Button::new("Open with...")).clicked() {
                if let Some(ref item) = one {
                    system::open_with(&item.save_path);
                }
                ui.close_menu();
            }
            if ui.add_enabled(one.is_some(), egui::Button::new("Open folder")).clicked() {
                if let Some(ref item) = one {
                    system::reveal_in_folder(&item.save_path);
                }
                ui.close_menu();
            }
            ui.separator();
            ui.add_enabled_ui(any, |ui| {
                ui.menu_button("Move to queue", |ui| {
                    crate::gui::theme::menu_style(ui);
                    let queues: Vec<(u32, String)> = app.settings.queues.iter().map(|q| (q.id, q.name.clone())).collect();
                    for (qid, name) in queues {
                        if ui.button(name).clicked() {
                            app.set_queue(&selected, Some(qid));
                            ui.close_menu();
                        }
                    }
                });
            });
            if ui.add_enabled(any, egui::Button::new("Remove from queue")).clicked() {
                app.set_queue(&selected, None);
                ui.close_menu();
            }
            ui.separator();
            if ui.add_enabled(any, egui::Button::new("Delete                 Del")).clicked() {
                app.request_delete(selected.clone());
                ui.close_menu();
            }
            if ui.add_enabled(one.is_some(), egui::Button::new("Properties...")).clicked() {
                app.show_file_properties_dialog = app.selected_id;
                ui.close_menu();
            }
        });

        ui.menu_button("Downloads", |ui| {
            crate::gui::theme::menu_style(ui);
            if ui.button("Pause All").on_hover_text("Stop the downloads that can resume later").clicked() {
                app.pause_all();
                ui.close_menu();
            }
            if ui.button("Stop All").clicked() {
                app.stop_all_downloads();
                ui.close_menu();
            }
            if ui.button("Delete all completed").clicked() {
                app.delete_completed_downloads();
                ui.close_menu();
            }
            ui.separator();
            let queues: Vec<(u32, String)> = app.settings.queues.iter().map(|q| (q.id, q.name.clone())).collect();
            ui.menu_button("Start queue", |ui| {
                crate::gui::theme::menu_style(ui);
                for (qid, name) in &queues {
                    if ui.add_enabled(!app.is_queue_running(*qid), egui::Button::new(name)).clicked() {
                        app.start_queue(*qid);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Stop queue", |ui| {
                crate::gui::theme::menu_style(ui);
                for (qid, name) in &queues {
                    if ui.add_enabled(app.is_queue_running(*qid), egui::Button::new(name)).clicked() {
                        app.stop_queue(*qid);
                        ui.close_menu();
                    }
                }
            });
            if ui.add(egui::Button::new("Find").shortcut_text("Ctrl+F")).clicked() {
                app.focus_search = true;
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Find next").shortcut_text("F3")).clicked() {
                app.find_next();
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Scheduler...").shortcut_text("Ctrl+S")).clicked() {
                app.show_scheduler_dialog = true;
                ui.close_menu();
            }
            ui.separator();
            ui.menu_button("Speed Limiter", |ui| {
                crate::gui::theme::menu_style(ui);
                let label = if app.settings.speed_limit_enabled { "Turn off" } else { "Turn on" };
                if ui.button(label).clicked() {
                    app.settings.speed_limit_enabled = !app.settings.speed_limit_enabled;
                    app.save_settings();
                    ui.close_menu();
                }
                if ui.button("Settings...").clicked() {
                    app.show_speed_limiter_dialog = true;
                    ui.close_menu();
                }
            });
            ui.separator();
            if ui.add(egui::Button::new("Options...").shortcut_text("Ctrl+O")).clicked() {
                app.open_options();
                ui.close_menu();
            }
        });

        ui.menu_button("View", |ui| {
            crate::gui::theme::menu_style(ui);
            ui.checkbox(&mut app.show_sidebar, "Categories");
            ui.menu_button("Toolbar", |ui| {
                crate::gui::theme::menu_style(ui);
                ui.checkbox(&mut app.show_toolbar, "Show toolbar");
                ui.separator();
                if ui.radio(!app.settings.toolbar_small, "Large buttons").clicked() {
                    app.settings.toolbar_small = false;
                    app.save_settings_only();
                }
                if ui.radio(app.settings.toolbar_small, "Small buttons").clicked() {
                    app.settings.toolbar_small = true;
                    app.save_settings_only();
                }
            });
            if ui.button("Customize URL List...").clicked() {
                app.show_columns_dialog = true;
                ui.close_menu();
            }
            ui.menu_button("Tray icon", |ui| {
                crate::gui::theme::menu_style(ui);
                if ui.radio(app.settings.show_tray_icon, "Show in the notification area").clicked() {
                    app.settings.show_tray_icon = true;
                    app.save_settings_only();
                }
                if ui.radio(!app.settings.show_tray_icon, "Don't show").clicked() {
                    app.settings.show_tray_icon = false;
                    app.save_settings_only();
                }
            });
            ui.menu_button("Font", |ui| {
                crate::gui::theme::menu_style(ui);
                let mut changed = false;
                if ui.button("Larger").clicked() {
                    app.settings.font_scale = (app.settings.font_scale + 0.1).min(2.0);
                    changed = true;
                }
                if ui.button("Smaller").clicked() {
                    app.settings.font_scale = (app.settings.font_scale - 0.1).max(0.7);
                    changed = true;
                }
                if ui.button("Reset to default size").clicked() {
                    app.settings.font_scale = 1.0;
                    changed = true;
                }
                if changed {
                    app.save_settings_only();
                }
            });
            ui.separator();
            ui.menu_button("Arrange files", |ui| {
                crate::gui::theme::menu_style(ui);
                let columns = [
                    (None, "by order of addition"),
                    (Some(SortColumn::Name), "by name"),
                    (Some(SortColumn::Size), "by size"),
                    (Some(SortColumn::Status), "by status"),
                    (Some(SortColumn::TimeLeft), "by time left"),
                    (Some(SortColumn::Speed), "by transfer rate"),
                    (Some(SortColumn::LastTry), "by last try date"),
                    (Some(SortColumn::Description), "by description"),
                    (Some(SortColumn::SavePath), "by save path"),
                    (Some(SortColumn::Referer), "by referer"),
                ];
                for (col, label) in columns {
                    let selected = app.sort.map(|(c, _)| c) == col;
                    if ui.selectable_label(selected, label).clicked() {
                        app.sort = col.map(|c| (c, true));
                        ui.close_menu();
                    }
                }
            });
            ui.separator();
            if ui.checkbox(&mut app.settings.show_status_bar, "Status bar").clicked() {
                app.save_settings_only();
            }
            if ui.checkbox(&mut app.settings.follow_system_theme, "Follow Windows theme (light / dark)").clicked() {
                app.apply_theme(ui.ctx());
                app.save_settings_only();
            }
            if ui
                .add_enabled(!app.settings.follow_system_theme, egui::Checkbox::new(&mut app.settings.dark_mode, "Dark mode"))
                .clicked()
            {
                app.apply_theme(ui.ctx());
                app.save_settings_only();
            }
        });

        ui.menu_button("Help", |ui| {
            crate::gui::theme::menu_style(ui);
            if ui.button("Quick guide (README)").clicked() {
                let readme = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.ancestors().map(|a| a.join("README.md")).find(|p| p.exists()));
                match readme {
                    Some(path) => {
                        let _ = open::that(path);
                    }
                    None => app.set_status("README.md was not found next to the program"),
                }
                ui.close_menu();
            }
            if ui.button("Browser extension folder").clicked() {
                match crate::native_host::extension_dir() {
                    Some(dir) => {
                        let _ = open::that(dir);
                    }
                    None => app.set_status("The extension folder was not found next to the program"),
                }
                ui.close_menu();
            }
            if ui.button("Paste link from clipboard").clicked() {
                if let Ok(mut clip) = arboard::Clipboard::new() {
                    if let Ok(text) = clip.get_text() {
                        app.handle_message(IpcMessage { url: text.trim().to_string(), ..Default::default() }, None);
                    }
                }
                ui.close_menu();
            }
            ui.separator();
            if ui.button("About Klippo").clicked() {
                app.show_about_dialog = true;
                ui.close_menu();
            }
        });
    });
}
