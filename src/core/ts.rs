//! Joins a video-only MPEG-TS stream with its separate audio stream into one
//! TS file. HLS sends separate audio either as TS or as "packed audio": ADTS
//! AAC (or AC-3 / E-AC-3) frames with an ID3 tag that carries the start
//! timestamp.
//!
//! The packets of both streams are written in time order (PES DTS/PTS, 90 kHz)
//! after a new PAT and PMT that list the video's and the audio's streams.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const PACKET: usize = 188;
const NULL_PID: u16 = 0x1FFF;
/// Written again after this many packets so players can join anywhere.
const TABLE_INTERVAL: u64 = 4000;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

type Packet = [u8; PACKET];

fn pid_of(p: &Packet) -> u16 {
    (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2])
}

fn set_pid(p: &mut Packet, pid: u16) {
    p[1] = (p[1] & 0xE0) | ((pid >> 8) as u8 & 0x1F);
    p[2] = pid as u8;
}

/// Payload of a packet (after the adaptation field).
fn payload(p: &Packet) -> &[u8] {
    let control = (p[3] >> 4) & 0x3;
    if control & 0x1 == 0 {
        return &[];
    }
    let start = if control & 0x2 != 0 { 5 + usize::from(p[4]) } else { 4 };
    p.get(start..).unwrap_or(&[])
}

/// 33-bit timestamp from the 5-byte PES encoding.
fn read_timestamp(b: &[u8]) -> u64 {
    (u64::from(b[0] >> 1) & 0x7) << 30 | u64::from(b[1]) << 22 | (u64::from(b[2]) >> 1) << 15 | u64::from(b[3]) << 7 | u64::from(b[4]) >> 1
}

fn write_timestamp(prefix: u8, ts: u64) -> [u8; 5] {
    [
        (prefix << 4) | (((ts >> 30) & 0x7) as u8) << 1 | 1,
        (ts >> 22) as u8,
        (((ts >> 15) & 0x7F) as u8) << 1 | 1,
        (ts >> 7) as u8,
        ((ts & 0x7F) as u8) << 1 | 1,
    ]
}

/// DTS (or PTS) of a PES packet that starts in this payload.
fn pes_time(pl: &[u8]) -> Option<u64> {
    if pl.len() < 14 || pl[..3] != [0, 0, 1] {
        return None;
    }
    let flags = pl[7] >> 6;
    match flags {
        3 if pl.len() >= 19 => Some(read_timestamp(&pl[14..19])),
        2 | 3 => Some(read_timestamp(&pl[9..14])),
        _ => None,
    }
}

/// MPEG-2 CRC32 of a PSI section.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Es {
    pid: u16,
    stream_type: u8,
    descriptors: Vec<u8>,
}

/// PSI section bytes from a packet that starts one (pointer field skipped).
fn section(p: &Packet) -> Option<&[u8]> {
    if p[1] & 0x40 == 0 {
        return None;
    }
    let pl = payload(p);
    let pointer = usize::from(*pl.first()?);
    let s = pl.get(1 + pointer..)?;
    let len = (usize::from(s.get(1)? & 0x0F) << 8) | usize::from(*s.get(2)?);
    s.get(..3 + len)
}

fn parse_pat(s: &[u8]) -> Option<u16> {
    let end = s.len().checked_sub(4)?;
    let mut i = 8;
    while i + 4 <= end {
        let program = u16::from_be_bytes([s[i], s[i + 1]]);
        let pid = (u16::from(s[i + 2] & 0x1F) << 8) | u16::from(s[i + 3]);
        if program != 0 {
            return Some(pid);
        }
        i += 4;
    }
    None
}

/// (PCR PID, streams) of a PMT section.
fn parse_pmt(s: &[u8]) -> Option<(u16, Vec<Es>)> {
    let pcr = (u16::from(*s.get(8)? & 0x1F) << 8) | u16::from(*s.get(9)?);
    let info_len = (usize::from(*s.get(10)? & 0x0F) << 8) | usize::from(*s.get(11)?);
    let end = s.len().checked_sub(4)?;
    let mut i = 12 + info_len;
    let mut streams = Vec::new();
    while i + 5 <= end {
        let stream_type = s[i];
        let pid = (u16::from(s[i + 1] & 0x1F) << 8) | u16::from(s[i + 2]);
        let es_len = (usize::from(s[i + 3] & 0x0F) << 8) | usize::from(s[i + 4]);
        let descriptors = s.get(i + 5..i + 5 + es_len)?.to_vec();
        streams.push(Es { pid, stream_type, descriptors });
        i += 5 + es_len;
    }
    Some((pcr, streams))
}

fn is_audio(stream_type: u8) -> bool {
    matches!(stream_type, 0x03 | 0x04 | 0x0F | 0x11 | 0x81 | 0x87)
}

fn read_packet(r: &mut impl Read) -> io::Result<Option<Packet>> {
    let mut p = [0u8; PACKET];
    let mut got = 0;
    while got < PACKET {
        let n = r.read(&mut p[got..])?;
        if n == 0 {
            return if got == 0 { Ok(None) } else { Err(invalid("TS file ends inside a packet")) };
        }
        got += n;
    }
    if p[0] != 0x47 {
        return Err(invalid("lost TS packet sync"));
    }
    Ok(Some(p))
}

/// PCR PID and streams from the first PAT/PMT of a TS file.
fn scan_tables(path: &Path) -> io::Result<(u16, Vec<Es>)> {
    let mut r = BufReader::new(File::open(path)?);
    let mut pmt_pid = None;
    for _ in 0..20_000 {
        let Some(p) = read_packet(&mut r)? else { break };
        let pid = pid_of(&p);
        if pid == 0 {
            if let Some(s) = section(&p) {
                pmt_pid = parse_pat(s).or(pmt_pid);
            }
        } else if Some(pid) == pmt_pid {
            if let Some(found) = section(&p).and_then(parse_pmt) {
                return Ok(found);
            }
        }
    }
    Err(invalid("no PAT/PMT in the TS stream"))
}

/// Media packets of a TS file in order, with the time of their PES.
struct TsSource {
    reader: BufReader<File>,
    keep: Vec<u16>,
    remap: HashMap<u16, u16>,
    times: HashMap<u16, u64>,
}

impl TsSource {
    fn next(&mut self) -> io::Result<Option<(u64, Packet)>> {
        while let Some(mut p) = read_packet(&mut self.reader)? {
            let pid = pid_of(&p);
            if !self.keep.contains(&pid) {
                continue;
            }
            if p[1] & 0x40 != 0 {
                if let Some(t) = pes_time(payload(&p)) {
                    self.times.insert(pid, t);
                }
            }
            let time = self.times.get(&pid).copied().unwrap_or(0);
            if let Some(&to) = self.remap.get(&pid) {
                set_pid(&mut p, to);
            }
            return Ok(Some((time, p)));
        }
        Ok(None)
    }
}

/// Codec of packed audio frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Packed {
    Aac,
    Ac3,
    Eac3,
}

impl Packed {
    /// Stream type and descriptors for the PMT.
    fn pmt_entry(self) -> (u8, Vec<u8>) {
        match self {
            Packed::Aac => (0x0F, Vec::new()),
            Packed::Ac3 => (0x81, vec![0x05, 4, b'A', b'C', b'-', b'3']),
            Packed::Eac3 => (0x87, vec![0x05, 4, b'E', b'A', b'C', b'3']),
        }
    }

    /// PES stream ID: MPEG audio for AAC, private stream 1 for Dolby audio.
    fn stream_id(self) -> u8 {
        if self == Packed::Aac { 0xC0 } else { 0xBD }
    }
}

/// Packed audio frames turned into TS packets.
struct PackedAudio {
    data: Vec<u8>,
    pos: usize,
    pts: u64,
    pid: u16,
    cc: u8,
    kind: Packed,
    queue: VecDeque<(u64, Packet)>,
}

const SAMPLE_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

/// AC-3 frame sizes in 16-bit words by frmsizecod, for 48, 44.1 and 32 kHz.
const AC3_FRAME_WORDS: [[u16; 3]; 38] = [
    [64, 69, 96], [64, 70, 96], [80, 87, 120], [80, 88, 120], [96, 104, 144], [96, 105, 144], [112, 121, 168], [112, 122, 168],
    [128, 139, 192], [128, 140, 192], [160, 174, 240], [160, 175, 240], [192, 208, 288], [192, 209, 288], [224, 243, 336],
    [224, 244, 336], [256, 278, 384], [256, 279, 384], [320, 348, 480], [320, 349, 480], [384, 417, 576], [384, 418, 576],
    [448, 487, 672], [448, 488, 672], [512, 557, 768], [512, 558, 768], [640, 696, 960], [640, 697, 960], [768, 835, 1152],
    [768, 836, 1152], [896, 975, 1344], [896, 976, 1344], [1024, 1114, 1536], [1024, 1115, 1536], [1152, 1253, 1728],
    [1152, 1254, 1728], [1280, 1393, 1920], [1280, 1394, 1920],
];

/// (codec, frame length, sample rate, samples) of the frame at the start of `d`.
fn audio_frame(d: &[u8]) -> Option<(Packed, usize, u32, u64)> {
    if d.len() < 7 {
        return None;
    }
    if d[0] == 0xFF && d[1] & 0xF0 == 0xF0 {
        let rate = *SAMPLE_RATES.get(usize::from((d[2] >> 2) & 0x0F))?;
        let len = (usize::from(d[3] & 0x03) << 11) | (usize::from(d[4]) << 3) | (usize::from(d[5]) >> 5);
        return Some((Packed::Aac, len, rate, 1024 * (u64::from(d[6] & 0x03) + 1)));
    }
    if d[0] != 0x0B || d[1] != 0x77 {
        return None;
    }
    let bsid = d[5] >> 3;
    let fscod = usize::from(d[4] >> 6);
    if bsid <= 10 {
        let rate = *[48000u32, 44100, 32000].get(fscod)?;
        let words = AC3_FRAME_WORDS.get(usize::from(d[4] & 0x3F))?[fscod];
        Some((Packed::Ac3, usize::from(words) * 2, rate, 1536))
    } else {
        let len = ((usize::from(d[2] & 0x07) << 8) | usize::from(d[3])) * 2 + 2;
        let code = usize::from((d[4] >> 4) & 0x03);
        let (rate, blocks) = if fscod == 3 {
            (*[24000u32, 22050, 16000].get(code)?, 6)
        } else {
            ([48000u32, 44100, 32000][fscod], [1u64, 2, 3, 6][code])
        };
        Some((Packed::Eac3, len, rate, 256 * blocks))
    }
}

/// Length of an ID3 tag at the start of `d`, if there is one.
fn id3_len(d: &[u8]) -> Option<usize> {
    if d.len() < 10 || &d[..3] != b"ID3" {
        return None;
    }
    let size = d[6..10].iter().fold(0usize, |acc, b| (acc << 7) | usize::from(b & 0x7F));
    let footer = if d[5] & 0x10 != 0 { 10 } else { 0 };
    Some((10 + size + footer).min(d.len()))
}

/// Codec of packed audio (after any leading ID3 tags).
fn packed_kind(data: &[u8]) -> io::Result<Packed> {
    let mut pos = 0;
    while let Some(len) = id3_len(&data[pos..]) {
        pos += len;
    }
    audio_frame(&data[pos..]).map(|f| f.0).ok_or_else(|| invalid("the separate audio is not AAC, AC-3 or E-AC-3"))
}

/// Start timestamp from an HLS packed-audio ID3 tag (Apple's PRIV frame).
fn id3_timestamp(tag: &[u8]) -> Option<u64> {
    let owner = b"com.apple.streaming.transportStreamTimestamp\0";
    let at = tag.windows(owner.len()).position(|w| w == owner)? + owner.len();
    let b = tag.get(at..at + 8)?;
    Some(u64::from_be_bytes(b.try_into().ok()?) & 0x1_FFFF_FFFF)
}

impl PackedAudio {
    fn new(data: Vec<u8>, pid: u16, start: u64, kind: Packed) -> Self {
        Self { data, pos: 0, pts: start, pid, cc: 0, kind, queue: VecDeque::new() }
    }

    fn next(&mut self) -> io::Result<Option<(u64, Packet)>> {
        while self.queue.is_empty() {
            let d = &self.data[self.pos..];
            if d.len() < 7 {
                return Ok(None);
            }
            if let Some(len) = id3_len(d) {
                if let Some(t) = id3_timestamp(&d[..len]) {
                    self.pts = t;
                }
                self.pos += len;
                continue;
            }
            let (kind, len, rate, samples) = audio_frame(d).ok_or_else(|| invalid("lost the audio frame sync"))?;
            if kind != self.kind || len < 7 || len > d.len() {
                return Err(invalid("broken audio frame"));
            }
            let frame = d[..len].to_vec();
            self.pos += len;
            self.packetize(&frame);
            self.pts += samples * 90_000 / u64::from(rate);
        }
        Ok(self.queue.pop_front())
    }

    /// One PES (with PTS) for one frame, split into TS packets.
    fn packetize(&mut self, frame: &[u8]) {
        let mut pes = vec![0, 0, 1, self.kind.stream_id()];
        let len = 3 + 5 + frame.len();
        pes.extend_from_slice(&(if len <= 0xFFFF { len as u16 } else { 0 }).to_be_bytes());
        pes.extend_from_slice(&[0x80, 0x80, 5]);
        pes.extend_from_slice(&write_timestamp(0x2, self.pts));
        pes.extend_from_slice(frame);
        let mut rest = &pes[..];
        let mut first = true;
        while !rest.is_empty() {
            let mut p = [0xFFu8; PACKET];
            p[0] = 0x47;
            p[1] = if first { 0x40 } else { 0 } | ((self.pid >> 8) as u8 & 0x1F);
            p[2] = self.pid as u8;
            let room = PACKET - 4;
            let take = rest.len().min(room);
            if take < room {
                // Stuff with an adaptation field so the payload ends the packet.
                let stuffing = room - take;
                p[3] = 0x30 | self.cc;
                p[4] = (stuffing - 1) as u8;
                if stuffing >= 2 {
                    p[5] = 0;
                }
                p[4 + stuffing..].copy_from_slice(&rest[..take]);
            } else {
                p[3] = 0x10 | self.cc;
                p[4..].copy_from_slice(&rest[..take]);
            }
            self.cc = (self.cc + 1) & 0x0F;
            self.queue.push_back((self.pts, p));
            rest = &rest[take..];
            first = false;
        }
    }
}

enum Audio {
    Ts(TsSource),
    Packed(PackedAudio),
}

impl Audio {
    fn next(&mut self) -> io::Result<Option<(u64, Packet)>> {
        match self {
            Audio::Ts(s) => s.next(),
            Audio::Packed(s) => s.next(),
        }
    }
}

/// PAT and PMT packets for the joined stream.
fn tables(pmt_pid: u16, pcr_pid: u16, streams: &[Es], cc: u8) -> [Packet; 2] {
    let packet = |pid: u16, mut section: Vec<u8>| {
        let crc = crc32(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        let mut p = [0xFFu8; PACKET];
        p[..4].copy_from_slice(&[0x47, 0x40 | ((pid >> 8) as u8 & 0x1F), pid as u8, 0x10 | (cc & 0x0F)]);
        p[4] = 0; // pointer field
        p[5..5 + section.len()].copy_from_slice(&section);
        p
    };
    let mut pat = vec![0x00, 0xB0, 13, 0x00, 0x01, 0xC1, 0, 0, 0x00, 0x01];
    pat.extend_from_slice(&[0xE0 | ((pmt_pid >> 8) as u8 & 0x1F), pmt_pid as u8]);

    let mut body = vec![0x00, 0x01, 0xC1, 0, 0, 0xE0 | ((pcr_pid >> 8) as u8 & 0x1F), pcr_pid as u8, 0xF0, 0];
    for s in streams {
        body.push(s.stream_type);
        body.extend_from_slice(&[0xE0 | ((s.pid >> 8) as u8 & 0x1F), s.pid as u8]);
        body.extend_from_slice(&[0xF0 | ((s.descriptors.len() >> 8) as u8 & 0x0F), s.descriptors.len() as u8]);
        body.extend_from_slice(&s.descriptors);
    }
    let len = body.len() + 4;
    let mut pmt = vec![0x02, 0xB0 | ((len >> 8) as u8 & 0x0F), len as u8];
    pmt.extend(body);
    [packet(0, pat), packet(pmt_pid, pmt)]
}

/// Joins `video` (TS) and `audio` (TS or ADTS AAC) into `output` (TS).
pub fn merge(video: &Path, audio: &Path, output: &Path) -> io::Result<()> {
    let (pcr_pid, video_streams) = scan_tables(video)?;
    let mut used: Vec<u16> = video_streams.iter().map(|s| s.pid).chain([pcr_pid, 0, NULL_PID]).collect();
    let free_pid = |used: &Vec<u16>, from: u16| (from..NULL_PID).find(|p| !used.contains(p)).unwrap_or(0x1FF0);

    let mut head = [0u8; 4];
    File::open(audio)?.read_exact(&mut head)?;
    let mut streams = video_streams.clone();
    let mut audio_src = if head[0] == 0x47 {
        let (_, found) = scan_tables(audio)?;
        let mut remap = HashMap::new();
        let mut keep = Vec::new();
        for s in found.into_iter().filter(|s| is_audio(s.stream_type)) {
            let pid = if used.contains(&s.pid) { free_pid(&used, 0x0101) } else { s.pid };
            used.push(pid);
            if pid != s.pid {
                remap.insert(s.pid, pid);
            }
            keep.push(s.pid);
            streams.push(Es { pid, ..s });
        }
        if keep.is_empty() {
            return Err(invalid("the separate audio TS has no audio stream"));
        }
        Audio::Ts(TsSource { reader: BufReader::new(File::open(audio)?), keep, remap, times: HashMap::new() })
    } else {
        let mut data = Vec::new();
        File::open(audio)?.read_to_end(&mut data)?;
        let kind = packed_kind(&data)?;
        let (stream_type, descriptors) = kind.pmt_entry();
        let pid = free_pid(&used, 0x0101);
        used.push(pid);
        streams.push(Es { pid, stream_type, descriptors });
        let mut video_start = TsSource {
            reader: BufReader::new(File::open(video)?),
            keep: video_streams.iter().map(|s| s.pid).collect(),
            remap: HashMap::new(),
            times: HashMap::new(),
        };
        // Without an ID3 timestamp the audio starts with the video.
        let mut start = 0;
        for _ in 0..2000 {
            match video_start.next()? {
                Some((t, _)) if t > 0 => {
                    start = t;
                    break;
                }
                Some(_) => continue,
                None => break,
            }
        }
        Audio::Packed(PackedAudio::new(data, pid, start, kind))
    };
    let pmt_pid = free_pid(&used, 0x1000);

    let mut video_src = TsSource {
        reader: BufReader::new(File::open(video)?),
        keep: video_streams.iter().map(|s| s.pid).chain([pcr_pid]).collect(),
        remap: HashMap::new(),
        times: HashMap::new(),
    };
    let mut out = BufWriter::with_capacity(1 << 20, File::create(output)?);
    let mut table_cc = 0u8;
    let mut written = 0u64;
    let mut v = video_src.next()?;
    let mut a = audio_src.next()?;
    while v.is_some() || a.is_some() {
        if written.is_multiple_of(TABLE_INTERVAL) {
            for p in tables(pmt_pid, pcr_pid, &streams, table_cc) {
                out.write_all(&p)?;
            }
            table_cc = (table_cc + 1) & 0x0F;
        }
        let take_video = match (&v, &a) {
            (Some((vt, _)), Some((at, _))) => vt <= at,
            (Some(_), None) => true,
            _ => false,
        };
        if take_video {
            out.write_all(&v.as_ref().unwrap().1)?;
            v = video_src.next()?;
        } else {
            out.write_all(&a.as_ref().unwrap().1)?;
            a = audio_src.next()?;
        }
        written += 1;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn es_packet(pid: u16, cc: u8, pts: Option<u64>, fill: u8) -> Packet {
        let mut p = [fill; PACKET];
        p[..4].copy_from_slice(&[0x47, if pts.is_some() { 0x40 } else { 0 } | (pid >> 8) as u8, pid as u8, 0x10 | cc]);
        if let Some(t) = pts {
            let mut pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5];
            pes.extend_from_slice(&write_timestamp(2, t));
            p[4..4 + pes.len()].copy_from_slice(&pes);
        }
        p
    }

    fn adts_frame(payload: &[u8]) -> Vec<u8> {
        let len = 7 + payload.len();
        // MPEG-4 AAC LC, 48 kHz (index 3), stereo, one raw block.
        let mut f = vec![0xFF, 0xF1, 0x4C, 0x80 | ((len >> 11) as u8 & 0x03), (len >> 3) as u8, ((len & 7) as u8) << 5 | 0x1F, 0xFC];
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn timestamps_round_trip_and_crc_matches_known_value() {
        let t = 0x1_2345_6789 & 0x1_FFFF_FFFF;
        assert_eq!(read_timestamp(&write_timestamp(2, t)), t);
        // CRC of a standard PAT (program 1 -> PMT PID 0x1000) is 0x2AB104B2.
        let pat = [0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xF0, 0x00];
        assert_eq!(crc32(&pat), 0x2AB1_04B2);
    }

    #[test]
    fn dolby_frame_headers() {
        // AC-3, 48 kHz, frmsizecod 28 (384 kbit/s): 768 words = 1536 bytes.
        let ac3 = [0x0B, 0x77, 0, 0, 28, 0x08 << 3, 0];
        assert_eq!(audio_frame(&ac3), Some((Packed::Ac3, 1536, 48000, 1536)));
        // E-AC-3 (bsid 16), frmsiz 0x2FF -> 1536 bytes, 48 kHz, 6 blocks.
        let eac3 = [0x0B, 0x77, 0x02, 0xFF, 0x30, 16 << 3, 0];
        assert_eq!(audio_frame(&eac3), Some((Packed::Eac3, 1536, 48000, 1536)));
        assert_eq!(Packed::Eac3.pmt_entry().0, 0x87);
        assert!(packed_kind(b"hello world").is_err());
    }

    #[test]
    fn joins_ts_video_with_packed_aac_in_time_order() {
        let dir = std::env::temp_dir().join(format!("klippo_ts_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (v, a, o) = (dir.join("v.ts"), dir.join("a.aac"), dir.join("out.ts"));

        // Video: PAT, PMT (H.264 on PID 0x100), PES at 0 s, 0.04 s, 0.08 s.
        let mut video = Vec::new();
        let [pat, pmt] = tables(0x1000, 0x100, &[Es { pid: 0x100, stream_type: 0x1B, descriptors: vec![] }], 0);
        video.extend_from_slice(&pat);
        video.extend_from_slice(&pmt);
        for (i, t) in [10_000u64, 13_600, 17_200].iter().enumerate() {
            video.extend_from_slice(&es_packet(0x100, i as u8, Some(*t), 0xAA));
            video.extend_from_slice(&es_packet(0x100, i as u8, None, 0xAB));
        }
        std::fs::write(&v, &video).unwrap();

        // Audio: ID3 tag with start 10_000, then four 48 kHz frames (1920 ticks each).
        let mut audio = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
        let mut priv_frame = b"PRIV".to_vec();
        let mut body = b"com.apple.streaming.transportStreamTimestamp\0".to_vec();
        body.extend_from_slice(&10_000u64.to_be_bytes());
        priv_frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        priv_frame.extend_from_slice(&[0, 0]);
        priv_frame.extend(body);
        audio[9] = priv_frame.len() as u8;
        audio.extend(priv_frame);
        for i in 0..4u8 {
            audio.extend(adts_frame(&[i; 20]));
        }
        std::fs::write(&a, &audio).unwrap();

        merge(&v, &a, &o).unwrap();
        let out = std::fs::read(&o).unwrap();
        assert_eq!(out.len() % PACKET, 0);
        let packets: Vec<Packet> = out.chunks(PACKET).map(|c| c.try_into().unwrap()).collect();
        let (pcr, streams) = parse_pmt(section(&packets[1]).unwrap()).unwrap();
        assert_eq!(pcr, 0x100);
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[1].stream_type, 0x0F);
        let audio_pid = streams[1].pid;

        // PES start times in output order must not go backwards.
        let mut last = 0;
        let mut audio_frames = 0;
        for p in &packets[2..] {
            if p[1] & 0x40 != 0 {
                let t = pes_time(payload(p)).unwrap();
                assert!(t >= last, "{} after {}", t, last);
                last = t;
                if pid_of(p) == audio_pid {
                    audio_frames += 1;
                }
            }
        }
        assert_eq!(audio_frames, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
