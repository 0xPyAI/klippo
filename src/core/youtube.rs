//! YouTube downloads through SABR, the protocol YouTube's web player streams with.
//!
//! YouTube gives the web player no plain file addresses any more. The player
//! POSTs a protobuf request to a `videoplayback?sabr=1` address and receives a
//! UMP stream: media headers, media data, and policies for the next request.
//! The browser extension records one of these requests from the player (the
//! address already carries the player's `n` value; the body carries the stream
//! configuration, client info and the proof-of-origin token). This module sends
//! that request again with the chosen formats and the playback position moved
//! forward until every segment of the video and audio formats has arrived, and
//! then joins the two streams with [`super::mp4mux`] (MP4) or
//! [`super::mkvmux`] (VP9 WebM qualities).

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, ORIGIN, REFERER, USER_AGENT};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use super::engine::{wait_for_true, DownloadStopped};
use super::hls::{HlsControl, HlsProgress};
use super::http::{self, RequestOptions};
use super::{mkvmux, mp4mux};
use super::protobuf::{self, Field, Writer};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// One stream of a YouTube video (an `adaptiveFormats` entry).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct YoutubeFormat {
    pub itag: u32,
    /// The format's `lastModified` value; with the itag it names the stream.
    #[serde(deserialize_with = "number_or_string")]
    pub lmt: u64,
    /// Variant tags (for example dynamic range compression of the audio).
    pub xtags: String,
    pub size: Option<u64>,
    pub mime: String,
}

/// YouTube writes `lastModified` as a string; JavaScript numbers lose digits above 2^53.
fn number_or_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(u64),
        Text(String),
    }
    match Raw::deserialize(d)? {
        Raw::Number(n) => Ok(n),
        Raw::Text(s) => s.trim().parse().map_err(serde::de::Error::custom),
    }
}

/// What the browser extension sends for a YouTube download.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct YoutubeSpec {
    pub video_id: String,
    /// The `videoplayback?sabr=1` address the player used.
    pub sabr_url: String,
    /// The player's request body (protobuf, base64).
    pub template: String,
    /// None downloads the audio only.
    pub video: Option<YoutubeFormat>,
    pub audio: YoutubeFormat,
    pub duration_ms: u64,
    /// A live stream: Klippo does not record live streams and refuses it.
    pub live: bool,
}

impl YoutubeSpec {
    pub fn total_size(&self) -> Option<u64> {
        let video = match &self.video {
            Some(v) => v.size?,
            None => 0,
        };
        Some(video + self.audio.size?)
    }

    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.video_id)
    }

    fn template_bytes(&self) -> Result<Vec<u8>, BoxError> {
        let t = self.template.trim();
        base64::engine::general_purpose::STANDARD
            .decode(t)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(t))
            .map_err(|_| "The recorded YouTube request is damaged.".into())
    }
}

// UMP part types.
const MEDIA_HEADER: u64 = 20;
const MEDIA: u64 = 21;
const MEDIA_END: u64 = 22;
const NEXT_REQUEST_POLICY: u64 = 35;
const FORMAT_INITIALIZATION_METADATA: u64 = 42;
const SABR_REDIRECT: u64 = 43;
const SABR_ERROR: u64 = 44;
const RELOAD_PLAYER_RESPONSE: u64 = 46;
const SABR_CONTEXT_UPDATE: u64 = 57;
const STREAM_PROTECTION_STATUS: u64 = 58;
const SABR_CONTEXT_SENDING_POLICY: u64 = 59;

/// UMP length/type numbers: the count of leading 1 bits in the first byte
/// says how many bytes follow (little-endian).
fn ump_varint(buf: &[u8], pos: usize) -> Option<(u64, usize)> {
    let first = u64::from(*buf.get(pos)?);
    let byte = |i: usize| buf.get(pos + i).map(|b| u64::from(*b));
    Some(match first {
        0..=0x7F => (first, 1),
        0x80..=0xBF => ((first & 0x3F) + 64 * byte(1)?, 2),
        0xC0..=0xDF => ((first & 0x1F) + 32 * (byte(1)? + 256 * byte(2)?), 3),
        0xE0..=0xEF => ((first & 0x0F) + 16 * (byte(1)? + 256 * (byte(2)? + 256 * byte(3)?)), 4),
        _ => (u64::from(u32::from_le_bytes(buf.get(pos + 1..pos + 5)?.try_into().ok()?)), 5),
    })
}

/// Splits a UMP byte stream into parts as the bytes arrive.
#[derive(Default)]
struct UmpReader {
    buf: Vec<u8>,
    pos: usize,
}

impl UmpReader {
    fn push(&mut self, data: &[u8]) {
        if self.pos > 0 && self.pos >= self.buf.len() / 2 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(data);
    }

    fn next_part(&mut self) -> Option<(u64, Vec<u8>)> {
        let (kind, a) = ump_varint(&self.buf, self.pos)?;
        let (len, b) = ump_varint(&self.buf, self.pos + a)?;
        let start = self.pos + a + b;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        if end > self.buf.len() {
            return None;
        }
        self.pos = end;
        Some((kind, self.buf[start..end].to_vec()))
    }

    fn leftover(&self) -> usize {
        self.buf.len() - self.pos
    }
}

/// Saved between runs so a paused download continues where it stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct TrackState {
    itag: u32,
    init: bool,
    /// Next media segment to write (segments start at 1).
    next_seq: u32,
    end_seq: Option<u32>,
    end_ms: u64,
    bytes: u64,
    /// First segment written and its start time.
    first_seq: u32,
    first_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedState {
    video: Option<TrackState>,
    audio: TrackState,
    url: String,
}

struct Track {
    format: YoutubeFormat,
    state: TrackState,
    file: File,
    /// Segments that arrived before the ones in front of them.
    pending: BTreeMap<u32, (Vec<u8>, u64)>,
}

impl Track {
    fn open(path: &Path, format: &YoutubeFormat, saved: Option<&TrackState>) -> std::io::Result<Self> {
        let mut file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
        let mut state = match saved {
            Some(s) if s.itag == format.itag && s.bytes <= file.metadata()?.len() => s.clone(),
            _ => TrackState { itag: format.itag, next_seq: 1, first_seq: 1, ..Default::default() },
        };
        if state.next_seq == 0 {
            state.next_seq = 1;
        }
        if state.first_seq == 0 {
            state.first_seq = 1; // saved by an older version
        }
        // Drop bytes written after the last saved state.
        file.set_len(state.bytes)?;
        file.seek(SeekFrom::End(0))?;
        Ok(Self { format: format.clone(), state, file, pending: BTreeMap::new() })
    }

    fn done(&self) -> bool {
        self.state.end_seq.is_some_and(|end| self.state.next_seq > end)
    }

    fn matches(&self, h: &PartHeader) -> bool {
        h.itag == self.format.itag && (h.lmt == 0 || self.format.lmt == 0 || h.lmt == self.format.lmt)
    }

    /// Stores a finished part; returns the bytes written to disk.
    fn accept(&mut self, h: PartHeader) -> std::io::Result<u64> {
        let mut written = 0;
        if h.init {
            if !self.state.init {
                self.file.write_all(&h.data)?;
                self.state.init = true;
                written += h.data.len() as u64;
            }
        } else if h.seq >= self.state.next_seq {
            // A segment without its length: YouTube's segments are 5 s or less.
            let duration = if h.duration_ms > 0 { h.duration_ms } else { 5000 };
            self.pending.insert(h.seq, (h.data, h.start_ms + duration));
        }
        if self.state.init {
            while let Some((data, end_ms)) = self.pending.remove(&self.state.next_seq) {
                self.file.write_all(&data)?;
                written += data.len() as u64;
                self.state.end_ms = end_ms;
                self.state.next_seq += 1;
            }
        }
        self.state.bytes += written;
        Ok(written)
    }

    fn format_id(&self) -> Vec<u8> {
        format_id(&self.format)
    }
}

fn format_id(f: &YoutubeFormat) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(1, u64::from(f.itag)).varint(2, f.lmt);
    if !f.xtags.is_empty() {
        w.bytes(3, f.xtags.as_bytes());
    }
    w.finish()
}

#[derive(Debug, Default)]
struct PartHeader {
    itag: u32,
    lmt: u64,
    init: bool,
    seq: u32,
    start_ms: u64,
    duration_ms: u64,
    data: Vec<u8>,
}

impl PartHeader {
    fn parse(payload: &[u8]) -> Option<(u64, Self)> {
        let f = protobuf::parse(payload)?;
        let id = protobuf::varint(&f, 1)?;
        Some((
            id,
            Self {
                itag: protobuf::varint(&f, 3).unwrap_or(0) as u32,
                lmt: protobuf::varint(&f, 4).unwrap_or(0),
                init: protobuf::varint(&f, 8).unwrap_or(0) != 0,
                seq: protobuf::varint(&f, 9).unwrap_or(0) as u32,
                start_ms: protobuf::varint(&f, 11).unwrap_or(0),
                duration_ms: protobuf::varint(&f, 12).unwrap_or(0),
                data: Vec::new(),
            },
        ))
    }
}

/// The parts of the player's request that are sent back unchanged.
struct Template {
    /// Top-level fields other than the ones rebuilt per request.
    top: Vec<Field>,
    /// Client ABR state (field 1).
    abr: Vec<Field>,
    /// Streamer context (field 19): client info and the proof-of-origin token.
    context: Vec<Field>,
}

impl Template {
    fn parse(body: &[u8]) -> Result<Self, BoxError> {
        let fields = protobuf::parse(body).ok_or("The recorded YouTube request is not readable.")?;
        let abr = protobuf::message(&fields, 1).unwrap_or_default();
        let context = protobuf::message(&fields, 19).ok_or("The recorded YouTube request has no streamer context.")?;
        let top = fields.into_iter().filter(|f| ![1, 2, 3, 16, 17, 19].contains(&f.number)).collect();
        Ok(Self { top, abr, context })
    }
}

/// What one response changed.
#[derive(Default)]
struct Outcome {
    segments: usize,
    backoff_ms: u64,
}

enum FetchError {
    Stopped,
    Retry(String),
    Fatal(String),
}

pub struct YoutubeGrabber {
    spec: YoutubeSpec,
    output_path: PathBuf,
    parts_dir: PathBuf,
    request: RequestOptions,
    progress: Arc<HlsProgress>,
    cancel_tx: Arc<watch::Sender<bool>>,
    cancel_rx: watch::Receiver<bool>,
}

impl YoutubeGrabber {
    pub const MAX_RETRIES: u32 = 6;
    /// Responses in a row without a new segment before giving up.
    const MAX_STALLS: u32 = 8;

    pub fn new<P: AsRef<Path>>(spec: YoutubeSpec, output_path: P) -> Self {
        let out = output_path.as_ref().to_path_buf();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        Self {
            spec,
            parts_dir: Self::parts_dir_for(&out),
            output_path: out,
            request: RequestOptions::default(),
            progress: Arc::new(HlsProgress::default()),
            cancel_tx: Arc::new(cancel_tx),
            cancel_rx,
        }
    }

    /// Folder that holds the two streams until they are joined.
    pub fn parts_dir_for(output_path: &Path) -> PathBuf {
        super::parts::parts_dir(output_path, ".yt_parts")
    }

    pub fn set_request_options(&mut self, options: RequestOptions) {
        self.request = options;
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

    fn state_path(&self) -> PathBuf {
        self.parts_dir.join("state.json")
    }

    fn save(&self, tracks: &[Track], url: &str) {
        let state = SavedState {
            video: tracks.iter().find(|t| Some(&t.format) == self.spec.video.as_ref()).map(|t| t.state.clone()),
            audio: tracks.iter().find(|t| t.format == self.spec.audio).map(|t| t.state.clone()).unwrap_or_default(),
            url: url.to_string(),
        };
        if let Ok(json) = serde_json::to_string(&state) {
            let tmp = self.state_path().with_extension("tmp");
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(&tmp, self.state_path());
            }
        }
    }

    fn body(&self, tpl: &Template, tracks: &[Track], cookie: Option<&[u8]>, contexts: &BTreeMap<u64, Vec<u8>>) -> Vec<u8> {
        let audio_only = self.spec.video.is_none();
        let player_time = tracks.iter().filter(|t| !t.done()).map(|t| t.state.end_ms).min().unwrap_or(0);

        let mut abr = Writer::new();
        for f in tpl.abr.iter().filter(|f| f.number != 28 && f.number != 40) {
            abr.field(f);
        }
        // 28: playback position; 40: enabled track types (1 audio, 3 audio and video).
        abr.varint(28, player_time).varint(40, if audio_only { 1 } else { 3 });

        let mut w = Writer::new();
        w.bytes(1, &abr.finish());
        for t in tracks.iter().filter(|t| t.state.init) {
            w.bytes(2, &t.format_id());
        }
        for t in tracks.iter().filter(|t| t.state.next_seq > t.state.first_seq) {
            let mut range = Writer::new();
            range
                .bytes(1, &t.format_id())
                .varint(2, t.state.first_ms)
                .varint(3, t.state.end_ms.saturating_sub(t.state.first_ms))
                .varint(4, u64::from(t.state.first_seq))
                .varint(5, u64::from(t.state.next_seq - 1));
            w.bytes(3, &range.finish());
        }
        for f in &tpl.top {
            w.field(f);
        }
        w.bytes(16, &format_id(&self.spec.audio));
        if let Some(ref v) = self.spec.video {
            w.bytes(17, &format_id(v));
        }
        let mut ctx = Writer::new();
        for f in tpl.context.iter().filter(|f| f.number != 3 && f.number != 5) {
            ctx.field(f);
        }
        if let Some(c) = cookie {
            ctx.bytes(3, c);
        }
        for (kind, value) in contexts {
            let mut c = Writer::new();
            c.varint(1, *kind).bytes(2, value);
            ctx.bytes(5, &c.finish());
        }
        w.bytes(19, &ctx.finish());
        w.finish()
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch(
        &self,
        client: &Client,
        url: &str,
        body: Vec<u8>,
        tracks: &mut [Track],
        cookie: &mut Option<Vec<u8>>,
        contexts: &mut BTreeMap<u64, Vec<u8>>,
        redirect: &mut Option<String>,
    ) -> Result<Outcome, FetchError> {
        let mut cancel = self.cancel_rx.clone();
        let req = client
            .post(url)
            .header(CONTENT_TYPE, "application/x-protobuf")
            .header(ACCEPT, "*/*")
            .header(ORIGIN, "https://www.youtube.com")
            .header(REFERER, "https://www.youtube.com/")
            .header(USER_AGENT, &self.request.user_agent)
            .body(body);
        let resp = tokio::select! {
            r = req.send() => r.map_err(|e| FetchError::Retry(http::describe_error(&e)))?,
            _ = wait_for_true(&mut cancel) => return Err(FetchError::Stopped),
        };
        let status = resp.status().as_u16();
        match status {
            200..=299 => {}
            401 | 403 | 410 => {
                return Err(FetchError::Fatal(format!(
                    "YouTube refused the stream (HTTP {}). The recorded session has expired: open the video page again and start the download from the video button.",
                    status
                )))
            }
            _ => return Err(FetchError::Retry(format!("HTTP {}", status))),
        }

        let committed: u64 = tracks.iter().map(|t| t.state.bytes).sum();
        let mut in_flight = 0u64;
        let mut outcome = Outcome::default();
        let mut headers: HashMap<u64, PartHeader> = HashMap::new();
        let mut reader = UmpReader::default();
        let mut stream = resp.bytes_stream();
        loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = wait_for_true(&mut cancel) => return Err(FetchError::Stopped),
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|e| FetchError::Retry(http::describe_error(&e)))?;
            reader.push(&chunk);
            while let Some((kind, payload)) = reader.next_part() {
                match kind {
                    MEDIA_HEADER => {
                        if let Some((id, h)) = PartHeader::parse(&payload) {
                            headers.insert(id, h);
                        }
                    }
                    MEDIA => {
                        let Some(id) = payload.first().map(|b| u64::from(*b)) else { continue };
                        if let Some(h) = headers.get_mut(&id) {
                            if tracks.iter().any(|t| t.matches(h)) {
                                h.data.extend_from_slice(&payload[1..]);
                                in_flight += payload.len() as u64 - 1;
                                self.progress.bytes.store(committed + in_flight, Ordering::Relaxed);
                            }
                        }
                    }
                    MEDIA_END => {
                        let Some(h) = payload.first().and_then(|id| headers.remove(&u64::from(*id))) else { continue };
                        if let Some(track) = tracks.iter_mut().find(|t| t.matches(&h)) {
                            let init = h.init;
                            track.accept(h).map_err(|e| FetchError::Fatal(format!("Cannot write the download: {}", e)))?;
                            if !init {
                                outcome.segments += 1;
                            }
                        }
                    }
                    FORMAT_INITIALIZATION_METADATA => {
                        let Some(f) = protobuf::parse(&payload) else { continue };
                        if let Some(id) = protobuf::bytes(&f, 1) {
                            if !id.is_empty() && id != self.spec.video_id.as_bytes() {
                                return Err(FetchError::Fatal(
                                    "The recorded YouTube stream belongs to another video. Play the video for a few seconds, then try again."
                                        .to_string(),
                                ));
                            }
                        }
                        let itag = protobuf::message(&f, 2).and_then(|id| protobuf::varint(&id, 1)).unwrap_or(0) as u32;
                        if let (Some(t), Some(end)) = (tracks.iter_mut().find(|t| t.format.itag == itag), protobuf::varint(&f, 4)) {
                            t.state.end_seq = Some(end as u32);
                        }
                    }
                    NEXT_REQUEST_POLICY => {
                        let Some(f) = protobuf::parse(&payload) else { continue };
                        if let Some(c) = protobuf::bytes(&f, 7) {
                            *cookie = Some(c.to_vec());
                        }
                        outcome.backoff_ms = protobuf::varint(&f, 4).unwrap_or(0);
                    }
                    SABR_REDIRECT => {
                        if let Some(u) = protobuf::parse(&payload).and_then(|f| protobuf::bytes(&f, 1).map(|b| String::from_utf8_lossy(b).into_owned())) {
                            *redirect = Some(u);
                        }
                    }
                    SABR_ERROR => {
                        let f = protobuf::parse(&payload).unwrap_or_default();
                        let kind = protobuf::bytes(&f, 1).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
                        let code = protobuf::varint(&f, 2).unwrap_or(0);
                        return Err(FetchError::Fatal(format!("YouTube stream error: {} {}", kind, code).trim().to_string()));
                    }
                    RELOAD_PLAYER_RESPONSE => {
                        return Err(FetchError::Fatal(
                            "YouTube asked to reload the player. Open the video page again and start the download from the video button."
                                .to_string(),
                        ));
                    }
                    STREAM_PROTECTION_STATUS => {
                        let status = protobuf::parse(&payload).and_then(|f| protobuf::varint(&f, 1)).unwrap_or(1);
                        if status == 3 {
                            return Err(FetchError::Fatal(
                                "YouTube wants a new proof-of-origin token. Play the video in the browser for a few seconds, then start the download again."
                                    .to_string(),
                            ));
                        }
                    }
                    SABR_CONTEXT_UPDATE => {
                        let Some(f) = protobuf::parse(&payload) else { continue };
                        if let (Some(kind), Some(value)) = (protobuf::varint(&f, 1), protobuf::bytes(&f, 3)) {
                            if protobuf::varint(&f, 4).unwrap_or(0) != 0 {
                                contexts.insert(kind, value.to_vec());
                            }
                        }
                    }
                    SABR_CONTEXT_SENDING_POLICY => {
                        let Some(f) = protobuf::parse(&payload) else { continue };
                        for field in f.iter().filter(|x| x.number == 2 || x.number == 3) {
                            if let protobuf::Value::Varint(kind) = field.value {
                                contexts.remove(&kind);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if reader.leftover() > 0 && outcome.segments == 0 {
            return Err(FetchError::Retry("The stream ended in the middle of a part".to_string()));
        }
        self.progress.bytes.store(tracks.iter().map(|t| t.state.bytes).sum(), Ordering::Relaxed);
        Ok(outcome)
    }

    pub async fn run(&self) -> Result<PathBuf, BoxError> {
        if self.spec.sabr_url.is_empty() || self.spec.template.is_empty() {
            return Err("This YouTube download has no recorded stream. Start it again from the video button on the YouTube page.".into());
        }
        if self.spec.live {
            return Err(super::hls::LIVE_STREAM.into());
        }
        let template = Template::parse(&self.spec.template_bytes()?)?;
        fs::create_dir_all(&self.parts_dir)?;
        let saved: SavedState = fs::read_to_string(self.state_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();

        let video_path = self.parts_dir.join("video.mp4");
        let audio_path = self.parts_dir.join("audio.m4a");
        let mut tracks = Vec::new();
        if let Some(ref v) = self.spec.video {
            tracks.push(Track::open(&video_path, v, saved.video.as_ref())?);
        }
        tracks.push(Track::open(&audio_path, &self.spec.audio, Some(&saved.audio))?);

        self.progress.known_total.store(self.spec.total_size().unwrap_or(0), Ordering::Relaxed);
        self.progress.total_parts.store(tracks.len(), Ordering::Relaxed);
        self.progress.bytes.store(tracks.iter().map(|t| t.state.bytes).sum(), Ordering::Relaxed);

        let client = self.request.build_client()?;
        let mut url = if saved.url.is_empty() { self.spec.sabr_url.clone() } else { saved.url.clone() };
        let mut cookie: Option<Vec<u8>> = None;
        let mut contexts: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut rn = 1u32;
        let (mut stalls, mut errors) = (0u32, 0u32);

        while !tracks.iter().all(Track::done) {
            if *self.cancel_rx.borrow() {
                self.save(&tracks, &url);
                return Err(Box::new(DownloadStopped));
            }
            let body = self.body(&template, &tracks, cookie.as_deref(), &contexts);
            let mut redirect = None;
            let result = self.fetch(&client, &with_rn(&url, rn), body, &mut tracks, &mut cookie, &mut contexts, &mut redirect).await;
            rn += 1;
            if let Some(u) = redirect {
                url = u;
            }
            self.save(&tracks, &url);
            match result {
                Ok(outcome) => {
                    errors = 0;
                    stalls = if outcome.segments == 0 { stalls + 1 } else { 0 };
                    if stalls >= Self::MAX_STALLS {
                        let started = tracks.iter().all(|t| t.state.init);
                        return Err(if started { "YouTube stopped sending data." } else { "YouTube did not send the chosen quality." }.into());
                    }
                    self.progress.done_parts.store(tracks.iter().filter(|t| t.done()).count(), Ordering::Relaxed);
                    if outcome.backoff_ms > 0 {
                        self.pause(outcome.backoff_ms.min(10_000)).await?;
                    }
                }
                Err(FetchError::Stopped) => return Err(Box::new(DownloadStopped)),
                Err(FetchError::Fatal(e)) => return Err(e.into()),
                Err(FetchError::Retry(e)) => {
                    errors += 1;
                    self.progress.bytes.store(tracks.iter().map(|t| t.state.bytes).sum(), Ordering::Relaxed);
                    if errors > Self::MAX_RETRIES {
                        return Err(format!("YouTube stream failed: {}", e).into());
                    }
                    self.pause(500 * (1 << errors.min(5))).await?;
                }
            }
        }

        drop(tracks);
        if let Some(parent) = self.output_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        if self.spec.video.is_some() && mkvmux::looks_like_webm(&video_path) {
            // VP9 qualities come as WebM with Opus audio.
            let (v, a, staged) = (video_path, audio_path, self.parts_dir.join("joined.mkv"));
            let target = staged.clone();
            let ext = tokio::task::spawn_blocking(move || mkvmux::merge(&v, &a, &target))
                .await
                .map_err(|e| format!("Internal error: {}", e))?
                .map_err(|e| format!("Cannot join the video and audio streams: {}", e))?;
            let out = self.output_path.with_extension(ext);
            let _ = fs::remove_file(&out);
            fs::rename(&staged, &out).or_else(|_| fs::copy(&staged, &out).map(|_| ()))?;
            let _ = fs::remove_dir_all(&self.parts_dir);
            return Ok(out);
        } else if self.spec.video.is_some() {
            let (v, a, out) = (video_path, audio_path, self.output_path.clone());
            tokio::task::spawn_blocking(move || mp4mux::merge(&v, &a, &out))
                .await
                .map_err(|e| format!("Internal error: {}", e))?
                .map_err(|e| format!("Cannot join the video and audio streams: {}", e))?;
        } else {
            let (a, out) = (audio_path, self.output_path.clone());
            tokio::task::spawn_blocking(move || mp4mux::remux(&[a.as_path()], &out))
                .await
                .map_err(|e| format!("Internal error: {}", e))?
                .map_err(|e| format!("Cannot write the audio file: {}", e))?;
        }
        let _ = fs::remove_dir_all(&self.parts_dir);
        Ok(self.output_path.clone())
    }

    async fn pause(&self, ms: u64) -> Result<(), BoxError> {
        let mut cancel = self.cancel_rx.clone();
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
            _ = wait_for_true(&mut cancel) => Err(Box::new(DownloadStopped)),
        }
    }
}

/// The request number (`rn`) goes up by one per request, like the player's.
fn with_rn(url: &str, rn: u32) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let pairs: Vec<(String, String)> = u.query_pairs().filter(|(k, _)| k != "rn").map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
            u.query_pairs_mut().clear().extend_pairs(pairs).append_pair("rn", &rn.to_string());
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ump_part(kind: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 128);
        let mut out = vec![kind, payload.len() as u8];
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn ump_numbers_and_split_parts() {
        assert_eq!(ump_varint(&[0x05], 0), Some((5, 1)));
        assert_eq!(ump_varint(&[0x81, 0x02], 0), Some((1 + 64 * 2, 2)));
        assert_eq!(ump_varint(&[0xC1, 0x01, 0x01], 0), Some((1 + 32 * 257, 3)));
        assert_eq!(ump_varint(&[0xF0, 1, 0, 0, 0], 0), Some((1, 5)));

        let mut stream = ump_part(20, b"head");
        stream.extend(ump_part(21, b"\x00data"));
        let mut r = UmpReader::default();
        r.push(&stream[..5]);
        assert_eq!(r.next_part(), None, "a part is returned only when complete");
        r.push(&stream[5..]);
        assert_eq!(r.next_part(), Some((20, b"head".to_vec())));
        assert_eq!(r.next_part(), Some((21, b"\x00data".to_vec())));
        assert_eq!(r.leftover(), 0);
    }

    #[test]
    fn spec_from_extension_json() {
        let spec: YoutubeSpec = serde_json::from_str(
            r#"{"video_id":"aqz-KE-bpKQ","sabr_url":"https://x/videoplayback?sabr=1","template":"CgA=",
                "video":{"itag":137,"lmt":"1719193506342742","xtags":"","size":100,"mime":"video/mp4"},
                "audio":{"itag":140,"lmt":1719185020722767,"size":20},"duration_ms":634566}"#,
        )
        .unwrap();
        assert_eq!(spec.video.as_ref().unwrap().lmt, 1_719_193_506_342_742);
        assert_eq!(spec.audio.lmt, 1_719_185_020_722_767);
        assert_eq!(spec.total_size(), Some(120));
        assert_eq!(spec.watch_url(), "https://www.youtube.com/watch?v=aqz-KE-bpKQ");
    }

    #[tokio::test]
    async fn live_stream_is_refused() {
        let spec = YoutubeSpec { video_id: "x".into(), sabr_url: "https://x/videoplayback?sabr=1".into(), template: "CgA=".into(), live: true, ..Default::default() };
        let out = std::env::temp_dir().join(format!("klippo_yt_live_{}.mp4", std::process::id()));
        let err = YoutubeGrabber::new(spec, &out).run().await.unwrap_err();
        assert_eq!(err.to_string(), super::super::hls::LIVE_STREAM);
        assert!(!out.exists() && !YoutubeGrabber::parts_dir_for(&out).exists());
    }

    #[test]
    fn rn_is_replaced() {
        assert_eq!(with_rn("https://x.googlevideo.com/videoplayback?sabr=1&rn=4&n=abc", 7), "https://x.googlevideo.com/videoplayback?sabr=1&n=abc&rn=7");
    }

    #[test]
    fn segments_are_written_in_order() {
        let dir = std::env::temp_dir().join(format!("klippo_yt_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let format = YoutubeFormat { itag: 140, lmt: 9, ..Default::default() };
        let mut t = Track::open(&dir.join("a.m4a"), &format, None).unwrap();
        let seg = |seq: u32, data: &[u8]| PartHeader { itag: 140, lmt: 9, seq, start_ms: u64::from(seq) * 10, duration_ms: 10, data: data.to_vec(), ..Default::default() };
        t.accept(seg(2, b"BB")).unwrap();
        assert_eq!(t.state.bytes, 0, "nothing before the init segment and segment 1");
        t.accept(PartHeader { itag: 140, init: true, data: b"I".to_vec(), ..Default::default() }).unwrap();
        t.accept(seg(1, b"A")).unwrap();
        t.state.end_seq = Some(2);
        assert!(t.done());
        assert_eq!(t.state.end_ms, 30);
        drop(t);
        assert_eq!(fs::read(dir.join("a.m4a")).unwrap(), b"IABB");

        let _ = fs::remove_dir_all(&dir);
    }
}
