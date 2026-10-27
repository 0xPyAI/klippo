use eframe::egui::{self, Align2, FontId, PopupCloseBehavior, Rect, Response, Sense, Stroke, Ui, Vec2};

use crate::gui::icons::{self, ToolIcon};
use crate::gui::state::{CategoryFilter, DownloadStatus, KlippoApp, MAIN_QUEUE};
use crate::gui::theme;

pub fn render_toolbar(app: &mut KlippoApp, ui: &mut Ui) {
    let small = app.settings.toolbar_small;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(if small { 2.0 } else { 0.0 }, 0.0);

        if tool_button(ui, small, ToolIcon::AddUrl, "Add URL", "Add new download address manually", true).clicked() {
            app.open_add_url_dialog();
        }

        let selected = app.selected_ids();
        let can_start = selected.iter().filter_map(|id| app.item(*id)).any(|d| d.status.can_start());
        let can_stop = selected.iter().filter_map(|id| app.item(*id)).any(|d| d.status.is_active());

        if tool_button(ui, small, ToolIcon::Resume, "Resume", "Start/Resume downloading selected file(s)", can_start).clicked() {
            app.resume_selected();
        }
        if tool_button(ui, small, ToolIcon::Stop, "Stop", "Stop downloading selected file(s)", can_stop).clicked() {
            app.pause_selected();
        }
        let any_active = app.downloads.iter().any(|d| d.status.is_active());
        if tool_button(ui, small, ToolIcon::StopAll, "Stop All", "Stop all downloads", any_active).clicked() {
            app.stop_all_downloads();
        }
        if tool_button(ui, small, ToolIcon::Delete, "Delete", "Remove selected file(s) from the list", !selected.is_empty()).clicked() {
            app.request_delete(selected.clone());
        }
        let any_completed = app.downloads.iter().any(|d| d.status == DownloadStatus::Completed);
        if tool_button(ui, small, ToolIcon::DeleteCompleted, "Delete C...", "Remove all completed files from the list", any_completed).clicked() {
            app.delete_completed_downloads();
        }
        if tool_button(ui, small, ToolIcon::Options, "Options", "Browsers/System integration, File types, Proxy, Passwords and others", true).clicked() {
            app.open_options();
        }
        if tool_button(ui, small, ToolIcon::Scheduler, "Scheduler", "Scheduler and download queues", true).clicked() {
            app.show_scheduler_dialog = true;
        }

        // The queue selected in the tree, otherwise the main queue (the drop-down default).
        let target_queue = match app.selected_category {
            CategoryFilter::Queue(q) => q,
            _ => MAIN_QUEUE,
        };
        let queues: Vec<(u32, String)> = app.settings.queues.iter().map(|q| (q.id, q.name.clone())).collect();
        let queue_name = app.queue_name(target_queue);
        let can_start_queue = !app.is_queue_running(target_queue)
            && app.downloads.iter().any(|d| d.queue == Some(target_queue) && d.status != DownloadStatus::Completed);
        let (button, arrow) = queue_button(ui, small, ToolIcon::StartQueue, "Start Qu...", &format!("Start \"{}\"", queue_name), can_start_queue);
        if button.clicked() {
            app.start_queue(target_queue);
        }
        if let Some(arrow) = arrow {
            queue_menu(ui, &arrow, "start_queue_menu", &queues, |id| app.start_queue(id));
        }
        let (button, arrow) = queue_button(ui, small, ToolIcon::StopQueue, "Stop Qu...", &format!("Stop \"{}\"", queue_name), app.is_queue_running(target_queue));
        if button.clicked() {
            app.stop_queue(target_queue);
        }
        if let Some(arrow) = arrow {
            queue_menu(ui, &arrow, "stop_queue_menu", &queues, |id| app.stop_queue(id));
        }

        // The Grabber button runs the Site Grabber (video streams: Tasks > Grab video stream).
        if tool_button(ui, small, ToolIcon::Grabber, "Grabber", "Site Grabber: get the pictures, media or files of a web site", true).clicked() {
            app.open_site_grabber();
        }
    });
}

/// Toolbar button: the line icon above its label (View > Toolbar > Large buttons),
/// or the icon alone with the label in the tooltip (small buttons).
fn tool_button(ui: &mut Ui, small: bool, icon: ToolIcon, label: &str, tooltip: &str, enabled: bool) -> Response {
    let size = if small { Vec2::new(30.0, 30.0) } else { Vec2::new(60.0, 56.0) };
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let tip = if small { format!("{}: {}", label.trim_end_matches("..."), tooltip) } else { tooltip.to_string() };
    let response = response.on_hover_text(tip);
    // The button's whole name for screen readers and UI Automation ("Delete C..." is cut short).
    let name = match icon {
        ToolIcon::DeleteCompleted => "Delete Completed",
        ToolIcon::StartQueue => "Start Queue",
        ToolIcon::StopQueue => "Stop Queue",
        _ => label,
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
    paint_button(ui, rect, &response, icon, label, enabled, small);
    response
}

fn paint_button(ui: &Ui, rect: Rect, response: &Response, icon: ToolIcon, label: &str, enabled: bool, small: bool) {
    let theme = theme::current(ui.ctx());
    let p = ui.painter();
    if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
        let fill = if response.is_pointer_button_down_on() { ui.visuals().widgets.active.bg_fill } else { ui.visuals().widgets.hovered.bg_fill };
        p.rect_filled(rect.shrink(1.0), 2.0, fill);
        p.rect_stroke(rect.shrink(1.0), 2.0, Stroke::new(1.0_f32, ui.visuals().widgets.hovered.bg_stroke.color));
    }
    if small {
        icons::tool(p, Rect::from_center_size(rect.center(), Vec2::splat(22.0)), icon, enabled, &theme);
        return;
    }
    let icon_rect = Rect::from_center_size(rect.center_top() + Vec2::new(0.0, 20.0), Vec2::splat(26.0));
    icons::tool(p, icon_rect, icon, enabled, &theme);
    let color = if enabled { theme.text_primary } else { theme.icon_disabled };
    p.text(rect.center_bottom() - Vec2::new(0.0, 11.0), Align2::CENTER_CENTER, label, FontId::proportional(theme::LIST_FONT), color);
}

/// Start/Stop Queue: the button plus a small drop-down arrow for another queue.
fn queue_button(ui: &mut Ui, small: bool, icon: ToolIcon, label: &str, tooltip: &str, enabled: bool) -> (Response, Option<Response>) {
    let button = tool_button(ui, small, icon, label, tooltip, enabled);
    if small {
        return (button, None);
    }
    let (rect, arrow) = ui.allocate_exact_size(Vec2::new(14.0, 56.0), Sense::click());
    let arrow = arrow.on_hover_text("Choose a queue");
    let name = if matches!(icon, ToolIcon::StartQueue) { "Start Queue menu" } else { "Stop Queue menu" };
    arrow.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    let color = theme::current(ui.ctx()).text_primary;
    let c = rect.center() - Vec2::new(0.0, 8.0);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![c + Vec2::new(-4.0, -2.0), c + Vec2::new(4.0, -2.0), c + Vec2::new(0.0, 2.5)],
        color,
        Stroke::NONE,
    ));
    (button, Some(arrow))
}

fn queue_menu(ui: &mut Ui, arrow: &Response, id: &str, queues: &[(u32, String)], mut pick: impl FnMut(u32)) {
    let popup = ui.make_persistent_id(id);
    if arrow.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    egui::popup_below_widget(ui, popup, arrow, PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(160.0);
        for (qid, name) in queues {
            if ui.button(name).clicked() {
                pick(*qid);
            }
        }
    });
}
