use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
    LAST_MODIFIED, RANGE,
};
use reqwest::{Client, StatusCode};
use tokio::sync::{watch, Mutex, RwLock};
use tokio::task::JoinSet;

use super::http::{self, ProxyConfig, RequestOptions};
use super::rate_limiter::TokenBucketLimiter;
use super::resume::ResumeManager;
use super::segment::{Segment, SegmentStatus};
use super::storage::DirectStorage;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineStatus {
    Idle,
    Connecting,
    Downloading,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// Returned by `DownloadEngine::run` when the download was stopped on request.
#[derive(Debug)]
pub struct DownloadStopped;

impl std::fmt::Display for DownloadStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Download stopped")
    }
}

impl std::error::Error for DownloadStopped {}

/// Server facts learned by the probe request.
#[derive(Debug, Clone, Default)]
pub struct EngineInfo {
    pub probed: bool,
    pub total_size: Option<u64>,
    pub resumable: bool,
    pub filename: String,
    pub output_path: PathBuf,
    pub effective_url: String,
    pub content_type: Option<String>,
}

/// Cloneable remote control for a running engine.
#[derive(Clone)]
pub struct EngineControl {
    cancel_tx: Arc<watch::Sender<bool>>,
    rate_limiter: TokenBucketLimiter,
}

impl EngineControl {
    /// Stops the download; progress is kept so it can resume later.
    pub fn stop(&self) {
        self.cancel_tx.send_replace(true);
    }

    pub fn is_stopped(&self) -> bool {
        *self.cancel_tx.borrow()
    }

    /// Changes this download's speed limit while it runs (0 = unlimited).
    pub fn set_speed_limit(&self, bytes_per_sec: u64) {
        self.rate_limiter.set_limit(bytes_per_sec);
    }
}

/// Smallest initial part; files below twice this size use one connection.
const MIN_INITIAL_PART: u64 = 256 * 1024;

pub struct DownloadEngine {
    url: String,
    effective_url: String,
    output_path: PathBuf,
    suggested_filename: String,
    filename_explicit: bool,
    connections: usize,
    request: RequestOptions,
    enable_dynamic_resegment: bool,
    min_split_size: u64,
    max_retries: u32,
    fresh_start: bool,
    ignore_time_changes: bool,
    set_file_time: bool,

    pub total_size: Option<u64>,
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,

    pub segments: Arc<RwLock<Vec<Segment>>>,
    next_segment_id: usize,

    storage: Option<DirectStorage>,
    resume_manager: ResumeManager,
    rate_limiter: TokenBucketLimiter,
    global_limiter: Option<TokenBucketLimiter>,

    cancel_tx: Arc<watch::Sender<bool>>,
    cancel_rx: watch::Receiver<bool>,

    pub status: Arc<RwLock<EngineStatus>>,
    pub current_speed_bps: Arc<Mutex<f64>>,
    pub error_message: Arc<Mutex<Option<String>>>,
    pub info: Arc<std::sync::Mutex<EngineInfo>>,
}

impl DownloadEngine {
    pub const DEFAULT_MIN_SPLIT: u64 = 1024 * 1024; // 1 MB
    pub const DEFAULT_MAX_RETRIES: u32 = 10;

    pub fn new(
        url: &str,
        output_dir: Option<&Path>,
        filename: Option<&str>,
        connections: usize,
        speed_limit_bps: u64,
    ) -> Self {
        let (cancel_tx, cancel_rx) = watch::channel(false);

        let explicit = filename.map(http::sanitize_filename).filter(|f| !f.is_empty());
        let filename_explicit = explicit.is_some();
        let fname = explicit
            .or_else(|| http::filename_from_url(url))
            .unwrap_or_else(|| "download".to_string());

        let out_path = match output_dir {
            Some(dir) => dir.join(&fname),
            None => PathBuf::from(&fname),
        };

        Self {
            url: url.to_string(),
            effective_url: url.to_string(),
            output_path: out_path.clone(),
            suggested_filename: fname,
            filename_explicit,
            connections: connections.clamp(1, 32),
            request: RequestOptions::default(),
            enable_dynamic_resegment: true,
            min_split_size: Self::DEFAULT_MIN_SPLIT,
            max_retries: Self::DEFAULT_MAX_RETRIES,
            fresh_start: false,
            ignore_time_changes: true,
            set_file_time: false,

            total_size: None,
            resumable: false,
            etag: None,
            last_modified: None,
            content_type: None,

            segments: Arc::new(RwLock::new(Vec::new())),
            next_segment_id: 0,

            storage: None,
            resume_manager: ResumeManager::new(&out_path),
            rate_limiter: TokenBucketLimiter::new(speed_limit_bps),
            global_limiter: None,

            cancel_tx: Arc::new(cancel_tx),
            cancel_rx,

            status: Arc::new(RwLock::new(EngineStatus::Idle)),
            current_speed_bps: Arc::new(Mutex::new(0.0)),
            error_message: Arc::new(Mutex::new(None)),
            info: Arc::new(std::sync::Mutex::new(EngineInfo::default())),
        }
    }

    pub fn set_min_split_size(&mut self, size: u64) {
        self.min_split_size = size.max(1);
    }

    pub fn set_dynamic_resegment(&mut self, enabled: bool) {
        self.enable_dynamic_resegment = enabled;
    }

    pub fn set_max_retries(&mut self, retries: u32) {
        self.max_retries = retries;
    }

    /// Discards saved progress and existing file data before downloading.
    pub fn set_fresh_start(&mut self, fresh: bool) {
        self.fresh_start = fresh;
    }

    /// When false, a changed Last-Modified header restarts the download.
    pub fn set_ignore_time_changes(&mut self, ignore: bool) {
        self.ignore_time_changes = ignore;
    }

    /// Gives the finished file the server's Last-Modified time.
    pub fn set_file_time_from_server(&mut self, enabled: bool) {
        self.set_file_time = enabled;
    }

    /// When false, the server's Content-Disposition name may replace the given name.
    pub fn set_filename_explicit(&mut self, explicit: bool) {
        self.filename_explicit = explicit;
    }

    pub fn set_headers(&mut self, headers: HeaderMap) {
        self.request.headers = headers;
    }

    pub fn add_header(&mut self, name: &str, value: &str) -> Result<(), BoxError> {
        let name = HeaderName::from_bytes(name.as_bytes())?;
        let value = HeaderValue::from_str(value)?;
        self.request.headers.insert(name, value);
        Ok(())
    }

    pub fn set_user_agent(&mut self, agent: &str) {
        if !agent.trim().is_empty() {
            self.request.user_agent = agent.trim().to_string();
        }
    }

    pub fn set_basic_auth(&mut self, username: &str, password: &str) {
        self.request.basic_auth = Some((username.to_string(), password.to_string()));
    }

    pub fn set_proxy(&mut self, proxy: Option<ProxyConfig>) {
        self.request.proxy = proxy;
    }

    pub fn set_request_options(&mut self, options: RequestOptions) {
        self.request = options;
    }

    /// Shares one bandwidth budget across several engines (global speed limiter).
    pub fn set_global_limiter(&mut self, limiter: Option<TokenBucketLimiter>) {
        self.global_limiter = limiter;
    }

    pub fn control(&self) -> EngineControl {
        EngineControl {
            cancel_tx: self.cancel_tx.clone(),
            rate_limiter: self.rate_limiter.clone(),
        }
    }

    pub async fn downloaded_bytes(&self) -> u64 {
        let segs = self.segments.read().await;
        segs.iter().map(|s| s.downloaded_bytes()).sum()
    }

    pub async fn active_connections(&self) -> usize {
        let segs = self.segments.read().await;
        segs.iter().filter(|s| s.status == SegmentStatus::Downloading).count()
    }

    pub async fn progress_fraction(&self) -> f64 {
        if let Some(total) = self.total_size {
            if total > 0 {
                return (self.downloaded_bytes().await as f64 / total as f64).min(1.0);
            }
        }
        if *self.status.read().await == EngineStatus::Completed {
            1.0
        } else {
            0.0
        }
    }

    /// Probe remote HTTP server for size, range support, validators and file name.
    pub async fn probe_server(&mut self) -> Result<(), BoxError> {
        *self.status.write().await = EngineStatus::Connecting;

        let client = self.request.build_client()?;
        let mut cancel_rx = self.cancel_rx.clone();
        let mut attempt = 0u32;
        let resp = loop {
            let sent = self.request.download(&client, &self.url).header(RANGE, "bytes=0-0").send().await;
            // As for the parts of a download: refused or cut connections, timeouts
            // and busy servers (5xx, 408, 429) are tried again with a growing pause.
            let retry = match &sent {
                Ok(r) => matches!(classify_status(r.status()), WorkerOutcome::Retry(_)),
                Err(e) => e.is_connect() || e.is_timeout() || e.is_request(),
            };
            if !retry || attempt >= self.max_retries {
                break sent.map_err(|e| http::describe_error(&e))?;
            }
            attempt += 1;
            let pause = Duration::from_millis(500 * 2u64.pow(attempt.min(6))).min(Duration::from_secs(30));
            tokio::select! {
                _ = tokio::time::sleep(pause) => {}
                _ = wait_for_true(&mut cancel_rx) => return Err(Box::new(DownloadStopped)),
            }
        };

        let headers = resp.headers().clone();
        let header_str = |name| headers.get(name).and_then(|v: &HeaderValue| v.to_str().ok()).map(|s: &str| s.to_string());

        self.effective_url = resp.url().to_string();
        self.etag = header_str(ETAG);
        self.last_modified = header_str(LAST_MODIFIED);
        self.content_type = header_str(CONTENT_TYPE);

        let status_code = resp.status();
        if status_code == StatusCode::PARTIAL_CONTENT {
            let total = header_str(CONTENT_RANGE)
                .and_then(|cr| http::parse_content_range(&cr))
                .and_then(|(_, total)| total);
            self.total_size = total;
            // Segmenting needs a known length.
            self.resumable = total.is_some();
        } else if status_code.is_success() {
            // The server ignored the Range header, so it cannot resume.
            self.resumable = false;
            self.total_size = header_str(CONTENT_LENGTH).and_then(|cl| cl.trim().parse::<u64>().ok());
        } else if status_code == StatusCode::RANGE_NOT_SATISFIABLE {
            // `bytes=0-0` of an empty file.
            self.resumable = false;
            self.total_size = Some(
                header_str(CONTENT_RANGE)
                    .and_then(|cr| http::parse_content_range(&cr))
                    .and_then(|(_, total)| total)
                    .unwrap_or(0),
            );
        } else {
            let mut msg = format!(
                "Server returned HTTP {} {}",
                status_code.as_u16(),
                status_code.canonical_reason().unwrap_or("")
            )
            .trim()
            .to_string();
            // The realm tells the user which login the server wants.
            let challenge = if status_code == StatusCode::PROXY_AUTHENTICATION_REQUIRED {
                header_str(reqwest::header::PROXY_AUTHENTICATE)
            } else {
                header_str(reqwest::header::WWW_AUTHENTICATE)
            };
            if let Some(realm) = challenge.as_deref().and_then(auth_realm) {
                msg.push_str(&format!(" (realm \"{}\")", realm));
            }
            return Err(msg.into());
        }
        drop(resp);

        if !self.filename_explicit {
            self.resolve_filename(header_str(CONTENT_DISPOSITION));
        }

        self.storage = Some(DirectStorage::new(&self.output_path, self.total_size));
        self.resume_manager = ResumeManager::new(&self.output_path);

        let mut info = self.info.lock().unwrap();
        *info = EngineInfo {
            probed: true,
            total_size: self.total_size,
            resumable: self.resumable,
            filename: self.suggested_filename.clone(),
            output_path: self.output_path.clone(),
            effective_url: self.effective_url.clone(),
            content_type: self.content_type.clone(),
        };

        Ok(())
    }

    /// Picks the server's name: Content-Disposition, then the redirected URL,
    /// then a MIME-based extension for extension-less names.
    fn resolve_filename(&mut self, content_disposition: Option<String>) {
        let mut name = content_disposition
            .as_deref()
            .and_then(http::parse_content_disposition)
            .or_else(|| {
                if self.effective_url != self.url {
                    http::filename_from_url(&self.effective_url)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| self.suggested_filename.clone());

        if Path::new(&name).extension().is_none() {
            if let Some(ext) = self.content_type.as_deref().and_then(http::extension_for_mime) {
                name = format!("{}.{}", name, ext);
            }
        }

        if name != self.suggested_filename {
            let dir = self.output_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            // Never clobber an unrelated file that happens to share the server's name.
            let candidate = http::unique_path(&dir.join(&name), |p| {
                p.exists() && !ResumeManager::state_path_for(p).exists()
            });
            self.suggested_filename = candidate
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or(name);
            self.output_path = candidate;
        }
    }

    /// Builds the segment list. Returns true when the target file must be truncated.
    async fn init_segments(&mut self) -> bool {
        if self.fresh_start {
            self.resume_manager.cleanup();
        }

        if self.resumable {
            if let Some(state) = self.resume_manager.load_state() {
                let valid = state.matches_remote(
                    self.total_size,
                    self.etag.as_deref(),
                    self.last_modified.as_deref(),
                    self.ignore_time_changes,
                ) && self.output_path.exists();
                if valid && !state.segments.is_empty() {
                    let restored = ResumeManager::reset_segments(state.segments);
                    self.next_segment_id = restored.iter().map(|s| s.segment_id).max().unwrap_or(0) + 1;
                    *self.segments.write().await = restored;
                    return false;
                }
            }
        }
        // No usable saved progress: start over.
        self.resume_manager.cleanup();

        let mut segs = self.segments.write().await;
        segs.clear();

        match self.total_size {
            Some(0) => {}
            Some(total) if self.resumable && self.connections > 1 => {
                let parts = (self.connections as u64).min((total / MIN_INITIAL_PART).max(1));
                let part_size = total / parts;
                for i in 0..parts {
                    let start = i * part_size;
                    let end = if i == parts - 1 { total - 1 } else { start + part_size - 1 };
                    segs.push(Segment::new(i as usize, start, end));
                }
            }
            Some(total) => segs.push(Segment::new(0, 0, total - 1)),
            None => segs.push(Segment::new_unbounded(0)),
        }
        self.next_segment_id = segs.len();
        true
    }

    /// Start and run the download to completion (or until stopped).
    pub async fn run(&mut self) -> Result<(), BoxError> {
        let result = self.run_inner().await;
        let final_status = match &result {
            Ok(()) => EngineStatus::Completed,
            Err(e) if e.is::<DownloadStopped>() => EngineStatus::Paused,
            Err(e) => {
                *self.error_message.lock().await = Some(e.to_string());
                EngineStatus::Failed
            }
        };
        *self.status.write().await = final_status;
        *self.current_speed_bps.lock().await = 0.0;
        result
    }

    async fn run_inner(&mut self) -> Result<(), BoxError> {
        let mut cancel_rx = self.cancel_rx.clone();
        if *cancel_rx.borrow() {
            return Err(Box::new(DownloadStopped));
        }

        tokio::select! {
            res = self.probe_server() => res?,
            _ = wait_for_true(&mut cancel_rx) => return Err(Box::new(DownloadStopped)),
        }

        let truncate = self.init_segments().await;
        let storage = self.storage.clone().expect("storage is set by probe_server");
        storage.open_with(truncate).map_err(|e| format!("Cannot open {}: {}", self.output_path.display(), e))?;

        if self.total_size == Some(0) {
            storage.truncate_to(0)?;
            storage.close();
            self.resume_manager.cleanup();
            return Ok(());
        }

        *self.status.write().await = EngineStatus::Downloading;

        let client = self.request.build_client()?;
        let (stop_tx, stop_rx) = watch::channel(false);
        let ctx = Arc::new(WorkerContext {
            // A form download sends its POST to the form's address again and
            // follows the redirects as the probe did (often to a GET address).
            url: if self.request.post.is_some() { self.url.clone() } else { self.effective_url.clone() },
            client,
            request: self.request.clone(),
            storage: storage.clone(),
            segments: self.segments.clone(),
            rate_limiter: self.rate_limiter.clone(),
            global_limiter: self.global_limiter.clone(),
            stop_rx,
            ranged: self.resumable && self.total_size.is_some(),
        });

        let mut workers: JoinSet<WorkerExit> = JoinSet::new();
        let mut active: HashSet<usize> = HashSet::new();
        let mut retry_at: HashMap<usize, Instant> = HashMap::new();
        let mut fatal: Option<String> = None;
        let mut stopped = false;
        let mut completed = false;

        let mut last_save = Instant::now();
        let mut speed_bytes = self.downloaded_bytes().await;
        let mut speed_time = Instant::now();

        loop {
            // 1. Collect finished workers.
            while let Some(joined) = workers.try_join_next() {
                let exit = match joined {
                    Ok(exit) => exit,
                    Err(e) => {
                        fatal = Some(format!("Internal worker error: {}", e));
                        continue;
                    }
                };
                active.remove(&exit.segment_id);
                let mut segs = self.segments.write().await;
                let Some(seg) = segs.iter_mut().find(|s| s.segment_id == exit.segment_id) else { continue };
                seg.speed_bps = 0.0;
                match exit.outcome {
                    WorkerOutcome::Finished => seg.status = SegmentStatus::Completed,
                    WorkerOutcome::Stopped => {
                        if seg.status != SegmentStatus::Completed {
                            seg.status = SegmentStatus::Pending;
                        }
                    }
                    WorkerOutcome::Retry(msg) => {
                        seg.retries = if exit.progressed { 1 } else { seg.retries + 1 };
                        if !ctx.ranged {
                            // Without range support a retry restarts the stream.
                            seg.current_offset = seg.start;
                        }
                        if seg.retries > self.max_retries {
                            seg.status = SegmentStatus::Failed;
                            fatal = Some(msg);
                        } else {
                            seg.status = SegmentStatus::Pending;
                            let backoff = Duration::from_millis(500 * 2u64.pow(seg.retries.min(6)));
                            retry_at.insert(seg.segment_id, Instant::now() + backoff.min(Duration::from_secs(30)));
                        }
                    }
                    WorkerOutcome::Fatal(msg) => {
                        seg.status = SegmentStatus::Failed;
                        fatal = Some(msg);
                    }
                }
            }

            if fatal.is_some() {
                break;
            }
            if *cancel_rx.borrow() {
                stopped = true;
                break;
            }

            {
                let mut segs = self.segments.write().await;

                // 2. Completion check.
                if active.is_empty() && !segs.is_empty() && segs.iter().all(|s| s.status == SegmentStatus::Completed) {
                    completed = true;
                    break;
                }

                // 3. Start workers for waiting segments.
                let now = Instant::now();
                for seg in segs.iter_mut() {
                    if active.len() >= self.connections {
                        break;
                    }
                    if seg.status != SegmentStatus::Pending || active.contains(&seg.segment_id) {
                        continue;
                    }
                    if retry_at.get(&seg.segment_id).is_some_and(|t| now < *t) {
                        continue;
                    }
                    if seg.is_finished() {
                        seg.status = SegmentStatus::Completed;
                        continue;
                    }
                    seg.status = SegmentStatus::Downloading;
                    seg.connection_id = Some(seg.segment_id);
                    active.insert(seg.segment_id);
                    let id = seg.segment_id;
                    let ctx = ctx.clone();
                    workers.spawn(async move { segment_worker(ctx, id).await });
                }

                // 4. Dynamic resegmentation: an idle slot steals half of the largest remaining range.
                let waiting = segs.iter().any(|s| s.status == SegmentStatus::Pending);
                if ctx.ranged && self.enable_dynamic_resegment && !waiting && active.len() < self.connections {
                    let candidate = segs
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.status == SegmentStatus::Downloading)
                        .max_by_key(|(_, s)| s.remaining_bytes())
                        .map(|(i, _)| i);
                    if let Some(idx) = candidate {
                        if let Some(new_seg) = segs[idx].split_remaining(self.next_segment_id, self.min_split_size) {
                            self.next_segment_id += 1;
                            let id = new_seg.segment_id;
                            let mut new_seg = new_seg;
                            new_seg.status = SegmentStatus::Downloading;
                            new_seg.connection_id = Some(id);
                            segs.push(new_seg);
                            active.insert(id);
                            let ctx = ctx.clone();
                            workers.spawn(async move { segment_worker(ctx, id).await });
                        }
                    }
                }

                // 5. Periodic atomic state snapshot.
                if ctx.ranged && last_save.elapsed() >= Duration::from_secs(1) {
                    let _ = self.resume_manager.save_state(
                        &self.url,
                        self.total_size,
                        self.etag.as_deref(),
                        self.last_modified.as_deref(),
                        &segs,
                    );
                    last_save = Instant::now();
                }

                // 6. Aggregate speed.
                let dt = speed_time.elapsed().as_secs_f64();
                if dt >= 0.5 {
                    let bytes: u64 = segs.iter().map(|s| s.downloaded_bytes()).sum();
                    *self.current_speed_bps.lock().await = bytes.saturating_sub(speed_bytes) as f64 / dt;
                    speed_bytes = bytes;
                    speed_time = Instant::now();
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                _ = wait_for_true(&mut cancel_rx) => {}
            }
        }

        // Stop and wait for every worker so nothing writes after the file closes.
        stop_tx.send_replace(true);
        while workers.join_next().await.is_some() {}

        let downloaded: u64 = {
            let mut segs = self.segments.write().await;
            for s in segs.iter_mut() {
                s.speed_bps = 0.0;
                if s.status == SegmentStatus::Downloading {
                    s.status = SegmentStatus::Pending;
                }
            }
            segs.iter().map(|s| s.downloaded_bytes()).sum()
        };

        if completed {
            let final_len = self.total_size.unwrap_or(downloaded);
            storage.truncate_to(final_len)?;
            storage.flush()?;
            storage.close();
            self.resume_manager.cleanup();
            if self.set_file_time {
                self.apply_server_time();
            }
            if self.total_size.is_none() {
                self.total_size = Some(final_len);
                self.info.lock().unwrap().total_size = Some(final_len);
            }
            return Ok(());
        }

        if ctx.ranged {
            let segs = self.segments.read().await.clone();
            let _ = self.resume_manager.save_state(
                &self.url,
                self.total_size,
                self.etag.as_deref(),
                self.last_modified.as_deref(),
                &segs,
            );
        }
        storage.close();

        if stopped {
            Err(Box::new(DownloadStopped))
        } else {
            Err(fatal.unwrap_or_else(|| "Download failed".to_string()).into())
        }
    }

    fn apply_server_time(&self) {
        let Some(modified) = self
            .last_modified
            .as_deref()
            .and_then(|lm| chrono::DateTime::parse_from_rfc2822(lm.trim()).ok())
        else {
            return;
        };
        let time: std::time::SystemTime = modified.with_timezone(&chrono::Utc).into();
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&self.output_path) {
            let _ = file.set_modified(time);
        }
    }

    /// Requests a stop; `run` returns `DownloadStopped` and keeps resume data.
    pub fn pause(&self) {
        self.cancel_tx.send_replace(true);
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    pub fn suggested_filename(&self) -> &str {
        &self.suggested_filename
    }

    pub fn effective_url(&self) -> &str {
        &self.effective_url
    }
}

/// `realm` parameter of a WWW-Authenticate / Proxy-Authenticate challenge.
fn auth_realm(challenge: &str) -> Option<String> {
    let lower = challenge.to_ascii_lowercase();
    let start = lower.find("realm=")? + 6;
    let rest = &challenge[start..];
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => rest.split([',', ' ']).next()?,
    };
    Some(value.to_string()).filter(|v| !v.is_empty())
}

/// Resolves when the watched flag becomes true (never, if the sender is gone).
pub(crate) async fn wait_for_true(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

struct WorkerContext {
    url: String,
    client: Client,
    request: RequestOptions,
    storage: DirectStorage,
    segments: Arc<RwLock<Vec<Segment>>>,
    rate_limiter: TokenBucketLimiter,
    global_limiter: Option<TokenBucketLimiter>,
    stop_rx: watch::Receiver<bool>,
    /// Range requests are possible (resumable server and known size).
    ranged: bool,
}

enum WorkerOutcome {
    Finished,
    Stopped,
    Retry(String),
    Fatal(String),
}

struct WorkerExit {
    segment_id: usize,
    outcome: WorkerOutcome,
    progressed: bool,
}

fn classify_status(status: StatusCode) -> WorkerOutcome {
    let msg = format!(
        "Server returned HTTP {} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
    .trim()
    .to_string();
    if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT || status == StatusCode::TOO_MANY_REQUESTS {
        WorkerOutcome::Retry(msg)
    } else {
        WorkerOutcome::Fatal(msg)
    }
}

async fn segment_worker(ctx: Arc<WorkerContext>, segment_id: usize) -> WorkerExit {
    let mut progressed = false;
    let outcome = segment_worker_inner(&ctx, segment_id, &mut progressed).await;
    WorkerExit { segment_id, outcome, progressed }
}

async fn segment_worker_inner(ctx: &WorkerContext, segment_id: usize, progressed: &mut bool) -> WorkerOutcome {
    let mut stop_rx = ctx.stop_rx.clone();

    let (start, end) = {
        let segs = ctx.segments.read().await;
        match segs.iter().find(|s| s.segment_id == segment_id) {
            Some(s) => (s.current_offset, s.end),
            None => return WorkerOutcome::Stopped,
        }
    };
    if start > end {
        return WorkerOutcome::Finished;
    }

    let mut req = ctx.request.download(&ctx.client, &ctx.url);
    if ctx.ranged {
        req = req.header(RANGE, format!("bytes={}-{}", start, end));
    }

    let resp = tokio::select! {
        r = req.send() => r,
        _ = wait_for_true(&mut stop_rx) => return WorkerOutcome::Stopped,
    };
    let resp = match resp {
        Ok(r) => r,
        Err(e) => return WorkerOutcome::Retry(http::describe_error(&e)),
    };

    let status = resp.status();
    if ctx.ranged {
        if status == StatusCode::PARTIAL_CONTENT {
            let served_start = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(http::parse_content_range)
                .and_then(|(range, _)| range)
                .map(|(s, _)| s);
            if served_start.is_some_and(|s| s != start) {
                return WorkerOutcome::Retry("Server returned a different byte range".to_string());
            }
        } else if status.is_success() {
            // A full-body reply is only usable for the first segment.
            if start != 0 {
                return WorkerOutcome::Retry("Server ignored the range request".to_string());
            }
        } else {
            return classify_status(status);
        }
    } else if !status.is_success() {
        return classify_status(status);
    }

    let mut stream = resp.bytes_stream();
    let mut window_bytes = 0u64;
    let mut window_start = Instant::now();

    loop {
        let next = tokio::select! {
            c = stream.next() => c,
            _ = wait_for_true(&mut stop_rx) => return WorkerOutcome::Stopped,
        };

        let chunk = match next {
            None => {
                let mut segs = ctx.segments.write().await;
                let Some(seg) = segs.iter_mut().find(|s| s.segment_id == segment_id) else {
                    return WorkerOutcome::Stopped;
                };
                // An unknown-length stream ends at EOF; a ranged one must reach its end.
                return if seg.is_unbounded() || seg.is_finished() {
                    seg.status = SegmentStatus::Completed;
                    WorkerOutcome::Finished
                } else {
                    WorkerOutcome::Retry("Connection closed before the part was complete".to_string())
                };
            }
            Some(Err(e)) => return WorkerOutcome::Retry(http::describe_error(&e)),
            Some(Ok(c)) => c,
        };
        if chunk.is_empty() {
            continue;
        }
        let len = chunk.len() as u64;

        if let Some(ref global) = ctx.global_limiter {
            global.consume(len, Some(&mut stop_rx)).await;
        }
        ctx.rate_limiter.consume(len, Some(&mut stop_rx)).await;
        if *stop_rx.borrow() {
            return WorkerOutcome::Stopped;
        }

        // Boundary check, write and advance happen under one lock so a split
        // cannot move `end` between them.
        let mut segs = ctx.segments.write().await;
        let Some(seg) = segs.iter_mut().find(|s| s.segment_id == segment_id) else {
            return WorkerOutcome::Stopped;
        };
        if seg.is_finished() {
            seg.status = SegmentStatus::Completed;
            return WorkerOutcome::Finished;
        }
        let allowed = len.min(seg.remaining_bytes()) as usize;
        if let Err(e) = ctx.storage.write_at(seg.current_offset, &chunk[..allowed]) {
            return WorkerOutcome::Fatal(format!("Disk write error: {}", e));
        }
        seg.advance(allowed as u64);
        *progressed = true;

        window_bytes += allowed as u64;
        let dt = window_start.elapsed().as_secs_f64();
        if dt >= 0.5 {
            seg.speed_bps = window_bytes as f64 / dt;
            window_bytes = 0;
            window_start = Instant::now();
        }

        if seg.is_finished() {
            seg.status = SegmentStatus::Completed;
            return WorkerOutcome::Finished;
        }
    }
}
