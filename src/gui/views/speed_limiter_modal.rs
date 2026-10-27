use eframe::egui::{self, Context};

use super::dialog_window;
use super::classic_ui::{button, right_buttons};
use crate::gui::state::KlippoApp;

/// "Speed Limiter settings" (Downloads > Speed Limiter > Settings...). The limiter
/// itself is turned on and off from the same menu.
pub fn render_speed_limiter_modal(app: &mut KlippoApp, ctx: &Context) {
    if !app.show_speed_limiter_dialog {
        return;
    }
    // Restore point for Cancel, taken when the dialog opens.
    let backup_id = egui::Id::new("speed_limiter_backup");
    let backup: (u64, bool) = ctx.data_mut(|d| {
        *d.get_temp_mut_or_insert_with(backup_id, || (app.settings.speed_limit_kbps, app.settings.speed_limit_on_startup))
    });
    let mut is_open = true;
    let mut cancel = false;
    let mut ok = false;

    dialog_window("Speed Limiter settings", ctx, 407.0)
        .id(egui::Id::new("speed_limiter_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(407.0);
            ui.set_max_width(407.0);
            ui.horizontal(|ui| {
                ui.label("Maximum download speed for all files");
                ui.add(egui::DragValue::new(&mut app.settings.speed_limit_kbps).speed(10).range(1..=1_000_000));
                ui.label("KBytes/sec");
            });
            ui.add_space(6.0);
            ui.label(
                "A very low limit can make some servers close the connection. Avoid it for downloads \
                 that cannot resume.",
            );
            ui.add_space(4.0);
            ui.checkbox(&mut app.settings.speed_limit_on_startup, "Always turn on Speed Limiter on Klippo startup");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    cancel = button(ui, true, "Cancel").clicked();
                    ok = button(ui, true, "OK").clicked();
                });
            });
        });

    if ok {
        app.save_settings();
    } else if cancel || !is_open {
        (app.settings.speed_limit_kbps, app.settings.speed_limit_on_startup) = backup;
    }
    if ok || cancel || !is_open {
        ctx.data_mut(|d| d.remove::<(u64, bool)>(backup_id));
        app.show_speed_limiter_dialog = false;
    }
}
