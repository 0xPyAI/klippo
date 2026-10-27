use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::RANGE;
use reqwest::Client;
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;
use url::Url;

use super::dash;
use super::engine::{wait_for_true, DownloadStopped};
use super::http::{self, RequestOptions};
use super::{mkvmux, mp4mux};

/// The error for a live stream: Klippo downloads finished videos only.
pub const LIVE_STREAM: &str = "This is a live stream. Klippo does not record live streams.";

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// One media resource of a playlist, optionally a byte range of a larger file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsSegment {
    pub url: String,
    /// (offset, length)
    pub byte_range: Option<(u64, u64)>,
    /// AES-128 key when the segment is encrypted (`#EXT-X-KEY:METHOD=AES-128`).
    pub key: Option<SegmentKey>,
}

/// Where the AES-128 key of a segment is, and its IV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentKey {
    pub uri: String,
    /// From the playlist's IV attribute, or the segment's media sequence number.
    pub iv: [u8; 16],
}

/// IV from `0x...` hex text (shorter values are left-padded with zeros).
fn parse_iv(text: &str) -> Option<[u8; 16]> {
    let hex = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    if hex.is_empty() || hex.len() > 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let padded = format!("{:0>32}", hex);
    let mut iv = [0u8; 16];
    for (i, byte) in iv.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&padded[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(iv)
}

/// AES-128-CBC with PKCS#7 padding, as HLS encrypts whole segments.
pub fn decrypt_segment(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>, String> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    cbc::Decryptor::<aes::Aes128>::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| "the segment does not decrypt with its key".to_string())
}

#[derive(Debug, Clone, Default)]
pub struct MediaPlaylist {
    /// `#EXT-X-MAP` initialization section (fragmented MP4 streams).
    pub init: Option<HlsSegment>,
    pub segments: Vec<HlsSegment>,
    /// A live playlist (no `#EXT-X-ENDLIST`): Klippo does not record live streams.
    pub live: bool,
}

/// Live counters for progress displays.
#[derive(Debug, Default)]
pub struct HlsProgress {
    pub total_parts: AtomicUsize,
    pub done_parts: AtomicUsize,
    pub bytes: AtomicU64,
    /// Exact final size when the source tells it (YouTube streams); 0 = unknown.
    pub known_total: AtomicU64,
}

impl HlsProgress {
    pub fn fraction(&self) -> f64 {
        let total = self.total_parts.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            self.done_parts.load(Ordering::Relaxed) as f64 / total as f64
        }
    }

    /// Final size estimate from the average part size so far.
    pub fn estimated_total(&self) -> Option<u64> {
        let known = self.known_total.load(Ordering::Relaxed);
        if known > 0 {
            return Some(known);
        }
        let done = self.done_parts.load(Ordering::Relaxed) as u64;
        let total = self.total_parts.load(Ordering::Relaxed) as u64;
        if done == 0 || total == 0 {
            return None;
        }
        let bytes = self.bytes.load(Ordering::Relaxed);
        if done >= total {
            Some(bytes)
        } else {
            Some(bytes / done * total)
        }
    }
}

#[derive(Clone)]
pub struct HlsControl {
    cancel_tx: Arc<watch::Sender<bool>>,
}

impl HlsControl {
    pub(crate) fn new(cancel_tx: Arc<watch::Sender<bool>>) -> Self {
        Self { cancel_tx }
    }

    pub fn stop(&self) {
        self.cancel_tx.send_replace(true);
    }
}

/// The separate audio of a video: an address still to read, or DASH segments.
enum AudioSource {
    Url(String),
    Segments(MediaPlaylist),
}

/// Which stream a part belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Video,
    Audio,
}

impl Stream {
    fn prefix(self) -> &'static str {
        match self {
            Stream::Video => "v",
            Stream::Audio => "a",
        }
    }
}

/// Downloads an HLS stream (or a plain media file) and, when the video has a
/// separate audio stream (`#EXT-X-MEDIA:TYPE=AUDIO`, or a DASH-style audio
/// file), the audio too; then joins everything into one file. Fragmented MP4
/// (CMAF) streams become one regular MP4; MPEG-TS streams are joined as `.ts`.
pub struct HLSGrabber {
    m3u8_url: String,
    audio_url: Option<String>,
    output_path: PathBuf,
    connections: usize,
    temp_dir: PathBuf,
    request: RequestOptions,
    progress: Arc<HlsProgress>,
    cancel_tx: Arc<watch::Sender<bool>>,
    cancel_rx: watch::Receiver<bool>,
}

impl HLSGrabber {
    pub const PART_RETRIES: u32 = 4;

    pub fn new<P: AsRef<Path>>(m3u8_url: &str, output_path: P, connections: usize) -> Self {
        let out = output_path.as_ref().to_path_buf();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        Self {
            m3u8_url: m3u8_url.to_string(),
            audio_url: None,
            temp_dir: Self::temp_dir_for(&out),
            output_path: out,
            connections: connections.clamp(1, 16),
            request: RequestOptions::default(),
            progress: Arc::new(HlsProgress::default()),
            cancel_tx: Arc::new(cancel_tx),
            cancel_rx,
        }
    }

    /// Folder that holds downloaded parts until they are merged.
    pub fn temp_dir_for(output_path: &Path) -> PathBuf {
        super::parts::parts_dir(output_path, ".hls_parts")
    }

    pub fn set_request_options(&mut self, options: RequestOptions) {
        self.request = options;
    }

    /// The separate audio stream (an HLS playlist or a plain audio file).
    pub fn set_audio_url(&mut self, url: Option<String>) {
        self.audio_url = url.filter(|u| !u.trim().is_empty());
    }

    pub fn progress(&self) -> Arc<HlsProgress> {
        self.progress.clone()
    }

    pub fn control(&self) -> HlsControl {
        HlsControl::new(self.cancel_tx.clone())
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    /// Media segment URLs of the playlist (the best variant of a master playlist).
    pub async fn fetch_playlist(&self) -> Result<Vec<String>, BoxError> {
        let playlist = self.resolve_playlist().await?;
        Ok(playlist.segments.into_iter().map(|s| s.url).collect())
    }

    pub async fn resolve_playlist(&self) -> Result<MediaPlaylist, BoxError> {
        let client = self.request.build_client()?;
        Ok(self.resolve_source(&client, &self.m3u8_url).await?.0)
    }

    /// A playlist's segments, or one "segment" for a plain media file. A master
    /// playlist resolves to its best variant and that variant's audio stream;
    /// a DASH manifest to its best MP4 video (or the height in `#height=720`)
    /// and its audio.
    async fn resolve_source(&self, client: &Client, url: &str) -> Result<(MediaPlaylist, Option<AudioSource>), BoxError> {
        let mut url = url.to_string();
        let mut audio = None;
        // Master playlists can nest; three levels is plenty.
        for _ in 0..3 {
            let Some(text) = self.fetch_if_playlist(client, &url).await? else {
                return Ok((MediaPlaylist { segments: vec![HlsSegment { url, byte_range: None, key: None }], ..Default::default() }, audio));
            };
            let parsed = Url::parse(&url)?;
            if dash::looks_like_mpd(&text) {
                let height = parsed.fragment().and_then(|f| f.strip_prefix("height=")).and_then(|h| h.parse().ok());
                let mpd = dash::parse(&text, &parsed)?;
                let (video, audio_rep) = mpd.choose(height)?;
                let audio = audio_rep.map(|a| mpd.playlist(a)).transpose()?.map(AudioSource::Segments);
                return Ok((mpd.playlist(video)?, audio));
            }
            match parse_playlist(&text, &parsed)? {
                Playlist::Master { video, audio: a } => {
                    url = video;
                    audio = audio.or(a.map(AudioSource::Url));
                }
                Playlist::Media(media) => return Ok((media, audio)),
            }
        }
        Err("Too many nested master playlists".into())
    }

    /// The text of `url` when it is an M3U8 playlist or a DASH manifest; None for media data.
    async fn fetch_if_playlist(&self, client: &Client, url: &str) -> Result<Option<String>, BoxError> {
        let resp = self.request.get(client, url).send().await.map_err(|e| http::describe_error(&e))?;
        if !resp.status().is_success() {
            return Err(format!("Request failed: HTTP {}", resp.status().as_u16()).into());
        }
        let mut stream = resp.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.map_err(|e| http::describe_error(&e))?);
            let head = String::from_utf8_lossy(&body[..body.len().min(64)]).trim_start_matches('\u{feff}').trim_start().to_string();
            if head.len() >= 7 && !head.starts_with("#EXTM3U") && !head.starts_with('<') {
                return Ok(None); // media data: the parts loop downloads it
            }
            if body.len() > 16 * 1024 * 1024 {
                return Err("The playlist is too large".into());
            }
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        let trimmed = text.trim_start_matches('\u{feff}').trim_start();
        Ok((trimmed.starts_with("#EXTM3U") || dash::looks_like_mpd(trimmed)).then_some(text))
    }

    /// Downloads every part and joins them; returns the file written (its
    /// extension follows the content: `.mp4` for MP4 streams).
    pub async fn run(&self) -> Result<PathBuf, BoxError> {
        let mut cancel_rx = self.cancel_rx.clone();
        let client = self.request.build_client()?;
        let (video, master_audio) = tokio::select! {
            p = self.resolve_source(&client, &self.m3u8_url) => p?,
            _ = wait_for_true(&mut cancel_rx) => return Err(Box::new(DownloadStopped)),
        };
        if video.segments.is_empty() {
            return Err("No media segments found in M3U8 playlist.".into());
        }
        let audio = match self.audio_url.clone().map(AudioSource::Url).or(master_audio) {
            Some(AudioSource::Url(url)) => Some(tokio::select! {
                p = self.resolve_source(&client, &url) => p?.0,
                _ = wait_for_true(&mut cancel_rx) => return Err(Box::new(DownloadStopped)),
            }),
            Some(AudioSource::Segments(playlist)) => Some(playlist),
            None => None,
        };

        if video.live || audio.as_ref().is_some_and(|a| a.live) {
            return Err(LIVE_STREAM.into());
        }

        let mut parts: Vec<(Stream, usize, HlsSegment)> = Vec::new();
        for (stream, playlist) in [(Stream::Video, Some(&video)), (Stream::Audio, audio.as_ref())] {
            if let Some(p) = playlist {
                for (i, part) in p.init.iter().chain(p.segments.iter()).enumerate() {
                    parts.push((stream, i, part.clone()));
                }
            }
        }
        self.progress.total_parts.store(parts.len(), Ordering::Relaxed);
        self.progress.done_parts.store(0, Ordering::Relaxed);
        self.progress.bytes.store(0, Ordering::Relaxed);

        // AES-128 keys, fetched once each.
        let mut keys: HashMap<String, [u8; 16]> = HashMap::new();
        self.load_keys(&client, parts.iter().map(|(_, _, p)| p), &mut keys).await?;
        let keys = Arc::new(keys);

        fs::create_dir_all(&self.temp_dir)?;
        let semaphore = Arc::new(Semaphore::new(self.connections));
        let mut tasks: JoinSet<Result<(), String>> = JoinSet::new();

        for (stream, idx, part) in &parts {
            let part_path = self.part_path(*stream, *idx);
            if let Ok(meta) = fs::metadata(&part_path) {
                // Parts are renamed into place only when complete.
                self.progress.bytes.fetch_add(meta.len(), Ordering::Relaxed);
                self.progress.done_parts.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            let permit = tokio::select! {
                p = semaphore.clone().acquire_owned() => p.expect("semaphore is never closed"),
                _ = wait_for_true(&mut cancel_rx) => break,
            };
            let client = client.clone();
            let request = self.request.clone();
            let part = part.clone();
            let progress = self.progress.clone();
            let mut stop_rx = self.cancel_rx.clone();
            let label = format!("{}{}", stream.prefix(), idx);
            let keys = keys.clone();
            tasks.spawn(async move {
                let _permit = permit;
                let mut last_err = String::new();
                for attempt in 0..=Self::PART_RETRIES {
                    if attempt > 0 {
                        tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
                    }
                    let res = tokio::select! {
                        r = fetch_part(&client, &request, &part, &part_path, &progress, &keys) => r,
                        _ = wait_for_true(&mut stop_rx) => return Ok(()),
                    };
                    match res {
                        Ok(()) => {
                            progress.done_parts.fetch_add(1, Ordering::Relaxed);
                            return Ok(());
                        }
                        Err(e) => last_err = e,
                    }
                }
                Err(format!("Segment {} failed: {}", label, last_err))
            });

            // Fail fast instead of queueing the rest behind a broken stream.
            while let Some(done) = tasks.try_join_next() {
                if let Ok(Err(e)) = done {
                    self.cancel_tx.send_replace(true);
                    return Err(e.into());
                }
            }
        }

        while let Some(done) = tasks.join_next().await {
            match done {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    self.cancel_tx.send_replace(true);
                    return Err(e.into());
                }
                Err(e) => return Err(format!("Internal error: {}", e).into()),
            }
        }

        if *self.cancel_rx.borrow() {
            return Err(Box::new(DownloadStopped));
        }

        let count = |s: Stream| parts.iter().filter(|(p, ..)| *p == s).count();
        self.assemble(count(Stream::Video), count(Stream::Audio)).await
    }

    /// Fetches the AES-128 keys of `segments` that are not loaded yet.
    async fn load_keys<'a>(
        &self,
        client: &Client,
        segments: impl Iterator<Item = &'a HlsSegment>,
        keys: &mut HashMap<String, [u8; 16]>,
    ) -> Result<(), BoxError> {
        for part in segments {
            let Some(ref k) = part.key else { continue };
            if keys.contains_key(&k.uri) {
                continue;
            }
            let resp = self.request.get(client, &k.uri).send().await.map_err(|e| http::describe_error(&e))?;
            if !resp.status().is_success() {
                return Err(format!("The stream's AES-128 key could not be loaded (HTTP {})", resp.status().as_u16()).into());
            }
            let bytes = resp.bytes().await.map_err(|e| http::describe_error(&e))?;
            let key: [u8; 16] = bytes.as_ref().try_into().map_err(|_| "The stream's AES-128 key is not 16 bytes long")?;
            keys.insert(k.uri.clone(), key);
        }
        Ok(())
    }

    /// Joins the downloaded parts into the output file.
    async fn assemble(&self, video_parts: usize, audio_parts: usize) -> Result<PathBuf, BoxError> {
        if let Some(parent) = self.output_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let video_file = self.temp_dir.join("video.bin");
        self.concat(Stream::Video, video_parts, &video_file)?;
        let audio_file = self.temp_dir.join("audio.bin");
        let has_audio = audio_parts > 0;
        if has_audio {
            self.concat(Stream::Audio, audio_parts, &audio_file)?;
        }

        let final_path = if mkvmux::looks_like_webm(&video_file) {
            let mut inputs = vec![video_file.clone()];
            if has_audio {
                if !mkvmux::looks_like_webm(&audio_file) {
                    return Err("The separate audio stream is not WebM and cannot be joined to WebM video.".into());
                }
                inputs.push(audio_file.clone());
            }
            let target = self.temp_dir.join("joined.mkv");
            let staged = target.clone();
            let ext = tokio::task::spawn_blocking(move || {
                let refs: Vec<&Path> = inputs.iter().map(PathBuf::as_path).collect();
                mkvmux::remux(&refs, &staged)
            })
            .await
            .map_err(|e| format!("Internal error: {}", e))?
            .map_err(|e| format!("Cannot join the stream into one WebM file: {}", e))?;
            let out = free_path(&with_extension(&self.output_path, ext), &self.output_path);
            let _ = fs::remove_file(&out);
            fs::rename(&target, &out).or_else(|_| fs::copy(&target, &out).map(|_| ()))?;
            out
        } else if mp4mux::looks_like_mp4(&video_file) {
            let out = free_path(&with_extension(&self.output_path, "mp4"), &self.output_path);
            let mut inputs = vec![video_file.clone()];
            if has_audio {
                if !mp4mux::looks_like_mp4(&audio_file) {
                    return Err("The separate audio stream is not MP4 and cannot be joined to MP4 video.".into());
                }
                inputs.push(audio_file.clone());
            }
            let target = out.clone();
            tokio::task::spawn_blocking(move || {
                let refs: Vec<&Path> = inputs.iter().map(PathBuf::as_path).collect();
                mp4mux::remux(&refs, &target)
            })
            .await
            .map_err(|e| format!("Internal error: {}", e))?
            .map_err(|e| format!("Cannot join the stream into one MP4 file: {}", e))?;
            out
        } else {
            let out = free_path(&with_extension(&self.output_path, "ts"), &self.output_path);
            if has_audio {
                super::ts::merge(&video_file, &audio_file, &out).map_err(|e| format!("Cannot join the video and audio streams: {}", e))?;
            } else {
                let _ = fs::remove_file(&out);
                fs::rename(&video_file, &out).or_else(|_| fs::copy(&video_file, &out).map(|_| ()))?;
            }
            out
        };

        let _ = fs::remove_dir_all(&self.temp_dir);
        Ok(final_path)
    }

    /// Joins the parts of one stream in order.
    fn concat(&self, stream: Stream, count: usize, to: &Path) -> io::Result<()> {
        let mut out = io::BufWriter::with_capacity(1 << 20, File::create(to)?);
        for idx in 0..count {
            let mut part = File::open(self.part_path(stream, idx))?;
            io::copy(&mut part, &mut out)?;
        }
        out.flush()
    }

    fn part_path(&self, stream: Stream, idx: usize) -> PathBuf {
        self.temp_dir.join(format!("{}_{:05}.part", stream.prefix(), idx))
    }
}

/// `path` with another extension, unless it already has a fitting one. Only a
/// media extension is replaced: "Part 1.5" becomes "Part 1.5.mp4".
fn with_extension(path: &Path, ext: &str) -> PathBuf {
    let current = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let fits = match ext {
        "mp4" => matches!(current.as_str(), "mp4" | "m4v" | "m4a" | "mov"),
        _ => current == ext,
    };
    if fits {
        return path.to_path_buf();
    }
    if matches!(current.as_str(), "ts" | "m3u8" | "m3u" | "mp4" | "m4v" | "m4a" | "mov" | "mkv" | "webm" | "bin" | "mpd") {
        path.with_extension(ext)
    } else {
        let mut name = path.as_os_str().to_os_string();
        name.push(".");
        name.push(ext);
        PathBuf::from(name)
    }
}

/// `wanted`, or "name (2).ext" when another file already has that name.
fn free_path(wanted: &Path, original: &Path) -> PathBuf {
    if wanted == original || !wanted.exists() {
        return wanted.to_path_buf();
    }
    let stem = wanted.file_stem().and_then(|s| s.to_str()).unwrap_or("video").to_string();
    let ext = wanted.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
    (2..1000)
        .map(|n| wanted.with_file_name(if ext.is_empty() { format!("{} ({})", stem, n) } else { format!("{} ({}).{}", stem, n, ext) }))
        .find(|p| !p.exists())
        .unwrap_or_else(|| wanted.to_path_buf())
}

/// Downloads one part to `path` (through a temporary name), counting bytes as
/// they arrive; encrypted parts are decrypted before they are stored.
async fn fetch_part(
    client: &Client,
    request: &RequestOptions,
    part: &HlsSegment,
    path: &Path,
    progress: &HlsProgress,
    keys: &HashMap<String, [u8; 16]>,
) -> Result<(), String> {
    let mut req = request.get(client, &part.url);
    if let Some((offset, len)) = part.byte_range {
        req = req.header(RANGE, format!("bytes={}-{}", offset, offset + len.saturating_sub(1)));
    }
    let resp = req.send().await.map_err(|e| http::describe_error(&e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let tmp = path.with_extension("tmp");
    let mut file = io::BufWriter::new(File::create(&tmp).map_err(|e| e.to_string())?);
    let mut stream = resp.bytes_stream();
    let mut received = 0u64;
    // Encrypted parts are small segments: keep them in memory until decrypted.
    let mut encrypted = Vec::new();
    let result: Result<(), String> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| http::describe_error(&e))?;
            if part.key.is_some() {
                encrypted.extend_from_slice(&chunk);
            } else {
                file.write_all(&chunk).map_err(|e| e.to_string())?;
            }
            received += chunk.len() as u64;
            progress.bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
        if let Some(ref k) = part.key {
            let key = keys.get(&k.uri).ok_or("missing AES-128 key")?;
            file.write_all(&decrypt_segment(&encrypted, key, &k.iv)?).map_err(|e| e.to_string())?;
        }
        file.flush().map_err(|e| e.to_string())
    }
    .await;
    if let Err(e) = result {
        progress.bytes.fetch_sub(received, Ordering::Relaxed);
        return Err(e);
    }
    if let Some((_, len)) = part.byte_range {
        if received != len {
            progress.bytes.fetch_sub(received, Ordering::Relaxed);
            return Err(format!("expected {} bytes, got {}", len, received));
        }
    }
    drop(file);
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

enum Playlist {
    /// The highest-bandwidth variant and its separate audio stream, if any.
    Master { video: String, audio: Option<String> },
    Media(MediaPlaylist),
}

fn attr_value(attrs: &str, key: &str) -> Option<String> {
    // Attribute lists are KEY=VALUE pairs; quoted values may contain commas.
    let mut rest = attrs;
    while !rest.is_empty() {
        let (k, after) = rest.split_once('=')?;
        let (value, next) = if let Some(stripped) = after.strip_prefix('"') {
            let end = stripped.find('"')?;
            (&stripped[..end], stripped[end + 1..].trim_start_matches(','))
        } else {
            match after.split_once(',') {
                Some((v, n)) => (v, n),
                None => (after, ""),
            }
        };
        if k.trim().eq_ignore_ascii_case(key) {
            return Some(value.to_string());
        }
        rest = next;
    }
    None
}

fn parse_byte_range(spec: &str, next_offset: u64) -> Option<(u64, u64)> {
    let (len, offset) = match spec.split_once('@') {
        Some((l, o)) => (l.trim().parse().ok()?, o.trim().parse().ok()?),
        None => (spec.trim().parse().ok()?, next_offset),
    };
    Some((offset, len))
}

fn parse_playlist(text: &str, base: &Url) -> Result<Playlist, BoxError> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty()).peekable();
    match lines.peek() {
        Some(first) if first.trim_start_matches('\u{feff}').starts_with("#EXTM3U") => {}
        _ => return Err("The address is not an M3U8 playlist".into()),
    }

    // ((AAC or unknown audio, bandwidth), url, audio group)
    let mut best_variant: Option<((bool, u64), String, Option<String>)> = None;
    let mut pending_variant: Option<((bool, u64), Option<String>)> = None;
    // Audio renditions with their own playlist: (group, url, default).
    let mut audio_renditions: Vec<(String, String, bool)> = Vec::new();
    let mut media = MediaPlaylist::default();
    let mut pending_range: Option<String> = None;
    let mut next_offset = 0u64;
    let mut sequence = 0u64;
    let mut ended = false;
    // Key in effect: (key address, explicit IV).
    let mut key: Option<(String, Option<[u8; 16]>)> = None;
    let key_for = |key: &Option<(String, Option<[u8; 16]>)>, sequence: u64| {
        key.as_ref().map(|(uri, iv)| SegmentKey {
            uri: uri.clone(),
            iv: iv.unwrap_or_else(|| {
                let mut b = [0u8; 16];
                b[8..].copy_from_slice(&sequence.to_be_bytes());
                b
            }),
        })
    };

    for line in lines {
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let bandwidth = attr_value(attrs, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0);
            // AAC audio joins into MP4 and TS alike; AC-3/E-AC-3 variants come second.
            let codecs = attr_value(attrs, "CODECS").unwrap_or_default().to_ascii_lowercase();
            let aac = !(codecs.contains("ac-3") || codecs.contains("ec-3")) || codecs.contains("mp4a");
            pending_variant = Some(((aac, bandwidth), attr_value(attrs, "AUDIO")));
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-MEDIA:") {
            let is_audio = attr_value(attrs, "TYPE").is_some_and(|t| t.eq_ignore_ascii_case("AUDIO"));
            if let (true, Some(uri)) = (is_audio, attr_value(attrs, "URI")) {
                let default = attr_value(attrs, "DEFAULT").is_some_and(|d| d.eq_ignore_ascii_case("YES"));
                audio_renditions.push((attr_value(attrs, "GROUP-ID").unwrap_or_default(), base.join(&uri)?.to_string(), default));
            }
        } else if let Some(n) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = n.trim().parse().unwrap_or(0);
        } else if line == "#EXT-X-ENDLIST" || line == "#EXT-X-PLAYLIST-TYPE:VOD" {
            ended = true;
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-KEY:") {
            let method = attr_value(attrs, "METHOD").unwrap_or_default();
            let format = attr_value(attrs, "KEYFORMAT").unwrap_or_else(|| "identity".to_string());
            if method.eq_ignore_ascii_case("NONE") {
                key = None;
            } else if method.eq_ignore_ascii_case("AES-128") && format == "identity" {
                let uri = attr_value(attrs, "URI").ok_or("The AES-128 key has no address")?;
                key = Some((base.join(&uri)?.to_string(), attr_value(attrs, "IV").and_then(|v| parse_iv(&v))));
            } else {
                // SAMPLE-AES and key systems such as FairPlay are DRM.
                return Err(format!("This HLS stream is DRM-protected ({}) and cannot be downloaded", method).into());
            }
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(uri) = attr_value(attrs, "URI") {
                let byte_range = attr_value(attrs, "BYTERANGE").and_then(|r| parse_byte_range(&r, 0));
                media.init = Some(HlsSegment { url: base.join(&uri)?.to_string(), byte_range, key: key_for(&key, sequence) });
            }
        } else if let Some(spec) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = Some(spec.to_string());
        } else if line.starts_with('#') {
            continue;
        } else if let Some((rank, group)) = pending_variant.take() {
            let url = base.join(line)?.to_string();
            if best_variant.as_ref().is_none_or(|(r, ..)| rank > *r) {
                best_variant = Some((rank, url, group));
            }
        } else {
            let url = base.join(line)?.to_string();
            let byte_range = pending_range.take().and_then(|r| parse_byte_range(&r, next_offset));
            if let Some((offset, len)) = byte_range {
                next_offset = offset + len;
            }
            media.segments.push(HlsSegment { url, byte_range, key: key_for(&key, sequence) });
            sequence += 1;
        }
    }

    media.live = !ended;
    match best_variant {
        Some((_, video, group)) => {
            let in_group: Vec<&(String, String, bool)> = audio_renditions.iter().filter(|r| Some(&r.0) == group.as_ref()).collect();
            let audio = in_group.iter().find(|r| r.2).or(in_group.first()).map(|r| r.1.clone());
            Ok(Playlist::Master { video, audio })
        }
        None => Ok(Playlist::Media(media)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_playlist_picks_highest_bandwidth() {
        let base = Url::parse("https://cdn.example.com/live/master.m3u8").unwrap();
        let text = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5000000,CODECS=\"avc1.4d401f,mp4a.40.2\"\nhigh/index.m3u8\n";
        match parse_playlist(text, &base).unwrap() {
            Playlist::Master { video, audio } => {
                assert_eq!(video, "https://cdn.example.com/live/high/index.m3u8");
                assert_eq!(audio, None);
            }
            Playlist::Media(_) => panic!("expected master playlist"),
        }
    }

    #[test]
    fn master_playlist_with_separate_audio() {
        // Reddit's layout: video-only variants, audio in its own playlists.
        let base = Url::parse("https://v.redd.it/abc/HLSPlaylist.m3u8?f=hd").unwrap();
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:URI=\"CMAF_AUDIO_128.m3u8\",TYPE=AUDIO,GROUP-ID=\"6\",NAME=\"audio 0\",DEFAULT=YES\n\
            #EXT-X-MEDIA:URI=\"CMAF_AUDIO_64.m3u8\",TYPE=AUDIO,GROUP-ID=\"7\",NAME=\"audio 0\",DEFAULT=YES\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1723198,RESOLUTION=720x1280,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"6\"\nCMAF_720.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=111310,RESOLUTION=220x392,AUDIO=\"7\"\nCMAF_220.m3u8\n";
        match parse_playlist(text, &base).unwrap() {
            Playlist::Master { video, audio } => {
                assert_eq!(video, "https://v.redd.it/abc/CMAF_720.m3u8");
                assert_eq!(audio.as_deref(), Some("https://v.redd.it/abc/CMAF_AUDIO_128.m3u8"));
            }
            Playlist::Media(_) => panic!("expected master playlist"),
        }
    }

    #[test]
    fn aac_variants_win_over_ac3() {
        // Apple's advanced example: the AC-3 copies of a quality list a higher bandwidth.
        let base = Url::parse("https://x/ex/master.m3u8").unwrap();
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud1\",DEFAULT=YES,URI=\"a1/prog_index.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud2\",DEFAULT=YES,URI=\"a2/prog_index.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=8178040,CODECS=\"avc1.64002a,mp4a.40.2\",AUDIO=\"aud1\"\nv9/prog_index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=8399417,CODECS=\"avc1.64002a,ac-3\",AUDIO=\"aud2\"\nv9/prog_index.m3u8\n";
        match parse_playlist(text, &base).unwrap() {
            Playlist::Master { audio, .. } => assert_eq!(audio.as_deref(), Some("https://x/ex/a1/prog_index.m3u8")),
            Playlist::Media(_) => panic!("expected master playlist"),
        }
    }

    #[test]
    fn media_playlist_with_map_and_byteranges() {
        let base = Url::parse("https://cdn.example.com/v/index.m3u8").unwrap();
        let text = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\n#EXT-X-BYTERANGE:100@0\nmain.mp4\n#EXTINF:4,\n#EXT-X-BYTERANGE:50\nmain.mp4\n#EXTINF:4,\nseg3.m4s\n#EXT-X-ENDLIST\n";
        match parse_playlist(text, &base).unwrap() {
            Playlist::Media(m) => {
                assert_eq!(m.init.unwrap().url, "https://cdn.example.com/v/init.mp4");
                assert_eq!(m.segments.len(), 3);
                assert_eq!(m.segments[0].byte_range, Some((0, 100)));
                assert_eq!(m.segments[1].byte_range, Some((100, 50)));
                assert_eq!(m.segments[2].byte_range, None);
            }
            Playlist::Master { .. } => panic!("expected media playlist"),
        }
    }

    #[test]
    fn aes128_keys_and_drm() {
        let base = Url::parse("https://x/v/y.m3u8").unwrap();
        let text = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:4,\na.ts\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"k2\",IV=0x0102\n#EXTINF:4,\nb.ts\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\nc.ts\n";
        let Playlist::Media(m) = parse_playlist(text, &base).unwrap() else { panic!("media playlist") };
        let a = m.segments[0].key.as_ref().unwrap();
        assert_eq!(a.uri, "https://x/v/key.bin");
        assert_eq!(a.iv[15], 7, "IV is the media sequence number");
        let b = m.segments[1].key.as_ref().unwrap();
        assert_eq!((b.iv[14], b.iv[15]), (1, 2));
        assert!(m.segments[2].key.is_none());
        assert!(parse_playlist("#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"\na.ts\n", &base).is_err());
        assert!(parse_playlist("<html></html>", &base).is_err());
    }

    #[test]
    fn live_and_vod_playlists() {
        let base = Url::parse("https://x/live/index.m3u8").unwrap();
        let live = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:120\n#EXTINF:6,\n120.ts\n#EXTINF:6,\n121.ts\n";
        let Playlist::Media(m) = parse_playlist(live, &base).unwrap() else { panic!("media playlist") };
        assert!(m.live);
        let vod = format!("{}#EXT-X-ENDLIST\n", live);
        let Playlist::Media(m) = parse_playlist(&vod, &base).unwrap() else { panic!("media playlist") };
        assert!(!m.live);
    }

    #[test]
    fn segment_decryption() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        let (key, iv) = ([7u8; 16], [9u8; 16]);
        let plain = b"0123456789 media segment bytes".to_vec();
        let enc = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(&plain);
        assert_eq!(decrypt_segment(&enc, &key, &iv).unwrap(), plain);
        assert!(decrypt_segment(&enc, &[1u8; 16], &iv).is_err());
    }

    #[test]
    fn output_names_follow_the_content() {
        assert_eq!(with_extension(Path::new("C:/v/Clip.ts"), "mp4"), PathBuf::from("C:/v/Clip.mp4"));
        assert_eq!(with_extension(Path::new("C:/v/Clip.mp4"), "mp4"), PathBuf::from("C:/v/Clip.mp4"));
        assert_eq!(with_extension(Path::new("C:/v/Clip.mp4"), "ts"), PathBuf::from("C:/v/Clip.ts"));
        assert_eq!(with_extension(Path::new("C:/v/Part 1.5"), "mp4"), PathBuf::from("C:/v/Part 1.5.mp4"));
    }
}
