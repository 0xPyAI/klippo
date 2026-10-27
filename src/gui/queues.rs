//! Download queues and their schedules (the Scheduler: several named queues,
//! each with its own start/stop times, parallel files and actions when done).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::Datelike;
use serde::{Deserialize, Serialize};

use crate::gui::state::{DownloadStatus, KlippoApp};
use crate::gui::system::{self, SoundEvent};

/// Id of the "Main download queue"; it always exists.
pub const MAIN_QUEUE: u32 = 0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueConfig {
    pub id: u32,
    pub name: String,
    /// Files downloaded at the same time.
    pub simultaneous: usize,
    /// Automatic retries for each failed file.
    pub retries: u32,

    pub start_enabled: bool,
    pub start_time: String,
    /// "Once at" a date instead of on days of the week.
    pub start_once: bool,
    /// YYYY-MM-DD for `start_once`.
    pub start_date: String,
    pub daily: bool,
    /// Monday first.
    pub days: [bool; 7],
    /// "Start again every" N minutes after the last start.
    pub repeat_enabled: bool,
    pub repeat_minutes: u32,
    pub stop_enabled: bool,
    pub stop_time: String,
    /// "Start download on Klippo startup".
    pub start_on_startup: bool,

    pub stop_when_done: bool,
    /// "Open the following file when done".
    pub open_when_done: String,
    pub hangup_when_done: bool,
    pub exit_when_done: bool,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self::new(MAIN_QUEUE, "Main download queue")
    }
}

impl QueueConfig {
    pub fn new(id: u32, name: &str) -> Self {
        Self {
            id,
            name: name.to_string(),
            simultaneous: 1,
            retries: 5,
            start_enabled: false,
            start_time: "02:00".to_string(),
            start_once: false,
            start_date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            daily: true,
            days: [true; 7],
            repeat_enabled: false,
            repeat_minutes: 120,
            stop_enabled: false,
            stop_time: "07:00".to_string(),
            start_on_startup: false,
            stop_when_done: true,
            open_when_done: String::new(),
            hangup_when_done: false,
            exit_when_done: false,
        }
    }

    /// True when the start time applies to `now` (date or weekday rules).
    fn start_day_matches(&self, now: &chrono::DateTime<chrono::Local>) -> bool {
        if self.start_once {
            return now.format("%Y-%m-%d").to_string() == self.start_date.trim();
        }
        self.daily || self.days[now.weekday().num_days_from_monday() as usize]
    }
}

/// "2:5" -> "02:05"; None when it is not a valid time.
pub fn normalize_time(s: &str) -> Option<String> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    (h < 24 && m < 60).then(|| format!("{:02}:{:02}", h, m))
}

impl KlippoApp {
    pub fn queue_name(&self, id: u32) -> String {
        self.settings
            .queues
            .iter()
            .find(|q| q.id == id)
            .map(|q| q.name.clone())
            .unwrap_or_else(|| "Main download queue".to_string())
    }

    pub fn queue_config(&self, id: u32) -> QueueConfig {
        self.settings.queues.iter().find(|q| q.id == id).cloned().unwrap_or_default()
    }

    pub fn is_queue_running(&self, id: u32) -> bool {
        self.running_queues.contains(&id)
    }

    pub fn any_queue_running(&self) -> bool {
        !self.running_queues.is_empty()
    }

    /// Creates a queue and returns its id.
    pub fn add_queue(&mut self, name: &str) -> u32 {
        let id = self.settings.queues.iter().map(|q| q.id).max().unwrap_or(MAIN_QUEUE) + 1;
        let name = if name.trim().is_empty() { format!("Queue {}", id) } else { name.trim().to_string() };
        self.settings.queues.push(QueueConfig::new(id, &name));
        self.save_settings_only();
        id
    }

    /// Deletes a queue (not the main one); its files stay in the list without a queue.
    pub fn delete_queue(&mut self, id: u32) {
        if id == MAIN_QUEUE {
            return;
        }
        self.stop_queue(id);
        self.settings.queues.retain(|q| q.id != id);
        for item in self.downloads.iter_mut().filter(|d| d.queue == Some(id)) {
            item.queue = None;
        }
        self.save_settings_only();
    }

    /// Puts files in a queue (or takes them out with `None`).
    pub fn set_queue(&mut self, ids: &[u64], queue: Option<u32>) {
        for &id in ids {
            if let Some(item) = self.item_mut(id) {
                item.queue = if item.status == DownloadStatus::Completed { None } else { queue };
            }
        }
        self.save_downloads();
    }

    /// Moves a file before the previous / after the next file of the same queue.
    pub fn move_in_queue(&mut self, id: u64, up: bool) {
        let Some(queue) = self.item(id).and_then(|d| d.queue) else { return };
        let queue_idx: Vec<usize> = self
            .downloads
            .iter()
            .enumerate()
            .filter(|(_, d)| d.queue == Some(queue))
            .map(|(i, _)| i)
            .collect();
        let Some(pos) = queue_idx.iter().position(|&i| self.downloads[i].id == id) else { return };
        let other = if up { pos.checked_sub(1) } else { Some(pos + 1).filter(|p| *p < queue_idx.len()) };
        if let Some(other) = other {
            self.downloads.swap(queue_idx[pos], queue_idx[other]);
            self.save_downloads();
        }
    }

    pub fn start_queue(&mut self, id: u32) {
        if !self.running_queues.contains(&id) && self.settings.sound_alerts && self.settings.sound_queue_start {
            crate::gui::system::play_sound(crate::gui::system::SoundEvent::QueueStarted, &self.settings.sound_queue_start_path);
        }
        self.running_queues.insert(id);
        self.queue_did_work.remove(&id);
        self.queue_last_start.insert(id, Instant::now());
        let ids: HashSet<u64> = self.downloads.iter().filter(|d| d.queue == Some(id)).map(|d| d.id).collect();
        self.queue_skip.retain(|i| !ids.contains(i));
        for item in self.downloads.iter_mut().filter(|d| d.queue == Some(id)) {
            item.retries_done = 0;
        }
        self.queue_tick();
    }

    /// Stops the queue and its files that are downloading.
    pub fn stop_queue(&mut self, id: u32) {
        self.running_queues.remove(&id);
        let ids: Vec<u64> = self
            .downloads
            .iter()
            .filter(|d| d.queue == Some(id) && d.status.is_active())
            .map(|d| d.id)
            .collect();
        for item_id in ids {
            self.pause_download(item_id);
            self.queue_skip.remove(&item_id);
        }
    }

    pub fn stop_all_queues(&mut self) {
        let running: Vec<u32> = self.running_queues.iter().copied().collect();
        for id in running {
            self.stop_queue(id);
        }
    }

    fn queue_waiting(&self, queue: u32, retries: u32) -> Vec<u64> {
        let now = Instant::now();
        self.downloads
            .iter()
            .filter(|d| d.queue == Some(queue) && d.status.can_start() && !self.queue_skip.contains(&d.id))
            .filter(|d| d.status != DownloadStatus::Failed || d.retries_done < retries)
            .filter(|d| self.retry_not_before.get(&d.id).is_none_or(|t| now >= *t))
            .map(|d| d.id)
            .collect()
    }

    /// True when a running queue still has files to start.
    pub fn queues_have_waiting(&self) -> bool {
        self.running_queues.iter().any(|q| {
            let cfg = self.queue_config(*q);
            !self.queue_waiting(*q, cfg.retries).is_empty()
        })
    }

    /// Starts waiting files of every running queue and detects finished queues.
    pub fn queue_tick(&mut self) {
        let running: Vec<u32> = self.running_queues.iter().copied().collect();
        for queue in running {
            let cfg = self.queue_config(queue);
            let active = self.downloads.iter().filter(|d| d.queue == Some(queue) && d.status.is_active()).count();
            let waiting = self.queue_waiting(queue, cfg.retries);

            for id in waiting.iter().take(cfg.simultaneous.max(1).saturating_sub(active)) {
                if let Some(item) = self.item_mut(*id) {
                    if item.status == DownloadStatus::Failed {
                        item.retries_done += 1;
                    }
                }
                self.start_item(*id, false);
                self.queue_did_work.insert(queue);
            }

            // Done when nothing runs and nothing can still be (re)tried.
            let pending_retry = self.downloads.iter().any(|d| {
                d.queue == Some(queue)
                    && d.status == DownloadStatus::Failed
                    && d.retries_done < cfg.retries
                    && !self.queue_skip.contains(&d.id)
            });
            if active == 0 && waiting.is_empty() && !pending_retry {
                let did_work = self.queue_did_work.remove(&queue);
                if cfg.stop_when_done || !did_work {
                    self.running_queues.remove(&queue);
                }
                if did_work {
                    self.on_queue_complete(&cfg);
                }
            }
        }
    }

    fn on_queue_complete(&mut self, cfg: &QueueConfig) {
        self.set_status(format!("{}: processing is complete", cfg.name));
        if self.settings.sound_alerts && self.settings.sound_queue_done {
            system::play_sound(SoundEvent::QueueComplete, &self.settings.sound_queue_done_path);
        }
        let open = cfg.open_when_done.trim();
        if !open.is_empty() {
            if let Err(e) = system::open_path(open) {
                self.set_status(format!("Could not open {}: {}", open, e));
            }
        }
        if cfg.hangup_when_done || self.settings.hangup_when_done {
            system::hang_up_connections();
        }
        if cfg.exit_when_done {
            self.exit_confirmed = true;
            self.pending_exit = true;
        }
    }

    /// Queues marked "Start download on Klippo startup".
    pub fn start_queues_on_startup(&mut self) {
        let ids: Vec<u32> = self.settings.queues.iter().filter(|q| q.start_on_startup).map(|q| q.id).collect();
        for id in ids {
            self.start_queue(id);
        }
    }

    /// Starts/stops queues at their scheduled minute; restarts periodic queues.
    pub fn scheduler_tick(&mut self) {
        let now = chrono::Local::now();
        let minute = now.format("%Y-%m-%d %H:%M").to_string();
        if self.scheduler_last_minute.as_deref() == Some(minute.as_str()) {
            return;
        }
        self.scheduler_last_minute = Some(minute);
        let hm = now.format("%H:%M").to_string();

        for cfg in self.settings.queues.clone() {
            if cfg.start_enabled
                && cfg.start_day_matches(&now)
                && normalize_time(&cfg.start_time).as_deref() == Some(hm.as_str())
            {
                self.start_queue(cfg.id);
                self.set_status(format!("Scheduler started \"{}\"", cfg.name));
            }
            if cfg.repeat_enabled && cfg.repeat_minutes > 0 && !self.is_queue_running(cfg.id) {
                let due = self
                    .queue_last_start
                    .get(&cfg.id)
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(cfg.repeat_minutes as u64 * 60));
                if due {
                    self.start_queue(cfg.id);
                    self.set_status(format!("Scheduler started \"{}\" again", cfg.name));
                }
            }
            if cfg.stop_enabled && normalize_time(&cfg.stop_time).as_deref() == Some(hm.as_str()) && self.is_queue_running(cfg.id) {
                self.stop_queue(cfg.id);
                self.set_status(format!("Scheduler stopped \"{}\"", cfg.name));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_normalization() {
        assert_eq!(normalize_time("2:5").as_deref(), Some("02:05"));
        assert_eq!(normalize_time("23:59").as_deref(), Some("23:59"));
        assert_eq!(normalize_time("24:00"), None);
        assert_eq!(normalize_time("abc"), None);
    }

    #[test]
    fn start_day_rules() {
        let mut q = QueueConfig::new(1, "q");
        let now = chrono::Local::now();
        assert!(q.start_day_matches(&now));
        q.daily = false;
        q.days = [false; 7];
        assert!(!q.start_day_matches(&now));
        q.days[now.weekday().num_days_from_monday() as usize] = true;
        assert!(q.start_day_matches(&now));
        q.start_once = true;
        q.start_date = "2000-01-01".to_string();
        assert!(!q.start_day_matches(&now));
        q.start_date = now.format("%Y-%m-%d").to_string();
        assert!(q.start_day_matches(&now));
    }
}
