use std::time::Duration;
use eframe::egui::{Color32, RichText, Ui, Vec2};
use crate::gui::state::{format_bytes, DownloadStatus, KlippoApp};

pub fn render_status_bar(app: &mut KlippoApp, ui: &mut Ui) {
    if app.status_message.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(8)) {
        app.status_message = None;
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(12.0, 0.0);

        let total = app.downloads.len();
        let active = app.downloads.iter().filter(|d| d.status.is_active()).count();
        let completed = app.downloads.iter().filter(|d| d.status == DownloadStatus::Completed).count();

        ui.label(RichText::new(format!("📊 Total: {} | Active: {} | Complete: {}", total, active, completed)).size(11.0));

        ui.separator();

        let running: Vec<String> = app.running_queues.iter().map(|q| app.queue_name(*q)).collect();
        if running.is_empty() {
            ui.label(RichText::new("⏱ Queues: stopped").size(11.0));
        } else {
            let queued = app
                .downloads
                .iter()
                .filter(|d| d.queue.is_some_and(|q| app.running_queues.contains(&q)) && d.status != DownloadStatus::Completed)
                .count();
            ui.label(RichText::new(format!("🚀 Running: {} ({} files)", running.join(", "), queued)).size(11.0).color(ui.visuals().warn_fg_color));
        }

        if let Some((ref msg, _)) = app.status_message {
            ui.separator();
            ui.label(RichText::new(msg).size(11.0).color(Color32::from_rgb(33, 150, 243)));
        }

        ui.with_layout(eframe::egui::Layout::right_to_left(eframe::egui::Align::Center), |ui| {
            if app.settings.speed_limit_enabled {
                let limit_str = format!("🛡 Limiter: {} KB/s", app.settings.speed_limit_kbps);
                ui.label(RichText::new(limit_str).size(11.0).color(Color32::from_rgb(255, 152, 0)));
                ui.separator();
            }

            let global_bps: f64 = app.downloads.iter().filter(|d| d.status == DownloadStatus::Downloading).map(|d| d.speed_bps).sum();
            ui.label(RichText::new(format!("⚡ Speed: {}/s", format_bytes(global_bps as u64))).size(11.0));
        });
    });
}
