use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::core::engine::{DownloadEngine, EngineControl, EngineStatus};
use crate::core::hls::{HLSGrabber, HlsControl, HlsProgress};
use crate::core::youtube::{YoutubeGrabber, YoutubeSpec};
use crate::core::http::RequestOptions;
use crate::core::rate_limiter::TokenBucketLimiter;
use crate::core::segment::{Segment, SegmentStatus};
use super::state::{DownloadKind, DownloadStatus, HashAlgo, SegmentSnapshot};

/// Everything the supervisor needs to run one download.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub id: u64,
    pub kind: DownloadKind,
    pub url: String,
    pub save_path: PathBuf,
    /// The name may be replaced by the server's Content-Disposition name.
    pub filename_auto: bool,
    pub connections: usize,
    /// Per-file limit in bytes per second (0 = none).
    pub speed_limit_bps: u64,
    pub request: RequestOptions,
    /// Discard saved progress (Redownload).
    pub fresh: bool,
    pub max_retries: u32,
    pub ignore_time_changes: bool,
    /// Give the finished file the server's Last-Modified time.
    pub set_file_time: bool,
    /// Stream session for `DownloadKind::Youtube`.
    pub youtube: Option<YoutubeSpec>,
    /// Separate audio stream of an HLS or DASH-style video.
    pub audio_url: Option<String>,
    /// How `DownloadKind::Ftp` connects.
    pub ftp: crate::core::ftp::FtpOptions,
}

/// Server facts for the "Enter new address" dialog.
#[derive(Debug, Clone)]
pub struct ProbeInfo {
    pub url: String,
    pub filename: String,
    pub total_size: Option<u64>,
    pub resumable: bool,
    pub content_type: Option<String>,
}

#[derive(Debug)]
pub enum GuiEvent {
    /// The probe resolved the real name, size and range support.
    Info {
        id: u64,
        filename: String,
        save_path: PathBuf,
        total: Option<u64>,
        resumable: bool,
    },
    Progress {
        id: u64,
        downloaded: u64,
        total: Option<u64>,
        speed_bps: f64,
        eta_seconds: Option<u64>,
        segments: Vec<SegmentSnapshot>,
    },
    StatusChanged {
        id: u64,
        status: DownloadStatus,
        error: Option<String>,
    },
    Completed {
        id: u64,
        save_path: PathBuf,
    },
    ProbeResult {
        probe_id: u64,
        result: Result<Box<ProbeInfo>, String>,
    },
    HashResult {
        id: u64,
        result: Result<String, String>,
    },
}

enum Command {
    Start(Box<DownloadRequest>),
    Stop { id: u64 },
    /// Stops the download, then deletes the given files and folders.
    Remove { id: u64, delete_paths: Vec<PathBuf> },
    SetSpeedLimit { id: u64, bps: u64 },
    SetGlobalLimit { bps: u64 },
    Probe { probe_id: u64, url: String, dir: PathBuf, request: Box<RequestOptions> },
    Hash { id: u64, path: PathBuf, algo: HashAlgo },
    Shutdown { done: Sender<()> },
}

enum Control {
    Http(EngineControl),
    Hls(HlsControl),
}

impl Control {
    fn stop(&self) {
        match self {
            Control::Http(c) => c.stop(),
            Control::Hls(c) => c.stop(),
        }
    }
}

struct Task {
    control: Control,
    handle: JoinHandle<()>,
}

/// Sends events to the GUI and wakes it up.
#[derive(Clone)]
struct EventSink {
    tx: Sender<GuiEvent>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl EventSink {
    fn send(&self, event: GuiEvent) {
        let _ = self.tx.send(event);
        if let Some(ctx) = self.ctx.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }
}

pub struct DownloadManagerBridge {
    cmd_tx: UnboundedSender<Command>,
    event_rx: Receiver<GuiEvent>,
    repaint_ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl Default for DownloadManagerBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadManagerBridge {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = unbounded_channel::<Command>();
        let (event_tx, event_rx) = channel::<GuiEvent>();
        let repaint_ctx = Arc::new(Mutex::new(None));
        let sink = EventSink { tx: event_tx, ctx: repaint_ctx.clone() };

        // Background supervisor thread with a dedicated Tokio runtime
        thread::Builder::new()
            .name("klippo-download-coordinator".to_string())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build()
                    .expect("Failed to initialize background Tokio runtime");
                rt.block_on(supervisor(cmd_rx, sink));
            })
            .expect("Failed to spawn Klippo download supervisor thread");

        Self { cmd_tx, event_rx, repaint_ctx }
    }

    /// Lets background events wake the GUI immediately.
    pub fn set_repaint_context(&self, ctx: &egui::Context) {
        *self.repaint_ctx.lock().unwrap() = Some(ctx.clone());
    }

    pub fn start_download(&self, request: DownloadRequest) {
        let _ = self.cmd_tx.send(Command::Start(Box::new(request)));
    }

    pub fn pause_download(&self, id: u64) {
        let _ = self.cmd_tx.send(Command::Stop { id });
    }

    /// Stops the download and deletes `delete_paths` once its files are closed.
    pub fn remove_download(&self, id: u64, delete_paths: Vec<PathBuf>) {
        let _ = self.cmd_tx.send(Command::Remove { id, delete_paths });
    }

    pub fn set_speed_limit(&self, id: u64, bps: u64) {
        let _ = self.cmd_tx.send(Command::SetSpeedLimit { id, bps });
    }

    pub fn set_global_speed_limit(&self, bps: u64) {
        let _ = self.cmd_tx.send(Command::SetGlobalLimit { bps });
    }

    pub fn probe(&self, probe_id: u64, url: String, dir: PathBuf, request: RequestOptions) {
        let _ = self.cmd_tx.send(Command::Probe { probe_id, url, dir, request: Box::new(request) });
    }

    pub fn compute_hash(&self, id: u64, path: PathBuf, algo: HashAlgo) {
        let _ = self.cmd_tx.send(Command::Hash { id, path, algo });
    }

    /// Stops every download and waits (bounded) until resume data is saved.
    pub fn shutdown(&self, timeout: Duration) {
        let (done_tx, done_rx) = channel();
        if self.cmd_tx.send(Command::Shutdown { done: done_tx }).is_ok() {
            let _ = done_rx.recv_timeout(timeout);
        }
    }

    pub fn try_recv_event(&self) -> Option<GuiEvent> {
        self.event_rx.try_recv().ok()
    }
}

async fn supervisor(mut cmd_rx: UnboundedReceiver<Command>, sink: EventSink) {
    let global_limiter = TokenBucketLimiter::new(0);
    let mut tasks: HashMap<u64, Task> = HashMap::new();

    while let Some(cmd) = cmd_rx.recv().await {
        tasks.retain(|_, t| !t.handle.is_finished());
        match cmd {
            Command::Start(request) => {
                let id = request.id;
                // A previous run of the same item must release the file first.
                let previous = tasks.remove(&id);
                if let Some(ref p) = previous {
                    p.control.stop();
                }
                let (control, job) = build_job(*request, global_limiter.clone(), sink.clone());
                let handle = tokio::spawn(async move {
                    if let Some(p) = previous {
                        let _ = p.handle.await;
                    }
                    job.await;
                });
                tasks.insert(id, Task { control, handle });
            }
            Command::Stop { id } => {
                if let Some(task) = tasks.get(&id) {
                    task.control.stop();
                }
            }
            Command::Remove { id, delete_paths } => {
                let task = tasks.remove(&id);
                if let Some(ref t) = task {
                    t.control.stop();
                }
                tokio::spawn(async move {
                    if let Some(t) = task {
                        let _ = t.handle.await;
                    }
                    for path in delete_paths {
                        if path.is_dir() {
                            let _ = std::fs::remove_dir_all(&path);
                        } else if path.exists() {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                });
            }
            Command::SetSpeedLimit { id, bps } => {
                if let Some(Task { control: Control::Http(c), .. }) = tasks.get(&id) {
                    c.set_speed_limit(bps);
                }
            }
            Command::SetGlobalLimit { bps } => global_limiter.set_limit(bps),
            Command::Probe { probe_id, url, dir, request } => {
                let sink = sink.clone();
                tokio::spawn(async move {
                    let result = probe(&url, &dir, *request).await.map(Box::new);
                    sink.send(GuiEvent::ProbeResult { probe_id, result });
                });
            }
            Command::Hash { id, path, algo } => {
                let sink = sink.clone();
                tokio::task::spawn_blocking(move || {
                    let result = hash_file(&path, algo).map_err(|e| e.to_string());
                    sink.send(GuiEvent::HashResult { id, result });
                });
            }
            Command::Shutdown { done } => {
                for task in tasks.values() {
                    task.control.stop();
                }
                let handles: Vec<JoinHandle<()>> = tasks.drain().map(|(_, t)| t.handle).collect();
                tokio::spawn(async move {
                    futures_util::future::join_all(handles).await;
                    let _ = done.send(());
                });
            }
        }
    }
}

async fn probe(url: &str, dir: &Path, request: RequestOptions) -> Result<ProbeInfo, String> {
    let mut engine = DownloadEngine::new(url, Some(dir), None, 1, 0);
    engine.set_request_options(request);
    match tokio::time::timeout(Duration::from_secs(25), engine.probe_server()).await {
        Err(_) => Err("Connection timed out".to_string()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(())) => Ok(ProbeInfo {
            url: url.to_string(),
            filename: engine.suggested_filename().to_string(),
            total_size: engine.total_size,
            resumable: engine.resumable,
            content_type: engine.content_type.clone(),
        }),
    }
}

/// Hashes a file with the chosen algorithm and returns a lowercase hex string.
/// The "Check Sum" dialog offers the same four: CRC32, MD5, SHA-1 and SHA-256.
fn hash_file(path: &Path, algo: HashAlgo) -> std::io::Result<String> {
    // One streaming pass; each RustCrypto hasher shares the `Digest` trait.
    fn digest<D: sha2::Digest>(mut file: std::fs::File) -> std::io::Result<String> {
        use std::io::Read;
        let mut hasher = D::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let out = hasher.finalize();
        Ok(out.iter().map(|b| format!("{:02x}", b)).collect::<String>())
    }

    let file = std::fs::File::open(path)?;
    match algo {
        HashAlgo::Crc32 => {
            use std::io::Read;
            let mut file = file;
            let mut hasher = crc32fast::Hasher::new();
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            Ok(format!("{:08x}", hasher.finalize()))
        }
        HashAlgo::Md5 => digest::<md5::Md5>(file),
        HashAlgo::Sha1 => digest::<sha1::Sha1>(file),
        HashAlgo::Sha256 => digest::<sha2::Sha256>(file),
    }
}

#[cfg(test)]
mod hash_tests {
    use super::hash_file;
    use crate::gui::state::HashAlgo;

    #[test]
    fn hashes_match_known_values_for_abc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(hash_file(&path, HashAlgo::Crc32).unwrap(), "352441c2");
        assert_eq!(hash_file(&path, HashAlgo::Md5).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hash_file(&path, HashAlgo::Sha1).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hash_file(&path, HashAlgo::Sha256).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

fn build_job(req: DownloadRequest, global_limiter: TokenBucketLimiter, sink: EventSink) -> (Control, Job) {
    match req.kind {
        DownloadKind::Http => {
            let dir = req.save_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
            let fname = req
                .save_path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| "download".to_string());
            let mut engine = DownloadEngine::new(&req.url, Some(&dir), Some(&fname), req.connections, req.speed_limit_bps);
            engine.set_filename_explicit(!req.filename_auto);
            engine.set_request_options(req.request.clone());
            engine.set_global_limiter(Some(global_limiter));
            engine.set_fresh_start(req.fresh);
            engine.set_max_retries(req.max_retries);
            engine.set_ignore_time_changes(req.ignore_time_changes);
            engine.set_file_time_from_server(req.set_file_time);
            let control = engine.control();
            (Control::Http(control), Box::pin(run_http(req.id, engine, sink)))
        }
        DownloadKind::Hls => {
            if req.fresh {
                let _ = std::fs::remove_dir_all(HLSGrabber::temp_dir_for(&req.save_path));
            }
            let mut grabber = HLSGrabber::new(&req.url, &req.save_path, req.connections);
            grabber.set_request_options(req.request.clone());
            grabber.set_audio_url(req.audio_url.clone());
            let control = grabber.control();
            let progress = grabber.progress();
            let job = async move { grabber.run().await };
            (Control::Hls(control), Box::pin(run_stream(req.id, progress, job, sink)))
        }
        DownloadKind::Youtube => {
            if req.fresh {
                let _ = std::fs::remove_dir_all(YoutubeGrabber::parts_dir_for(&req.save_path));
            }
            let mut grabber = YoutubeGrabber::new(req.youtube.clone().unwrap_or_default(), &req.save_path);
            grabber.set_request_options(req.request.clone());
            let control = grabber.control();
            let progress = grabber.progress();
            let job = async move { grabber.run().await };
            (Control::Hls(control), Box::pin(run_stream(req.id, progress, job, sink)))
        }
        DownloadKind::Ftp => {
            if req.fresh {
                let _ = std::fs::remove_file(&req.save_path);
            }
            let mut grabber = crate::core::ftp::FtpGrabber::new(&req.url, &req.save_path);
            grabber.set_options(req.ftp.clone());
            let control = grabber.control();
            let progress = grabber.progress();
            let job = async move { grabber.run().await };
            (Control::Hls(control), Box::pin(run_stream(req.id, progress, job, sink)))
        }
    }
}

fn snapshot(segs: &[Segment]) -> Vec<SegmentSnapshot> {
    segs.iter()
        .map(|s| SegmentSnapshot {
            segment_id: s.segment_id,
            start: s.start,
            end: s.end,
            current_offset: s.current_offset,
            is_active: s.status == SegmentStatus::Downloading,
            is_completed: s.status == SegmentStatus::Completed,
            speed_bps: s.speed_bps,
        })
        .collect()
}

fn eta(total: Option<u64>, downloaded: u64, speed_bps: f64) -> Option<u64> {
    match total {
        Some(t) if speed_bps > 1.0 => Some((t.saturating_sub(downloaded) as f64 / speed_bps) as u64),
        _ => None,
    }
}

async fn run_http(id: u64, mut engine: DownloadEngine, sink: EventSink) {
    sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Connecting, error: None });

    let segments = engine.segments.clone();
    let speed = engine.current_speed_bps.clone();
    let status = engine.status.clone();
    let info = engine.info.clone();
    let mon_sink = sink.clone();

    // Live telemetry for the table and the progress window.
    let monitor = tokio::spawn(async move {
        let mut info_sent = false;
        let mut downloading_sent = false;
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let current = info.lock().unwrap().clone();
            if !current.probed {
                continue;
            }
            if !info_sent {
                info_sent = true;
                mon_sink.send(GuiEvent::Info {
                    id,
                    filename: current.filename.clone(),
                    save_path: current.output_path.clone(),
                    total: current.total_size,
                    resumable: current.resumable,
                });
            }
            if !downloading_sent && *status.read().await == EngineStatus::Downloading {
                downloading_sent = true;
                mon_sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Downloading, error: None });
            }
            let (downloaded, snaps) = {
                let segs = segments.read().await;
                (segs.iter().map(|s| s.downloaded_bytes()).sum::<u64>(), snapshot(&segs))
            };
            let speed_bps = *speed.lock().await;
            mon_sink.send(GuiEvent::Progress {
                id,
                downloaded,
                total: current.total_size,
                speed_bps,
                eta_seconds: eta(current.total_size, downloaded, speed_bps),
                segments: snaps,
            });
        }
    });

    let result = engine.run().await;
    monitor.abort();
    let _ = monitor.await;

    let (downloaded, snaps) = {
        let segs = engine.segments.read().await;
        (segs.iter().map(|s| s.downloaded_bytes()).sum::<u64>(), snapshot(&segs))
    };
    let info = engine.info.lock().unwrap().clone();
    if info.probed {
        sink.send(GuiEvent::Info {
            id,
            filename: info.filename.clone(),
            save_path: info.output_path.clone(),
            total: engine.total_size,
            resumable: info.resumable,
        });
    }
    sink.send(GuiEvent::Progress {
        id,
        downloaded,
        total: engine.total_size,
        speed_bps: 0.0,
        eta_seconds: None,
        segments: snaps,
    });

    match *engine.status.read().await {
        EngineStatus::Completed => {
            sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Completed, error: None });
            sink.send(GuiEvent::Completed { id, save_path: engine.output_path().to_path_buf() });
        }
        EngineStatus::Paused => {
            sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Paused, error: None });
        }
        _ => {
            let error = result.err().map(|e| e.to_string()).unwrap_or_else(|| "Download failed".to_string());
            sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Failed, error: Some(error) });
        }
    }
}

/// Runs a stream download (HLS or YouTube) and reports its progress.
async fn run_stream<F>(id: u64, progress: Arc<HlsProgress>, job: F, sink: EventSink)
where
    F: Future<Output = Result<PathBuf, Box<dyn std::error::Error + Send + Sync>>>,
{
    sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Connecting, error: None });

    let mon_progress = progress.clone();
    let mon_sink = sink.clone();
    let monitor = tokio::spawn(async move {
        let mut last_bytes = 0u64;
        let mut last_time = Instant::now();
        let mut speed_bps = 0.0;
        let mut downloading_sent = false;
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let total_parts = mon_progress.total_parts.load(Ordering::Relaxed);
            if total_parts == 0 {
                continue;
            }
            if !downloading_sent {
                downloading_sent = true;
                mon_sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Downloading, error: None });
            }
            let bytes = mon_progress.bytes.load(Ordering::Relaxed);
            let dt = last_time.elapsed().as_secs_f64();
            if dt >= 1.0 {
                speed_bps = bytes.saturating_sub(last_bytes) as f64 / dt;
                last_bytes = bytes;
                last_time = Instant::now();
            }
            let total = mon_progress.estimated_total();
            mon_sink.send(GuiEvent::Progress {
                id,
                downloaded: bytes,
                total,
                speed_bps,
                eta_seconds: eta(total, bytes, speed_bps),
                segments: Vec::new(),
            });
        }
    });

    let result = job.await;
    monitor.abort();
    let _ = monitor.await;

    let bytes = progress.bytes.load(Ordering::Relaxed);
    match result {
        Ok(output) => {
            let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(bytes);
            sink.send(GuiEvent::Progress { id, downloaded: size, total: Some(size), speed_bps: 0.0, eta_seconds: None, segments: Vec::new() });
            sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Completed, error: None });
            sink.send(GuiEvent::Completed { id, save_path: output });
        }
        Err(e) if e.is::<crate::core::DownloadStopped>() => {
            sink.send(GuiEvent::Progress { id, downloaded: bytes, total: progress.estimated_total(), speed_bps: 0.0, eta_seconds: None, segments: Vec::new() });
            sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Paused, error: None });
        }
        Err(e) => {
            sink.send(GuiEvent::StatusChanged { id, status: DownloadStatus::Failed, error: Some(e.to_string()) });
        }
    }
}
