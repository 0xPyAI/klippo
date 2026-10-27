//! Chrome / Edge / Firefox native messaging host and its registration.
//!
//! The browser starts `klippo.exe` with the extension origin as an argument
//! and talks over stdin/stdout with 32-bit length-prefixed JSON messages.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::core::youtube::YoutubeSpec;
use crate::gui::ipc::{deliver_or_launch, IpcMessage};
use crate::gui::state::{app_data_dir, load_settings};

pub const HOST_NAME: &str = "org.klippo.downloader";
/// ID of the extension in the `extension/chromium` folder (fixed by its manifest key).
pub const BUNDLED_EXTENSION_ID: &str = "hkfpllhmbmikkeobmbnggbjloggpibef";
pub const FIREFOX_EXTENSION_ID: &str = "klippo@klippo.local";

/// Registry keys of the browsers' native messaging hosts (under HKCU).
const BROWSER_KEYS: [&str; 5] = [
    r"Software\Google\Chrome\NativeMessagingHosts",
    r"Software\Chromium\NativeMessagingHosts",
    r"Software\Microsoft\Edge\NativeMessagingHosts",
    r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
    r"Software\Mozilla\NativeMessagingHosts",
];

/// The native messaging host name.
fn host_names() -> impl Iterator<Item = &'static str> {
    std::iter::once(HOST_NAME)
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ExtensionRequest {
    pub action: Option<String>,
    pub url: String,
    pub cookies: Option<String>,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
    #[serde(alias = "filename")]
    pub suggested_filename: Option<String>,
    pub file_size: Option<u64>,
    pub mime: Option<String>,
    /// Sent by "Download with Klippo" and the video button: skip the file-type filter.
    pub force: bool,
    /// "hls" when the video button sends a playlist, "youtube" with `youtube`.
    pub media_type: Option<String>,
    /// YouTube stream session recorded from the web player.
    pub youtube: Option<YoutubeSpec>,
    /// Separate audio of the video (HLS audio playlist or DASH-style audio file).
    pub audio_url: Option<String>,
    /// "batch": the links of a page ("Download all links").
    pub urls: Vec<String>,
    /// The browser's own proxy (scheme://host:port), tried after download errors.
    pub browser_proxy: Option<String>,
    /// A download that a form started: the POST body (base64) and its Content-Type.
    pub post_data: Option<String>,
    pub post_type: Option<String>,
    /// "adopt": the file the browser saved itself (a blob: or data: download).
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExtensionResponse {
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_types: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exceptions: Option<String>,
    /// Special keys and context menu items for the extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub menu: Option<serde_json::Value>,
    /// The download panels (web players, selected links).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panel: Option<serde_json::Value>,
    /// Whether to take files that a page fetches itself (off by default; a web
    /// app such as webmail also fetches files it does not mean to download).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_fetched: Option<bool>,
}

impl ExtensionResponse {
    fn new(status: &str, message: impl Into<String>) -> Self {
        Self { status: status.to_string(), message: message.into(), file_types: None, exceptions: None, keys: None, menu: None, panel: None, capture_fetched: None }
    }
}

/// True when the browser launched us as a native messaging host.
pub fn is_native_host_invocation(args: &[String]) -> bool {
    args.iter().skip(1).any(|a| {
        a == "native-host"
            || a.starts_with("chrome-extension://")
            || a.starts_with("--parent-window")
            || host_names().any(|name| a.ends_with(&format!("{}.json", name)) || a.ends_with(&format!("{}.moz.json", name)))
            || a == FIREFOX_EXTENSION_ID
    })
}

fn read_message(stdin: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_bytes = [0u8; 4];
    match stdin.read_exact(&mut len_bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let msg_len = u32::from_ne_bytes(len_bytes) as usize;
    if msg_len > 64 * 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let mut buffer = vec![0u8; msg_len];
    stdin.read_exact(&mut buffer)?;
    Ok(Some(buffer))
}

fn write_message(stdout: &mut impl Write, resp: &ExtensionResponse) -> io::Result<()> {
    let json = serde_json::to_vec(resp)?;
    stdout.write_all(&(json.len() as u32).to_ne_bytes())?;
    stdout.write_all(&json)?;
    stdout.flush()
}

/// Handles one request from the extension.
pub fn handle_request(req: ExtensionRequest) -> ExtensionResponse {
    let settings = load_settings();
    match req.action.as_deref().unwrap_or("download") {
        "ping" => ExtensionResponse::new("ok", format!("Klippo {}", env!("CARGO_PKG_VERSION"))),
        "config" => ExtensionResponse {
            file_types: Some(settings.file_types_extensions.clone()),
            exceptions: Some(settings.exceptions_servers.clone()),
            keys: Some(serde_json::json!({
                "prevent": { "enabled": settings.keys_prevent_enabled, "alt": settings.keys_prevent[0], "shift": settings.keys_prevent[1], "ctrl": settings.keys_prevent[2] },
                "force": { "enabled": settings.keys_force_enabled, "alt": settings.keys_force[0], "shift": settings.keys_force[1], "ctrl": settings.keys_force[2], "ins": settings.keys_force[3] },
                "check_click": settings.keys_check_click,
                "skip_web": settings.keys_skip_web,
            })),
            menu: Some(serde_json::json!({
                "download": settings.browser_menu[0],
                "last_video": settings.browser_menu[1],
                "videos": settings.browser_menu[2],
                "all_links": settings.browser_menu[3],
            })),
            panel: Some(settings.panel.to_json()),
            capture_fetched: Some(settings.capture_fetched_files),
            ..ExtensionResponse::new("ok", "config")
        },
        "batch" => {
            let urls: Vec<String> = req.urls.iter().map(|u| u.trim().to_string()).filter(|u| u.starts_with("http://") || u.starts_with("https://") || u.starts_with("ftp://")).collect();
            if urls.is_empty() {
                return ExtensionResponse::new("ignored", "no links");
            }
            let msg = IpcMessage { batch_urls: urls, from_browser: true, ..Default::default() };
            if deliver_or_launch(&msg) {
                ExtensionResponse::new("ok", "Links handed to Klippo")
            } else {
                ExtensionResponse::new("error", "Klippo could not be started")
            }
        }
        "download" => {
            let youtube = req.youtube.filter(|y| !y.sabr_url.is_empty() && !y.template.is_empty());
            let mut url = req.url.trim().to_string();
            if let Some(ref y) = youtube {
                if url.is_empty() {
                    url = y.watch_url();
                }
            }
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return ExtensionResponse::new("ignored", "unsupported address");
            }
            if !req.force && !settings.should_capture(&url, req.suggested_filename.as_deref(), req.mime.as_deref()) {
                return ExtensionResponse::new("ignored", "file type or site is not captured");
            }
            let msg = IpcMessage {
                url,
                filename: req.suggested_filename.filter(|f| !f.trim().is_empty()),
                referer: req.referer.filter(|r| !r.is_empty()),
                cookies: req.cookies.filter(|c| !c.is_empty()),
                user_agent: req.user_agent,
                from_browser: true,
                hls: req.media_type.as_deref() == Some("hls"),
                youtube,
                audio_url: req.audio_url.filter(|a| a.starts_with("http://") || a.starts_with("https://")),
                browser_proxy: req.browser_proxy.filter(|p| ["http://", "https://", "socks5://", "socks5h://", "socks4://"].iter().any(|s| p.starts_with(s))),
                post_data: req.post_data.filter(|d| !d.is_empty()),
                post_type: req.post_type,
                ..Default::default()
            };
            if deliver_or_launch(&msg) {
                ExtensionResponse::new("ok", "Download handed to Klippo")
            } else {
                ExtensionResponse::new("error", "Klippo could not be started")
            }
        }
        "adopt" => {
            // The browser saved the file itself: Klippo cannot fetch a blob: or data: address.
            let Some(path) = req.path.filter(|p| std::path::Path::new(p).is_file()) else {
                return ExtensionResponse::new("error", "the browser's file is not there");
            };
            let url = req.url.trim().to_string();
            // A data: address names no site: the page's address counts for the exceptions.
            let site = if url.starts_with("data:") { req.referer.clone().unwrap_or_default() } else { url.clone() };
            if !req.force && !settings.should_capture(&site, req.suggested_filename.as_deref(), req.mime.as_deref()) {
                return ExtensionResponse::new("ignored", "file type or site is not captured");
            }
            let msg = IpcMessage {
                url,
                filename: req.suggested_filename.filter(|f| !f.trim().is_empty()),
                referer: req.referer.filter(|r| !r.is_empty()),
                from_browser: true,
                adopt_path: Some(path),
                ..Default::default()
            };
            if deliver_or_launch(&msg) {
                ExtensionResponse::new("ok", "File handed to Klippo")
            } else {
                ExtensionResponse::new("error", "Klippo could not be started")
            }
        }
        other => ExtensionResponse::new("error", format!("unknown action {}", other)),
    }
}

/// Keeps the browser's pipes out of processes we start (the GUI), so the
/// browser sees the pipes close when this host exits.
fn make_stdio_private() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = GetStdHandle(which);
            if !handle.is_null() {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

pub fn run_native_host() -> io::Result<()> {
    make_stdio_private();
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();

    while let Some(buffer) = read_message(&mut stdin)? {
        let resp = match serde_json::from_slice::<ExtensionRequest>(&buffer) {
            Ok(req) => handle_request(req),
            Err(e) => ExtensionResponse::new("error", format!("bad request: {}", e)),
        };
        write_message(&mut stdout, &resp)?;
    }
    Ok(())
}

/// Which browser families to register the host for.
#[derive(Debug, Clone, Copy)]
pub struct BrowserTargets {
    /// Chrome, and Chromium-based browsers that read Chrome's registry key (Opera, Vivaldi).
    pub chromium: bool,
    pub edge: bool,
    pub brave: bool,
    pub firefox: bool,
}

impl BrowserTargets {
    pub fn all() -> Self {
        Self { chromium: true, edge: true, brave: true, firefox: true }
    }
}

fn host_dir() -> PathBuf {
    app_data_dir().join("native-host")
}

fn reg(args: &[&str]) -> bool {
    let mut cmd = Command::new("reg");
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

/// Writes the host manifests and registry keys. Returns a short report.
pub fn register_native_host_for(extension_ids: &[String], targets: BrowserTargets) -> io::Result<String> {
    let current_exe = std::env::current_exe()?;
    let dir = host_dir();
    std::fs::create_dir_all(&dir)?;

    let mut ids: Vec<String> = extension_ids
        .iter()
        .map(|s| s.trim().trim_start_matches("chrome-extension://").trim_end_matches('/').to_string())
        .filter(|s| s.len() == 32 && s.chars().all(|c| ('a'..='p').contains(&c)))
        .collect();
    if !ids.iter().any(|i| i == BUNDLED_EXTENSION_ID) {
        ids.push(BUNDLED_EXTENSION_ID.to_string());
    }
    let origins: Vec<String> = ids.iter().map(|id| format!("chrome-extension://{}/", id)).collect();
    let firefox_ids: Vec<&str> = vec![FIREFOX_EXTENSION_ID];

    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for name in host_names() {
        let chrome_manifest = dir.join(format!("{}.json", name));
        let manifest = serde_json::json!({
            "name": name,
            "description": "Klippo download manager integration",
            "path": current_exe.to_string_lossy(),
            "type": "stdio",
            "allowed_origins": origins,
        });
        std::fs::write(&chrome_manifest, serde_json::to_string_pretty(&manifest)?)?;

        let moz_manifest = dir.join(format!("{}.moz.json", name));
        let manifest = serde_json::json!({
            "name": name,
            "description": "Klippo download manager integration",
            "path": current_exe.to_string_lossy(),
            "type": "stdio",
            "allowed_extensions": firefox_ids,
        });
        std::fs::write(&moz_manifest, serde_json::to_string_pretty(&manifest)?)?;

        let mut keys: Vec<(&str, &PathBuf, &str)> = Vec::new();
        if targets.chromium {
            keys.push((BROWSER_KEYS[0], &chrome_manifest, "Chrome"));
            keys.push((BROWSER_KEYS[1], &chrome_manifest, "Chromium"));
        }
        if targets.edge {
            keys.push((BROWSER_KEYS[2], &chrome_manifest, "Edge"));
        }
        if targets.brave {
            keys.push((BROWSER_KEYS[3], &chrome_manifest, "Brave"));
        }
        if targets.firefox {
            keys.push((BROWSER_KEYS[4], &moz_manifest, "Firefox"));
        }
        for (key, manifest, browser) in keys {
            let full = format!("HKCU\\{}\\{}", key, name);
            let value = manifest.to_string_lossy().to_string();
            let done = reg(&["add", &full, "/ve", "/t", "REG_SZ", "/d", &value, "/f"]);
            // The report names each browser once, for the new name.
            if name == HOST_NAME {
                if done {
                    ok.push(browser);
                } else {
                    failed.push(browser);
                }
            }
        }
    }

    let mut report = format!("Browser integration registered for: {}", if ok.is_empty() { "none".to_string() } else { ok.join(", ") });
    if !failed.is_empty() {
        report.push_str(&format!(" (failed: {})", failed.join(", ")));
    }
    Ok(report)
}

/// True when the browser host manifest points at this program.
pub fn is_registered() -> bool {
    let manifest = host_dir().join(format!("{}.json", HOST_NAME));
    let Ok(text) = std::fs::read_to_string(manifest) else { return false };
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["path"].as_str().map(|p| p.eq_ignore_ascii_case(&exe)))
        .unwrap_or(false)
}

/// CLI `install-host`: every browser, with the IDs saved in the settings.
pub fn register_native_host() -> io::Result<()> {
    let settings = load_settings();
    let report = register_native_host_for(&settings.extension_ids, BrowserTargets::all())?;
    println!("{}", report);
    println!("Manifests: {}", host_dir().display());
    if let Some(dir) = extension_dir() {
        println!("Load the extension from: {}", dir.join("chromium").display());
    }
    Ok(())
}

pub fn unregister_native_host() -> io::Result<()> {
    for key in BROWSER_KEYS {
        for name in host_names() {
            let full = format!("HKCU\\{}\\{}", key, name);
            if reg(&["delete", &full, "/f"]) {
                println!("Removed {}", full);
            }
        }
    }
    let _ = std::fs::remove_dir_all(host_dir());
    Ok(())
}

/// The `extension` folder shipped next to the program (or in the source tree).
pub fn extension_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .take(4)
        .map(|dir| dir.join("extension"))
        .find(|dir| dir.join("chromium").join("manifest.json").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_browser_launch_arguments() {
        let chrome = vec!["klippo.exe".to_string(), format!("chrome-extension://{}/", BUNDLED_EXTENSION_ID), "--parent-window=0".to_string()];
        assert!(is_native_host_invocation(&chrome));
        let firefox = vec!["klippo.exe".to_string(), "C:\\x\\org.klippo.downloader.moz.json".to_string(), FIREFOX_EXTENSION_ID.to_string()];
        assert!(is_native_host_invocation(&firefox));
        let normal = vec!["klippo.exe".to_string(), "/d".to_string(), "https://a.com/x.zip".to_string()];
        assert!(!is_native_host_invocation(&normal));
    }

    #[test]
    fn video_button_request_fields() {
        let req: ExtensionRequest = serde_json::from_str(
            r#"{"action":"download","force":true,"url":"https://cdn.example/v/master","filename":"Talk","media_type":"hls","file_size":null}"#,
        )
        .unwrap();
        assert!(req.force);
        assert_eq!(req.media_type.as_deref(), Some("hls"));
        assert_eq!(req.suggested_filename.as_deref(), Some("Talk"));
    }

    #[test]
    fn message_framing_round_trip() {
        let resp = ExtensionResponse::new("ok", "hi");
        let mut buf = Vec::new();
        write_message(&mut buf, &resp).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let body = read_message(&mut cursor).unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(read_message(&mut cursor).unwrap().is_none());
    }
}
