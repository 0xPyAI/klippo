use std::cmp::Ordering;

use eframe::egui::{self, Color32, RichText, Sense, Ui, Vec2};
use egui_extras::{Column, TableBuilder};

use crate::gui::icons::{self, SmallIcon};
use crate::gui::state::{CategoryFilter, DownloadItem, DownloadStatus, KlippoApp, SortColumn};
use crate::gui::system;
use crate::gui::theme;

impl KlippoApp {
    /// Ids of the rows shown for the current category, search text and sort order.
    pub fn visible_ids(&self) -> Vec<u64> {
        let mut items: Vec<&DownloadItem> = self
            .downloads
            .iter()
            .filter(|d| match self.selected_category {
                CategoryFilter::All => true,
                CategoryFilter::Category(cat) => d.category == cat,
                CategoryFilter::Unfinished => d.status != DownloadStatus::Completed,
                CategoryFilter::Finished => d.status == DownloadStatus::Completed,
                CategoryFilter::UnfinishedIn(cat) => d.category == cat && d.status != DownloadStatus::Completed,
                CategoryFilter::FinishedIn(cat) => d.category == cat && d.status == DownloadStatus::Completed,
                CategoryFilter::Queue(q) => d.queue == Some(q),
            })
            .collect();

        if let Some((column, ascending)) = self.sort {
            items.sort_by(|a, b| {
                let ord = compare(a, b, column);
                if ascending { ord } else { ord.reverse() }
            });
        }
        items.into_iter().map(|d| d.id).collect()
    }
}

fn compare(a: &DownloadItem, b: &DownloadItem, column: SortColumn) -> Ordering {
    match column {
        SortColumn::Name => a.filename.to_lowercase().cmp(&b.filename.to_lowercase()),
        SortColumn::Size => a.total_size.cmp(&b.total_size),
        SortColumn::Status => a
            .progress_fraction()
            .partial_cmp(&b.progress_fraction())
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.status.display_text().cmp(b.status.display_text())),
        SortColumn::TimeLeft => a.eta_seconds.cmp(&b.eta_seconds),
        SortColumn::Speed => a.speed_bps.partial_cmp(&b.speed_bps).unwrap_or(Ordering::Equal),
        SortColumn::LastTry => a.last_try_string().cmp(b.last_try_string()),
        SortColumn::Description => a.description.cmp(&b.description),
        SortColumn::SavePath => a.save_path.cmp(&b.save_path),
        SortColumn::Referer => a.referer.cmp(&b.referer),
    }
}

/// Table columns (key, header), in the classic order. The file name is always shown.
pub const COLUMNS: [(&str, &str); 11] = [
    ("name", "File Name"),
    ("q", "Q"),
    ("size", "Size"),
    ("status", "Status"),
    ("time_left", "Time left"),
    ("speed", "Transfer rate"),
    ("progress", "Progress"),
    ("last_try", "Last Try Date"),
    ("description", "Description"),
    ("save_to", "Save To"),
    ("referer", "Referer"),
];

fn column_spec(key: &str) -> (Column, Option<SortColumn>) {
    match key {
        "name" => (Column::initial(150.0).at_least(80.0).clip(true), Some(SortColumn::Name)),
        "q" => (Column::initial(70.0).at_least(18.0).clip(true), None),
        "size" => (Column::initial(74.0).at_least(40.0).clip(true), Some(SortColumn::Size)),
        "status" => (Column::initial(65.0).at_least(40.0).clip(true), Some(SortColumn::Status)),
        "time_left" => (Column::initial(74.0).at_least(40.0).clip(true), Some(SortColumn::TimeLeft)),
        "speed" => (Column::initial(95.0).at_least(40.0).clip(true), Some(SortColumn::Speed)),
        "progress" => (Column::initial(120.0).at_least(60.0), None),
        "last_try" => (Column::initial(115.0).at_least(60.0).clip(true), Some(SortColumn::LastTry)),
        "description" => (Column::initial(110.0).at_least(40.0).clip(true), Some(SortColumn::Description)),
        "save_to" => (Column::initial(160.0).at_least(60.0).clip(true), Some(SortColumn::SavePath)),
        _ => (Column::initial(160.0).at_least(60.0).clip(true), Some(SortColumn::Referer)),
    }
}

impl KlippoApp {
    /// "Find" / F3: selects the next row that matches the "Find file" settings.
    pub fn find_next(&mut self) {
        let needle = self.search_query.trim().to_string();
        if needle.is_empty() {
            self.focus_search = true;
            return;
        }
        let o = self.find_options.clone();
        let norm = |s: &str| if o.match_case { s.to_string() } else { s.to_lowercase() };
        let needle = norm(&needle);
        let hit = |text: &str| {
            let text = norm(text);
            if o.whole { text == needle } else { text.contains(&needle) }
        };
        let matches = |d: &DownloadItem| {
            (o.in_name && hit(&d.filename))
                || (o.in_description && hit(&d.description))
                || (o.in_site && (hit(&d.url) || d.referer.as_deref().is_some_and(hit)))
        };
        let visible = self.visible_ids();
        let start = self.selected_id.and_then(|id| visible.iter().position(|v| *v == id)).map(|p| p + 1).unwrap_or(0);
        let found = (0..visible.len())
            .map(|k| visible[(start + k) % visible.len()])
            .find(|id| self.item(*id).is_some_and(matches));
        match found {
            Some(id) => self.select_only(id),
            None => self.set_status(format!("\"{}\" was not found", self.search_query.trim())),
        }
    }
}

#[derive(Clone, Copy)]
enum TableAction {
    Open,
    OpenWith,
    OpenFolder,
    Resume,
    Stop,
    Redownload,
    CopyLink,
    ShowProgress,
    ShowProperties,
    MoveToQueue(u32),
    RemoveFromQueue,
    Delete,
}

pub fn render_downloads_table(app: &mut KlippoApp, ui: &mut Ui) {
    let visible = app.visible_ids();
    let modifiers = ui.input(|i| i.modifiers);
    let mut clicked: Option<u64> = None;
    let mut double_clicked: Option<u64> = None;
    let mut action: Option<(u64, TableAction)> = None;
    let mut sort_click: Option<SortColumn> = None;

    ui.style_mut().interaction.selectable_labels = false;
    let text_color = ui.visuals().text_color();
    let dim = text_color.linear_multiply(0.7);

    let header_cell = |ui: &mut Ui, label: &str, column: Option<SortColumn>, sort: Option<(SortColumn, bool)>, out: &mut Option<SortColumn>| {
        let arrow = match (column, sort) {
            (Some(c), Some((s, asc))) if c == s => if asc { " ▲" } else { " ▼" },
            _ => "",
        };
        let theme = theme::current(ui.ctx());
        let cell = ui.max_rect().expand2(Vec2::new(3.0, 2.0));
        ui.painter().rect_filled(cell, 0.0, theme.table_header_bg);
        ui.painter().vline(cell.right() - 0.5, cell.y_range(), egui::Stroke::new(1.0_f32, theme.header_sep));
        ui.painter().hline(cell.x_range(), cell.bottom() - 0.5, egui::Stroke::new(1.0_f32, theme.header_sep));
        let text = RichText::new(format!("{}{}", label, arrow)).size(theme::LIST_FONT);
        match column {
            Some(c) => {
                if ui.add(egui::Label::new(text).sense(Sense::click())).clicked() {
                    *out = Some(c);
                }
            }
            None => {
                ui.label(text);
            }
        }
    };

    let sort = app.sort;
    let queues: Vec<(u32, String)> = app.settings.queues.iter().map(|q| (q.id, q.name.clone())).collect();
    let columns: Vec<&str> = COLUMNS
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| *key == "name" || !app.settings.hidden_columns.iter().any(|h| h == key))
        .collect();

    let theme = theme::current(ui.ctx());
    let grid = egui::Stroke::new(1.0_f32, theme.grid_line);
    // Grid lines are drawn between rows and columns instead of stripes.
    let cell_grid = |ui: &Ui| {
        let r = ui.max_rect().expand2(Vec2::new(3.0, 0.0));
        ui.painter().hline(r.x_range(), r.bottom() + 0.5, grid);
        ui.painter().vline(r.right() - 0.5, r.y_range(), grid);
    };
    // Rows sit edge to edge, like a Windows list view (21 px at 125 %).
    ui.spacing_mut().item_spacing.y = 0.0;
    let area = ui.max_rect();
    let mut header_bottom = area.top();
    let mut last_row_bottom: Option<f32> = None;
    let mut builder = TableBuilder::new(ui)
        .striped(false)
        .resizable(true)
        .sense(Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .min_scrolled_height(0.0);
    for (i, key) in columns.iter().enumerate() {
        let column = if i + 1 == columns.len() { Column::remainder().at_least(80.0).clip(true) } else { column_spec(key).0 };
        builder = builder.column(column);
    }

    builder
        .header(ROW_HEADER, |mut header| {
            for key in &columns {
                let label = COLUMNS.iter().find(|(k, _)| k == key).map(|(_, l)| *l).unwrap_or("");
                let sortable = column_spec(key).1;
                header.col(|ui| {
                    header_bottom = ui.max_rect().bottom();
                    header_cell(ui, label, sortable, sort, &mut sort_click)
                });
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, visible.len(), |mut row| {
                let id = visible[row.index()];
                let Some(item) = app.downloads.iter().find(|d| d.id == id) else { return };
                row.set_selected(app.selection.contains(&id));

                for key in &columns {
                    row.col(|ui| {
                        cell_grid(ui);
                        let bottom = ui.max_rect().bottom();
                        last_row_bottom = Some(last_row_bottom.map_or(bottom, |b: f32| b.max(bottom)));
                        match *key {
                        "name" => {
                            let (icon, _) = ui.allocate_exact_size(Vec2::splat(12.8), Sense::hover());
                            icons::small(ui.painter(), icon, SmallIcon::Category(item.category));
                            ui.label(RichText::new(&item.filename).size(theme::LIST_FONT)).on_hover_text(&item.url);
                        }
                        "q" => {
                            if item.queue.is_some() {
                                let (icon, _) = ui.allocate_exact_size(Vec2::splat(11.0), Sense::hover());
                                icons::small(ui.painter(), icon, SmallIcon::Queue);
                            }
                        }
                        "size" => {
                            ui.label(RichText::new(item.size_string()).size(theme::LIST_FONT));
                        }
                        "status" => {
                            // The state is written as plain text ("Complete", "45.27%", "Error").
                            let label = ui.label(RichText::new(item.status_string()).size(theme::LIST_FONT).color(text_color));
                            if let Some(ref err) = item.error_message {
                                if item.status == DownloadStatus::Failed {
                                    label.on_hover_text(err);
                                }
                            }
                        }
                        "time_left" => {
                            ui.label(RichText::new(item.eta_string()).size(theme::LIST_FONT));
                        }
                        "speed" => {
                            ui.label(RichText::new(item.speed_string()).size(theme::LIST_FONT));
                        }
                        "progress" => progress_cell(ui, item),
                        "last_try" => {
                            ui.label(RichText::new(item.last_try_string()).size(theme::LIST_FONT).color(dim));
                        }
                        "description" => {
                            ui.label(RichText::new(&item.description).size(theme::LIST_FONT).color(dim));
                        }
                        "save_to" => {
                            let dir = item.save_path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                            ui.label(RichText::new(dir).size(theme::LIST_FONT).color(dim));
                        }
                        _ => {
                            ui.label(RichText::new(item.referer.as_deref().unwrap_or("")).size(theme::LIST_FONT).color(dim));
                        }
                        }
                    });
                }

                let response = row.response();
                let selected = app.selection.contains(&id);
                response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &item.filename));
                if response.clicked() {
                    clicked = Some(id);
                }
                if response.double_clicked() {
                    double_clicked = Some(id);
                }
                if response.secondary_clicked() && !app.selection.contains(&id) {
                    clicked = Some(id);
                }
                response.context_menu(|ui| {
                    crate::gui::theme::menu_style(ui);
                    context_menu(ui, item, &queues, &mut action);
                });
            });
        });

    if let Some(column) = sort_click {
        app.sort = match app.sort {
            Some((c, asc)) if c == column => Some((c, !asc)),
            _ => Some((column, true)),
        };
    }

    // Windows Explorer style selection.
    if let Some(id) = clicked {
        if modifiers.command {
            if !app.selection.remove(&id) {
                app.selection.insert(id);
            }
            app.selected_id = Some(id);
        } else if modifiers.shift {
            let anchor = app.selected_id.unwrap_or(id);
            let a = visible.iter().position(|v| *v == anchor).unwrap_or(0);
            let b = visible.iter().position(|v| *v == id).unwrap_or(0);
            let (lo, hi) = (a.min(b), a.max(b));
            app.selection = visible[lo..=hi].iter().copied().collect();
        } else {
            app.select_only(id);
        }
    }

    if let Some(id) = double_clicked {
        if let Some(item) = app.item(id) {
            if item.status == DownloadStatus::Completed {
                system::open_file(&item.save_path);
            } else {
                app.show_progress_window(id);
            }
        }
    }

    if let Some((id, act)) = action {
        // Actions apply to the whole selection when the clicked row is part of it.
        let targets = if app.selection.contains(&id) { app.selected_ids() } else { vec![id] };
        match act {
            TableAction::Open => {
                if let Some(item) = app.item(id) {
                    system::open_file(&item.save_path);
                }
            }
            TableAction::OpenWith => {
                if let Some(item) = app.item(id) {
                    system::open_with(&item.save_path);
                }
            }
            TableAction::OpenFolder => {
                if let Some(item) = app.item(id) {
                    system::reveal_in_folder(&item.save_path);
                }
            }
            TableAction::Resume => {
                for t in targets {
                    app.resume_download(t);
                }
            }
            TableAction::Stop => {
                for t in targets {
                    app.pause_download(t);
                }
            }
            TableAction::Redownload => app.redownload(id),
            TableAction::CopyLink => {
                let links: Vec<String> = targets.iter().filter_map(|t| app.item(*t)).map(|d| d.url.clone()).collect();
                app.copy_to_clipboard(&links.join("\r\n"));
            }
            TableAction::ShowProgress => app.show_progress_window(id),
            TableAction::ShowProperties => app.show_file_properties_dialog = Some(id),
            TableAction::MoveToQueue(q) => app.set_queue(&targets, Some(q)),
            TableAction::RemoveFromQueue => app.set_queue(&targets, None),
            TableAction::Delete => app.request_delete(targets),
        }
    }

    // Like a Windows list view, the grid goes on below the last row to the bottom.
    let painter = ui.painter().with_clip_rect(area);
    let mut y = last_row_bottom.unwrap_or(header_bottom) + ROW_HEIGHT;
    while y < area.bottom() {
        painter.hline(area.x_range(), y + 0.5, grid);
        y += ROW_HEIGHT;
    }
}

/// The list view at 125 %: a 24 px header and 21 px rows.
const ROW_HEADER: f32 = 19.2;
const ROW_HEIGHT: f32 = 16.8;

fn progress_cell(ui: &mut Ui, item: &DownloadItem) {
    let frac = item.progress_fraction();
    let width = ui.available_width().max(20.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 11.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(220, 224, 230));
    if frac > 0.0 {
        let fill = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width() * frac, rect.height()));
        let color = match item.status {
            DownloadStatus::Completed => Color32::from_rgb(46, 175, 80),
            DownloadStatus::Paused | DownloadStatus::Cancelled => Color32::from_rgb(170, 176, 188),
            DownloadStatus::Failed => Color32::from_rgb(220, 120, 120),
            _ => Color32::from_rgb(33, 150, 243),
        };
        ui.painter().rect_filled(fill, 2.0, color);
    }
    let text = if item.total_size.is_none() && item.status != DownloadStatus::Completed {
        "-".to_string()
    } else {
        format!("{:.1}%", frac * 100.0)
    };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(9.0), Color32::from_rgb(30, 30, 30));
}

fn pick(ui: &mut Ui, action: &mut Option<(u64, TableAction)>, id: u64, enabled: bool, label: &str, act: TableAction) {
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        *action = Some((id, act));
        ui.close_menu();
    }
}

fn context_menu(ui: &mut Ui, item: &DownloadItem, queues: &[(u32, String)], action: &mut Option<(u64, TableAction)>) {
    let id = item.id;
    let completed = item.status == DownloadStatus::Completed;

    if completed {
        pick(ui, action, id, true, "Open", TableAction::Open);
        pick(ui, action, id, true, "Open with...", TableAction::OpenWith);
    }
    pick(ui, action, id, true, "Open folder", TableAction::OpenFolder);
    ui.separator();
    pick(ui, action, id, item.status.can_start(), "Resume download", TableAction::Resume);
    pick(ui, action, id, item.status.is_active(), "Stop download", TableAction::Stop);
    pick(ui, action, id, true, "Redownload", TableAction::Redownload);
    ui.separator();
    pick(ui, action, id, true, "Copy download link", TableAction::CopyLink);
    pick(ui, action, id, !completed, "Show download progress", TableAction::ShowProgress);
    pick(ui, action, id, true, "Properties...", TableAction::ShowProperties);
    if !completed {
        ui.menu_button("Move to queue", |ui| {
            crate::gui::theme::menu_style(ui);
            for (qid, name) in queues {
                let label = if item.queue == Some(*qid) { format!("• {}", name) } else { name.clone() };
                pick(ui, action, id, true, &label, TableAction::MoveToQueue(*qid));
            }
        });
    }
    if item.queue.is_some() {
        pick(ui, action, id, true, "Remove from queue", TableAction::RemoveFromQueue);
    }
    ui.separator();
    pick(ui, action, id, true, "Delete...", TableAction::Delete);
}
