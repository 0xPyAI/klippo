//! "Find file" (Downloads > Find, Ctrl+F) and "Columns" (View > Customize URL List...).

use eframe::egui::{self, Context};

use super::dialog_window;
use super::downloads_table::COLUMNS;
use super::classic_ui::{button, right_buttons};
use crate::gui::state::KlippoApp;

/// Where "Find" looks and how it compares.
#[derive(Debug, Clone)]
pub struct FindOptions {
    pub in_name: bool,
    pub in_description: bool,
    pub in_site: bool,
    pub match_case: bool,
    pub whole: bool,
}

impl Default for FindOptions {
    fn default() -> Self {
        Self { in_name: true, in_description: false, in_site: false, match_case: false, whole: false }
    }
}

pub fn render_find_dialog(app: &mut KlippoApp, ctx: &Context) {
    if !app.focus_search {
        return;
    }
    let mut open = true;
    let mut find = false;
    let mut cancel = false;
    dialog_window("Find file", ctx, 306.0)
        .id(egui::Id::new("find_file_dialog"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(306.0);
            ui.label("Find:");
            let edit = ui.add(egui::TextEdit::singleline(&mut app.search_query).margin(crate::gui::theme::EDIT_MARGIN).desired_width(f32::INFINITY));
            if !ui.data(|d| d.get_temp::<bool>(egui::Id::new("find_focused")).unwrap_or(false)) {
                edit.request_focus();
                ui.data_mut(|d| d.insert_temp(egui::Id::new("find_focused"), true));
            }
            let o = &mut app.find_options;
            ui.checkbox(&mut o.in_name, "File name or part of the name");
            ui.checkbox(&mut o.in_description, "Description or part of the description");
            ui.checkbox(&mut o.in_site, "Site name/Download link/Parent web page/Referer");
            ui.separator();
            ui.checkbox(&mut o.match_case, "Match case");
            ui.checkbox(&mut o.whole, "Match whole string only");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(60.0);
                find = button(ui, !app.search_query.trim().is_empty(), "Find").clicked();
                ui.add_space(26.0);
                cancel = button(ui, true, "Cancel").clicked();
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !app.search_query.trim().is_empty() {
                find = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
    if find {
        app.find_next();
    }
    if find || cancel || !open {
        app.focus_search = false;
        ctx.data_mut(|d| d.remove::<bool>(egui::Id::new("find_focused")));
    }
}

pub fn render_columns_dialog(app: &mut KlippoApp, ctx: &Context) {
    if !app.show_columns_dialog {
        return;
    }
    let edit_id = egui::Id::new("columns_edit");
    let mut hidden: Vec<String> = ctx.data_mut(|d| d.get_temp_mut_or_insert_with(edit_id, || app.settings.hidden_columns.clone()).clone());
    let sel_id = egui::Id::new("columns_sel");
    let mut sel: Option<usize> = ctx.data(|d| d.get_temp(sel_id)).flatten();
    let mut open = true;
    let mut ok = false;
    let mut cancel = false;
    dialog_window("Columns", ctx, 314.0)
        .id(egui::Id::new("columns_dialog"))
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(314.0);
            ui.set_max_width(314.0);
            ui.label("Tick the columns to show in the download list, or pick one and press Show or Hide.");
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                let look = crate::gui::theme::current(ui.ctx());
                egui::Frame::none().fill(look.list_bg).stroke(egui::Stroke::new(1.0_f32, look.border_color)).inner_margin(egui::Margin::same(3.0)).show(ui, |ui| {
                    ui.set_width(195.0);
                    ui.vertical(|ui| egui::ScrollArea::vertical().max_height(139.0).auto_shrink([false, false]).show(ui, |ui| {
                        for (row, (key, label)) in COLUMNS.iter().enumerate() {
                            let mut shown = !hidden.iter().any(|h| h == key);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(*key != "name", egui::Checkbox::without_text(&mut shown)).changed() {
                                    set_shown(&mut hidden, key, shown);
                                }
                                if ui.selectable_label(sel == Some(row), *label).clicked() {
                                    sel = Some(row);
                                }
                            });
                        }
                    }));
                });
                ui.vertical(|ui| {
                    button(ui, false, "Move Up").on_disabled_hover_text("The column order is fixed");
                    button(ui, false, "Move Down").on_disabled_hover_text("The column order is fixed");
                    let key = sel.map(|i| COLUMNS[i].0).filter(|k| *k != "name");
                    if button(ui, key.is_some(), "Show").clicked() {
                        set_shown(&mut hidden, key.unwrap_or_default(), true);
                    }
                    if button(ui, key.is_some(), "Hide").clicked() {
                        set_shown(&mut hidden, key.unwrap_or_default(), false);
                    }
                    if button(ui, true, "Reset").clicked() {
                        hidden = crate::gui::state::AppSettings::default().hidden_columns;
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(30.0);
                ui.label("Drag a column's edge in the list to change its width.");
            });
            ui.separator();
            ui.horizontal(|ui| {
                right_buttons(ui, |ui| {
                    cancel = button(ui, true, "Cancel").clicked();
                    ok = button(ui, true, "OK").clicked();
                });
            });
        });
    if ok {
        app.settings.hidden_columns = hidden.clone();
        app.save_settings_only();
    }
    if ok || cancel || !open {
        app.show_columns_dialog = false;
        ctx.data_mut(|d| {
            d.remove::<Vec<String>>(edit_id);
            d.remove::<Option<usize>>(sel_id);
        });
    } else {
        ctx.data_mut(|d| {
            d.insert_temp(edit_id, hidden);
            d.insert_temp(sel_id, sel);
        });
    }
}

fn set_shown(hidden: &mut Vec<String>, key: &str, shown: bool) {
    if key.is_empty() {
        return;
    }
    hidden.retain(|h| h != key);
    if !shown {
        hidden.push(key.to_string());
    }
}
