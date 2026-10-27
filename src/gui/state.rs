use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER};
use serde::{Deserialize, Serialize};

use crate::core::hls::HLSGrabber;
use crate::core::http::{self, PostBody, ProxyConfig, RequestOptions};
use crate::core::resume::ResumeManager;
use crate::core::youtube::{YoutubeGrabber, YoutubeSpec};
use crate::gui::clipboard::{parse_file_types, url_matches_file_types, ClipboardWatcher};
use crate::gui::ipc::{IpcMessage, IpcServer};
pub use crate::gui::queues::{normalize_time, QueueConfig, MAIN_QUEUE};
use crate::gui::manager::{DownloadManagerBridge, DownloadRequest, GuiEvent, ProbeInfo};
use crate::gui::system::{self, SoundEvent};
use crate::gui::theme::IdmTheme;
use crate::gui::views::options_modal::OptionsTab;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum Category {
    #[default]
    General,
    Compressed,
    Documents,
    Music,
    Programs,
    Video,
    /// A category the user added (Options > Save to > New); its name is in
    /// `AppSettings::user_categories`.
    User(u16),
}

/// A category the user added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCategory {
    pub id: u16,
    pub name: String,
}

// Saved as text ("Video", "User3"), also as a key of the settings maps.
impl Serialize for Category {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.key())
    }
}

impl<'de> Deserialize<'de> for Category {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Category::from_key(&text))
    }
}

impl Category {
    /// The categories that are always there.
    pub const ALL: [Category; 6] = [
        Category::General,
        Category::Compressed,
        Category::Documents,
        Category::Music,
        Category::Programs,
        Category::Video,
    ];

    /// The fixed categories, then the user's categories.
    pub fn all() -> Vec<Category> {
        let user = USER_CATEGORIES.read().map(|list| list.iter().map(|c| Category::User(c.id)).collect::<Vec<_>>()).unwrap_or_default();
        Category::ALL.into_iter().chain(user).collect()
    }

    /// Uses the categories from Options > Save to for `name`, `all` and `from_extension`.
    pub fn set_user_categories(list: &[UserCategory]) {
        if let Ok(mut guard) = USER_CATEGORIES.write() {
            *guard = list.to_vec();
        }
    }

    fn key(&self) -> String {
        match self {
            Category::User(id) => format!("User{}", id),
            fixed => fixed.fixed_name().to_string(),
        }
    }

    /// Unknown text (a category that was deleted) becomes General.
    fn from_key(text: &str) -> Self {
        Category::ALL
            .into_iter()
            .find(|c| c.fixed_name() == text)
            .or_else(|| text.strip_prefix("User").and_then(|n| n.parse().ok()).map(Category::User))
            .unwrap_or_default()
    }

    fn fixed_name(&self) -> &'static str {
        match self {
            Category::General => "General",
            Category::Compressed => "Compressed",
            Category::Documents => "Documents",
            Category::Music => "Music",
            Category::Programs => "Programs",
            Category::Video => "Video",
            Category::User(_) => "",
        }
    }

    pub fn name(&self) -> String {
        match self {
            Category::User(id) => USER_CATEGORIES
                .read()
                .ok()
                .and_then(|list| list.iter().find(|c| c.id == *id).map(|c| c.name.clone()))
                .unwrap_or_else(|| format!("Category {}", id)),
            fixed => fixed.fixed_name().to_string(),
        }
    }

    pub fn is_user(&self) -> bool {
        matches!(self, Category::User(_))
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Category::General | Category::User(_) => "📁",
            Category::Compressed => "📦",
            Category::Documents => "📄",
            Category::Music => "🎵",
            Category::Programs => "💿",
            Category::Video => "🎬",
        }
    }

    /// The file types put in this category unless the user changed them.
    pub fn default_types(self) -> &'static str {
        match self {
            Category::General | Category::User(_) => "",
            Category::Compressed => "ZIP RAR 7Z TAR GZ BZ2 ISO XZ TGZ ARJ LZH ACE Z CAB ZST IMG",
            Category::Documents => "DOC DOCX PDF TXT RTF XLS XLSX PPT PPTX PPS CSV EPUB ODT ODS ODP MOBI",
            Category::Music => "MP3 WAV FLAC AAC OGG WMA M4A OPUS AIF AIFF MPA RA",
            Category::Programs => "EXE MSI MSU BAT CMD APK BIN DEB RPM SH DMG PKG APPX MSIX",
            Category::Video => "MP4 MKV AVI MOV WMV FLV WEBM M4V TS M3U8 3GP MPG MPEG OGV ASF RM RMVB QT",
        }
    }

    /// Uses the lists from Options > Save to for `from_extension`.
    pub fn set_custom_types(types: &std::collections::BTreeMap<Category, String>) {
        let map = types
            .iter()
            .map(|(c, list)| (*c, list.split_whitespace().map(|t| t.to_lowercase()).collect::<Vec<_>>()))
            .collect();
        if let Ok(mut guard) = CUSTOM_TYPES.write() {
            *guard = Some(map);
        }
    }

    pub fn from_extension(ext: &str) -> Self {
        let e = ext.to_lowercase();
        if let Some(map) = CUSTOM_TYPES.read().ok().and_then(|g| g.clone()) {
            // The user's categories first: their types can come from a fixed category.
            let all = Category::all();
            let (user, fixed): (Vec<Category>, Vec<Category>) = all[1..].iter().partition(|c| c.is_user());
            return user
                .into_iter()
                .chain(fixed)
                .find(|c| map.get(c).is_some_and(|list| list.iter().any(|t| http::wildcard_match(t, &e))))
                .unwrap_or_default();
        }
        match e.as_str() {
            "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "iso" | "xz" | "tgz" | "arj" | "lzh" | "ace" | "z" | "cab"
            | "zst" | "img" => Category::Compressed,
            "doc" | "docx" | "pdf" | "txt" | "rtf" | "xls" | "xlsx" | "ppt" | "pptx" | "pps" | "csv" | "epub" | "odt"
            | "ods" | "odp" | "mobi" => Category::Documents,
            "mp3" | "wav" | "flac" | "aac" | "ogg" | "wma" | "m4a" | "opus" | "aif" | "aiff" | "mpa" | "ra" => {
                Category::Music
            }
            "exe" | "msi" | "msu" | "bat" | "cmd" | "apk" | "bin" | "deb" | "rpm" | "sh" | "dmg" | "pkg" | "appx"
            | "msix" => Category::Programs,
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "ts" | "m3u8" | "3gp" | "mpg" | "mpeg"
            | "ogv" | "asf" | "rm" | "rmvb" | "qt" => Category::Video,
            _ => Category::General,
        }
    }

    pub fn from_filename(name: &str) -> Self {
        Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(Self::from_extension)
            .unwrap_or_default()
    }
}

static CUSTOM_TYPES: std::sync::RwLock<Option<std::collections::BTreeMap<Category, Vec<String>>>> = std::sync::RwLock::new(None);
static USER_CATEGORIES: std::sync::RwLock<Vec<UserCategory>> = std::sync::RwLock::new(Vec::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryFilter {
    All,
    Category(Category),
    Unfinished,
    Finished,
    /// The categories tree: Unfinished > Video, Finished > Music, ...
    UnfinishedIn(Category),
    FinishedIn(Category),
    Queue(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadStatus {
    Queued,
    Connecting,
    Downloading,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn display_text(&self) -> &'static str {
        match self {
            DownloadStatus::Queued => "Queued",
            DownloadStatus::Connecting => "Connecting...",
            DownloadStatus::Downloading => "Downloading",
            DownloadStatus::Paused => "Paused",
            DownloadStatus::Completed => "Complete",
            DownloadStatus::Failed => "Error",
            DownloadStatus::Cancelled => "Stopped",
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, DownloadStatus::Connecting | DownloadStatus::Downloading)
    }

    /// Can be started or resumed.
    pub fn can_start(&self) -> bool {
        matches!(
            self,
            DownloadStatus::Queued | DownloadStatus::Paused | DownloadStatus::Failed | DownloadStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DownloadKind {
    #[default]
    Http,
    /// HLS (.m3u8) stream assembled from media segments.
    Hls,
    /// YouTube video fetched through the player's SABR stream and joined into one MP4.
    Youtube,
    /// `ftp://` file over one FTP connection.
    Ftp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentSnapshot {
    pub segment_id: usize,
    pub start: u64,
    pub end: u64,
    pub current_offset: u64,
    pub is_active: bool,
    pub is_completed: bool,
    pub speed_bps: f64,
}

impl SegmentSnapshot {
    pub fn is_unbounded(&self) -> bool {
        self.end == u64::MAX
    }

    pub fn progress_fraction(&self) -> f32 {
        if self.is_completed {
            return 1.0;
        }
        if self.end < self.start || self.is_unbounded() {
            return 0.0;
        }
        let total = (self.end - self.start).saturating_add(1);
        let done = self.downloaded_bytes();
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }

    pub fn downloaded_bytes(&self) -> u64 {
        self.current_offset.min(self.end.saturating_add(1)).saturating_sub(self.start)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadItem {
    pub id: u64,
    pub url: String,
    pub filename: String,
    pub save_path: PathBuf,
    pub category: Category,
    pub total_size: Option<u64>,
    pub downloaded_bytes: u64,
    pub status: DownloadStatus,
    pub speed_bps: f64,
    pub eta_seconds: Option<u64>,
    pub date_added: String,
    pub resumable: bool,
    pub connections: usize,
    pub segments: Vec<SegmentSnapshot>,
    pub error_message: Option<String>,
    /// Queue the file belongs to (`None` = not queued).
    #[serde(default)]
    pub queue: Option<u32>,
    /// Older versions only had one queue; read and migrated into `queue`.
    #[serde(default, rename = "in_queue", skip_serializing)]
    pub legacy_in_queue: bool,
    #[serde(default)]
    pub kind: DownloadKind,
    /// The name came from the URL and the server may still rename it.
    #[serde(default)]
    pub filename_auto: bool,
    #[serde(default)]
    pub last_try: String,
    /// Per-file limit from the progress window (0 = none).
    #[serde(default)]
    pub speed_limit_kbps: u64,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Automatic queue retries used after errors.
    #[serde(default)]
    pub retries_done: u32,
    /// Stream session recorded by the browser for a YouTube download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub youtube: Option<YoutubeSpec>,
    /// Separate audio stream joined to the video (HLS audio playlist, DASH-style file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_url: Option<String>,
    /// Final place chosen in "Download File Info" after the download had started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_to: Option<PathBuf>,
    /// The browser's proxy when the browser handed the download over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_proxy: Option<String>,
    /// Retrying through `browser_proxy` after an error.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub via_browser_proxy: bool,
    /// A download that a form started: the form's POST body (base64) and its
    /// Content-Type, sent again for the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_type: Option<String>,
    /// A checksum the user entered to verify the finished file against (empty = none).
    #[serde(default)]
    pub expected_hash: String,
    #[serde(default)]
    pub hash_algo: HashAlgo,
    /// Result of the checksum check: Some(true) = matched, Some(false) = mismatch.
    #[serde(default)]
    pub verified: Option<bool>,
}

impl DownloadItem {
    pub fn progress_fraction(&self) -> f32 {
        if self.status == DownloadStatus::Completed {
            return 1.0;
        }
        if let Some(total) = self.total_size {
            if total > 0 {
                return (self.downloaded_bytes as f32 / total as f32).clamp(0.0, 1.0);
            }
        }
        0.0
    }

    pub fn size_string(&self) -> String {
        match self.total_size {
            Some(total) if self.kind == DownloadKind::Hls && self.status != DownloadStatus::Completed => {
                format!("~{}", format_bytes(total))
            }
            Some(total) => format_bytes(total),
            None if self.downloaded_bytes > 0 => format!("{}+", format_bytes(self.downloaded_bytes)),
            None => "Unknown".to_string(),
        }
    }

    pub fn speed_string(&self) -> String {
        if self.status == DownloadStatus::Downloading && self.speed_bps > 0.0 {
            format!("{}/s", format_bytes(self.speed_bps as u64))
        } else {
            String::new()
        }
    }

    pub fn eta_string(&self) -> String {
        if self.status != DownloadStatus::Downloading {
            return String::new();
        }
        match self.eta_seconds {
            Some(eta) => format_duration(eta),
            None => String::new(),
        }
    }

    pub fn status_string(&self) -> String {
        match self.status {
            DownloadStatus::Downloading | DownloadStatus::Paused if self.total_size.is_some() => {
                format!("{:.2}%", self.progress_fraction() * 100.0)
            }
            _ => self.status.display_text().to_string(),
        }
    }

    pub fn last_try_string(&self) -> &str {
        if self.last_try.is_empty() {
            &self.date_added
        } else {
            &self.last_try
        }
    }

    /// Files to delete when an unfinished download is removed.
    pub fn temp_paths(&self) -> Vec<PathBuf> {
        match self.kind {
            DownloadKind::Http => vec![self.save_path.clone(), ResumeManager::state_path_for(&self.save_path)],
            DownloadKind::Hls => vec![HLSGrabber::temp_dir_for(&self.save_path)],
            DownloadKind::Youtube => vec![YoutubeGrabber::parts_dir_for(&self.save_path)],
            DownloadKind::Ftp => vec![self.save_path.clone()],
        }
    }
}

pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{} sec", secs)
    } else if secs < 3600 {
        format!("{} min {} sec", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{} hr {} min", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{} days {} hr", secs / 86_400, (secs % 86_400) / 3600)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionSpeedType {
    HighSpeed,
    MediumSpeed,
    LowSpeed,
}

impl ConnectionSpeedType {
    pub fn label(&self) -> &'static str {
        match self {
            ConnectionSpeedType::HighSpeed => "High speed: Direct connection (Ethernet/Cable) / Wi-Fi / 4G / 5G",
            ConnectionSpeedType::MediumSpeed => "Medium speed: DSL / Cable / LAN",
            ConnectionSpeedType::LowSpeed => "Low speed: Dial-Up / Mobile 2G / 3G",
        }
    }

    /// The suggested connection count for the line type.
    pub fn default_connections(&self) -> usize {
        match self {
            ConnectionSpeedType::HighSpeed => 16,
            ConnectionSpeedType::MediumSpeed => 8,
            ConnectionSpeedType::LowSpeed => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteLogin {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl SiteLogin {
    /// Matches `https://host/path` prefixes or `*.host` style patterns.
    pub fn matches(&self, url: &str) -> bool {
        let pattern = self.url.trim();
        if pattern.is_empty() {
            return false;
        }
        if pattern.contains('*') {
            let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.to_string()));
            return http::wildcard_match(pattern, url) || host.is_some_and(|h| http::wildcard_match(pattern, &h));
        }
        if pattern.contains("://") {
            return url.to_lowercase().starts_with(&pattern.to_lowercase());
        }
        url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case(pattern) || h.to_lowercase().ends_with(&format!(".{}", pattern.to_lowercase()))))
            .unwrap_or(false)
    }
}

/// What to do when a link that is already in the list is added again
/// (the "duplicate download link" case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DuplicateAction {
    #[default]
    Ask,
    /// Add the duplicate with a numbered file name.
    Numbered,
    /// Add the duplicate and overwrite the existing file.
    Overwrite,
    /// Show the complete dialog if the existing file is complete, else resume it.
    ResumeExisting,
}

impl DuplicateAction {
    pub fn label(&self) -> &'static str {
        match self {
            DuplicateAction::Ask => "Show a dialog and ask what to do",
            DuplicateAction::Numbered => "Add the duplicate with a numbered file name",
            DuplicateAction::Overwrite => "Add the duplicate and overwrite the existing file",
            DuplicateAction::ResumeExisting => "If the existing file is complete, show it; otherwise resume it",
        }
    }
}

/// "Max. connections number for a server" exception.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerLimit {
    /// Host name; `*` is a wildcard.
    pub server: String,
    pub max_connections: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerTab {
    Schedule,
    FilesQueue,
}

/// "Customize download panels in browsers": the panel over web players
/// and the panel for selected links.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelSettings {
    /// Panel view: mini mode (an icon) instead of full mode.
    pub mini: bool,
    /// File types the web-player panel is shown for, and whether each is on.
    pub types: Vec<(String, bool)>,
    /// Media files smaller than this get no panel (streams have no size).
    pub min_size_kb: u64,
    /// Sites without the panel, separated by spaces (* is a wildcard).
    pub exceptions: String,
    /// "Don't capture downloads from web-players automatically".
    pub no_auto_capture: bool,
    /// "Show download panel for protected content which may not download".
    pub protected: bool,
    /// The panel for selected links.
    pub links: LinksPanel,
    pub links_sites: Vec<String>,
    pub links_mini: bool,
}

/// When the selected-links panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LinksPanel {
    #[default]
    Any,
    Never,
    Sites,
}

impl Default for PanelSettings {
    fn default() -> Self {
        let types = "MP4 WEBM M4V MKV MOV AVI FLV 3GP WMV MPG MPEG OGV TS M3U8 MPD MP3 M4A AAC OGG OPUS WAV WMA FLAC";
        Self {
            mini: false,
            types: types.split_whitespace().map(|t| (t.to_string(), true)).collect(),
            min_size_kb: 100,
            exceptions: String::new(),
            no_auto_capture: false,
            protected: true,
            links: LinksPanel::Any,
            links_sites: Vec::new(),
            links_mini: false,
        }
    }
}

impl PanelSettings {
    /// What the browser extension reads ("config" answer of the native host).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "mini": self.mini,
            "types": self.types.iter().filter(|(_, on)| *on).map(|(t, _)| t.to_lowercase()).collect::<Vec<_>>(),
            "min_size": self.min_size_kb.saturating_mul(1024),
            "exceptions": self.exceptions.split_whitespace().collect::<Vec<_>>(),
            "no_auto_capture": self.no_auto_capture,
            "protected": self.protected,
            "links": match self.links {
                LinksPanel::Any => "any",
                LinksPanel::Never => "never",
                LinksPanel::Sites => "sites",
            },
            "links_sites": self.links_sites,
            "links_mini": self.links_mini,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub default_dir: PathBuf,
    pub category_dirs: HashMap<Category, PathBuf>,
    pub max_connections: usize,
    pub speed_limit_enabled: bool,
    pub speed_limit_kbps: u64,
    pub monitor_clipboard: bool,
    pub auto_start: bool,
    pub show_progress_window: bool,
    pub show_complete_window: bool,
    pub sound_alerts: bool,
    pub dark_mode: bool,
    /// Light or dark like Windows apps; `dark_mode` is used otherwise.
    pub follow_system_theme: bool,
    /// The status line under the list (off by default).
    pub show_status_bar: bool,
    /// Legacy single-queue setting, migrated into `queues`.
    #[serde(skip_serializing)]
    pub queue_simultaneous: usize,

    // General tab options
    pub launch_on_startup: bool,
    pub browser_chrome: bool,
    pub browser_edge: bool,
    pub browser_firefox: bool,
    pub browser_brave: bool,
    pub browser_opera: bool,
    pub browser_vivaldi: bool,
    pub advanced_browser_integration: bool,
    pub use_alt_prevent: bool,
    pub use_ctrl_force: bool,
    /// Chromium extension IDs allowed to call the native host.
    pub extension_ids: Vec<String>,

    // File types tab options
    pub file_types_extensions: String,
    /// Take browser downloads of every file type, not only the list above
    /// (web pages stay in the browser).
    pub capture_all_types: bool,
    /// Also take files that a page fetches itself (fetch/XHR with an
    /// "attachment" response), for file hosts such as gofile. Off by default:
    /// a web app such as webmail also fetches files it does not mean to save.
    pub capture_fetched_files: bool,
    pub exceptions_servers: String,

    // Save to tab options
    pub temp_dir: PathBuf,
    pub remember_last_path: bool,
    /// Last state of the "Remember this path for category" checkbox in the
    /// "Download File Info" dialog, so the box opens with the user's last choice.
    pub remember_path_default: bool,

    // Downloads tab options
    pub show_start_dialog: bool,
    pub auto_close_progress: bool,
    pub open_file_on_completion: bool,
    pub ignore_time_changes: bool,
    pub antivirus_scan: bool,
    pub antivirus_path: String,
    pub antivirus_args: String,
    pub confirm_delete: bool,
    pub confirm_exit: bool,
    /// The close button hides the window to the notification area.
    pub close_to_tray: bool,

    // Connection tab options
    pub connection_speed_type: ConnectionSpeedType,
    pub max_retries: u32,

    // Proxy / SOCKS tab options
    pub proxy_http_enabled: bool,
    pub proxy_http_host: String,
    pub proxy_http_port: u16,
    pub proxy_http_user: String,
    pub proxy_http_pass: String,
    pub proxy_https_enabled: bool,
    pub proxy_https_host: String,
    pub proxy_https_port: u16,
    pub proxy_socks_enabled: bool,
    pub proxy_socks_host: String,
    pub proxy_socks_port: u16,
    pub proxy_socks_user: String,
    pub proxy_socks_pass: String,
    pub proxy_pac_enabled: bool,
    pub proxy_pac_url: String,
    pub proxy_exceptions: String,
    pub ftp_pasv: bool,
    /// "Use this proxy for the following protocols: ftp" (through SOCKS5).
    pub proxy_protocol_ftp: bool,
    /// "Use proxy/socks from a browser's request if there are download errors".
    pub browser_proxy_fallback: bool,
    /// "Using special keys": keys that stop or force a take-over (alt, shift, ctrl[, ins]).
    pub keys_prevent_enabled: bool,
    pub keys_prevent: [bool; 3],
    pub keys_force_enabled: bool,
    pub keys_force: [bool; 4],
    /// The key must be held while the link is clicked.
    pub keys_check_click: bool,
    /// While a key is held, web pages, pictures and scripts stay in the browser.
    pub keys_skip_web: bool,
    /// Browser context menu: Download with, last video, last 10 videos, all links.
    pub browser_menu: [bool; 4],
    /// The download panels in browsers.
    pub panel: PanelSettings,
    /// Options > Dial Up / VPN.
    pub dialup_enabled: bool,
    pub dialup_entry: String,
    pub dialup_user: String,
    pub dialup_pass: String,
    pub dialup_save_pass: bool,

    // Site logins tab options
    pub site_logins: Vec<SiteLogin>,

    // Dial Up / VPN tab options
    pub redial_attempts: u32,
    pub redial_wait_seconds: u32,
    pub hangup_when_done: bool,

    // Legacy scheduler fields (read from older settings, migrated into `queues`)
    #[serde(skip_serializing)]
    pub scheduler_start_enabled: bool,
    #[serde(skip_serializing)]
    pub scheduler_start_time: String,
    #[serde(skip_serializing)]
    pub scheduler_stop_enabled: bool,
    #[serde(skip_serializing)]
    pub scheduler_stop_time: String,
    #[serde(skip_serializing)]
    pub scheduler_daily: bool,
    /// Monday first.
    #[serde(skip_serializing)]
    pub scheduler_days: [bool; 7],
    #[serde(skip_serializing)]
    pub scheduler_hangup: bool,
    #[serde(skip_serializing)]
    pub scheduler_exit_idm: bool,
    #[serde(skip_serializing)]
    pub scheduler_retries: u32,
    #[serde(skip_serializing)]
    pub queue_stop_when_done: bool,

    /// Download queues; the main queue (id 0) always exists.
    pub queues: Vec<QueueConfig>,

    // Downloads tab options (continued)
    pub duplicate_action: DuplicateAction,
    /// Links from the browser or clipboard only go to the main queue.
    pub captured_to_queue: bool,
    /// User-Agent for manually added downloads (empty = built-in).
    pub manual_user_agent: String,
    /// "Set file creation date as provided by the server".
    pub set_file_time_from_server: bool,

    // Connection tab options (continued)
    pub server_limits: Vec<ServerLimit>,
    /// "Download no more than N MBytes every M hours".
    pub download_limit_enabled: bool,
    pub download_limit_mb: u64,
    pub download_limit_hours: u32,
    pub download_limit_warn: bool,
    /// "Always turn on Speed Limiter on Klippo startup".
    pub speed_limit_on_startup: bool,

    // Proxy tab options (continued)
    /// Use the Windows (Internet Options) proxy.
    pub proxy_use_system: bool,

    // View menu
    pub toolbar_small: bool,
    pub show_tray_icon: bool,
    pub font_scale: f32,
    pub hidden_columns: Vec<String>,

    // Sounds tab options
    pub sound_complete: bool,
    pub sound_complete_path: String,
    pub sound_failed: bool,
    pub sound_failed_path: String,
    pub sound_queue_done: bool,
    pub sound_queue_done_path: String,
    pub sound_queue_start: bool,
    pub sound_queue_start_path: String,
    /// Options > Save to: file types of each category (space separated).
    pub category_types: std::collections::BTreeMap<Category, String>,
    /// Options > Save to > New: the categories the user added.
    pub user_categories: Vec<UserCategory>,
    /// "Remember the speed limit for this server": KBytes/sec for each server name.
    pub server_speed_limits: std::collections::BTreeMap<String, u64>,
    /// Options > File types: addresses (with * wildcards) never captured.
    pub exception_addresses: Vec<String>,
    /// Options > Downloads: ask for a queue on "Download Later" and after a batch.
    pub queue_panel_on_later: bool,
    pub queue_panel_on_batch: bool,
    /// "Start downloading immediately while displaying Download File Info dialog".
    pub start_while_dialog: bool,
    /// Options > Proxy: protocols that use the manual proxy.
    pub proxy_protocol_http: bool,
    pub proxy_protocol_https: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        let default_dir = dirs_downloads().unwrap_or_else(|| PathBuf::from("C:\\Downloads"));
        let mut category_dirs = HashMap::new();
        category_dirs.insert(Category::General, default_dir.clone());
        for cat in &Category::ALL[1..] {
            category_dirs.insert(*cat, default_dir.join(cat.name()));
        }

        let temp_dir = app_data_dir().join("Temp");

        Self {
            default_dir,
            category_dirs,
            max_connections: 8,
            speed_limit_enabled: false,
            speed_limit_kbps: 1024,
            monitor_clipboard: true,
            auto_start: true,
            show_progress_window: true,
            show_complete_window: true,
            sound_alerts: true,
            dark_mode: false,
            follow_system_theme: true,
            show_status_bar: false,
            queue_simultaneous: 1,

            launch_on_startup: false,
            browser_chrome: true,
            browser_edge: true,
            browser_firefox: true,
            browser_brave: true,
            browser_opera: true,
            browser_vivaldi: true,
            advanced_browser_integration: true,
            use_alt_prevent: true,
            use_ctrl_force: true,
            extension_ids: vec![crate::native_host::BUNDLED_EXTENSION_ID.to_string()],

            file_types_extensions: "3GP 7Z AAC ACE AIF APK ARJ ASF AVI BIN BZ2 EXE GZ GZIP IMG ISO LZH M4A M4V MKV MOV MP3 MP4 MPA MPE MPEG MPG MSI MSU OGG OGV PDF PLJ PPS PPT QT R0* R1* RA RAR RM RMVB SEA SIT SITX TAR TIF TIFF WAV WMA WMV Z ZIP".to_string(),
            capture_all_types: true,
            capture_fetched_files: false,
            exceptions_servers: "*.update.microsoft.com download.windowsupdate.com *.download.windowsupdate.com siteseal.thawte.com ecom.cimetz.com *.voice2page.com".to_string(),

            temp_dir,
            remember_last_path: true,
            remember_path_default: false,

            show_start_dialog: true,
            auto_close_progress: false,
            open_file_on_completion: false,
            ignore_time_changes: true,
            antivirus_scan: false,
            antivirus_path: "C:\\Program Files\\Windows Defender\\MpCmdRun.exe".to_string(),
            antivirus_args: "-Scan -ScanType 3 -File \"%1\"".to_string(),
            confirm_delete: true,
            confirm_exit: true,
            close_to_tray: true,

            connection_speed_type: ConnectionSpeedType::HighSpeed,
            max_retries: 10,

            proxy_http_enabled: false,
            proxy_http_host: String::new(),
            proxy_http_port: 8080,
            proxy_http_user: String::new(),
            proxy_http_pass: String::new(),
            proxy_https_enabled: false,
            proxy_https_host: String::new(),
            proxy_https_port: 8080,
            proxy_socks_enabled: false,
            proxy_socks_host: String::new(),
            proxy_socks_port: 1080,
            proxy_socks_user: String::new(),
            proxy_socks_pass: String::new(),
            proxy_pac_enabled: false,
            proxy_pac_url: String::new(),
            proxy_exceptions: "<local>;*.internal".to_string(),
            ftp_pasv: true,
            proxy_protocol_ftp: false,
            browser_proxy_fallback: true,
            keys_prevent_enabled: true,
            keys_prevent: [true, false, false],
            keys_force_enabled: true,
            keys_force: [false, false, false, true],
            keys_check_click: true,
            keys_skip_web: true,
            browser_menu: [true, true, true, true],
            panel: PanelSettings::default(),
            dialup_enabled: false,
            dialup_entry: String::new(),
            dialup_user: String::new(),
            dialup_pass: String::new(),
            dialup_save_pass: true,

            site_logins: Vec::new(),

            redial_attempts: 0,
            redial_wait_seconds: 30,
            hangup_when_done: false,

            scheduler_start_enabled: false,
            scheduler_start_time: "02:00".to_string(),
            scheduler_stop_enabled: false,
            scheduler_stop_time: "07:00".to_string(),
            scheduler_daily: true,
            scheduler_days: [true; 7],
            scheduler_hangup: false,
            scheduler_exit_idm: false,
            scheduler_retries: 5,
            queue_stop_when_done: true,

            queues: vec![QueueConfig::default()],

            duplicate_action: DuplicateAction::Ask,
            captured_to_queue: false,
            manual_user_agent: String::new(),
            set_file_time_from_server: false,

            server_limits: Vec::new(),
            download_limit_enabled: false,
            download_limit_mb: 1024,
            download_limit_hours: 5,
            download_limit_warn: true,
            speed_limit_on_startup: false,

            proxy_use_system: false,

            toolbar_small: false,
            show_tray_icon: true,
            font_scale: 1.0,
            hidden_columns: vec!["progress".to_string(), "save_to".to_string(), "referer".to_string()],

            sound_complete: true,
            sound_complete_path: String::new(),
            sound_failed: true,
            sound_failed_path: String::new(),
            sound_queue_done: true,
            sound_queue_done_path: String::new(),
            sound_queue_start: false,
            sound_queue_start_path: String::new(),
            category_types: Category::ALL[1..].iter().map(|c| (*c, c.default_types().to_string())).collect(),
            user_categories: Vec::new(),
            server_speed_limits: std::collections::BTreeMap::new(),
            exception_addresses: Vec::new(),
            queue_panel_on_later: true,
            queue_panel_on_batch: true,
            start_while_dialog: true,
            proxy_protocol_http: true,
            proxy_protocol_https: true,
        }
    }
}

impl AppSettings {
    /// Builds the main queue from the single-queue fields of older versions.
    fn migrate_legacy_queue(&mut self) {
        let main = QueueConfig {
            simultaneous: self.queue_simultaneous.max(1),
            retries: self.scheduler_retries,
            start_enabled: self.scheduler_start_enabled,
            start_time: self.scheduler_start_time.clone(),
            stop_enabled: self.scheduler_stop_enabled,
            stop_time: self.scheduler_stop_time.clone(),
            daily: self.scheduler_daily,
            days: self.scheduler_days,
            hangup_when_done: self.scheduler_hangup,
            exit_when_done: self.scheduler_exit_idm,
            stop_when_done: self.queue_stop_when_done,
            ..Default::default()
        };
        self.queues = vec![main];
    }

    pub fn ensure_main_queue(&mut self) {
        if !self.queues.iter().any(|q| q.id == MAIN_QUEUE) {
            self.queues.insert(0, QueueConfig::default());
        }
    }

    /// Connection count for a URL after the per-server limits.
    pub fn connections_for(&self, url: &str, wanted: usize) -> usize {
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.to_string())).unwrap_or_default();
        self.server_limits
            .iter()
            .filter(|l| !l.server.trim().is_empty() && http::wildcard_match(l.server.trim(), &host))
            .map(|l| l.max_connections.max(1))
            .min()
            .map_or(wanted, |limit| wanted.min(limit))
            .clamp(1, 32)
    }

    pub fn category_dir(&self, cat: Category) -> PathBuf {
        self.category_dirs
            .get(&cat)
            .cloned()
            .unwrap_or_else(|| self.default_dir.join(http::sanitize_filename(&cat.name())))
    }

    /// The remembered speed limit (KBytes/sec) for the server of `url`.
    pub fn remembered_limit(&self, url: &str) -> Option<u64> {
        server_of(url).and_then(|host| self.server_speed_limits.get(&host).copied()).filter(|k| *k > 0)
    }

    pub fn global_limit_bps(&self) -> u64 {
        if self.speed_limit_enabled {
            self.speed_limit_kbps.saturating_mul(1024)
        } else {
            0
        }
    }

    pub fn proxy_config(&self) -> Option<ProxyConfig> {
        let user = Some(self.proxy_http_user.clone()).filter(|u| !u.trim().is_empty());
        let pass = user.as_ref().map(|_| self.proxy_http_pass.clone());
        let socks = self.socks_url();
        let manual = socks.clone().or_else(|| {
            (self.proxy_http_enabled && !self.proxy_http_host.trim().is_empty()).then(|| proxy_url(&self.proxy_http_host, self.proxy_http_port))
        });
        // "Use this proxy for the following protocols: http, https".
        let http = manual.clone().filter(|_| self.proxy_protocol_http);
        let https = if socks.is_none() && self.proxy_https_enabled && !self.proxy_https_host.trim().is_empty() {
            Some(proxy_url(&self.proxy_https_host, self.proxy_https_port))
        } else {
            manual.filter(|_| self.proxy_protocol_https)
        };
        if http.is_none() && https.is_none() {
            return None;
        }
        Some(ProxyConfig {
            http,
            https,
            username: user,
            password: pass,
            bypass: Some(self.proxy_exceptions.replace("<local>", "localhost,127.0.0.1")).filter(|b| !b.trim().is_empty()),
        })
    }

    /// `socks5h://[user:pass@]host:port` when "Use socks" is on (names are resolved by the proxy).
    pub fn socks_url(&self) -> Option<String> {
        if !self.proxy_socks_enabled || self.proxy_socks_host.trim().is_empty() {
            return None;
        }
        let enc = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
        let login = if self.proxy_socks_user.trim().is_empty() {
            String::new()
        } else {
            format!("{}:{}@", enc(self.proxy_socks_user.trim()), enc(&self.proxy_socks_pass))
        };
        Some(format!("socks5h://{}{}:{}", login, self.proxy_socks_host.trim(), self.proxy_socks_port))
    }

    /// How FTP downloads connect (PASV, and SOCKS5 when "ftp" is ticked for the proxy).
    pub fn ftp_options(&self) -> crate::core::ftp::FtpOptions {
        let socks = (self.proxy_protocol_ftp && self.proxy_socks_enabled && !self.proxy_socks_host.trim().is_empty())
            .then(|| format!("{}:{}", self.proxy_socks_host.trim(), self.proxy_socks_port));
        let login = (!self.proxy_socks_user.trim().is_empty()).then(|| (self.proxy_socks_user.trim().to_string(), self.proxy_socks_pass.clone()));
        crate::core::ftp::FtpOptions { passive: self.ftp_pasv, socks_login: socks.as_ref().and(login), socks, login: None }
    }

    /// True when the host is in "Don't start downloading automatically from these addresses".
    pub fn is_exception(&self, url: &str) -> bool {
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.to_string())).unwrap_or_default();
        let site = self
            .exceptions_servers
            .split_whitespace()
            .any(|p| http::wildcard_match(p, &host) || http::wildcard_match(p, url));
        // "Don't start downloading automatically from the following addresses": a prefix or a pattern.
        site || self.exception_addresses.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).any(|a| {
            url.to_lowercase().starts_with(&a.to_lowercase()) || http::wildcard_match(a, url)
        })
    }

    /// Should a download offered by the browser be taken over?
    pub fn should_capture(&self, url: &str, filename: Option<&str>, mime: Option<&str>) -> bool {
        // A blob: address names the page's site after "blob:".
        if self.is_exception(url.strip_prefix("blob:").unwrap_or(url)) {
            return false;
        }
        if self.capture_all_types {
            // "Save page as" and the like need the browser.
            return !is_web_page(filename, mime);
        }
        let types = parse_file_types(&self.file_types_extensions);
        let by_name = filename
            .and_then(|f| f.rsplit_once('.'))
            .is_some_and(|(_, ext)| types.iter().any(|t| http::wildcard_match(t, ext)));
        by_name || url_matches_file_types(url, &types)
    }
}

/// The POST of a form download, from its base64 body and its Content-Type.
pub fn post_body(data: Option<&str>, content_type: Option<&str>) -> Option<PostBody> {
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.decode(data?).ok()?;
    let content_type = content_type.filter(|t| !t.trim().is_empty()).unwrap_or("application/x-www-form-urlencoded");
    Some(PostBody { data, content_type: content_type.to_string() })
}

/// A web page that the browser saves ("Save page as"), by its type or its name.
pub fn is_web_page(filename: Option<&str>, mime: Option<&str>) -> bool {
    let mime = mime.unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if matches!(mime.as_str(), "text/html" | "application/xhtml+xml" | "multipart/related" | "message/rfc822") {
        return true;
    }
    let ext = filename.and_then(|f| f.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "htm" | "html" | "shtml" | "xhtml" | "mht" | "mhtml")
}

/// Proxy settings from Windows: `host:port` or `http=h:p;https=h:p`, bypass list with `;`.
pub fn system_proxy_config(server: &str, bypass: &str) -> Option<ProxyConfig> {
    let with_scheme = |s: &str| if s.contains("://") { s.to_string() } else { format!("http://{}", s) };
    let (mut http, mut https) = (None, None);
    if server.contains('=') {
        for part in server.split(';') {
            if let Some((scheme, addr)) = part.split_once('=') {
                match scheme.trim().to_ascii_lowercase().as_str() {
                    "http" => http = Some(with_scheme(addr.trim())),
                    "https" => https = Some(with_scheme(addr.trim())),
                    _ => {}
                }
            }
        }
    } else if !server.trim().is_empty() {
        http = Some(with_scheme(server.trim()));
        https = http.clone();
    }
    if http.is_none() && https.is_none() {
        return None;
    }
    let bypass = bypass.replace("<local>", "localhost,127.0.0.1").replace(';', ",");
    Some(ProxyConfig { http, https, username: None, password: None, bypass: Some(bypass).filter(|b| !b.trim().is_empty()) })
}

fn proxy_url(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.contains("://") {
        format!("{}:{}", host.trim_end_matches('/'), port)
    } else {
        format!("http://{}:{}", host, port)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// The server name of an address, in lower case ("www.a.org" for "https://WWW.a.org/x").
pub fn server_of(url: &str) -> Option<String> {
    url::Url::parse(url.trim()).ok().and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
}

pub fn app_data_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("Klippo")
    } else {
        PathBuf::from(".").join(".klippo_data")
    }
}

pub fn dirs_downloads() -> Option<PathBuf> {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(profile).join("Downloads");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

pub fn load_settings() -> AppSettings {
    let path = app_data_dir().join("settings.json");
    match fs::read_to_string(&path) {
        // Missing fields take their defaults instead of discarding the file.
        Ok(data) => {
            let mut settings: AppSettings = serde_json::from_str(&data).unwrap_or_default();
            if !data.contains("\"queues\"") {
                settings.migrate_legacy_queue();
            }
            settings.ensure_main_queue();
            Category::set_user_categories(&settings.user_categories);
            Category::set_custom_types(&settings.category_types);
            crate::core::parts::set_parts_root(Some(settings.temp_dir.clone()));
            // Settings from before the redesign: hide the columns the new layout does not show.
            if !data.contains("\"follow_system_theme\"") && settings.hidden_columns == ["referer"] {
                settings.hidden_columns = AppSettings::default().hidden_columns;
            }
            settings
        }
        Err(_) => AppSettings::default(),
    }
}

pub fn load_state_from_disk() -> (Vec<DownloadItem>, AppSettings) {
    let dir = app_data_dir();
    let _ = fs::create_dir_all(&dir);
    let settings = load_settings();

    let downloads = {
        let path = dir.join("downloads.json");
        match fs::read_to_string(&path) {
            Ok(data) => {
                let mut list: Vec<DownloadItem> = match serde_json::from_str(&data) {
                    Ok(list) => list,
                    Err(_) => {
                        // Keep a copy instead of silently losing the list.
                        let _ = fs::copy(&path, dir.join("downloads.json.bad"));
                        Vec::new()
                    }
                };
                // Nothing is running right after start-up.
                for item in &mut list {
                    if item.legacy_in_queue && item.queue.is_none() {
                        item.queue = Some(MAIN_QUEUE);
                    }
                    if item.status.is_active() {
                        item.status = DownloadStatus::Paused;
                    }
                    item.speed_bps = 0.0;
                    item.eta_seconds = None;
                }
                list
            }
            Err(_) => Vec::new(),
        }
    };

    (downloads, settings)
}

fn write_atomic(path: &Path, data: &str) {
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, data).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Settings as written to settings.json (secrets the user chose not to keep are left out).
fn settings_for_disk(settings: &AppSettings) -> std::borrow::Cow<'_, AppSettings> {
    if settings.dialup_save_pass || settings.dialup_pass.is_empty() {
        return std::borrow::Cow::Borrowed(settings);
    }
    let mut copy = settings.clone();
    copy.dialup_pass.clear();
    std::borrow::Cow::Owned(copy)
}

pub fn save_state_to_disk(downloads: &[DownloadItem], settings: &AppSettings) {
    let dir = app_data_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(settings_for_disk(settings).as_ref()) {
        write_atomic(&dir.join("settings.json"), &json);
    }
    if let Ok(json) = serde_json::to_string_pretty(downloads) {
        write_atomic(&dir.join("downloads.json"), &json);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Size,
    Status,
    TimeLeft,
    Speed,
    LastTry,
    Description,
    SavePath,
    Referer,
}

#[derive(Debug, Clone, Default)]
pub enum ProbeState {
    #[default]
    Idle,
    Pending { probe_id: u64, url: String },
    Done(ProbeInfo),
    Failed { url: String, error: String },
}

/// A download is added in two steps: the address first, then its file info.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AddStep {
    #[default]
    Address,
    Info,
}

/// State of the "Enter new address to download" / "Download File Info" dialog.
#[derive(Debug, Clone, Default)]
pub struct AddDialog {
    pub open: bool,
    pub step: AddStep,
    /// The download that started while the dialog is open (an early start).
    pub started_id: Option<u64>,
    /// The path of the early download when it finished while the dialog is still
    /// open: it waits here until the user answers, so nothing completes on its own.
    pub early_done: Option<PathBuf>,
    /// The browser's proxy for this link.
    pub browser_proxy: Option<String>,
    pub url: String,
    pub filename: String,
    pub filename_edited: bool,
    pub dir: PathBuf,
    pub dir_edited: bool,
    pub category: Category,
    pub connections: usize,
    pub auth_enabled: bool,
    pub username: String,
    pub password: String,
    pub description: String,
    pub remember_path: bool,
    pub kind: DownloadKind,
    pub referer: Option<String>,
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    pub probe: ProbeState,
    pub url_changed_at: Option<Instant>,
    /// What to do if the address is already in the list.
    pub dup_action: DuplicateAction,
    pub dup_remember: bool,
    /// The address field already got keyboard focus.
    pub focused: bool,
    /// Queue for "Download Later".
    pub queue: u32,
    /// The browser sent the link (the dialog is "Download File Info" then).
    pub from_browser: bool,
    /// Opened for another program (browser, clipboard, command line): the
    /// dialog comes in front of it.
    pub to_front: bool,
    /// YouTube stream session from the video button.
    pub youtube: Option<YoutubeSpec>,
    /// Separate audio stream that is joined to the video.
    pub audio_url: Option<String>,
    /// A form download from the browser: POST body (base64) and Content-Type.
    pub post_data: Option<String>,
    pub post_type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GrabberDialog {
    pub open: bool,
    pub url: String,
    pub filename: String,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BatchMode {
    /// Pattern mode: `http://site/file*.zip` with a range.
    #[default]
    Pattern,
    /// A list of addresses.
    List,
}

#[derive(Debug, Clone)]
pub struct BatchDialog {
    pub open: bool,
    pub mode: BatchMode,
    pub urls_text: String,
    pub pattern: String,
    pub letters: bool,
    pub from: String,
    pub to: String,
    pub wildcard_size: usize,
    pub dir: PathBuf,
    pub auth_enabled: bool,
    pub username: String,
    pub password: String,
    /// The queue selection panel after OK.
    pub choosing_queue: bool,
    pub queue: u32,
    pub start_queue: bool,
    /// The links came from the browser ("Download all links", selected links).
    pub from_browser: bool,
    /// The dialog comes in front of the browser when it opens.
    pub to_front: bool,
}

impl Default for BatchDialog {
    fn default() -> Self {
        Self {
            open: false,
            mode: BatchMode::Pattern,
            urls_text: String::new(),
            pattern: String::new(),
            letters: false,
            from: "1".to_string(),
            to: "10".to_string(),
            wildcard_size: 1,
            choosing_queue: false,
            queue: crate::gui::queues::MAIN_QUEUE,
            start_queue: true,
            from_browser: false,
            to_front: false,
            dir: PathBuf::new(),
            auth_enabled: false,
            username: String::new(),
            password: String::new(),
        }
    }
}

impl BatchDialog {
    /// Expands the pattern (or the URL list) into addresses.
    pub fn urls(&self) -> Vec<String> {
        match self.mode {
            BatchMode::List => self
                .urls_text
                .split(|c: char| c.is_whitespace())
                .map(|l| l.trim().to_string())
                .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
                .collect(),
            BatchMode::Pattern => expand_batch_pattern(&self.pattern, self.letters, &self.from, &self.to, self.wildcard_size),
        }
    }
}

/// "Batch download": replaces `*` with numbers (zero padded to `width`) or letters.
pub fn expand_batch_pattern(pattern: &str, letters: bool, from: &str, to: &str, width: usize) -> Vec<String> {
    let pattern = pattern.trim();
    if !pattern.contains('*') || !(pattern.starts_with("http://") || pattern.starts_with("https://")) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if letters {
        let (Some(a), Some(b)) = (from.trim().chars().next(), to.trim().chars().next()) else { return out };
        if !a.is_ascii_alphabetic() || !b.is_ascii_alphabetic() || a > b {
            return out;
        }
        for c in a..=b {
            out.push(pattern.replacen('*', &c.to_string(), 1));
        }
    } else {
        let (Ok(a), Ok(b)) = (from.trim().parse::<u64>(), to.trim().parse::<u64>()) else { return out };
        if a > b || b - a > 10_000 {
            return out;
        }
        for n in a..=b {
            out.push(pattern.replacen('*', &format!("{:0width$}", n, width = width.max(1)), 1));
        }
    }
    out
}

pub struct DeleteConfirm {
    pub ids: Vec<u64>,
    pub delete_files: bool,
    pub dont_ask: bool,
}

/// "Connecting to the site": the server wants a login (HTTP 401).
pub struct LoginPrompt {
    pub id: u64,
    pub realm: String,
    pub username: String,
    pub password: String,
    pub save: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// One address per line.
    Text,
    /// Export file (.ef2) with referrer, cookies and logins.
    Ef2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    Queue(u32),
    Selected,
    All,
}

/// "Export download list".
pub struct ExportDialog {
    pub format: ExportFormat,
    pub scope: ExportScope,
}

/// "Download limits exceeded!"
pub struct LimitDialog {
    /// Ask before stopping (otherwise the downloads were already stopped).
    pub warn: bool,
    pub downloaded: u64,
}

#[derive(Debug, Clone)]
pub enum HashState {
    Computing,
    Done(String),
    Failed(String),
}

/// Checksum algorithm for file verification, the set the "Check Sum" dialog offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HashAlgo {
    Crc32,
    Md5,
    Sha1,
    #[default]
    Sha256,
}

impl HashAlgo {
    pub fn label(self) -> &'static str {
        match self {
            HashAlgo::Crc32 => "CRC32",
            HashAlgo::Md5 => "MD5",
            HashAlgo::Sha1 => "SHA-1",
            HashAlgo::Sha256 => "SHA-256",
        }
    }

    pub fn all() -> [HashAlgo; 4] {
        [HashAlgo::Crc32, HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256]
    }
}

/// What a new download needs, from any source (dialog, batch, browser).
#[derive(Debug, Clone, Default)]
pub struct NewDownload {
    pub url: String,
    pub filename: String,
    pub filename_auto: bool,
    pub dir: PathBuf,
    pub category: Category,
    pub connections: usize,
    pub kind: DownloadKind,
    pub username: Option<String>,
    pub password: Option<String>,
    pub referer: Option<String>,
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    pub description: String,
    pub total_size: Option<u64>,
    pub resumable: bool,
    pub youtube: Option<YoutubeSpec>,
    pub audio_url: Option<String>,
    pub browser_proxy: Option<String>,
    /// A form download: POST body (base64) and Content-Type.
    pub post_data: Option<String>,
    pub post_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Now,
    /// Add to a queue without starting.
    Queue(u32),
    /// Add to the list only.
    Later,
}

pub fn is_hls_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or("");
    path.ends_with(".m3u8") || path.ends_with(".m3u")
}

/// The download in `list` with the address `url` (the "Duplicate download
/// link"). `started` is the download that "Download File Info" started while it
/// shows: it is not a duplicate of itself.
fn duplicate_in<'a>(list: &'a [DownloadItem], url: &str, started: Option<u64>) -> Option<&'a DownloadItem> {
    let url = url.trim();
    list.iter().find(|d| d.url == url && Some(d.id) != started)
}

/// Whether a download that just finished is the one "Download File Info" started
/// early and must wait: while that dialog is open, its own early download does
/// not finalize, so nothing completes until the user clicks Start Download.
fn early_download_waits(dialog_open: bool, started: Option<u64>, finished: u64) -> bool {
    dialog_open && started == Some(finished)
}

/// HLS segments are joined into one `.ts` file: "Talk.m3u8" or "Talk" becomes
/// "Talk.ts" (a page title such as "Part 1.5" keeps its dot).
pub fn hls_filename(name: &str) -> String {
    let lower = name.to_lowercase();
    // The download keeps the name; it becomes .mp4 or .ts to match the stream.
    if lower.ends_with(".ts") || lower.ends_with(".mp4") {
        return name.to_string();
    }
    let stem = [".m3u8", ".m3u"]
        .iter()
        .find_map(|ext| lower.ends_with(ext).then(|| &name[..name.len() - ext.len()]))
        .unwrap_or(name);
    format!("{}.ts", if stem.trim().is_empty() { "video" } else { stem })
}

pub struct KlippoApp {
    pub downloads: Vec<DownloadItem>,
    pub selection: BTreeSet<u64>,
    /// Focused row (anchor for Shift-click ranges).
    pub selected_id: Option<u64>,
    pub selected_category: CategoryFilter,
    pub search_query: String,
    pub settings: AppSettings,
    /// Settings as they were when the Options dialog opened (for Cancel).
    pub settings_backup: Option<AppSettings>,
    pub show_sidebar: bool,
    pub show_toolbar: bool,
    pub sort: Option<(SortColumn, bool)>,

    // Dialogs
    pub add: AddDialog,
    pub pending_adds: VecDeque<IpcMessage>,
    pub grabber: GrabberDialog,
    pub site_grabber: crate::gui::views::site_grabber_modal::SiteGrabberDialog,
    /// "Add new category".
    pub new_category: Option<crate::gui::views::category_dialog::NewCategoryDialog>,
    pub batch: BatchDialog,
    pub progress_windows: Vec<u64>,
    /// A progress window opened for the browser: it comes in front when it shows.
    pub progress_front: Option<u64>,
    pub show_complete_dialog: Option<u64>,
    pub show_options_dialog: bool,
    pub show_scheduler_dialog: bool,
    pub show_speed_limiter_dialog: bool,
    pub show_file_properties_dialog: Option<u64>,
    pub show_about_dialog: bool,
    pub show_site_login_modal: bool,
    pub delete_confirm: Option<DeleteConfirm>,
    pub login_prompt: Option<LoginPrompt>,
    pub export_dialog: Option<ExportDialog>,
    pub limit_dialog: Option<LimitDialog>,
    /// Bytes received, for "Download no more than N MB every M hours".
    pub byte_history: VecDeque<(Instant, u64)>,
    pub limit_ignored_until: Option<Instant>,
    /// Ctrl+F shows the "Find file" dialog.
    pub focus_search: bool,
    pub find_options: crate::gui::views::find_modal::FindOptions,
    /// View > Customize URL List... ("Columns").
    pub show_columns_dialog: bool,
    /// Windows app theme read in the background (0 unknown, 1 light, 2 dark).
    pub system_theme: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Dark or light as last applied.
    pub applied_dark: Option<bool>,
    /// Tree nodes the user folded (All Downloads, Unfinished, Finished, Queues).
    pub tree_closed: BTreeSet<&'static str>,
    pub show_exit_confirm: bool,

    pub file_hashes: HashMap<u64, HashState>,

    // Site logins editor state
    pub site_login_edit_idx: Option<usize>,
    pub site_login_url: String,
    pub site_login_user: String,
    pub site_login_pass: String,

    pub options_active_tab: OptionsTab,
    pub scheduler_active_tab: SchedulerTab,
    pub options_save_to_category: Category,
    /// A small dialog opened from an Options page.
    pub options_sub: Option<crate::gui::views::options_modal::OptionsSub>,

    // Queue state
    pub running_queues: BTreeSet<u32>,
    /// Queues that started at least one file since they were started.
    pub queue_did_work: HashSet<u32>,
    pub queue_last_start: HashMap<u32, Instant>,
    /// Queue shown in the Scheduler dialog.
    pub scheduler_queue: u32,
    pub new_queue_name: String,
    /// "Enter queue name": (queue to rename, or None for a new one; the name).
    pub queue_name_edit: Option<(Option<u32>, String)>,
    /// Queue files the user stopped by hand; the queue leaves them alone.
    pub queue_skip: HashSet<u64>,
    pub retry_not_before: HashMap<u64, Instant>,
    /// `/q`: exit once nothing is left to download.
    pub quit_when_done: bool,
    pub scheduler_last_minute: Option<String>,
    pub status_message: Option<(String, Instant)>,

    // Background workers
    pub bridge: DownloadManagerBridge,
    pub clipboard_watcher: ClipboardWatcher,
    pub ipc: Option<IpcServer>,
    pub next_id: u64,
    pub probe_seq: u64,

    pub exit_confirmed: bool,
    /// Close the window on the next frame (scheduler, /q, Tasks > Exit).
    pub pending_exit: bool,
    /// The next close really exits instead of hiding to the tray.
    pub exit_requested: bool,
    /// Native window handle (Windows), used for hiding to the tray.
    pub hwnd: Option<isize>,
    pub in_tray: bool,
    /// When the window went to the tray (its minimized state settles after this).
    pub tray_since: Option<Instant>,
    #[cfg(windows)]
    pub tray: Option<crate::gui::tray::Tray>,
    pub start_minimized: bool,
    pub frame_count: u64,
    /// Window minimized last frame: used to show completion balloons only when
    /// Klippo runs in the background.
    pub last_minimized: bool,
    /// Windows notification-area balloon sender, made on first use.
    #[cfg(windows)]
    pub notifier: Option<system::Notifier>,
    dirty_since: Option<Instant>,
}

impl KlippoApp {
    pub fn new(start_minimized: bool, ipc: Option<IpcServer>) -> Self {
        let (downloads, mut settings) = load_state_from_disk();
        // The registry is the source of truth for autostart.
        settings.launch_on_startup = system::is_launch_on_startup();
        if settings.speed_limit_on_startup {
            settings.speed_limit_enabled = true;
        }
        let max_id = downloads.iter().map(|d| d.id).max().unwrap_or(0);
        let clip_enabled = settings.monitor_clipboard;

        let bridge = DownloadManagerBridge::new();
        bridge.set_global_speed_limit(settings.global_limit_bps());
        let clipboard_watcher = ClipboardWatcher::new(clip_enabled, &settings.file_types_extensions);

        Self {
            downloads,
            selection: BTreeSet::new(),
            selected_id: None,
            selected_category: CategoryFilter::All,
            search_query: String::new(),
            settings,
            settings_backup: None,
            show_sidebar: true,
            show_toolbar: true,
            sort: None,

            add: AddDialog::default(),
            pending_adds: VecDeque::new(),
            grabber: GrabberDialog::default(),
            site_grabber: Default::default(),
            new_category: None,
            batch: BatchDialog::default(),
            progress_windows: Vec::new(),
            progress_front: None,
            show_complete_dialog: None,
            show_options_dialog: false,
            show_scheduler_dialog: false,
            show_speed_limiter_dialog: false,
            show_file_properties_dialog: None,
            show_about_dialog: false,
            show_site_login_modal: false,
            delete_confirm: None,
            login_prompt: None,
            export_dialog: None,
            limit_dialog: None,
            byte_history: VecDeque::new(),
            limit_ignored_until: None,
            focus_search: false,
            find_options: Default::default(),
            show_columns_dialog: false,
            system_theme: Self::watch_system_theme(),
            applied_dark: None,
            tree_closed: ["unfinished", "finished", "queues"].into_iter().collect(),
            show_exit_confirm: false,

            file_hashes: HashMap::new(),

            site_login_edit_idx: None,
            site_login_url: String::new(),
            site_login_user: String::new(),
            site_login_pass: String::new(),

            options_active_tab: OptionsTab::General,
            scheduler_active_tab: SchedulerTab::Schedule,
            options_save_to_category: Category::General,
            options_sub: None,

            running_queues: BTreeSet::new(),
            queue_did_work: HashSet::new(),
            queue_last_start: HashMap::new(),
            scheduler_queue: MAIN_QUEUE,
            new_queue_name: String::new(),
            queue_name_edit: None,
            queue_skip: HashSet::new(),
            retry_not_before: HashMap::new(),
            quit_when_done: false,
            scheduler_last_minute: None,
            status_message: None,

            bridge,
            clipboard_watcher,
            ipc,
            next_id: max_id + 1,
            probe_seq: 0,

            exit_confirmed: false,
            pending_exit: false,
            exit_requested: false,
            hwnd: None,
            in_tray: false,
            tray_since: None,
            #[cfg(windows)]
            tray: None,
            start_minimized,
            frame_count: 0,
            last_minimized: false,
            #[cfg(windows)]
            notifier: None,
            dirty_since: None,
        }
    }

    pub fn item(&self, id: u64) -> Option<&DownloadItem> {
        self.downloads.iter().find(|d| d.id == id)
    }

    pub fn item_mut(&mut self, id: u64) -> Option<&mut DownloadItem> {
        self.downloads.iter_mut().find(|d| d.id == id)
    }

    pub fn selected_download(&self) -> Option<&DownloadItem> {
        self.selected_id.and_then(|id| self.item(id))
    }

    /// Selected ids in list order.
    pub fn selected_ids(&self) -> Vec<u64> {
        self.downloads.iter().filter(|d| self.selection.contains(&d.id)).map(|d| d.id).collect()
    }

    pub fn select_only(&mut self, id: u64) {
        self.selection.clear();
        self.selection.insert(id);
        self.selected_id = Some(id);
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    /// Copies text without the clipboard monitor treating it as a new link.
    pub fn copy_to_clipboard(&mut self, text: &str) {
        self.clipboard_watcher.ignore_text(text);
        if let Ok(mut clip) = arboard::Clipboard::new() {
            let _ = clip.set_text(text.to_string());
        }
    }

    // ------------------------------------------------------------------
    // Add download dialog
    // ------------------------------------------------------------------

    fn reset_add_dialog(&mut self) {
        self.add = AddDialog {
            connections: self.settings.max_connections,
            remember_path: self.settings.remember_path_default,
            dup_action: match self.settings.duplicate_action {
                DuplicateAction::Ask => DuplicateAction::Numbered,
                other => other,
            },
            ..Default::default()
        };
    }

    /// Applies "Overwrite" or "use the existing download" to a duplicate link.
    /// Returns true when the existing entry handled the request.
    fn handle_duplicate(&mut self, existing: u64, action: DuplicateAction, mode: StartMode, new: &NewDownload) -> bool {
        match action {
            DuplicateAction::Ask | DuplicateAction::Numbered => false,
            DuplicateAction::ResumeExisting => {
                let completed = self.item(existing).is_some_and(|d| d.status == DownloadStatus::Completed);
                if completed {
                    self.show_complete_dialog = Some(existing);
                } else {
                    match mode {
                        StartMode::Queue(q) => self.set_queue(&[existing], Some(q)),
                        _ => self.resume_download(existing),
                    }
                }
                self.select_only(existing);
                true
            }
            DuplicateAction::Overwrite => {
                // Reuse the entry so the old run releases the file first.
                if let Some(item) = self.item_mut(existing) {
                    item.referer = new.referer.clone().or(item.referer.take());
                    item.cookies = new.cookies.clone().or(item.cookies.take());
                    item.user_agent = new.user_agent.clone().or(item.user_agent.take());
                    if new.username.is_some() {
                        item.username = new.username.clone();
                        item.password = new.password.clone();
                    }
                    item.downloaded_bytes = 0;
                    item.segments.clear();
                    item.error_message = None;
                }
                let active = self.item(existing).is_some_and(|d| d.status.is_active());
                match mode {
                    StartMode::Now => self.redownload(existing),
                    _ if active => self.redownload(existing),
                    _ => {
                        // Not running: drop the saved progress and wait in the queue.
                        if let Some(item) = self.item(existing) {
                            for path in item.temp_paths().into_iter().filter(|p| *p != item.save_path) {
                                if path.is_dir() {
                                    let _ = fs::remove_dir_all(&path);
                                } else {
                                    let _ = fs::remove_file(&path);
                                }
                            }
                        }
                        if let Some(item) = self.item_mut(existing) {
                            item.status = DownloadStatus::Queued;
                            if let StartMode::Queue(q) = mode {
                                item.queue = Some(q);
                            }
                        }
                        self.save_downloads();
                    }
                }
                self.select_only(existing);
                true
            }
        }
    }

    pub fn open_add_url_dialog(&mut self) {
        if self.add.open {
            return;
        }
        self.reset_add_dialog();
        if let Ok(mut clip) = arboard::Clipboard::new() {
            if let Ok(text) = clip.get_text() {
                let trimmed = text.trim();
                if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
                    && !trimmed.contains(char::is_whitespace)
                {
                    self.add.url = trimmed.to_string();
                }
            }
        }
        self.on_add_url_changed();
        self.add.open = true;
    }

    pub fn open_add_dialog_with(&mut self, msg: &IpcMessage) {
        self.reset_add_dialog();
        self.add.url = msg.url.trim().to_string();
        self.add.referer = msg.referer.clone();
        self.add.cookies = msg.cookies.clone();
        self.add.user_agent = msg.user_agent.clone();
        self.on_add_url_changed();
        if let Some(ref f) = msg.filename {
            let clean = http::sanitize_filename(f);
            if !clean.is_empty() {
                self.add.filename = clean;
                self.add.filename_edited = true;
                self.add.category = Category::from_filename(&self.add.filename);
                self.update_add_dir_for_category();
            }
        }
        // A page title from the video button names the stream; it still becomes a .ts file.
        if msg.hls || self.add.kind == DownloadKind::Hls {
            self.add.kind = DownloadKind::Hls;
            self.add.filename = hls_filename(&self.add.filename);
            self.add.category = Category::Video;
            self.update_add_dir_for_category();
        }
        if let Some(ref spec) = msg.youtube {
            self.add.kind = DownloadKind::Youtube;
            self.add.category = if spec.video.is_some() { Category::Video } else { Category::Music };
            self.update_add_dir_for_category();
            self.add.youtube = Some(spec.clone());
        }
        if let Some(ref audio) = msg.audio_url {
            // A video with its own audio stream is fetched like a stream and joined.
            if self.add.kind == DownloadKind::Http {
                self.add.kind = DownloadKind::Hls;
                self.add.category = Category::Video;
                self.update_add_dir_for_category();
            }
            self.add.audio_url = Some(audio.clone());
        }
        if let Some(ref d) = msg.dir {
            self.add.dir = PathBuf::from(d);
            self.add.dir_edited = true;
        }
        self.add.from_browser = msg.from_browser;
        self.add.browser_proxy = msg.browser_proxy.clone();
        self.add.post_data = msg.post_data.clone();
        self.add.post_type = msg.post_type.clone();
        // Links from the browser, the clipboard or the command line go straight to the file info.
        self.add.step = AddStep::Info;
        self.add.to_front = true;
        self.add.open = true;
        self.start_add_early();
    }

    pub fn on_add_url_changed(&mut self) {
        let url = self.add.url.trim().to_string();
        if !self.add.filename_edited {
            self.add.filename = http::filename_from_url(&url).unwrap_or_else(|| "download".to_string());
            self.add.category = Category::from_filename(&self.add.filename);
            self.update_add_dir_for_category();
        }
        self.add.kind = if crate::core::ftp::is_ftp_url(&url) {
            DownloadKind::Ftp
        } else if is_hls_url(&url) {
            DownloadKind::Hls
        } else {
            DownloadKind::Http
        };
        if self.add.kind == DownloadKind::Hls && !self.add.filename_edited {
            let stem = Path::new(&self.add.filename).file_stem().and_then(|s| s.to_str()).unwrap_or("video").to_string();
            self.add.filename = format!("{}.ts", stem);
            self.add.category = Category::Video;
            self.update_add_dir_for_category();
        }
        // A recorded YouTube session, audio stream or form belongs to the address it came with.
        self.add.youtube = None;
        self.add.audio_url = None;
        self.add.post_data = None;
        self.add.post_type = None;
        self.add.probe = ProbeState::Idle;
        self.add.url_changed_at = Some(Instant::now());
    }

    pub fn update_add_dir_for_category(&mut self) {
        if !self.add.dir_edited {
            self.add.dir = self.settings.category_dir(self.add.category);
        }
    }

    pub fn add_dialog_duplicate(&self) -> Option<&DownloadItem> {
        duplicate_in(&self.downloads, &self.add.url, self.add.started_id)
    }

    /// The proxy from Options > Proxy / Socks for `url`.
    fn proxy_for(&self, url: &str) -> Option<ProxyConfig> {
        if self.settings.proxy_pac_enabled && !self.settings.proxy_pac_url.trim().is_empty() {
            // Windows evaluates the script (WinHTTP), as it does for its own downloads.
            system::pac_proxy_for(url, self.settings.proxy_pac_url.trim()).map(|p| {
                let p = if p.contains("://") { p } else { format!("http://{}", p) };
                ProxyConfig { http: Some(p.clone()), https: Some(p), ..Default::default() }
            })
        } else if self.settings.proxy_use_system {
            system::windows_proxy().and_then(|(server, bypass)| system_proxy_config(&server, &bypass))
        } else {
            self.settings.proxy_config()
        }
    }

    /// The proxy and the agent for reading web pages (the Site Grabber).
    pub fn page_request_options(&self, url: &str) -> RequestOptions {
        let mut options = RequestOptions { proxy: self.proxy_for(url), ..Default::default() };
        if !self.settings.manual_user_agent.trim().is_empty() {
            options.user_agent = self.settings.manual_user_agent.trim().to_string();
        }
        options
    }

    fn request_options_for(&self, url: &str, item: Option<&DownloadItem>) -> RequestOptions {
        // After an error, a download from the browser may retry through the browser's proxy.
        let proxy = match item.filter(|i| i.via_browser_proxy).and_then(|i| i.browser_proxy.clone()) {
            Some(p) => Some(ProxyConfig { http: Some(p.clone()), https: Some(p), ..Default::default() }),
            None => self.proxy_for(url),
        };
        let mut options = RequestOptions { proxy, ..Default::default() };
        if !self.settings.manual_user_agent.trim().is_empty() {
            options.user_agent = self.settings.manual_user_agent.trim().to_string();
        }
        let mut headers = HeaderMap::new();
        let (referer, cookies, agent, user, pass) = match item {
            Some(i) => (i.referer.clone(), i.cookies.clone(), i.user_agent.clone(), i.username.clone(), i.password.clone()),
            None => (
                self.add.referer.clone(),
                self.add.cookies.clone(),
                self.add.user_agent.clone(),
                self.add.auth_enabled.then(|| self.add.username.clone()),
                self.add.auth_enabled.then(|| self.add.password.clone()),
            ),
        };
        if let Some(r) = referer.filter(|r| !r.is_empty()).and_then(|r| HeaderValue::from_str(&r).ok()) {
            headers.insert(REFERER, r);
        }
        if let Some(c) = cookies.filter(|c| !c.is_empty()).and_then(|c| HeaderValue::from_str(&c).ok()) {
            headers.insert(COOKIE, c);
        }
        options.headers = headers;
        if let Some(a) = agent.filter(|a| !a.trim().is_empty()) {
            options.user_agent = a;
        }
        match user.filter(|u| !u.is_empty()) {
            Some(u) => options.basic_auth = Some((u, pass.unwrap_or_default())),
            None => {
                if let Some(login) = self.settings.site_logins.iter().find(|l| l.matches(url)) {
                    options.basic_auth = Some((login.username.clone(), login.password.clone()));
                }
            }
        }
        options.post = match item {
            Some(i) => post_body(i.post_data.as_deref(), i.post_type.as_deref()),
            None => post_body(self.add.post_data.as_deref(), self.add.post_type.as_deref()),
        };
        options
    }

    /// Sends a probe for the dialog URL once typing has paused.
    pub fn tick_add_probe(&mut self) {
        if !self.add.open || self.add.kind != DownloadKind::Http {
            return;
        }
        let url = self.add.url.trim().to_string();
        if url::Url::parse(&url).map(|u| !matches!(u.scheme(), "http" | "https")).unwrap_or(true) {
            return;
        }
        let ready = self.add.url_changed_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(600));
        if ready && matches!(self.add.probe, ProbeState::Idle) {
            self.probe_seq += 1;
            let probe_id = self.probe_seq;
            let options = self.request_options_for(&url, None);
            self.bridge.probe(probe_id, url.clone(), self.add.dir.clone(), options);
            self.add.probe = ProbeState::Pending { probe_id, url };
        }
    }

    fn apply_probe_result(&mut self, probe_id: u64, result: Result<Box<ProbeInfo>, String>) {
        let ProbeState::Pending { probe_id: pending, ref url } = self.add.probe else { return };
        if pending != probe_id {
            return;
        }
        let url = url.clone();
        match result {
            Ok(info) => {
                if !self.add.filename_edited && !info.filename.is_empty() {
                    self.add.filename = info.filename.clone();
                    self.add.category = Category::from_filename(&info.filename);
                    self.update_add_dir_for_category();
                }
                let is_playlist = info.content_type.as_deref().is_some_and(|ct| ct.to_lowercase().contains("mpegurl"));
                if is_playlist {
                    self.add.kind = DownloadKind::Hls;
                    if !self.add.filename_edited {
                        let stem = Path::new(&self.add.filename).file_stem().and_then(|s| s.to_str()).unwrap_or("video").to_string();
                        self.add.filename = format!("{}.ts", stem);
                        self.add.category = Category::Video;
                        self.update_add_dir_for_category();
                    }
                }
                self.add.probe = ProbeState::Done(*info);
            }
            Err(error) => self.add.probe = ProbeState::Failed { url, error },
        }
    }

    /// "Start Download" / "Download Later" in the add dialog.
    /// "Start downloading immediately while displaying Download File Info":
    /// a plain file from the browser or clipboard starts before the user answers.
    pub fn start_add_early(&mut self) {
        let ok_url = url::Url::parse(self.add.url.trim()).map(|u| matches!(u.scheme(), "http" | "https")).unwrap_or(false);
        if !self.settings.start_while_dialog
            || self.add.started_id.is_some()
            || self.add.step != AddStep::Info
            || self.add.kind != DownloadKind::Http
            || !ok_url
            || self.add_dialog_duplicate().is_some()
        {
            return;
        }
        let new = self.new_download_from_dialog();
        let id = self.add_item(new, StartMode::Now);
        self.add.started_id = Some(id);
    }

    fn new_download_from_dialog(&self) -> NewDownload {
        let (total_size, resumable) = match (&self.add.youtube, &self.add.probe) {
            (Some(spec), _) => (spec.total_size(), true),
            (None, ProbeState::Done(ref info)) => (info.total_size, info.resumable),
            _ => (None, false),
        };
        let filename_probed = matches!(self.add.probe, ProbeState::Done(_));
        let mut filename = http::sanitize_filename(self.add.filename.trim());
        if filename.is_empty() {
            filename = "download".to_string();
        }
        NewDownload {
            url: self.add.url.trim().to_string(),
            filename,
            // Let the server name the file if neither the user nor a probe did.
            filename_auto: !self.add.filename_edited && !filename_probed,
            dir: self.add.dir.clone(),
            category: self.add.category,
            connections: self.add.connections,
            kind: self.add.kind,
            username: self.add.auth_enabled.then(|| self.add.username.clone()),
            password: self.add.auth_enabled.then(|| self.add.password.clone()),
            referer: self.add.referer.clone(),
            cookies: self.add.cookies.clone(),
            user_agent: self.add.user_agent.clone(),
            description: self.add.description.clone(),
            total_size,
            resumable,
            youtube: self.add.youtube.clone(),
            audio_url: self.add.audio_url.clone(),
            browser_proxy: self.add.browser_proxy.clone(),
            post_data: self.add.post_data.clone(),
            post_type: self.add.post_type.clone(),
        }
    }

    pub fn submit_add_dialog(&mut self, mode: StartMode) {
        let url = self.add.url.trim().to_string();
        if url.is_empty() {
            return;
        }
        // Keep the "Remember this path" checkbox state for the next dialog, so it
        // opens already checked when the user left it checked last time.
        if self.settings.remember_path_default != self.add.remember_path {
            self.settings.remember_path_default = self.add.remember_path;
            self.save_settings_only();
        }
        if let Some(id) = self.add.started_id.take() {
            self.finish_early_add(id, mode);
            return;
        }
        let new = self.new_download_from_dialog();
        if self.add.remember_path || (self.settings.remember_last_path && self.add.dir_edited) {
            self.settings.category_dirs.insert(self.add.category, self.add.dir.clone());
            self.save_settings_only();
        }
        if let Some(existing) = self.downloads.iter().find(|d| d.url == new.url).map(|d| d.id) {
            let action = self.add.dup_action;
            if self.add.dup_remember {
                self.settings.duplicate_action = action;
                self.save_settings_only();
            }
            if self.handle_duplicate(existing, action, mode, &new) {
                self.add.open = false;
                return;
            }
        }
        let id = self.add_item(new, mode);
        self.add.open = false;
        if mode == StartMode::Now && self.settings.show_progress_window {
            self.show_progress_window(id);
            // Bring the progress window to the front and keep it there, so it does
            // not fall behind the browser or look minimized after Start Download.
            self.progress_front = Some(id);
        }
    }

    pub fn close_add_dialog(&mut self) {
        // Cancel also drops a download that started early, with its partial file.
        if let Some(id) = self.add.started_id.take() {
            self.delete_downloads(&[id], false);
        }
        self.add.open = false;
    }

    /// The user answered "Download File Info" for a download that already runs.
    fn finish_early_add(&mut self, id: u64, mode: StartMode) {
        if self.add.remember_path || (self.settings.remember_last_path && self.add.dir_edited) {
            self.settings.category_dirs.insert(self.add.category, self.add.dir.clone());
            self.save_settings_only();
        }
        let wanted = self.add.dir.join(http::sanitize_filename(self.add.filename.trim()));
        let description = self.add.description.clone();
        let category = self.add.category;
        let changed_place = self.add.filename_edited || self.add.dir_edited;
        if let Some(item) = self.item_mut(id) {
            item.description = description;
            item.category = category;
            if changed_place && wanted != item.save_path && !wanted.as_os_str().is_empty() {
                item.move_to = Some(wanted);
            }
        }
        // The early download already finished while the dialog was open: finalize
        // it now (move it to its folder, mark it complete), so Start is instant.
        // A finished file cannot be queued, so the mode no longer applies.
        if let Some(path) = self.add.early_done.take() {
            self.on_download_completed(id, path);
            self.add.open = false;
            self.save_downloads();
            return;
        }
        if let StartMode::Queue(q) = mode {
            self.pause_download(id);
            self.set_queue(&[id], Some(q));
        } else if self.settings.show_progress_window {
            self.show_progress_window(id);
            // Keep the progress window in front after Start Download (see
            // submit_add_dialog), not behind the browser or seemingly minimized.
            self.progress_front = Some(id);
        }
        self.add.open = false;
        self.save_downloads();
    }

    /// Adds a download to the list and starts or queues it.
    pub fn add_item(&mut self, new: NewDownload, mode: StartMode) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        let save_path = self.unique_save_path(&new.dir.join(&new.filename), None);
        let filename = save_path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(new.filename);
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        // "Remember the speed limit for this server" in a progress window.
        let speed_limit_kbps = if new.kind == DownloadKind::Http { self.settings.remembered_limit(&new.url).unwrap_or(0) } else { 0 };

        let item = DownloadItem {
            id,
            url: new.url,
            category: if new.kind == DownloadKind::Hls { Category::Video } else { new.category },
            filename,
            save_path,
            total_size: new.total_size,
            downloaded_bytes: 0,
            status: if mode == StartMode::Now { DownloadStatus::Connecting } else { DownloadStatus::Queued },
            speed_bps: 0.0,
            eta_seconds: None,
            date_added: now,
            resumable: new.resumable,
            connections: new.connections.clamp(1, 32),
            segments: Vec::new(),
            error_message: None,
            queue: match mode {
                StartMode::Queue(q) => Some(q),
                _ => None,
            },
            legacy_in_queue: false,
            kind: new.kind,
            filename_auto: new.filename_auto,
            last_try: String::new(),
            speed_limit_kbps,
            referer: new.referer,
            cookies: new.cookies,
            user_agent: new.user_agent,
            username: new.username,
            password: new.password,
            description: new.description,
            retries_done: 0,
            youtube: new.youtube,
            audio_url: new.audio_url,
            move_to: None,
            browser_proxy: new.browser_proxy,
            via_browser_proxy: false,
            post_data: new.post_data,
            post_type: new.post_type,
            expected_hash: String::new(),
            hash_algo: HashAlgo::default(),
            verified: None,
        };

        self.downloads.push(item);
        self.select_only(id);
        self.save_downloads();

        if mode == StartMode::Now {
            self.start_item(id, false);
        }
        id
    }

    /// A path no other list item or unrelated file uses.
    pub fn unique_save_path(&self, wanted: &Path, exclude: Option<u64>) -> PathBuf {
        http::unique_path(wanted, |p| {
            p.exists()
                || ResumeManager::state_path_for(p).exists()
                || self.downloads.iter().any(|d| Some(d.id) != exclude && d.save_path == p)
        })
    }

    /// A file that the browser saved itself (from a blob: or data: address,
    /// which only the page can make): it goes to the folder of its category
    /// and is listed as complete, as if Klippo had downloaded it.
    pub fn adopt_browser_file(&mut self, path: &Path, msg: &IpcMessage) -> Result<u64, String> {
        let size = fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len()).ok_or_else(|| format!("{} is not there", path.display()))?;
        let name = msg
            .filename
            .as_deref()
            .map(http::sanitize_filename)
            .filter(|f| !f.is_empty())
            .or_else(|| path.file_name().map(|n| http::sanitize_filename(&n.to_string_lossy())))
            .unwrap_or_else(|| "download".to_string());
        let category = Category::from_filename(&name);
        let dir = msg.dir.clone().map(PathBuf::from).unwrap_or_else(|| self.settings.category_dir(category));
        // Already in that folder (the browser saves to the same Downloads folder): it stays.
        let in_place = path.parent().is_some_and(|p| p.to_string_lossy().trim_end_matches('\\').eq_ignore_ascii_case(dir.to_string_lossy().trim_end_matches('\\')));
        let new = NewDownload {
            url: msg.url.clone(),
            filename: name,
            dir,
            category,
            connections: 1,
            referer: msg.referer.clone(),
            total_size: Some(size),
            ..Default::default()
        };
        let id = self.add_item(new, StartMode::Later);
        let target = if in_place {
            path.to_path_buf()
        } else {
            let target = self.item(id).map(|i| i.save_path.clone()).unwrap_or_default();
            let moved = target
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|_| crate::gui::views::file_properties_modal::move_path(path, &target));
            if let Err(e) = moved {
                self.downloads.retain(|d| d.id != id);
                self.save_downloads();
                return Err(e.to_string());
            }
            target
        };
        self.on_download_completed(id, target);
        Ok(id)
    }

    pub fn add_from_message(&mut self, msg: IpcMessage) {
        let url = msg.url.trim().to_string();
        let filename = msg
            .filename
            .as_deref()
            .map(http::sanitize_filename)
            .filter(|f| !f.is_empty())
            .or_else(|| http::filename_from_url(&url))
            .unwrap_or_else(|| "download".to_string());
        let kind = if msg.youtube.is_some() {
            DownloadKind::Youtube
        } else if crate::core::ftp::is_ftp_url(&url) {
            DownloadKind::Ftp
        } else if msg.hls || msg.audio_url.is_some() || is_hls_url(&url) {
            DownloadKind::Hls
        } else {
            DownloadKind::Http
        };
        let category = match (&msg.youtube, kind) {
            (Some(spec), _) if spec.video.is_none() => Category::Music,
            (_, DownloadKind::Hls | DownloadKind::Youtube) => Category::Video,
            _ => Category::from_filename(&filename),
        };
        let filename = if kind == DownloadKind::Hls { hls_filename(&filename) } else { filename };
        let new = NewDownload {
            total_size: msg.youtube.as_ref().and_then(YoutubeSpec::total_size),
            resumable: msg.youtube.is_some(),
            youtube: msg.youtube.clone(),
            audio_url: msg.audio_url.clone(),
            browser_proxy: msg.browser_proxy.clone(),
            post_data: msg.post_data.clone(),
            post_type: msg.post_type.clone(),
            filename_auto: msg.filename.is_none(),
            dir: msg.dir.clone().map(PathBuf::from).unwrap_or_else(|| self.settings.category_dir(category)),
            url,
            filename,
            category,
            connections: self.settings.max_connections,
            kind,
            referer: msg.referer,
            cookies: msg.cookies,
            user_agent: msg.user_agent,
            ..Default::default()
        };
        // "Do not start downloading, only add files to the queue" for captured links.
        let captured = msg.from_browser || msg.from_clipboard;
        let mode = if msg.queue_only || (captured && self.settings.captured_to_queue) {
            StartMode::Queue(MAIN_QUEUE)
        } else {
            StartMode::Now
        };
        if let Some(existing) = self.downloads.iter().find(|d| d.url == new.url).map(|d| d.id) {
            if self.handle_duplicate(existing, self.settings.duplicate_action, mode, &new) {
                return;
            }
        }
        let id = self.add_item(new, mode);
        if mode == StartMode::Now && self.settings.show_progress_window && !msg.silent {
            self.show_progress_window(id);
            self.progress_front = Some(id);
        }
    }

    // ------------------------------------------------------------------
    // Download control
    // ------------------------------------------------------------------

    fn build_request(&self, item: &DownloadItem, fresh: bool) -> DownloadRequest {
        let request = self.request_options_for(&item.url, Some(item));
        let ftp = crate::core::ftp::FtpOptions { login: request.basic_auth.clone(), ..self.settings.ftp_options() };
        DownloadRequest {
            id: item.id,
            kind: item.kind,
            url: item.url.clone(),
            save_path: item.save_path.clone(),
            filename_auto: item.filename_auto,
            connections: self.settings.connections_for(&item.url, item.connections),
            speed_limit_bps: item.speed_limit_kbps.saturating_mul(1024),
            request,
            fresh,
            max_retries: self.settings.max_retries,
            ignore_time_changes: self.settings.ignore_time_changes,
            set_file_time: self.settings.set_file_time_from_server,
            youtube: item.youtube.clone(),
            audio_url: item.audio_url.clone(),
            ftp,
        }
    }

    /// Bytes received inside the "Download limits" window.
    pub fn bytes_in_limit_window(&mut self) -> u64 {
        let window = Duration::from_secs(self.settings.download_limit_hours.max(1) as u64 * 3600);
        while self.byte_history.front().is_some_and(|(t, _)| t.elapsed() > window) {
            self.byte_history.pop_front();
        }
        self.byte_history.iter().map(|(_, b)| *b).sum()
    }

    /// True while the download limit blocks new downloads.
    pub fn limit_reached(&mut self) -> bool {
        if !self.settings.download_limit_enabled || self.limit_ignored_until.is_some_and(|t| Instant::now() < t) {
            return false;
        }
        self.bytes_in_limit_window() >= self.settings.download_limit_mb.saturating_mul(1024 * 1024)
    }

    /// Warns or stops when the download limit is reached.
    pub fn limits_tick(&mut self) {
        if self.limit_dialog.is_some() || !self.limit_reached() {
            return;
        }
        if !self.downloads.iter().any(|d| d.status.is_active()) {
            return;
        }
        let downloaded = self.bytes_in_limit_window();
        if self.settings.download_limit_warn {
            self.limit_dialog = Some(LimitDialog { warn: true, downloaded });
        } else {
            self.stop_all_downloads();
            self.limit_dialog = Some(LimitDialog { warn: false, downloaded });
        }
    }

    pub(crate) fn start_item(&mut self, id: u64, fresh: bool) {
        if self.limit_reached() {
            self.set_status("Download limits exceeded: the download starts when the limit period ends");
            return;
        }
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let Some(item) = self.item_mut(id) else { return };
        item.status = DownloadStatus::Connecting;
        item.error_message = None;
        item.last_try = now;
        if fresh {
            item.downloaded_bytes = 0;
            item.segments.clear();
        }
        let request = self.build_request(self.item(id).unwrap(), fresh);
        if self.settings.dialup_enabled {
            system::ensure_dialup(&self.settings);
        }
        self.bridge.start_download(request);
        self.save_downloads();
    }

    pub fn show_progress_window(&mut self, id: u64) {
        if !self.progress_windows.contains(&id) {
            self.progress_windows.push(id);
        }
    }

    pub fn hide_progress_window(&mut self, id: u64) {
        self.progress_windows.retain(|w| *w != id);
    }

    /// User-initiated start/resume.
    pub fn resume_download(&mut self, id: u64) {
        let Some(item) = self.item(id) else { return };
        if !item.status.can_start() {
            return;
        }
        self.queue_skip.remove(&id);
        if let Some(item) = self.item_mut(id) {
            item.retries_done = 0;
        }
        self.start_item(id, false);
        if self.settings.show_progress_window {
            self.show_progress_window(id);
        }
    }

    pub fn resume_selected(&mut self) {
        for id in self.selected_ids() {
            self.resume_download(id);
        }
    }

    pub fn pause_download(&mut self, id: u64) {
        let Some(item) = self.item_mut(id) else { return };
        if !item.status.is_active() {
            return;
        }
        let queue = item.queue;
        item.status = DownloadStatus::Paused;
        item.speed_bps = 0.0;
        item.eta_seconds = None;
        self.bridge.pause_download(id);
        // A running queue must not restart a file the user stopped.
        if queue.is_some_and(|q| self.running_queues.contains(&q)) {
            self.queue_skip.insert(id);
        }
        self.save_downloads();
    }

    pub fn pause_selected(&mut self) {
        for id in self.selected_ids() {
            self.pause_download(id);
        }
    }

    pub fn stop_all_downloads(&mut self) {
        let ids: Vec<u64> = self.downloads.iter().filter(|d| d.status.is_active()).map(|d| d.id).collect();
        for id in ids {
            self.pause_download(id);
        }
        self.running_queues.clear();
    }

    /// "Pause All": stops the downloads that can resume; others keep running.
    pub fn pause_all(&mut self) {
        let ids: Vec<u64> = self
            .downloads
            .iter()
            .filter(|d| d.status.is_active() && (d.resumable || d.kind != DownloadKind::Http))
            .map(|d| d.id)
            .collect();
        let count = ids.len();
        for id in ids {
            self.pause_download(id);
        }
        let left = self.downloads.iter().filter(|d| d.status.is_active()).count();
        if left > 0 {
            self.set_status(format!("Paused {} downloads; {} cannot resume and keep running", count, left));
        }
    }

    /// Starts over from byte zero.
    pub fn redownload(&mut self, id: u64) {
        if self.item(id).is_none() {
            return;
        }
        self.queue_skip.remove(&id);
        self.start_item(id, true);
        if self.settings.show_progress_window {
            self.show_progress_window(id);
        }
    }

    /// Sets the per-file speed limit (0 = none) and applies it immediately.
    pub fn set_item_speed_limit(&mut self, id: u64, kbps: u64) {
        if let Some(item) = self.item_mut(id) {
            item.speed_limit_kbps = kbps;
        }
        self.bridge.set_speed_limit(id, kbps.saturating_mul(1024));
        self.mark_dirty();
        // A remembered server keeps the new limit; no limit forgets the server.
        let host = self.item(id).and_then(|i| server_of(&i.url));
        if let Some(host) = host.filter(|h| self.settings.server_speed_limits.contains_key(h)) {
            self.remember_server_limit_for(&host, kbps);
        }
    }

    /// "Remember the speed limit for this server" in the progress window of `id`.
    pub fn remember_server_limit(&mut self, id: u64, remember: bool) {
        let Some(item) = self.item(id) else { return };
        let Some(host) = server_of(&item.url) else { return };
        let kbps = if remember { item.speed_limit_kbps } else { 0 };
        self.remember_server_limit_for(&host, kbps);
    }

    fn remember_server_limit_for(&mut self, host: &str, kbps: u64) {
        // Also in the copy that Cancel in the Options dialog puts back.
        for settings in std::iter::once(&mut self.settings).chain(self.settings_backup.as_mut()) {
            if kbps > 0 {
                settings.server_speed_limits.insert(host.to_string(), kbps);
            } else {
                settings.server_speed_limits.remove(host);
            }
        }
        save_state_to_disk(&self.downloads, self.settings_backup.as_ref().unwrap_or(&self.settings));
    }

    pub fn request_delete(&mut self, ids: Vec<u64>) {
        if ids.is_empty() {
            return;
        }
        if self.settings.confirm_delete {
            self.delete_confirm = Some(DeleteConfirm { ids, delete_files: false, dont_ask: false });
        } else {
            self.delete_downloads(&ids, false);
        }
    }

    pub fn delete_downloads(&mut self, ids: &[u64], delete_completed_files: bool) {
        for &id in ids {
            let Some(idx) = self.downloads.iter().position(|d| d.id == id) else { continue };
            let item = self.downloads.remove(idx);
            // Unfinished downloads leave no partial data behind.
            let mut paths = if item.status == DownloadStatus::Completed { Vec::new() } else { item.temp_paths() };
            if item.status == DownloadStatus::Completed && delete_completed_files {
                paths.push(item.save_path.clone());
            }
            self.bridge.remove_download(id, paths);
            self.selection.remove(&id);
            self.queue_skip.remove(&id);
            self.file_hashes.remove(&id);
            self.hide_progress_window(id);
            if self.selected_id == Some(id) {
                self.selected_id = None;
            }
            if self.show_complete_dialog == Some(id) {
                self.show_complete_dialog = None;
            }
            if self.show_file_properties_dialog == Some(id) {
                self.show_file_properties_dialog = None;
            }
        }
        self.save_downloads();
    }

    pub fn delete_completed_downloads(&mut self) {
        let ids: Vec<u64> = self.downloads.iter().filter(|d| d.status == DownloadStatus::Completed).map(|d| d.id).collect();
        self.request_delete(ids);
    }

    // ------------------------------------------------------------------
    // Background events
    // ------------------------------------------------------------------

    pub fn mark_dirty(&mut self) {
        if self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
    }

    /// Writes progress to disk at most every few seconds.
    pub fn flush_if_dirty(&mut self) {
        if self.dirty_since.is_some_and(|t| t.elapsed() >= Duration::from_secs(5)) {
            self.save_downloads();
        }
    }

    pub fn process_background_events(&mut self) {
        while let Some(event) = self.bridge.try_recv_event() {
            match event {
                GuiEvent::Info { id, filename, save_path, total, resumable } => {
                    if let Some(item) = self.item_mut(id) {
                        if item.filename != filename || item.save_path != save_path {
                            item.filename = filename;
                            item.category = Category::from_filename(&item.filename);
                            item.save_path = save_path;
                        }
                        // From now on the name is fixed.
                        item.filename_auto = false;
                        item.total_size = total.or(item.total_size);
                        item.resumable = resumable;
                    }
                    self.mark_dirty();
                }
                GuiEvent::Progress { id, downloaded, total, speed_bps, eta_seconds, segments } => {
                    let previous = self.item(id).map(|d| d.downloaded_bytes).unwrap_or(0);
                    if downloaded > previous && self.settings.download_limit_enabled {
                        self.byte_history.push_back((Instant::now(), downloaded - previous));
                    }
                    if let Some(item) = self.item_mut(id) {
                        item.downloaded_bytes = downloaded;
                        if total.is_some() {
                            item.total_size = total;
                        }
                        item.speed_bps = speed_bps;
                        item.eta_seconds = eta_seconds;
                        if !segments.is_empty() {
                            item.segments = segments;
                        }
                    }
                    self.mark_dirty();
                }
                GuiEvent::StatusChanged { id, status, error } => {
                    // The dialog's own early download must not look complete
                    // before the user answers "Download File Info": hold it.
                    if status == DownloadStatus::Completed && early_download_waits(self.add.open, self.add.started_id, id) {
                        continue;
                    }
                    let Some(item) = self.item_mut(id) else { continue };
                    // A late "Downloading" must not undo a pause the user just made.
                    if item.status == DownloadStatus::Paused && status.is_active() {
                        continue;
                    }
                    item.status = status;
                    if error.is_some() {
                        item.error_message = error;
                    }
                    if !status.is_active() {
                        item.speed_bps = 0.0;
                        item.eta_seconds = None;
                    }
                    if status == DownloadStatus::Failed {
                        self.on_download_failed(id);
                    }
                    self.save_downloads();
                }
                GuiEvent::Completed { id, save_path } => {
                    // Hold the finished early download for the open dialog:
                    // submit_add_dialog finalizes it, close_add_dialog deletes it.
                    if early_download_waits(self.add.open, self.add.started_id, id) {
                        self.add.early_done = Some(save_path);
                    } else {
                        self.on_download_completed(id, save_path);
                    }
                }
                GuiEvent::ProbeResult { probe_id, result } => self.apply_probe_result(probe_id, result),
                GuiEvent::HashResult { id, result } => match result {
                    Ok(h) => {
                        // Verify against the checksum the user entered, if any.
                        let expected = self.item(id).map(|i| i.expected_hash.trim().to_lowercase()).unwrap_or_default();
                        if !expected.is_empty() {
                            let ok = h.trim().to_lowercase() == expected;
                            let name = self.item(id).map(|i| i.filename.clone()).unwrap_or_default();
                            if let Some(it) = self.item_mut(id) {
                                it.verified = Some(ok);
                            }
                            if ok {
                                self.set_status(format!("Checksum verified: {}", name));
                            } else {
                                self.set_status(format!("Checksum mismatch: {} may be corrupt", name));
                                if self.settings.sound_alerts && self.settings.sound_failed {
                                    system::play_sound(SoundEvent::DownloadFailed, &self.settings.sound_failed_path);
                                }
                            }
                            self.save_downloads();
                        }
                        self.file_hashes.insert(id, HashState::Done(h));
                    }
                    Err(e) => {
                        self.file_hashes.insert(id, HashState::Failed(e));
                    }
                },
            }
        }
    }

    fn on_download_failed(&mut self, id: u64) {
        self.retry_not_before.insert(id, Instant::now() + Duration::from_secs(5));
        let fallback = self.settings.browser_proxy_fallback
            && self.item(id).is_some_and(|i| i.browser_proxy.is_some() && !i.via_browser_proxy && i.kind == DownloadKind::Http);
        if fallback {
            if let Some(item) = self.item_mut(id) {
                item.via_browser_proxy = true;
            }
            self.set_status("Download error: trying again through the browser's proxy");
            self.resume_download(id);
            return;
        }
        let error = self.item(id).and_then(|i| i.error_message.clone()).unwrap_or_default();
        if error.contains("HTTP 401") && self.login_prompt.is_none() {
            let realm = error
                .split("realm \"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .unwrap_or("")
                .to_string();
            let username = self.item(id).and_then(|i| i.username.clone()).unwrap_or_default();
            self.login_prompt = Some(LoginPrompt { id, realm, username, password: String::new(), save: true });
        } else if error.contains("HTTP 407") {
            self.set_status("The proxy server needs a login: set it in Options > Proxy / Socks");
        }
        if self.settings.sound_alerts && self.settings.sound_failed {
            system::play_sound(SoundEvent::DownloadFailed, &self.settings.sound_failed_path);
        }
        let name = self.item(id).map(|i| i.filename.clone()).unwrap_or_default();
        let err = self.item(id).and_then(|i| i.error_message.clone()).unwrap_or_default();
        self.set_status(format!("{}: {}", name, err));
        if self.in_tray || self.last_minimized {
            self.os_notify("Download failed", &name, true);
        }
    }

    fn on_download_completed(&mut self, id: u64, save_path: PathBuf) {
        let mut save_path = save_path;
        if let Some(target) = self.item_mut(id).and_then(|i| i.move_to.take()) {
            if let Some(dir) = target.parent() {
                let _ = fs::create_dir_all(dir);
            }
            if fs::rename(&save_path, &target).or_else(|_| fs::copy(&save_path, &target).and_then(|_| fs::remove_file(&save_path))).is_ok() {
                save_path = target;
            }
        }
        if let Some(item) = self.item_mut(id) {
            item.status = DownloadStatus::Completed;
            // Streams may end in another container than planned (.ts -> .mp4).
            if let Some(name) = save_path.file_name() {
                item.filename = name.to_string_lossy().into_owned();
            }
            item.save_path = save_path.clone();
            item.speed_bps = 0.0;
            item.eta_seconds = None;
            item.queue = None;
            item.retries_done = 0;
            if let Some(total) = item.total_size {
                item.downloaded_bytes = total;
            }
        }
        self.queue_skip.remove(&id);

        if self.settings.sound_alerts && self.settings.sound_complete {
            system::play_sound(SoundEvent::DownloadComplete, &self.settings.sound_complete_path);
        }
        // Klippo shows a notification-area balloon when it runs in the background.
        if self.in_tray || self.last_minimized {
            let name = self.item(id).map(|i| i.filename.clone()).unwrap_or_default();
            self.os_notify("Download complete", &name, false);
        }
        if self.settings.antivirus_scan {
            if let Err(e) = system::scan_file(&self.settings.antivirus_path, &self.settings.antivirus_args, &save_path) {
                self.set_status(format!("Virus scanner could not start: {}", e));
            }
        }
        if self.settings.open_file_on_completion {
            crate::gui::system::open_file(&save_path);
        }

        // The progress window is replaced with the "Download complete" window.
        let had_window = self.progress_windows.contains(&id);
        if had_window && (self.settings.auto_close_progress || self.settings.show_complete_window) {
            self.hide_progress_window(id);
        }
        if self.settings.show_complete_window && !self.settings.open_file_on_completion {
            self.show_complete_dialog = Some(id);
        }

        // Verify the finished file if the user set a checksum to check against.
        let verify = self
            .item(id)
            .filter(|i| !i.expected_hash.trim().is_empty())
            .map(|i| (i.hash_algo, i.save_path.clone()));
        if let Some((algo, path)) = verify {
            if let Some(it) = self.item_mut(id) {
                it.verified = None;
            }
            self.file_hashes.insert(id, HashState::Computing);
            self.bridge.compute_hash(id, path, algo);
        }

        self.save_downloads();
    }

    /// Shows a Windows notification-area balloon, in addition to the sound and
    /// the completion dialog. Used only while Klippo runs in the background.
    #[cfg(windows)]
    fn os_notify(&mut self, title: &str, body: &str, failed: bool) {
        if let Some(hwnd) = self.hwnd {
            let notifier = self.notifier.get_or_insert_with(|| system::Notifier::new(hwnd));
            notifier.notify(title, body, failed);
        }
    }

    #[cfg(not(windows))]
    fn os_notify(&mut self, _title: &str, _body: &str, _failed: bool) {}

    /// Starts a download for each link dropped onto the window (drag-and-drop).
    pub fn process_dropped_urls(&mut self) {
        #[cfg(windows)]
        for url in crate::gui::dnd::take() {
            if self.settings.is_exception(&url) || self.downloads.iter().any(|d| d.url == url) {
                continue;
            }
            let msg = IpcMessage { url, ..Default::default() };
            if self.settings.show_start_dialog {
                self.queue_add_request(msg);
            } else {
                self.add_from_message(IpcMessage { silent: true, ..msg });
            }
        }
    }

    pub fn process_clipboard_events(&mut self) {
        if let Some(url) = self.clipboard_watcher.take_detected_url() {
            if self.settings.is_exception(&url) || self.downloads.iter().any(|d| d.url == url) {
                return;
            }
            let msg = IpcMessage { url, from_clipboard: true, ..Default::default() };
            if self.settings.show_start_dialog {
                self.queue_add_request(msg);
            } else {
                self.add_from_message(IpcMessage { silent: true, ..msg });
            }
        }
    }

    fn queue_add_request(&mut self, msg: IpcMessage) {
        // The same request twice (a double click in the browser) opens one dialog.
        let open_same = self.add.open && self.add.url == msg.url.trim() && self.add.youtube == msg.youtube;
        if open_same || self.pending_adds.iter().any(|m| m.url == msg.url && m.youtube == msg.youtube) {
            return;
        }
        if self.add.open {
            self.pending_adds.push_back(msg);
        } else {
            self.open_add_dialog_with(&msg);
        }
    }

    /// Opens the next waiting browser/clipboard request once the dialog is free.
    pub fn next_pending_add(&mut self) {
        if !self.add.open {
            if let Some(msg) = self.pending_adds.pop_front() {
                self.open_add_dialog_with(&msg);
            }
        }
    }

    /// Requests from other Klippo processes and the browser extension.
    pub fn process_ipc_messages(&mut self, ctx: &egui::Context) {
        let messages: Vec<IpcMessage> = match self.ipc {
            Some(ref server) => server.rx.try_iter().collect(),
            None => return,
        };
        for msg in messages {
            self.handle_message(msg, Some(ctx));
        }
    }

    pub fn handle_message(&mut self, msg: IpcMessage, ctx: Option<&egui::Context>) {
        if let Some(ref path) = msg.adopt_path {
            if let Err(e) = self.adopt_browser_file(Path::new(path), &msg) {
                self.set_status(format!("Could not take the browser's file: {}", e));
            }
            return;
        }
        if msg.quit_when_done {
            self.quit_when_done = true;
        }
        if !msg.batch_urls.is_empty() {
            self.batch.mode = BatchMode::List;
            self.batch.urls_text = msg.batch_urls.join("\n");
            self.batch.from_browser = true;
            self.batch.to_front = true;
            self.batch.open = true;
        }
        let has_url = !msg.url.trim().is_empty();
        if has_url {
            let interactive = !msg.silent && !msg.queue_only && self.settings.show_start_dialog;
            if interactive {
                self.queue_add_request(msg.clone());
            } else {
                self.add_from_message(msg.clone());
            }
        }
        if msg.start_queue {
            self.start_queue(MAIN_QUEUE);
        }
        // A download request shows only its own window (the "Download File
        // Info", the batch dialog or the progress window), in front of the
        // browser; the main window stays in the notification area or wherever
        // it is. A start of Klippo without a request shows the main window.
        let request = has_url || !msg.batch_urls.is_empty() || msg.start_queue || msg.quit_when_done;
        if let (Some(ctx), false) = (ctx, request) {
            self.show_main_window(ctx);
        }
    }

    /// `/q`: exit when nothing is downloading or waiting in the queue.
    pub fn check_quit_when_done(&mut self) {
        if !self.quit_when_done {
            return;
        }
        let busy = self.downloads.iter().any(|d| d.status.is_active())
            || self.queues_have_waiting()
            || self.add.open;
        if !busy {
            self.quit_when_done = false;
            self.exit_confirmed = true;
            self.pending_exit = true;
        }
    }

    // ------------------------------------------------------------------
    // Settings and persistence
    // ------------------------------------------------------------------

    pub fn open_options(&mut self) {
        if !self.show_options_dialog {
            self.settings.launch_on_startup = system::is_launch_on_startup();
            self.settings_backup = Some(self.settings.clone());
            self.show_options_dialog = true;
        }
    }

    pub fn cancel_options(&mut self, ctx: &egui::Context) {
        if let Some(backup) = self.settings_backup.take() {
            self.settings = backup;
            Category::set_user_categories(&self.settings.user_categories);
            Category::set_custom_types(&self.settings.category_types);
            self.apply_theme(ctx);
        }
        self.show_options_dialog = false;
    }

    /// Saves settings and applies everything that has a side effect.
    pub fn save_settings(&mut self) {
        self.clipboard_watcher.set_enabled(self.settings.monitor_clipboard);
        self.clipboard_watcher.set_file_types(&self.settings.file_types_extensions);
        self.bridge.set_global_speed_limit(self.settings.global_limit_bps());
        if self.settings.launch_on_startup != system::is_launch_on_startup() {
            if let Err(e) = system::set_launch_on_startup(self.settings.launch_on_startup) {
                self.set_status(format!("Could not change Windows startup entry: {}", e));
                self.settings.launch_on_startup = system::is_launch_on_startup();
            }
        }
        self.save_settings_only();
    }

    pub fn save_settings_only(&mut self) {
        Category::set_user_categories(&self.settings.user_categories);
        Category::set_custom_types(&self.settings.category_types);
        crate::core::parts::set_parts_root(Some(self.settings.temp_dir.clone()));
        save_state_to_disk(&self.downloads, &self.settings);
        self.dirty_since = None;
    }

    pub fn save_downloads(&mut self) {
        save_state_to_disk(&self.downloads, self.settings_backup.as_ref().unwrap_or(&self.settings));
        self.dirty_since = None;
    }

    /// Dark when Windows apps are dark (or when the user chose dark mode).
    pub fn wants_dark(&self) -> bool {
        if self.settings.follow_system_theme {
            match self.system_theme.load(std::sync::atomic::Ordering::Relaxed) {
                1 => return false,
                2 => return true,
                _ => {}
            }
        }
        self.settings.dark_mode
    }

    pub fn apply_theme(&mut self, ctx: &egui::Context) {
        let dark = self.wants_dark();
        let theme = if dark { IdmTheme::idm_dark() } else { IdmTheme::idm_light() };
        theme.apply(ctx);
        self.applied_dark = Some(dark);
    }

    /// Follows a change of the Windows theme while the program runs.
    pub fn sync_theme(&mut self, ctx: &egui::Context) {
        if self.applied_dark != Some(self.wants_dark()) {
            self.apply_theme(ctx);
        }
    }

    fn watch_system_theme() -> std::sync::Arc<std::sync::atomic::AtomicU8> {
        use std::sync::atomic::{AtomicU8, Ordering};
        let state = std::sync::Arc::new(AtomicU8::new(match system::windows_apps_dark() {
            Some(true) => 2,
            Some(false) => 1,
            None => 0,
        }));
        let shared = state.clone();
        let _ = std::thread::Builder::new().name("klippo-theme".to_string()).spawn(move || loop {
            std::thread::sleep(Duration::from_secs(2));
            let value = match system::windows_apps_dark() {
                Some(true) => 2,
                Some(false) => 1,
                None => continue,
            };
            shared.store(value, Ordering::Relaxed);
        });
        state
    }

    /// Saves the login from the "Connecting to the site" prompt and retries.
    pub fn apply_login(&mut self, prompt: LoginPrompt) {
        let url = self.item(prompt.id).map(|i| i.url.clone()).unwrap_or_default();
        if let Some(item) = self.item_mut(prompt.id) {
            item.username = Some(prompt.username.clone());
            item.password = Some(prompt.password.clone());
        }
        if prompt.save {
            if let Some(host) = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(|h| h.to_string())) {
                self.settings.site_logins.retain(|l| !l.url.eq_ignore_ascii_case(&host));
                self.settings.site_logins.push(SiteLogin { url: host, username: prompt.username, password: prompt.password });
                self.save_settings_only();
            }
        }
        self.resume_download(prompt.id);
    }

    /// Ids for an export scope, in list order.
    pub fn export_ids(&self, scope: ExportScope) -> Vec<u64> {
        match scope {
            ExportScope::Queue(q) => self.downloads.iter().filter(|d| d.queue == Some(q)).map(|d| d.id).collect(),
            ExportScope::Selected => self.selected_ids(),
            ExportScope::All => self.downloads.iter().map(|d| d.id).collect(),
        }
    }

    /// "Export": a text file (one address per line) or an export file (.ef2).
    pub fn export_list(&mut self, path: &Path, format: ExportFormat, scope: ExportScope) {
        let items: Vec<&DownloadItem> = self.export_ids(scope).iter().filter_map(|id| self.item(*id)).collect();
        let text = match format {
            ExportFormat::Text => items.iter().map(|d| format!("{}\r\n", d.url)).collect::<String>(),
            ExportFormat::Ef2 => write_ef2(&items),
        };
        let count = items.len();
        match fs::write(path, text) {
            Ok(()) => self.set_status(format!("Exported {} links to {}", count, path.display())),
            Err(e) => self.set_status(format!("Export failed: {}", e)),
        }
    }

    /// "Import" from a text file or an export file (.ef2): every
    /// address becomes a file in the main queue.
    pub fn import_list(&mut self, path: &Path) {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.set_status(format!("Import failed: {}", e));
                return;
            }
        };
        let text = decode_text(&bytes);
        let entries = if text.lines().any(|l| l.trim() == "<") { parse_ef2(&text) } else { parse_url_list(&text) };
        let mut added = 0;
        for entry in entries {
            if self.downloads.iter().any(|d| d.url == entry.url) {
                continue;
            }
            let has_login = entry.username.is_some();
            let (user, pass) = (entry.username.clone(), entry.password.clone());
            self.add_from_message(IpcMessage {
                url: entry.url.clone(),
                referer: entry.referer,
                cookies: entry.cookies,
                user_agent: entry.user_agent,
                queue_only: true,
                silent: true,
                ..Default::default()
            });
            if has_login {
                if let Some(item) = self.downloads.iter_mut().rev().find(|d| d.url == entry.url) {
                    item.username = user;
                    item.password = pass;
                }
            }
            added += 1;
        }
        self.save_downloads();
        self.set_status(format!("Imported {} links into the main queue", added));
    }

    /// Stops all downloads (saving resume data) before the process exits.
    pub fn shutdown_background(&mut self) {
        self.bridge.shutdown(Duration::from_secs(4));
        for item in self.downloads.iter_mut().filter(|d| d.status.is_active()) {
            item.status = DownloadStatus::Paused;
            item.speed_bps = 0.0;
        }
        self.save_downloads();
        if let Some(ref ipc) = self.ipc {
            ipc.cleanup();
        }
    }
}

/// One link of an import file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportEntry {
    pub url: String,
    pub referer: Option<String>,
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Text files may be UTF-8 or UTF-16 (older exports are ANSI or UTF-8).
fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

fn parse_url_list(text: &str) -> Vec<ImportEntry> {
    text.split_whitespace()
        .map(str::trim)
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .map(|u| ImportEntry { url: u.to_string(), ..Default::default() })
        .collect()
}

/// Export file: blocks of `<`, the address, `key: value` lines, `>`.
pub fn parse_ef2(text: &str) -> Vec<ImportEntry> {
    let mut out = Vec::new();
    let mut current: Option<ImportEntry> = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "<" {
            current = Some(ImportEntry::default());
        } else if line == ">" {
            if let Some(entry) = current.take() {
                if entry.url.starts_with("http://") || entry.url.starts_with("https://") {
                    out.push(entry);
                }
            }
        } else if let Some(ref mut entry) = current {
            if entry.url.is_empty() {
                entry.url = line.to_string();
            } else if let Some((key, value)) = line.split_once(':') {
                let value = Some(value.trim().to_string()).filter(|v| !v.is_empty());
                match key.trim().to_ascii_lowercase().as_str() {
                    "referer" => entry.referer = value,
                    "cookie" => entry.cookies = value,
                    "user-agent" => entry.user_agent = value,
                    "username" => entry.username = value,
                    "password" => entry.password = value,
                    _ => {}
                }
            }
        }
    }
    out
}

pub fn write_ef2(items: &[&DownloadItem]) -> String {
    let mut out = String::new();
    for d in items {
        out.push_str("<\r\n");
        out.push_str(&d.url);
        out.push_str("\r\n");
        let fields = [
            ("referer", &d.referer),
            ("cookie", &d.cookies),
            ("User-Agent", &d.user_agent),
            ("username", &d.username),
            ("password", &d.password),
        ];
        for (key, value) in fields {
            if let Some(v) = value.as_ref().filter(|v| !v.is_empty()) {
                out.push_str(&format!("{}: {}\r\n", key, v));
            }
        }
        out.push_str(">\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_names_end_in_ts() {
        assert_eq!(hls_filename("index.m3u8"), "index.ts");
        assert_eq!(hls_filename("Big Buck Bunny"), "Big Buck Bunny.ts");
        assert_eq!(hls_filename("Part 1.5 (720p)"), "Part 1.5 (720p).ts");
        assert_eq!(hls_filename("clip.TS"), "clip.TS");
        assert_eq!(hls_filename("Clip.mp4"), "Clip.mp4");
        assert_eq!(hls_filename(".m3u8"), "video.ts");
    }

    #[test]
    fn ef2_round_trip() {
        let text = "<\r\nhttps://a.com/x.zip\r\nreferer: https://a.com/\r\ncookie: s=1; t=2\r\nUser-Agent: UA/1\r\n>\r\n<\r\nhttps://b.org/y.iso\r\n>\r\n";
        let entries = parse_ef2(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].referer.as_deref(), Some("https://a.com/"));
        assert_eq!(entries[0].cookies.as_deref(), Some("s=1; t=2"));
        assert_eq!(entries[1].url, "https://b.org/y.iso");
        assert_eq!(parse_url_list("https://a.com/1 junk\nhttps://a.com/2").len(), 2);
    }

    #[test]
    fn windows_proxy_strings() {
        let p = system_proxy_config("proxy.corp:8080", "<local>;*.corp").unwrap();
        assert_eq!(p.http.as_deref(), Some("http://proxy.corp:8080"));
        assert_eq!(p.https.as_deref(), Some("http://proxy.corp:8080"));
        assert!(p.bypass.unwrap().contains("*.corp"));
        let p = system_proxy_config("http=h1:80;https=h2:443;ftp=h3:21", "").unwrap();
        assert_eq!(p.http.as_deref(), Some("http://h1:80"));
        assert_eq!(p.https.as_deref(), Some("http://h2:443"));
    }

    #[test]
    fn batch_pattern_expansion() {
        let urls = expand_batch_pattern("https://a.com/pic*.jpg", false, "8", "10", 2);
        assert_eq!(urls, vec!["https://a.com/pic08.jpg", "https://a.com/pic09.jpg", "https://a.com/pic10.jpg"]);
        let urls = expand_batch_pattern("https://a.com/*.zip", true, "a", "c", 1);
        assert_eq!(urls, vec!["https://a.com/a.zip", "https://a.com/b.zip", "https://a.com/c.zip"]);
        assert!(expand_batch_pattern("https://a.com/x.zip", false, "1", "3", 1).is_empty());
    }

    #[test]
    fn legacy_queue_fields_migrate() {
        let mut s: AppSettings =
            serde_json::from_str("{\"queue_simultaneous\": 3, \"scheduler_start_enabled\": true, \"scheduler_start_time\": \"03:30\"}").unwrap();
        s.migrate_legacy_queue();
        assert_eq!(s.queues.len(), 1);
        assert_eq!(s.queues[0].simultaneous, 3);
        assert!(s.queues[0].start_enabled);
        assert_eq!(s.queues[0].start_time, "03:30");
        let item: DownloadItem = serde_json::from_str(
            "{\"id\":1,\"url\":\"u\",\"filename\":\"f\",\"save_path\":\"p\",\"category\":\"General\",\"total_size\":null,\"downloaded_bytes\":0,\"status\":\"Queued\",\"speed_bps\":0.0,\"eta_seconds\":null,\"date_added\":\"\",\"resumable\":false,\"connections\":8,\"segments\":[],\"error_message\":null,\"in_queue\":true}",
        )
        .unwrap();
        assert!(item.legacy_in_queue);
    }

    #[test]
    fn server_connection_limits() {
        let mut s = AppSettings::default();
        s.server_limits.push(ServerLimit { server: "*.example.com".into(), max_connections: 2 });
        assert_eq!(s.connections_for("https://dl.example.com/a.zip", 8), 2);
        assert_eq!(s.connections_for("https://other.org/a.zip", 8), 8);
    }

    #[test]
    fn settings_tolerate_missing_fields() {
        let s: AppSettings = serde_json::from_str("{\"max_connections\": 4}").unwrap();
        assert_eq!(s.max_connections, 4);
        assert!(s.confirm_delete);
    }

    #[test]
    fn remember_path_checkbox_state_persists() {
        // Old settings without the field default to unchecked.
        let s: AppSettings = serde_json::from_str("{\"max_connections\": 4}").unwrap();
        assert!(!s.remember_path_default);
        // A saved checked state is read back, so the dialog can open pre-checked.
        let s: AppSettings = serde_json::from_str("{\"remember_path_default\": true}").unwrap();
        assert!(s.remember_path_default);
    }

    #[test]
    fn capture_rules() {
        let mut s = AppSettings { capture_all_types: false, ..AppSettings::default() };
        assert!(s.should_capture("https://example.com/setup.exe", None, None));
        assert!(s.should_capture("https://example.com/dl?id=3", Some("movie.mkv"), None));
        assert!(!s.should_capture("https://example.com/page.html", None, None));
        assert!(!s.should_capture("https://example.com/notes.txt", Some("notes.txt"), Some("text/plain")));
        assert!(!s.should_capture("https://dl.update.microsoft.com/a.exe", None, None));
        // Every file type; web pages stay in the browser.
        s.capture_all_types = true;
        assert!(s.should_capture("https://example.com/notes.txt", Some("notes.txt"), Some("text/plain")));
        assert!(s.should_capture("https://example.com/get?id=7", Some("report"), Some("application/octet-stream")));
        assert!(!s.should_capture("https://example.com/", Some("page.html"), Some("text/html")));
        assert!(!s.should_capture("https://example.com/saved", Some("Saved page.mhtml"), Some("multipart/related")));
        assert!(!s.should_capture("https://dl.update.microsoft.com/a.exe", None, None), "the exceptions still count");
        assert!(!s.should_capture("blob:https://dl.update.microsoft.com/0d1e", Some("a.bin"), None), "also for a page's blob: file");
        assert!(AppSettings::default().capture_all_types, "new and older settings take every file type");
    }

    #[test]
    fn form_downloads_keep_their_post() {
        let p = post_body(Some("YT0xJmI9Mg=="), None).unwrap();
        assert_eq!(p.data, b"a=1&b=2");
        assert_eq!(p.content_type, "application/x-www-form-urlencoded");
        assert_eq!(post_body(Some("eA=="), Some("multipart/form-data; boundary=k")).unwrap().content_type, "multipart/form-data; boundary=k");
        assert!(post_body(None, None).is_none() && post_body(Some("%%"), None).is_none());
        let msg: IpcMessage = serde_json::from_str(r#"{"url":"https://a.org/export","post_data":"YT0x","post_type":"application/x-www-form-urlencoded"}"#).unwrap();
        assert_eq!(msg.post_data.as_deref(), Some("YT0x"));
    }

    #[test]
    fn the_dialogs_own_download_is_no_duplicate() {
        let item = |id: u64, url: &str| -> DownloadItem {
            serde_json::from_value(serde_json::json!({
                "id": id, "url": url, "filename": "f", "save_path": "p", "category": "General", "total_size": null,
                "downloaded_bytes": 0, "status": "Queued", "speed_bps": 0.0, "eta_seconds": null, "date_added": "",
                "resumable": false, "connections": 8, "segments": [], "error_message": null
            }))
            .unwrap()
        };
        let list = vec![item(1, "https://a.org/x.zip"), item(2, "https://a.org/y.zip")];
        assert_eq!(duplicate_in(&list, " https://a.org/x.zip ", None).map(|d| d.id), Some(1));
        assert_eq!(duplicate_in(&list, "https://a.org/x.zip", Some(2)).map(|d| d.id), Some(1));
        assert!(duplicate_in(&list, "https://a.org/y.zip", Some(2)).is_none(), "the download the dialog started early");
        assert!(duplicate_in(&list, "https://a.org/z.zip", None).is_none());
    }

    #[test]
    fn early_download_waits_until_the_dialog_is_answered() {
        // The dialog's own early download holds when the dialog is open.
        assert!(early_download_waits(true, Some(7), 7));
        // Any other finished download completes at once, even with the dialog open.
        assert!(!early_download_waits(true, Some(7), 9));
        // After the dialog is answered or closed it no longer holds anything.
        assert!(!early_download_waits(false, Some(7), 7));
        // With no early download, nothing is held.
        assert!(!early_download_waits(true, None, 7));
    }

    #[test]
    fn site_login_matching() {
        let login = SiteLogin { url: "files.example.com".into(), username: "u".into(), password: "p".into() };
        assert!(login.matches("https://files.example.com/a.zip"));
        assert!(login.matches("https://cdn.files.example.com/a.zip"));
        assert!(!login.matches("https://example.org/a.zip"));
        let prefix = SiteLogin { url: "https://example.com/private/".into(), username: "u".into(), password: "p".into() };
        assert!(prefix.matches("https://example.com/private/x.iso"));
        assert!(!prefix.matches("https://example.com/public/x.iso"));
    }

    #[test]
    fn hls_detection() {
        assert!(is_hls_url("https://a.com/live/master.m3u8?token=1"));
        assert!(!is_hls_url("https://a.com/file.mp4"));
    }

    #[test]
    fn user_categories() {
        // Other tests use the same lists: only test types change them.
        let mut s = AppSettings::default();
        s.user_categories.push(UserCategory { id: 7, name: "Test books".into() });
        let books = Category::User(7);
        s.category_types.insert(books, "ZZQ ZZW".into());
        s.category_types.entry(Category::Documents).or_default().push_str(" ZZQ");
        Category::set_user_categories(&s.user_categories);
        Category::set_custom_types(&s.category_types);
        assert_eq!(books.name(), "Test books");
        assert!(Category::all().ends_with(&[books]));
        assert_eq!(Category::from_extension("zzw"), books);
        assert_eq!(Category::from_extension("zzq"), books, "the user's category comes before Documents");
        assert_eq!(Category::from_extension("pdf"), Category::Documents);
        // Saved as text, also as the key of the folder map.
        s.category_dirs.insert(books, PathBuf::from("D:\\Books"));
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"User7\":") && json.contains("\"Video\":"));
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.category_dir(books), PathBuf::from("D:\\Books"));
        assert_eq!(back.category_types.get(&books).map(String::as_str), Some("ZZQ ZZW"));
        assert_eq!(back.user_categories, s.user_categories);
        assert_eq!(serde_json::from_str::<Category>("\"Video\"").unwrap(), Category::Video);
        assert_eq!(serde_json::from_str::<Category>("\"Removed\"").unwrap(), Category::General);
        assert_eq!(Category::User(99).name(), "Category 99");
    }

    #[test]
    fn remembered_server_limits() {
        let mut s = AppSettings::default();
        s.server_speed_limits.insert("dl.example.com".into(), 300);
        assert_eq!(s.remembered_limit("https://DL.example.com/a.zip"), Some(300));
        assert_eq!(s.remembered_limit("https://example.com/a.zip"), None);
        assert_eq!(server_of("http://WWW.A.org:8080/x").as_deref(), Some("www.a.org"));
        let back: AppSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.server_speed_limits.get("dl.example.com"), Some(&300));
    }
}
