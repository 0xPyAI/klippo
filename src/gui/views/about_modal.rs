use eframe::egui::{self, Context, RichText, Sense, Vec2};

use super::dialog_window;
use super::classic_ui::{button, group_box};
use crate::gui::icons::{self, ToolIcon};
use crate::gui::state::{app_data_dir, KlippoApp};
use crate::gui::theme;

const NOTICE: &str = "Klippo is an independent download manager modeled on the look and behavior of \
                      a classic download manager. It is not made by Tonec FZE and contains none of its code or images.";

/// Laid out like a classic download manager's "About" dialog.
pub fn render_about_modal(app: &mut KlippoApp, ctx: &Context) {
    if !app.show_about_dialog {
        return;
    }
    let mut is_open = true;
    dialog_window("About Klippo", ctx, 348.0)
        .id(egui::Id::new("about_dialog"))
        .open(&mut is_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(348.0);
            ui.set_max_width(348.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                icons::tool(ui.painter(), rect, ToolIcon::Resume, true, &theme::current(ui.ctx()));
                ui.vertical(|ui| {
                    ui.label(RichText::new("Klippo").size(22.0));
                    ui.label(format!("Version: {}", env!("CARGO_PKG_VERSION")));
                });
            });
            ui.vertical_centered(|ui| {
                button(ui, false, "Check for Update").on_disabled_hover_text("Klippo is built from its source code; there is no update server");
            });
            group_box(ui, "This product is licensed to:", |ui| {
                ui.label("Everyone: free and open-source software");
                ui.add_space(30.0);
            });
            ui.add_space(6.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(80.0).show(ui, |ui| ui.label(NOTICE));
            });
            ui.add_space(4.0);
            egui::Grid::new("about_links").num_columns(2).spacing([40.0, 4.0]).show(ui, |ui| {
                ui.label("Data folder:");
                if ui.link("Open").on_hover_text(app_data_dir().display().to_string()).clicked() {
                    let _ = open::that(app_data_dir());
                }
                ui.end_row();
                ui.label("Help:");
                if ui.link("README.md").clicked() {
                    if let Some(readme) = std::env::current_exe().ok().and_then(|exe| exe.ancestors().map(|a| a.join("README.md")).find(|p| p.exists())) {
                        let _ = open::that(readme);
                    }
                }
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label("Copyright © Klippo authors.");
            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                if button(ui, true, "Close").clicked() {
                    app.show_about_dialog = false;
                }
            });
        });
    if !is_open {
        app.show_about_dialog = false;
    }
}
