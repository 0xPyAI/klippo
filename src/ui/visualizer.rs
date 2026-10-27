use std::io::{self, Write};
use crate::core::segment::SegmentStatus;

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

pub struct TerminalVisualizer;

impl TerminalVisualizer {
    pub async fn render(
        filename: &str,
        total_size: Option<u64>,
        resumable: bool,
        segments: &tokio::sync::RwLock<Vec<crate::core::segment::Segment>>,
        current_speed: &tokio::sync::Mutex<f64>,
    ) {
        let segs = segments.read().await;
        let downloaded = segs.iter().map(|s| s.downloaded_bytes()).sum::<u64>();
        let total = total_size.unwrap_or(downloaded);
        let pct = if total > 0 { (downloaded as f64 / total as f64) * 100.0 } else { 0.0 };
        let speed = *current_speed.lock().await;
        let active_conns = segs.iter().filter(|s| s.status == SegmentStatus::Downloading).count();

        // Calculate ETA
        let eta_str = if speed > 0.0 && total > downloaded {
            let rem_secs = ((total - downloaded) as f64 / speed) as u64;
            let m = rem_secs / 60;
            let s = rem_secs % 60;
            format!("{:02}:{:02}", m, s)
        } else {
            "--:--".to_string()
        };

        // Clear screen / move cursor up
        let mut stdout = io::stdout();
        print!("\x1B[2J\x1B[H"); // Clear ANSI

        println!("================================================================================");
        println!("  Klippo (Rust) - Multi-Connection Acceleration Engine");
        println!("================================================================================");
        println!(
            "  File:       {}",
            filename
        );
        println!(
            "  Progress:   {} / {} ({:.1}%)  |  ETA: {}",
            format_bytes(downloaded),
            format_bytes(total),
            pct,
            eta_str
        );
        println!(
            "  Speed:      {}/s  |  Active Conns: {} ({})",
            format_bytes(speed as u64),
            active_conns,
            if resumable { "Resumable" } else { "Single Stream" }
        );

        // Master bar
        let bar_width: usize = 50;
        let filled = ((pct / 100.0) * (bar_width as f64)).clamp(0.0, bar_width as f64) as usize;
        let empty = bar_width.saturating_sub(filled);
        println!("  [{}{}]", "=".repeat(filled), " ".repeat(empty));
        println!("--------------------------------------------------------------------------------");
        println!("  Conn #   Byte Range                      Downloaded     Speed       Status");
        println!("--------------------------------------------------------------------------------");

        for seg in segs.iter() {
            let conn_num = seg.connection_id.unwrap_or(seg.segment_id);
            let range = if seg.is_unbounded() {
                format!("{}-end of stream", format_bytes(seg.start))
            } else {
                format!("{}-{}", format_bytes(seg.start), format_bytes(seg.end.saturating_add(1)))
            };
            let d_str = format_bytes(seg.downloaded_bytes());
            let spd_str = if seg.speed_bps > 0.0 {
                format!("{}/s", format_bytes(seg.speed_bps as u64))
            } else {
                "-".to_string()
            };
            let status_str = match seg.status {
                SegmentStatus::Downloading => "ACTIVE",
                SegmentStatus::Completed => "DONE",
                SegmentStatus::Pending => "PENDING",
                SegmentStatus::Paused => "PAUSED",
                SegmentStatus::Failed => "FAILED",
            };

            let seg_pct = seg.progress_fraction();
            let seg_bar_filled = ((seg_pct * 12.0) as usize).min(12);
            let seg_bar = format!("[{}{}]", "█".repeat(seg_bar_filled), "░".repeat(12 - seg_bar_filled));

            println!(
                "  #{:<6}  {:<30}  {:<12}  {:<10}  {:<8} {}",
                conn_num, range, d_str, spd_str, status_str, seg_bar
            );
        }
        println!("================================================================================");
        let _ = stdout.flush();
    }
}
