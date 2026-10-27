//! Joins WebM / Matroska streams (for example VP9 video and Opus audio from
//! DASH or YouTube) into one file, like classic download managers do for WebM qualities.
//!
//! The inputs are `EBML Segment(Info Tracks Cluster*)` files, possibly cut
//! into parts (clusters of unknown size, repeated headers). The output has
//! one segment with all tracks, clusters that mix the tracks in time order
//! (a new cluster at each video keyframe) and cues, so players play the
//! sound and seek.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;
const TAGS: u32 = 0x1254_C367;
const CHAPTERS: u32 = 0x1043_A770;
const ATTACHMENTS: u32 = 0x1941_A469;
const TIMECODE_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const DEFAULT_DURATION: u32 = 0x23_E383;
const TIMECODE: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const REFERENCE_BLOCK: u32 = 0xFB;
/// Elements at segment level: they end a cluster of unknown size.
const LEVEL1: [u32; 9] = [SEEK_HEAD, INFO, TRACKS, CLUSTER, CUES, TAGS, CHAPTERS, ATTACHMENTS, EBML];
const UNKNOWN: u64 = u64::MAX;
/// Output time unit: 1 ms.
const OUT_SCALE: i64 = 1_000_000;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// True when the file starts with an EBML header (WebM, MKV).
pub fn looks_like_webm(path: &Path) -> bool {
    let mut head = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && u32::from_be_bytes(head) == EBML
}

// ---- reading ----

/// Element ID at `at` in `d`: (ID with its marker bits, length).
fn id_at(d: &[u8], at: usize) -> Option<(u32, usize)> {
    let first = *d.get(at)?;
    let n = first.leading_zeros() as usize + 1;
    if n > 4 || at + n > d.len() {
        return None;
    }
    Some((d[at..at + n].iter().fold(0u32, |v, b| (v << 8) | u32::from(*b)), n))
}

/// Element size at `at` in `d`: (size or UNKNOWN, length).
fn size_at(d: &[u8], at: usize) -> Option<(u64, usize)> {
    let first = *d.get(at)?;
    let n = first.leading_zeros() as usize + 1;
    if n > 8 || at + n > d.len() {
        return None;
    }
    let mask = (1u64 << (8 - n)) - 1;
    let mut v = u64::from(first) & mask;
    let mut ones = v == mask;
    for b in &d[at + 1..at + n] {
        ones &= *b == 0xFF;
        v = (v << 8) | u64::from(*b);
    }
    Some((if ones { UNKNOWN } else { v }, n))
}

/// Child elements of `d`: (ID, header start, data start, data end).
fn children(d: &[u8]) -> Vec<(u32, usize, usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some((id, idn)) = id_at(d, at) {
        let Some((size, sn)) = size_at(d, at + idn) else { break };
        let start = at + idn + sn;
        let end = if size == UNKNOWN { d.len() } else { start.saturating_add(size as usize).min(d.len()) };
        out.push((id, at, start, end));
        at = end;
    }
    out
}

fn uint(d: &[u8]) -> u64 {
    d.iter().take(8).fold(0u64, |v, b| (v << 8) | u64::from(*b))
}

fn child(d: &[u8], id: u32) -> Option<&[u8]> {
    children(d).into_iter().find(|c| c.0 == id).map(|(_, _, s, e)| &d[s..e])
}

/// Track number, relative time and flags at the start of a (Simple)Block.
fn block_header(d: &[u8]) -> Option<(u64, usize, i16, u8)> {
    let (track, n) = size_at(d, 0)?;
    let rel = i16::from_be_bytes([*d.get(n)?, *d.get(n + 1)?]);
    Some((track, n, rel, *d.get(n + 2)?))
}

struct InTrack {
    entry: Vec<u8>,
    number: u64,
    kind: u64,
    codec: String,
    default_duration: u64,
}

struct Block {
    /// Presentation time in ns.
    time: i64,
    track: u64,
    key: bool,
    group: bool,
    payload: Vec<u8>,
}

struct Input {
    r: BufReader<File>,
    pos: u64,
    len: u64,
    scale: i64,
    tracks: Vec<InTrack>,
    pending: VecDeque<Block>,
    done: bool,
}

impl Input {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut input = Input {
            r: BufReader::with_capacity(1 << 20, file),
            pos: 0,
            len,
            scale: OUT_SCALE,
            tracks: Vec::new(),
            pending: VecDeque::new(),
            done: false,
        };
        // Header elements up to the first cluster.
        while input.pos < input.len {
            let start = input.pos;
            let id = input.read_id()?;
            let size = input.read_size()?;
            match id {
                SEGMENT => {} // its children follow
                CLUSTER => {
                    input.seek_to(start)?;
                    break;
                }
                INFO if size != UNKNOWN => {
                    let info = input.read_bytes(size)?;
                    if let Some(s) = child(&info, TIMECODE_SCALE) {
                        input.scale = (uint(s) as i64).max(1);
                    }
                }
                TRACKS if size != UNKNOWN => {
                    let tracks = input.read_bytes(size)?;
                    for (cid, _, s, e) in children(&tracks) {
                        if cid != TRACK_ENTRY {
                            continue;
                        }
                        let entry = tracks[s..e].to_vec();
                        input.tracks.push(InTrack {
                            number: child(&entry, TRACK_NUMBER).map(uint).unwrap_or(0),
                            kind: child(&entry, TRACK_TYPE).map(uint).unwrap_or(0),
                            codec: child(&entry, CODEC_ID).map(|c| String::from_utf8_lossy(c).trim_end_matches('\0').to_string()).unwrap_or_default(),
                            default_duration: child(&entry, DEFAULT_DURATION).map(uint).unwrap_or(0),
                            entry,
                        });
                    }
                }
                _ if size == UNKNOWN => return Err(invalid("unexpected element of unknown size")),
                _ => input.skip(size)?,
            }
        }
        if input.tracks.is_empty() {
            return Err(invalid("no tracks in the WebM file"));
        }
        Ok(input)
    }

    fn byte(&mut self) -> io::Result<u8> {
        let mut b = [0u8];
        self.r.read_exact(&mut b)?;
        self.pos += 1;
        Ok(b[0])
    }

    fn read_vint(&mut self, max: usize) -> io::Result<Vec<u8>> {
        let first = self.byte()?;
        let n = first.leading_zeros() as usize + 1;
        if n > max {
            return Err(invalid("damaged WebM element"));
        }
        let mut v = vec![first];
        for _ in 1..n {
            v.push(self.byte()?);
        }
        Ok(v)
    }

    fn read_id(&mut self) -> io::Result<u32> {
        let v = self.read_vint(4)?;
        Ok(id_at(&v, 0).ok_or_else(|| invalid("damaged WebM element"))?.0)
    }

    fn read_size(&mut self) -> io::Result<u64> {
        let v = self.read_vint(8)?;
        Ok(size_at(&v, 0).ok_or_else(|| invalid("damaged WebM element"))?.0)
    }

    fn read_bytes(&mut self, n: u64) -> io::Result<Vec<u8>> {
        if n > self.len.saturating_sub(self.pos) {
            return Err(invalid("WebM element runs past the end of the file"));
        }
        let mut v = vec![0u8; n as usize];
        self.r.read_exact(&mut v)?;
        self.pos += n;
        Ok(v)
    }

    fn skip(&mut self, n: u64) -> io::Result<()> {
        let n = n.min(self.len.saturating_sub(self.pos));
        self.r.seek_relative(n as i64)?;
        self.pos += n;
        Ok(())
    }

    fn seek_to(&mut self, at: u64) -> io::Result<()> {
        self.r.seek(SeekFrom::Start(at))?;
        self.pos = at;
        Ok(())
    }

    /// Reads clusters until one has blocks (or the file ends).
    fn fill(&mut self) -> io::Result<()> {
        while self.pending.is_empty() && !self.done {
            if self.len.saturating_sub(self.pos) < 2 {
                self.done = true;
                break;
            }
            let id = self.read_id()?;
            let size = self.read_size()?;
            match id {
                SEGMENT => {} // a joined copy of the whole file: read on
                CLUSTER => self.read_cluster(size)?,
                // Headers repeated by a stream that was cut into parts.
                _ if size == UNKNOWN => {}
                _ => self.skip(size)?,
            }
        }
        Ok(())
    }

    fn read_cluster(&mut self, size: u64) -> io::Result<()> {
        let end = if size == UNKNOWN { self.len } else { self.pos.saturating_add(size).min(self.len) };
        let mut timecode = 0i64;
        while self.pos + 2 <= end {
            let start = self.pos;
            let id = self.read_id()?;
            if size == UNKNOWN && LEVEL1.contains(&id) {
                self.seek_to(start)?; // the next cluster or header
                return Ok(());
            }
            let n = self.read_size()?;
            if n == UNKNOWN {
                return Err(invalid("damaged WebM cluster"));
            }
            match id {
                TIMECODE => timecode = uint(&self.read_bytes(n)?) as i64,
                SIMPLE_BLOCK => {
                    let payload = self.read_bytes(n)?;
                    if let Some((track, _, rel, flags)) = block_header(&payload) {
                        let time = (timecode + i64::from(rel)) * self.scale;
                        self.pending.push_back(Block { time, track, key: flags & 0x80 != 0, group: false, payload });
                    }
                }
                BLOCK_GROUP => {
                    let payload = self.read_bytes(n)?;
                    let header = child(&payload, BLOCK).and_then(block_header);
                    if let Some((track, _, rel, _)) = header {
                        let time = (timecode + i64::from(rel)) * self.scale;
                        let key = child(&payload, REFERENCE_BLOCK).is_none();
                        self.pending.push_back(Block { time, track, key, group: true, payload });
                    }
                }
                _ => self.skip(n)?,
            }
        }
        self.seek_to(end)
    }

    fn next(&mut self) -> io::Result<Option<Block>> {
        self.fill()?;
        Ok(self.pending.pop_front())
    }

    fn peek_time(&mut self) -> io::Result<Option<i64>> {
        self.fill()?;
        Ok(self.pending.front().map(|b| b.time))
    }
}

// ---- writing ----

fn put_id(out: &mut Vec<u8>, id: u32) {
    let b = id.to_be_bytes();
    out.extend_from_slice(&b[(id.leading_zeros() / 8) as usize..]);
}

fn put_size(out: &mut Vec<u8>, n: u64) {
    let mut len = 1;
    while len < 8 && n >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let v = n | (1u64 << (7 * len));
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

/// An 8-byte size, so the value can be written again later.
fn size8(n: u64) -> [u8; 8] {
    (n | (1u64 << 56)).to_be_bytes()
}

fn element(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 12);
    put_id(&mut out, id);
    put_size(&mut out, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

fn uint_el(id: u32, v: u64) -> Vec<u8> {
    let b = v.to_be_bytes();
    let skip = ((v.leading_zeros() / 8) as usize).min(7);
    element(id, &b[skip..])
}

fn ebml_header(doc_type: &str) -> Vec<u8> {
    let body = [
        uint_el(0x4286, 1),
        uint_el(0x42F7, 1),
        uint_el(0x42F2, 4),
        uint_el(0x42F3, 8),
        element(0x4282, doc_type.as_bytes()),
        uint_el(0x4287, 4),
        uint_el(0x4285, 2),
    ]
    .concat();
    element(EBML, &body)
}

/// SeekHead with fixed-size entries (for Info, Tracks, Cues).
fn seek_head(entries: &[(u32, u64)]) -> Vec<u8> {
    let mut body = Vec::new();
    for &(id, pos) in entries {
        let seek = [element(0x53AB, &id.to_be_bytes()), element(0x53AC, &pos.to_be_bytes())].concat();
        body.extend(element(0x4DBB, &seek));
    }
    element(SEEK_HEAD, &body)
}

/// Info with a fixed size (the duration is written again at the end).
fn info(duration_ms: f64) -> Vec<u8> {
    let body = [
        uint_el(TIMECODE_SCALE, OUT_SCALE as u64),
        element(0x4D80, b"Klippo"),
        element(0x5741, b"Klippo"),
        element(DURATION, &duration_ms.to_be_bytes()),
    ]
    .concat();
    element(INFO, &body)
}

/// The track entry with a new number and UID.
fn track_entry(entry: &[u8], number: u64) -> Vec<u8> {
    let uid = number.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut body = Vec::new();
    let mut has_uid = false;
    for (id, h, _, e) in children(entry) {
        match id {
            TRACK_NUMBER => body.extend(uint_el(TRACK_NUMBER, number)),
            TRACK_UID => {
                has_uid = true;
                body.extend(uint_el(TRACK_UID, uid));
            }
            _ => body.extend_from_slice(&entry[h..e]),
        }
    }
    if !has_uid {
        body.extend(uint_el(TRACK_UID, uid));
    }
    element(TRACK_ENTRY, &body)
}

/// A (Simple)Block payload with the output track number and relative time.
fn rewrite_block(d: &[u8], track: u64, rel: i16) -> Option<Vec<u8>> {
    let (_, n, _, flags) = block_header(d)?;
    let mut out = Vec::with_capacity(d.len() + 2);
    put_size(&mut out, track);
    out.extend_from_slice(&rel.to_be_bytes());
    out.push(flags);
    out.extend_from_slice(&d[n + 3..]);
    Some(out)
}

fn block_element(b: &Block, track: u64, rel: i16) -> Option<Vec<u8>> {
    if !b.group {
        return Some(element(SIMPLE_BLOCK, &rewrite_block(&b.payload, track, rel)?));
    }
    let mut body = Vec::with_capacity(b.payload.len() + 4);
    for (id, h, s, e) in children(&b.payload) {
        if id == BLOCK {
            body.extend(element(BLOCK, &rewrite_block(&b.payload[s..e], track, rel)?));
        } else {
            body.extend_from_slice(&b.payload[h..e]);
        }
    }
    Some(element(BLOCK_GROUP, &body))
}

/// Joins the tracks of `inputs` into one WebM (or MKV for other codecs);
/// returns the file extension that fits ("webm" or "mkv").
pub fn remux(inputs: &[&Path], output: &Path) -> io::Result<&'static str> {
    let mut ins = inputs.iter().map(|p| Input::open(p)).collect::<io::Result<Vec<_>>>()?;

    // Output track numbers: inputs in order, then their tracks.
    let mut map: Vec<(usize, u64, u64)> = Vec::new(); // (input, input track, output track)
    let mut entries = Vec::new();
    let mut lead = 0u64;
    let mut webm = true;
    let mut default_ms = Vec::new();
    for (i, input) in ins.iter().enumerate() {
        for t in &input.tracks {
            let number = map.len() as u64 + 1;
            map.push((i, t.number, number));
            entries.extend(track_entry(&t.entry, number));
            if t.kind == 1 && lead == 0 {
                lead = number;
            }
            webm &= ["V_VP8", "V_VP9", "V_AV1", "A_OPUS", "A_VORBIS"].contains(&t.codec.as_str());
            default_ms.push(t.default_duration as f64 / 1e6);
        }
    }
    let video = lead != 0;
    if lead == 0 {
        lead = 1;
    }

    let mut out = BufWriter::with_capacity(1 << 20, File::create(output)?);
    let header = ebml_header(if webm { "webm" } else { "matroska" });
    out.write_all(&header)?;
    let mut segment_head = Vec::new();
    put_id(&mut segment_head, SEGMENT);
    out.write_all(&segment_head)?;
    out.write_all(&size8(0))?;
    let segment_data = (header.len() + segment_head.len() + 8) as u64;

    let mut pos = 0u64; // inside the segment
    let blank_seek = seek_head(&[(INFO, 0), (TRACKS, 0), (CUES, 0)]);
    out.write_all(&blank_seek)?;
    pos += blank_seek.len() as u64;
    let info_pos = pos;
    let blank_info = info(0.0);
    out.write_all(&blank_info)?;
    pos += blank_info.len() as u64;
    let tracks_pos = pos;
    let tracks = element(TRACKS, &entries);
    out.write_all(&tracks)?;
    pos += tracks.len() as u64;

    // Clusters: blocks of all tracks in time order, each track in its own order.
    let mut cues: Vec<(i64, u64)> = Vec::new();
    let mut cluster: Option<(i64, Vec<u8>, bool)> = None; // (time, body, starts at a keyframe)
    let mut last_end = vec![0f64; map.len()];
    let flush = |c: Option<(i64, Vec<u8>, bool)>, out: &mut BufWriter<File>, pos: &mut u64, cues: &mut Vec<(i64, u64)>| -> io::Result<()> {
        if let Some((tc, body, key)) = c {
            if key {
                cues.push((tc, *pos));
            }
            let el = element(CLUSTER, &body);
            out.write_all(&el)?;
            *pos += el.len() as u64;
        }
        Ok(())
    };
    loop {
        let mut next: Option<(usize, i64)> = None;
        for (i, input) in ins.iter_mut().enumerate() {
            if let Some(t) = input.peek_time()? {
                if next.is_none_or(|(_, best)| t < best) {
                    next = Some((i, t));
                }
            }
        }
        let Some((i, _)) = next else { break };
        let Some(block) = ins[i].next()? else { continue };
        let Some(&(_, _, track)) = map.iter().find(|m| m.0 == i && m.1 == block.track) else { continue };
        let ms = block.time.div_euclid(OUT_SCALE) + i64::from(block.time.rem_euclid(OUT_SCALE) >= OUT_SCALE / 2);
        let lead_key = track == lead && block.key;
        let start_new = match cluster {
            None => true,
            Some((tc, ref body, _)) => {
                let span = ms - tc;
                (lead_key && span > 0 && (video || span >= 5000)) || !(-30_000..=30_000).contains(&span) || body.len() > 32 << 20
            }
        };
        if start_new {
            flush(cluster.take(), &mut out, &mut pos, &mut cues)?;
            cluster = Some((ms, uint_el(TIMECODE, ms.max(0) as u64), lead_key));
        }
        let (tc, body, _) = cluster.as_mut().expect("a cluster is open");
        let rel = i16::try_from(ms - *tc).map_err(|_| invalid("block time out of range"))?;
        body.extend(block_element(&block, track, rel).ok_or_else(|| invalid("damaged WebM block"))?);
        let index = (track - 1) as usize;
        last_end[index] = last_end[index].max(ms as f64 + default_ms[index]);
    }
    flush(cluster.take(), &mut out, &mut pos, &mut cues)?;

    let cues_pos = pos;
    let mut cue_body = Vec::new();
    for (tc, cluster_pos) in &cues {
        let positions = [uint_el(0xF7, lead), uint_el(0xF1, *cluster_pos)].concat();
        let point = [uint_el(0xB3, (*tc).max(0) as u64), element(0xB7, &positions)].concat();
        cue_body.extend(element(0xBB, &point));
    }
    let cues_el = element(CUES, &cue_body);
    out.write_all(&cues_el)?;
    pos += cues_el.len() as u64;

    // Sizes and positions known only now.
    let duration = last_end.iter().cloned().fold(0f64, f64::max);
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(segment_data - 8))?;
    file.write_all(&size8(pos))?;
    file.write_all(&seek_head(&[(INFO, info_pos), (TRACKS, tracks_pos), (CUES, cues_pos)]))?;
    file.seek(SeekFrom::Start(segment_data + info_pos))?;
    file.write_all(&info(duration))?;
    file.flush()?;
    Ok(if webm { "webm" } else { "mkv" })
}

/// Joins `video` and `audio` into `output`.
pub fn merge(video: &Path, audio: &Path, output: &Path) -> io::Result<&'static str> {
    remux(&[video, audio], output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown_size(id: u32) -> Vec<u8> {
        let mut v = Vec::new();
        put_id(&mut v, id);
        v.push(0xFF);
        v
    }

    fn simple(track: u8, rel: i16, key: bool) -> Vec<u8> {
        let mut p = vec![0x80 | track];
        p.extend_from_slice(&rel.to_be_bytes());
        p.push(if key { 0x80 } else { 0 });
        p.extend_from_slice(&[track; 10]);
        element(SIMPLE_BLOCK, &p)
    }

    fn input(kind: u64, codec: &str, clusters: &[(u64, Vec<(i16, bool)>)], known_sizes: bool) -> Vec<u8> {
        let entry = [uint_el(TRACK_NUMBER, 1), uint_el(TRACK_UID, 77), uint_el(TRACK_TYPE, kind), element(CODEC_ID, codec.as_bytes())].concat();
        let mut file = ebml_header("webm");
        file.extend(unknown_size(SEGMENT));
        file.extend(element(INFO, &uint_el(TIMECODE_SCALE, 1_000_000)));
        file.extend(element(TRACKS, &element(TRACK_ENTRY, &entry)));
        for (tc, blocks) in clusters {
            let mut body = uint_el(TIMECODE, *tc);
            for (rel, key) in blocks {
                body.extend(simple(1, *rel, *key));
            }
            if known_sizes {
                file.extend(element(CLUSTER, &body));
            } else {
                file.extend(unknown_size(CLUSTER));
                file.extend(body);
            }
        }
        file
    }

    #[test]
    fn joins_video_and_audio_in_time_order() {
        let dir = std::env::temp_dir().join(format!("klippo_mkvmux_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (v, a, o) = (dir.join("v.webm"), dir.join("a.webm"), dir.join("o.webm"));
        // Video: two GOPs; the second cluster has an unknown size, like a stream part.
        std::fs::write(&v, input(1, "V_VP9", &[(0, vec![(0, true), (33, false), (66, false)]), (100, vec![(0, true), (33, false)])], false)).unwrap();
        std::fs::write(&a, input(2, "A_OPUS", &[(0, (0..7).map(|i| (i * 20, true)).collect())], true)).unwrap();
        assert!(looks_like_webm(&v));
        assert_eq!(merge(&v, &a, &o).unwrap(), "webm");

        let data = std::fs::read(&o).unwrap();
        let top = children(&data);
        assert_eq!(top[0].0, EBML);
        assert_eq!(child(&data[top[0].2..top[0].3], 0x4282), Some(&b"webm"[..]));
        assert_eq!(top[1].0, SEGMENT);
        assert_eq!(top[1].3, data.len(), "segment size covers the file");

        let mut out = Input::open(&o).unwrap();
        assert_eq!(out.tracks.iter().map(|t| (t.number, t.kind)).collect::<Vec<_>>(), vec![(1, 1), (2, 2)]);
        let mut seen = Vec::new();
        while let Some(b) = out.next().unwrap() {
            seen.push((b.track, b.time / 1_000_000, b.key));
        }
        assert_eq!(seen.iter().filter(|s| s.0 == 1).count(), 5);
        assert_eq!(seen.iter().filter(|s| s.0 == 2).count(), 7);
        assert!(seen.windows(2).all(|w| w[0].1 <= w[1].1), "time order: {:?}", seen);

        let segment = &data[top[1].2..];
        let cues = child(segment, CUES).unwrap();
        let points: Vec<u64> = children(cues).iter().map(|(_, _, s, e)| child(&cues[*s..*e], 0xB3).map(uint).unwrap()).collect();
        assert_eq!(points, vec![0, 100], "a cue per video keyframe");
        let info = child(segment, INFO).unwrap();
        let duration = f64::from_be_bytes(child(info, DURATION).unwrap().try_into().unwrap());
        assert!((133.0..=134.0).contains(&duration), "{}", duration);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
