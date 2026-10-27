use eframe::egui::{self, Context, RichText, Sense, Vec2};

use super::{dialog_window, fixed_width};
use super::classic_ui::{button, right_buttons};
use crate::gui::icons::{self, SmallIcon};
use crate::gui::state::KlippoApp;
use crate::gui::system;

/// "Download complete".
pub fn render_complete_modal(app: &mut KlippoApp, ctx: &Context) {
    let Some(target_id) = app.show_complete_dialog else { return };
    let Some(item) = app.item(target_id).cloned() else {
        app.show_complete_dialog = None;
        return;
    };

    let mut is_open = true;
    let mut close = false;
    let mut dont_show = !app.settings.show_complete_window;

    dialog_window("Download complete", ctx, 408.0)
        .id(egui::Id::new("download_complete_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .minimizable(true)
        .show(ctx, |ui| {
            ui.set_width(408.0);
            ui.set_max_width(408.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
                icons::small(ui.painter(), rect, SmallIcon::Category(item.category));
                ui.vertical(|ui| {
                    ui.label(RichText::new("Download complete").size(crate::gui::theme::TITLE_FONT));
                    let bytes = std::fs::metadata(&item.save_path).map(|m| m.len()).ok().or(item.total_size);
                    let text = match bytes {
                        Some(b) => format!("Downloaded {} ({} Bytes)", item.size_string(), group_digits(b)),
                        None => format!("Downloaded {}", item.size_string()),
                    };
                    ui.label(text);
                });
            });
            ui.add_space(6.0);
            ui.label("Address");
            let mut url = item.url.clone();
            fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut url).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY).interactive(false)));
            ui.add_space(4.0);
            ui.label("The file saved as");
            let mut path = item.save_path.to_string_lossy().to_string();
            fixed_width(ui, f32::INFINITY, |ui| ui.add(egui::TextEdit::singleline(&mut path).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY).interactive(false)));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if button(ui, true, "Open").clicked() {
                    crate::gui::system::open_file(&item.save_path);
                    close = true;
                }
                if button(ui, true, "Open with...").clicked() {
                    system::open_with(&item.save_path);
                    close = true;
                }
                if button(ui, true, "Open folder").clicked() {
                    system::reveal_in_folder(&item.save_path);
                    close = true;
                }
                right_buttons(ui, |ui| {
                    if button(ui, true, "Close").clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(4.0);
            ui.checkbox(&mut dont_show, "Don't show this dialog again");
        });

    if dont_show == app.settings.show_complete_window {
        app.settings.show_complete_window = !dont_show;
        app.save_settings_only();
    }
    if close || !is_open {
        app.show_complete_dialog = None;
    }
}

/// 734003200 -> "734,003,200".
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
