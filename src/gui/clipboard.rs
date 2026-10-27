use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use crate::core::http::wildcard_match;

pub struct ClipboardWatcher {
    last_text: Arc<Mutex<Option<String>>>,
    detected_url: Arc<Mutex<Option<String>>>,
    enabled: Arc<AtomicBool>,
    file_types: Arc<RwLock<Vec<String>>>,
}

impl ClipboardWatcher {
    pub fn new(enabled: bool, file_types: &str) -> Self {
        let last_text: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let detected_url = Arc::new(Mutex::new(None));
        let enabled_flag = Arc::new(AtomicBool::new(enabled));
        let types = Arc::new(RwLock::new(parse_file_types(file_types)));

        let last_clone = last_text.clone();
        let detected_clone = detected_url.clone();
        let enabled_thread = enabled_flag.clone();
        let types_thread = types.clone();

        thread::Builder::new()
            .name("klippo-clipboard-monitor".to_string())
            .spawn(move || {
                let mut clipboard = match arboard::Clipboard::new() {
                    Ok(c) => c,
                    Err(_) => return,
                };

                // Whatever was copied before Klippo started is not a new link.
                if let Ok(text) = clipboard.get_text() {
                    *last_clone.lock().unwrap() = Some(text.trim().to_string());
                }

                loop {
                    thread::sleep(Duration::from_millis(500));

                    let Ok(text) = clipboard.get_text() else { continue };
                    let trimmed = text.trim().to_string();
                    {
                        let mut last = last_clone.lock().unwrap();
                        if last.as_deref() == Some(trimmed.as_str()) {
                            continue;
                        }
                        *last = Some(trimmed.clone());
                    }

                    if !enabled_thread.load(Ordering::Relaxed) {
                        continue;
                    }
                    let types = types_thread.read().unwrap();
                    if is_downloadable_url(&trimmed, &types) {
                        *detected_clone.lock().unwrap() = Some(trimmed);
                    }
                }
            })
            .expect("Failed to spawn clipboard watcher");

        Self {
            last_text,
            detected_url,
            enabled: enabled_flag,
            file_types: types,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn set_file_types(&self, file_types: &str) {
        *self.file_types.write().unwrap() = parse_file_types(file_types);
    }

    /// Text Klippo itself puts on the clipboard must not trigger a capture.
    pub fn ignore_text(&self, text: &str) {
        *self.last_text.lock().unwrap() = Some(text.trim().to_string());
    }

    pub fn take_detected_url(&self) -> Option<String> {
        self.detected_url.lock().unwrap().take()
    }
}

/// Splits the "File types" option (space or comma separated, wildcards allowed).
pub fn parse_file_types(list: &str) -> Vec<String> {
    list.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(|s| s.trim().trim_start_matches('.').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// True when the URL path ends in one of the configured file types.
pub fn url_matches_file_types(url: &str, types: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else { return false };
    let Some(last) = parsed.path_segments().and_then(|mut s| s.rfind(|p| !p.is_empty())) else {
        return false;
    };
    let Some((_, ext)) = last.rsplit_once('.') else { return false };
    types.iter().any(|t| wildcard_match(t, ext))
}

fn is_downloadable_url(s: &str, types: &[String]) -> bool {
    if !s.starts_with("http://") && !s.starts_with("https://") {
        return false;
    }
    if s.len() > 2048 || s.contains(char::is_whitespace) {
        return false;
    }
    let lower = s.to_lowercase();
    url_matches_file_types(&lower, types) || lower.contains(".m3u8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_type_matching() {
        let types = parse_file_types("ZIP EXE R0* MP4");
        assert!(is_downloadable_url("https://a.com/x/setup.exe", &types));
        assert!(is_downloadable_url("https://a.com/archive.r01?token=1", &types));
        assert!(!is_downloadable_url("https://a.com/page.html", &types));
        assert!(!is_downloadable_url("https://a.com/.zipper/index", &types));
        assert!(!is_downloadable_url("see https://a.com/a.zip", &types));
        assert!(is_downloadable_url("https://cdn.a.com/live/master.m3u8", &types));
    }
}
