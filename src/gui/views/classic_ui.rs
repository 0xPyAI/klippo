//! Parts of the classic Windows dialogs drawn with egui: the two-row tab strip of
//! the Options property sheet, group boxes with a caption on the border,
//! the icon-and-title header of each page, fixed-size buttons and list views.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Frame, Margin, Rect, Response, Sense, Stroke, Ui, Vec2};
use egui_extras::{Column, TableBuilder};

use crate::gui::icons::{self, PageIcon};
use crate::gui::theme;

/// Background of a tab page (a shade lighter than the dialog, as Windows draws it).
pub fn page_bg(ui: &Ui) -> Color32 {
    theme::current(ui.ctx()).page_bg
}

fn text_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
    ui.fonts(|f| f.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE).size().x)
}

/// Windows multi-line tabs: every row fills the width, and the row holding the
/// selected tab moves next to the page.
pub fn tab_rows<T: Copy + PartialEq>(ui: &mut Ui, rows: &[&[(T, &str)]], selected: &mut T) {
    let theme = theme::current(ui.ctx());
    let width = ui.available_width();
    let h = 16.8;
    let front = rows.iter().position(|r| r.iter().any(|(t, _)| *t == *selected)).unwrap_or(rows.len() - 1);
    let mut order: Vec<usize> = (0..rows.len()).filter(|i| *i != front).collect();
    order.push(front);
    let (area, _) = ui.allocate_exact_size(vec2(width, h * rows.len() as f32 + 2.0), Sense::hover());
    let font = FontId::proportional(theme::LIST_FONT);
    let border = Stroke::new(1.0_f32, theme.border_color);
    let mut chosen = None;
    let mut selected_rect = None;
    for (line, &ri) in order.iter().enumerate() {
        let row = rows[ri];
        let y = area.top() + 2.0 + line as f32 * h;
        let widths: Vec<f32> = row.iter().map(|(_, l)| text_width(ui, l, &font) + 22.0).collect();
        let scale = width / widths.iter().sum::<f32>();
        let mut x = area.left();
        for ((tab, label), w) in row.iter().zip(widths) {
            let w = w * scale;
            let rect = Rect::from_min_size(pos2(x, y), vec2(w, h));
            x += w;
            let resp = ui.interact(rect, ui.id().with(("idm_tab", label)), Sense::click());
            let is_selected = *tab == *selected;
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, is_selected, *label));
            if resp.clicked() {
                chosen = Some(*tab);
            }
            if *tab == *selected {
                selected_rect = Some((rect, *label));
                continue;
            }
            let fill = if resp.hovered() { ui.visuals().widgets.hovered.bg_fill } else { theme.panel_bg };
            ui.painter().rect_filled(rect, 0.0, fill);
            ui.painter().rect_stroke(rect, 0.0, border);
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, *label, font.clone(), theme.text_primary);
        }
    }
    // The selected tab is drawn last, a little larger and open towards the page.
    if let Some((rect, label)) = selected_rect {
        let rect = Rect::from_min_max(rect.min - vec2(2.0, 2.0), rect.max + vec2(2.0, 1.0));
        ui.painter().rect_filled(rect, 0.0, theme.page_bg);
        let p = ui.painter();
        p.line_segment([rect.left_bottom(), rect.left_top()], border);
        p.line_segment([rect.left_top(), rect.right_top()], border);
        p.line_segment([rect.right_top(), rect.right_bottom()], border);
        p.text(rect.center(), Align2::CENTER_CENTER, label, font, theme.text_primary);
    }
    if let Some(tab) = chosen {
        *selected = tab;
    }
}

/// The page under the tabs: a framed area of a fixed height.
pub fn tab_page<R>(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let theme = theme::current(ui.ctx());
    Frame::none()
        .fill(theme.page_bg)
        .stroke(Stroke::new(1.0_f32, theme.border_color))
        .inner_margin(Margin { left: 11.0, right: 11.0, top: 6.0, bottom: 6.0 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(height);
            ui.set_max_height(height);
            add(ui)
        })
        .inner
}

/// The page header: a 32-point icon at the left, the title at the right and a line under it.
pub fn page_header(ui: &mut Ui, icon: PageIcon, title: &str, large: bool) {
    let theme = theme::current(ui.ctx());
    // A 20 x 20 unit icon, the title at y 7 and the line at y 19 (1 unit = 1.6 px).
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    icons::page(ui.painter(), Rect::from_min_size(rect.left_top(), Vec2::splat(32.0)), icon);
    let font = FontId::proportional(if large { theme::TITLE_FONT } else { theme::LIST_FONT });
    let galley = ui.painter().layout_no_wrap(title.to_string(), font, theme.text_primary);
    let pos = pos2(rect.right() - galley.size().x - 4.0, rect.top() + 7.0);
    ui.painter().galley(pos, galley.clone(), theme.text_primary);
    let line_y = rect.top() + 24.5;
    ui.painter().hline((rect.left() + 44.0)..=rect.right(), line_y, Stroke::new(1.0_f32, theme.border_color));
}

/// A group box with its caption on the top border.
pub fn group_box<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    group_inner(ui, Caption::Text(title), add)
}

/// A group box whose caption is a check box that turns its contents on and off.
pub fn check_group<R>(ui: &mut Ui, value: &mut bool, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    group_inner(ui, Caption::Check(value, title), add)
}

enum Caption<'a> {
    Text(&'a str),
    Check(&'a mut bool, &'a str),
}

fn group_inner<R>(ui: &mut Ui, caption: Caption<'_>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let theme = theme::current(ui.ctx());
    let bg = theme.page_bg;
    ui.add_space(7.0);
    let enabled = match caption {
        Caption::Check(ref v, _) => **v,
        Caption::Text(_) => true,
    };
    let resp = Frame::none()
        .stroke(Stroke::new(1.0_f32, theme.group_border))
        .inner_margin(Margin { left: 8.0, right: 8.0, top: 11.0, bottom: 6.0 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add_enabled_ui(enabled, add).inner
        });
    let r = resp.response.rect;
    let font = FontId::proportional(theme::LIST_FONT);
    match caption {
        Caption::Text(title) => {
            let w = text_width(ui, title, &font);
            let at = pos2(r.left() + 6.0, r.top());
            ui.painter().rect_filled(Rect::from_min_size(at - vec2(0.0, 7.0), vec2(w + 6.0, 14.0)), 0.0, bg);
            ui.painter().text(at + vec2(3.0, 0.0), Align2::LEFT_CENTER, title, font, theme.text_primary);
        }
        Caption::Check(value, title) => {
            let w = text_width(ui, title, &font) + 26.0;
            let at = Rect::from_min_size(pos2(r.left() + 6.0, r.top() - 8.0), vec2(w, 16.0));
            ui.painter().rect_filled(at, 0.0, bg);
            let mut caption_ui = ui.new_child(egui::UiBuilder::new().max_rect(at).layout(egui::Layout::left_to_right(egui::Align::Center)));
            caption_ui.add(egui::Checkbox::new(value, title));
        }
    }
    resp.inner
}

/// A dialog button of Windows' standard size (wider for long text).
pub fn button(ui: &mut Ui, enabled: bool, text: &str) -> Response {
    // 50 x 14 dialog units, the classic button size (1.4 x 1.6 pixels per unit at 125 %).
    let w = (text_width(ui, text, &FontId::proportional(theme::LIST_FONT)) + 16.0).max(70.0);
    ui.add_enabled(enabled, egui::Button::new(text).min_size(vec2(w, 22.4)))
}

/// Names a combo box after the label beside it, for screen readers and UI
/// Automation (egui gives a combo box without its own label no name); the
/// shown choice is its value.
pub fn name_combo<R>(combo: &egui::InnerResponse<R>, name: &str, shown: &str) {
    let response = &combo.response;
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, response.enabled(), name);
        info.current_text_value = Some(shown.to_string());
        info
    });
}

/// A gray explanation line.
pub fn note(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(theme::LIST_FONT).color(theme::current(ui.ctx()).text_secondary));
}

/// A Windows list view: header row, rows that can be selected, white box.
pub fn list_view(
    ui: &mut Ui,
    id: &str,
    columns: &[(&str, f32)],
    rows: usize,
    height: f32,
    selected: &mut Option<usize>,
    cell: impl FnMut(&mut Ui, usize, usize),
) -> Option<usize> {
    list_view_impl(ui, id, columns, rows, height, selected, false, cell)
}

/// A list view with grid lines on every column and on the empty rows below
/// the items (the Connection exceptions and Sounds lists).
pub fn grid_list_view(
    ui: &mut Ui,
    id: &str,
    columns: &[(&str, f32)],
    rows: usize,
    height: f32,
    selected: &mut Option<usize>,
    cell: impl FnMut(&mut Ui, usize, usize),
) -> Option<usize> {
    list_view_impl(ui, id, columns, rows, height, selected, true, cell)
}

#[allow(clippy::too_many_arguments)]
fn list_view_impl(
    ui: &mut Ui,
    id: &str,
    columns: &[(&str, f32)],
    rows: usize,
    height: f32,
    selected: &mut Option<usize>,
    grid: bool,
    mut cell: impl FnMut(&mut Ui, usize, usize),
) -> Option<usize> {
    let theme = theme::current(ui.ctx());
    let mut double = None;
    let mut col_right: Vec<f32> = Vec::new();
    let mut header_bottom = 0.0_f32;
    let mut last_row_bottom: Option<f32> = None;
    Frame::none()
        .fill(theme.list_bg)
        .stroke(Stroke::new(1.0_f32, theme.border_color))
        .inner_margin(Margin::same(1.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(height);
            let area = ui.max_rect();
            ui.spacing_mut().item_spacing.y = 0.0;
            let gap = 0.0;
            ui.vertical(|ui| ui.push_id(id, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                let mut table = TableBuilder::new(ui).striped(false).sense(Sense::click()).max_scroll_height(height - 21.0).min_scrolled_height(height - 21.0);
                for (i, (_, w)) in columns.iter().enumerate() {
                    table = table.column(if i + 1 == columns.len() { Column::remainder() } else { Column::exact(*w) });
                }
                table
                    .header(19.2, |mut header| {
                        for (title, _) in columns {
                            header.col(|ui| {
                                let r = ui.max_rect().expand2(vec2(3.0, 2.0));
                                ui.painter().rect_filled(r, 0.0, theme.table_header_bg);
                                ui.painter().vline(r.right() - 0.5, r.y_range(), Stroke::new(1.0_f32, theme.header_sep));
                                ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0_f32, theme.header_sep));
                                col_right.push(r.right() - 0.5);
                                header_bottom = r.bottom() - 2.0;
                                ui.label(*title);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(16.8, rows, |mut row| {
                            let i = row.index();
                            row.set_selected(*selected == Some(i));
                            for c in 0..columns.len() {
                                row.col(|ui| {
                                    let r = ui.max_rect().expand2(vec2(3.0, 0.0));
                                    ui.painter().hline(r.x_range(), r.bottom() + 0.5, Stroke::new(1.0_f32, theme.grid_line));
                                    last_row_bottom = Some(last_row_bottom.map_or(r.bottom(), |b| b.max(r.bottom())));
                                    cell(ui, i, c);
                                });
                            }
                            let resp = row.response();
                            if resp.clicked() {
                                *selected = Some(i);
                            }
                            if resp.double_clicked() {
                                double = Some(i);
                            }
                        });
                    });
            }));
            if grid {
                // Lines for the empty rows and the columns down to the bottom of the list.
                let painter = ui.painter().with_clip_rect(area);
                let stroke = Stroke::new(1.0_f32, theme.grid_line);
                let step = 16.8 + gap;
                let mut y = last_row_bottom.map_or(header_bottom + step, |b| b + step) + 0.5;
                while y < area.bottom() {
                    painter.hline(area.x_range(), y, stroke);
                    y += step;
                }
                for x in col_right.iter().take(col_right.len().saturating_sub(1)) {
                    painter.vline(*x, header_bottom..=area.bottom(), stroke);
                }
            }
        });
    double
}

/// OK / Cancel style row aligned to the right.
pub fn right_buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
}
