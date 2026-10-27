//! Single-instance support: later launches (command line, browser native host)
//! hand their request to the running window over a local TCP socket.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};

use super::state::app_data_dir;

/// A request from another Klippo process or the browser.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct IpcMessage {
    /// Download address; empty means "show the window".
    pub url: String,
    pub filename: Option<String>,
    pub dir: Option<String>,
    pub referer: Option<String>,
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    /// `/n`: add and start without asking.
    pub silent: bool,
    /// `/a`: add to the queue without starting.
    pub queue_only: bool,
    /// `/s`: start the main queue.
    pub start_queue: bool,
    /// `/q`: exit after the downloads finish.
    pub quit_when_done: bool,
    /// The request came from the browser extension.
    pub from_browser: bool,
    /// The link was copied to the clipboard.
    pub from_clipboard: bool,
    /// The address is an HLS playlist even if its name does not end in `.m3u8`.
    pub hls: bool,
    /// YouTube stream session recorded by the browser extension.
    pub youtube: Option<crate::core::youtube::YoutubeSpec>,
    /// Separate audio stream that belongs to the video (joined into one file).
    pub audio_url: Option<String>,
    /// "Download all links": the links of a page, offered in the batch dialog.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batch_urls: Vec<String>,
    /// The browser's proxy for this download (use it after download errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_proxy: Option<String>,
    /// A download that a form started: the POST body (base64) and its Content-Type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_type: Option<String>,
    /// A file that the browser saved itself (a blob: or data: download, which
    /// only the page can make): Klippo moves it into its folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopt_path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct InstanceFile {
    port: u16,
    token: String,
    pid: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    token: String,
    message: IpcMessage,
}

fn instance_file() -> PathBuf {
    app_data_dir().join("instance.json")
}

fn random_token() -> String {
    use sha2::{Digest, Sha256};
    let seed = format!(
        "{:?}{}{:?}",
        std::time::SystemTime::now(),
        std::process::id(),
        thread::current().id()
    );
    format!("{:x}", Sha256::digest(seed.as_bytes()))
}

/// Delivers `message` to a running instance. Returns false when none answers.
pub fn send_to_running_instance(message: &IpcMessage) -> bool {
    let Ok(data) = std::fs::read_to_string(instance_file()) else { return false };
    let Ok(info) = serde_json::from_str::<InstanceFile>(&data) else { return false };
    let addr = SocketAddr::from(([127, 0, 0, 1], info.port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(700)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let envelope = Envelope { token: info.token, message: message.clone() };
    let Ok(mut line) = serde_json::to_string(&envelope) else { return false };
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Starts the GUI if needed and delivers the message (used by the native host).
pub fn deliver_or_launch(message: &IpcMessage) -> bool {
    if send_to_running_instance(message) {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    if spawn_detached(&exe).is_err() {
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(250));
        if send_to_running_instance(message) {
            return true;
        }
    }
    false
}

/// Starts the GUI without the caller's stdio pipes and, when allowed, outside
/// the browser's job object (so closing the browser does not close Klippo).
fn spawn_detached(exe: &std::path::Path) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(exe);
    // In the notification area: the browser's download shows only its dialog.
    cmd.arg("--startup");
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
        // The job may forbid breakaway; start normally then.
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map(|_| ())
}

pub struct IpcServer {
    pub rx: Receiver<IpcMessage>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl IpcServer {
    /// Listens on a random loopback port and records it for later launches.
    pub fn start() -> Option<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
        let port = listener.local_addr().ok()?.port();
        let token = random_token();
        let info = InstanceFile { port, token: token.clone(), pid: std::process::id() };
        let _ = std::fs::create_dir_all(app_data_dir());
        std::fs::write(instance_file(), serde_json::to_string(&info).ok()?).ok()?;

        let (tx, rx) = channel();
        let ctx: Arc<Mutex<Option<egui::Context>>> = Arc::new(Mutex::new(None));
        let ctx_thread = ctx.clone();
        thread::Builder::new()
            .name("klippo-ipc".to_string())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                    let mut reader = BufReader::new(&stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        continue;
                    }
                    let Ok(envelope) = serde_json::from_str::<Envelope>(&line) else { continue };
                    if envelope.token != token {
                        continue;
                    }
                    let _ = (&stream).write_all(b"ok\n");
                    if tx.send(envelope.message).is_err() {
                        break;
                    }
                    if let Some(ctx) = ctx_thread.lock().unwrap().as_ref() {
                        ctx.request_repaint();
                    }
                }
            })
            .ok()?;

        Some(Self { rx, ctx })
    }

    pub fn set_repaint_context(&self, ctx: &egui::Context) {
        *self.ctx.lock().unwrap() = Some(ctx.clone());
    }

    /// Removes the instance record if it still belongs to this process.
    pub fn cleanup(&self) {
        if let Ok(data) = std::fs::read_to_string(instance_file()) {
            if let Ok(info) = serde_json::from_str::<InstanceFile>(&data) {
                if info.pid == std::process::id() {
                    let _ = std::fs::remove_file(instance_file());
                }
            }
        }
    }
}
