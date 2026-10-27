//! Turns YouTube's fragmented MP4 streams (one video-only, one audio-only)
//! into one regular MP4 file.
//!
//! The inputs are `ftyp moov [sidx] (moof mdat)*` with one track each. The
//! output is a normal (not fragmented) MP4: `ftyp`, a `moov` with full sample
//! tables, and one `mdat`. Fragmented files play, but players such as VLC
//! cannot find key frames in them when seeking and show a frozen picture; with
//! a sync-sample table (`stss`) every player seeks to the right frame.
//!
//! Each fragment of each input becomes one chunk; chunks of the two tracks are
//! written in time order, so audio and video play from nearby bytes.

use std::fs::File;
use std::collections::HashSet;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn be32(d: &[u8], at: usize) -> io::Result<u32> {
    d.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| invalid("box too short"))
}

fn be64(d: &[u8], at: usize) -> io::Result<u64> {
    d.get(at..at + 8).map(|b| u64::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| invalid("box too short"))
}

fn put_u32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

#[derive(Debug, Clone, Copy)]
struct BoxRef {
    kind: [u8; 4],
    start: u64,
    size: u64,
}

/// Top-level boxes of a file (headers only).
fn top_level(file: &mut File) -> io::Result<Vec<BoxRef>> {
    let len = file.metadata()?.len();
    let mut out = Vec::new();
    let mut pos = 0u64;
    while pos + 8 <= len {
        file.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 16];
        file.read_exact(&mut hdr[..8])?;
        let mut size = u64::from(u32::from_be_bytes(hdr[..4].try_into().unwrap()));
        if size == 1 {
            file.read_exact(&mut hdr[8..16])?;
            size = u64::from_be_bytes(hdr[8..16].try_into().unwrap());
        } else if size == 0 {
            size = len - pos;
        }
        if size < 8 || pos + size > len {
            return Err(invalid("broken MP4 box"));
        }
        out.push(BoxRef { kind: hdr[4..8].try_into().unwrap(), start: pos, size });
        pos += size;
    }
    Ok(out)
}

fn read_box(file: &mut File, b: &BoxRef) -> io::Result<Vec<u8>> {
    let mut data = vec![0u8; usize::try_from(b.size).map_err(|_| invalid("box too large"))?];
    file.seek(SeekFrom::Start(b.start))?;
    file.read_exact(&mut data)?;
    Ok(data)
}

/// Child boxes inside an in-memory box: (kind, start, payload start, end).
fn children(data: &[u8], from: usize) -> Vec<([u8; 4], usize, usize, usize)> {
    let mut out = Vec::new();
    let mut pos = from;
    while pos + 8 <= data.len() {
        let mut size = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let mut header = 8;
        if size == 1 && pos + 16 <= data.len() {
            size = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap()) as usize;
            header = 16;
        } else if size == 0 {
            size = data.len() - pos;
        }
        if size < header || pos + size > data.len() {
            break;
        }
        out.push((data[pos + 4..pos + 8].try_into().unwrap(), pos, pos + header, pos + size));
        pos += size;
    }
    out
}

/// Payload offsets of the first box of `kind` found along `path` (e.g. traf/tfhd).
fn find(data: &[u8], from: usize, path: &[&[u8; 4]]) -> Option<(usize, usize)> {
    let (first, rest) = path.split_first()?;
    let (_, _, payload, end) = children(data, from).into_iter().find(|(k, ..)| &k == first)?;
    if rest.is_empty() {
        Some((payload, end))
    } else {
        find(&data[..end], payload, rest)
    }
}

/// The whole box (header included) of the first `kind` along `path`.
fn whole(data: &[u8], from: usize, path: &[&[u8; 4]]) -> Option<Vec<u8>> {
    let (last, parents) = path.split_last()?;
    let (start, end) = if parents.is_empty() { (from, data.len()) } else { find(data, from, parents)? };
    children(&data[..end], start).into_iter().find(|(k, ..)| &k == last).map(|(_, s, _, e)| data[s..e].to_vec())
}

fn make_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut p = ((u32::from(version) << 24) | (flags & 0x00FF_FFFF)).to_be_bytes().to_vec();
    p.extend_from_slice(body);
    make_box(kind, &p)
}

/// One media sample: where it is in the input and how it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
    offset: u64,
    size: u32,
    duration: u32,
    cto: i32,
    sync: bool,
}

/// One fragment's samples (they become one chunk).
struct Chunk {
    decode_time: u64,
    samples: Vec<Sample>,
}

/// One track of an input file: its boxes and samples.
struct Track {
    /// Index of the input file that holds the samples.
    input: usize,
    trak: Vec<u8>,
    ftyp: Vec<u8>,
    timescale: u32,
    chunks: Vec<Chunk>,
}

const NON_SYNC: u32 = 0x0001_0000;

/// Sample defaults of a track (from `trex`): duration, size, flags.
type Defaults = (u32, u32, u32);

/// Samples of each `traf` in one `moof`: (track ID, decode time, samples).
fn moof_samples(moof: &[u8], moof_start: u64, defaults: &dyn Fn(u32) -> Defaults) -> io::Result<Vec<(u32, u64, Vec<Sample>)>> {
    let mut out = Vec::new();
    // Without an explicit base, a traf's data follows the previous one's.
    let mut prev_end = moof_start;
    for (kind, _, traf_body, traf_end) in children(moof, 8) {
        if &kind != b"traf" {
            continue;
        }
        let traf = &moof[..traf_end];
        let (tfhd, _) = find(traf, traf_body, &[b"tfhd"]).ok_or_else(|| invalid("no tfhd box"))?;
        let flags = be32(moof, tfhd)? & 0x00FF_FFFF;
        let track_id = be32(moof, tfhd + 4)?;
        let mut at = tfhd + 8;
        let mut base = if flags & 0x020000 != 0 || out.is_empty() { moof_start } else { prev_end };
        if flags & 0x1 != 0 {
            base = be64(moof, at)?;
            at += 8;
        }
        if flags & 0x2 != 0 {
            at += 4;
        }
        let (mut dur, mut size, mut sflags) = defaults(track_id);
        if flags & 0x8 != 0 {
            dur = be32(moof, at)?;
            at += 4;
        }
        if flags & 0x10 != 0 {
            size = be32(moof, at)?;
            at += 4;
        }
        if flags & 0x20 != 0 {
            sflags = be32(moof, at)?;
        }
        let decode_time = match find(traf, traf_body, &[b"tfdt"]) {
            Some((p, _)) if moof[p] == 1 => be64(moof, p + 4)?,
            Some((p, _)) => u64::from(be32(moof, p + 4)?),
            None => 0,
        };

        let mut samples = Vec::new();
        let mut next_data = base;
        for (kind, _, body, _) in children(traf, traf_body) {
            if &kind != b"trun" {
                continue;
            }
            let version = moof[body];
            let tflags = be32(moof, body)? & 0x00FF_FFFF;
            let count = be32(moof, body + 4)?;
            let mut at = body + 8;
            let mut data = next_data;
            if tflags & 0x1 != 0 {
                data = base.wrapping_add_signed(i64::from(be32(moof, at)? as i32));
                at += 4;
            }
            let mut first_flags = None;
            if tflags & 0x4 != 0 {
                first_flags = Some(be32(moof, at)?);
                at += 4;
            }
            for i in 0..count {
                let mut s = Sample { offset: data, size, duration: dur, cto: 0, sync: false };
                let mut f = if i == 0 { first_flags.unwrap_or(sflags) } else { sflags };
                if tflags & 0x100 != 0 {
                    s.duration = be32(moof, at)?;
                    at += 4;
                }
                if tflags & 0x200 != 0 {
                    s.size = be32(moof, at)?;
                    at += 4;
                }
                if tflags & 0x400 != 0 {
                    f = be32(moof, at)?;
                    at += 4;
                }
                if tflags & 0x800 != 0 {
                    let raw = be32(moof, at)?;
                    s.cto = if version == 0 { raw.min(i32::MAX as u32) as i32 } else { raw as i32 };
                    at += 4;
                }
                s.sync = f & NON_SYNC == 0;
                data += u64::from(s.size);
                samples.push(s);
            }
            next_data = data;
        }
        prev_end = next_data;
        out.push((track_id, decode_time, samples));
    }
    Ok(out)
}

/// Full box payload of `kind` inside the track's `stbl`.
fn stbl_box<'a>(trak: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    find(trak, 8, &[b"mdia", b"minf", b"stbl", kind]).map(|(p, e)| &trak[p..e])
}

/// (count, value) entries of an `stts` or `ctts` payload.
fn time_table(table: Option<&[u8]>, signed: bool) -> io::Result<Vec<(u32, i64)>> {
    let Some(t) = table else { return Ok(Vec::new()) };
    let version = t.first().copied().unwrap_or(0);
    (0..be32(t, 4)? as usize)
        .map(|i| {
            let raw = be32(t, 12 + i * 8)?;
            let v = if signed && version == 1 { i64::from(raw as i32) } else { i64::from(raw) };
            Ok((be32(t, 8 + i * 8)?, v))
        })
        .collect()
}

/// Samples of a regular (not fragmented) track from its sample tables;
/// each input chunk stays one chunk.
fn table_chunks(trak: &[u8]) -> io::Result<Vec<Chunk>> {
    let Some(stsz) = stbl_box(trak, b"stsz") else { return Ok(Vec::new()) };
    let fixed = be32(stsz, 4)?;
    let count = be32(stsz, 8)? as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    let size_of = |i: usize| if fixed != 0 { Ok(fixed) } else { be32(stsz, 12 + i * 4) };

    let offsets: Vec<u64> = if let Some(co64) = stbl_box(trak, b"co64") {
        (0..be32(co64, 4)? as usize).map(|i| be64(co64, 8 + i * 8)).collect::<io::Result<_>>()?
    } else {
        let stco = stbl_box(trak, b"stco").ok_or_else(|| invalid("no chunk offsets"))?;
        (0..be32(stco, 4)? as usize).map(|i| be32(stco, 8 + i * 4).map(u64::from)).collect::<io::Result<_>>()?
    };
    let stsc = stbl_box(trak, b"stsc").ok_or_else(|| invalid("no stsc box"))?;
    let stsc: Vec<(u32, u32)> = (0..be32(stsc, 4)? as usize)
        .map(|i| Ok((be32(stsc, 8 + i * 12)?, be32(stsc, 12 + i * 12)?)))
        .collect::<io::Result<_>>()?;
    let stts = time_table(stbl_box(trak, b"stts"), false)?;
    let ctts = time_table(stbl_box(trak, b"ctts"), true)?;
    let sync: Option<HashSet<u32>> = match stbl_box(trak, b"stss") {
        Some(t) => Some((0..be32(t, 4)? as usize).map(|i| be32(t, 8 + i * 4)).collect::<io::Result<_>>()?),
        None => None,
    };
    let mut durations = stts.iter().flat_map(|&(n, d)| std::iter::repeat_n(d as u32, n as usize));
    let mut ctos = ctts
        .iter()
        .flat_map(|&(n, c)| std::iter::repeat_n(c.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32, n as usize));

    let mut chunks = Vec::new();
    let mut sample = 0usize;
    let mut time = 0u64;
    for (ci, &offset) in offsets.iter().enumerate() {
        let chunk_no = ci as u32 + 1;
        let per_chunk = stsc.iter().rev().find(|(first, _)| *first <= chunk_no).map(|e| e.1).unwrap_or(0);
        let mut pos = offset;
        let mut samples = Vec::new();
        let start = time;
        for _ in 0..per_chunk {
            if sample >= count {
                break;
            }
            let size = size_of(sample)?;
            let duration = durations.next().unwrap_or(0);
            samples.push(Sample {
                offset: pos,
                size,
                duration,
                cto: ctos.next().unwrap_or(0),
                sync: sync.as_ref().is_none_or(|set| set.contains(&(sample as u32 + 1))),
            });
            pos += u64::from(size);
            time += u64::from(duration);
            sample += 1;
        }
        if !samples.is_empty() {
            chunks.push(Chunk { decode_time: start, samples });
        }
    }
    Ok(chunks)
}

/// All tracks of one input file, fragmented or not.
fn open_input(path: &Path, input: usize) -> io::Result<(File, Vec<Track>)> {
    let mut file = File::open(path)?;
    let boxes = top_level(&mut file)?;
    let get = |kind: &[u8; 4]| boxes.iter().find(|b| &b.kind == kind).copied();
    let ftyp = match get(b"ftyp") {
        Some(b) => read_box(&mut file, &b)?,
        None => Vec::new(),
    };
    let moov = read_box(&mut file, &get(b"moov").ok_or_else(|| invalid("no moov box"))?)?;

    let mut tracks = Vec::new();
    let mut ids = Vec::new();
    for (kind, start, _, end) in children(&moov, 8) {
        if &kind != b"trak" {
            continue;
        }
        let trak = moov[start..end].to_vec();
        let (tkhd, _) = find(&trak, 8, &[b"tkhd"]).ok_or_else(|| invalid("no tkhd box"))?;
        let id = if trak[tkhd] == 1 { be32(&trak, tkhd + 20)? } else { be32(&trak, tkhd + 12)? };
        let (mdhd, _) = find(&trak, 8, &[b"mdia", b"mdhd"]).ok_or_else(|| invalid("no mdhd box"))?;
        let timescale = if trak[mdhd] == 1 { be32(&trak, mdhd + 20)? } else { be32(&trak, mdhd + 12)? };
        let chunks = table_chunks(&trak)?;
        tracks.push(Track { input, trak, ftyp: ftyp.clone(), timescale: timescale.max(1), chunks });
        ids.push(id);
    }

    // Sample defaults per track ID.
    let mut trex = Vec::new();
    if let Some((mvex, mvex_end)) = find(&moov, 8, &[b"mvex"]) {
        for (kind, _, p, _) in children(&moov[..mvex_end], mvex) {
            if &kind == b"trex" {
                trex.push((be32(&moov, p + 4)?, (be32(&moov, p + 12)?, be32(&moov, p + 16)?, be32(&moov, p + 20)?)));
            }
        }
    }
    let defaults = |id: u32| trex.iter().find(|(t, _)| *t == id).map(|(_, d)| *d).unwrap_or((0, 0, 0));
    for b in boxes.iter().filter(|b| &b.kind == b"moof") {
        let moof = read_box(&mut file, b)?;
        for (id, decode_time, samples) in moof_samples(&moof, b.start, &defaults)? {
            // A single-track file may number its fragments' track differently.
            let index = ids.iter().position(|t| *t == id).or(if ids.len() == 1 { Some(0) } else { None });
            if let (Some(i), false) = (index, samples.is_empty()) {
                tracks[i].chunks.push(Chunk { decode_time, samples });
            }
        }
    }
    tracks.retain(|t| !t.chunks.is_empty());
    if tracks.is_empty() {
        return Err(invalid("no media samples"));
    }
    Ok((file, tracks))
}

/// Run-length table of (count, value) pairs.
fn runs<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

/// `stbl` with the new sample tables (the sample description is kept).
fn sample_table(track: &Track, chunk_offsets: &[u64]) -> io::Result<Vec<u8>> {
    let samples = || track.chunks.iter().flat_map(|c| c.samples.iter());
    let count = samples().count() as u32;
    let stsd = whole(&track.trak, 8, &[b"mdia", b"minf", b"stbl", b"stsd"]).ok_or_else(|| invalid("no stsd box"))?;

    let mut body = stsd;
    let stts = runs(samples().map(|s| s.duration));
    let mut p = (stts.len() as u32).to_be_bytes().to_vec();
    for (n, d) in &stts {
        p.extend_from_slice(&n.to_be_bytes());
        p.extend_from_slice(&d.to_be_bytes());
    }
    body.extend(full_box(b"stts", 0, 0, &p));

    if samples().any(|s| s.cto != 0) {
        let ctts = runs(samples().map(|s| s.cto));
        let version = u8::from(ctts.iter().any(|(_, c)| *c < 0));
        let mut p = (ctts.len() as u32).to_be_bytes().to_vec();
        for (n, c) in &ctts {
            p.extend_from_slice(&n.to_be_bytes());
            p.extend_from_slice(&c.to_be_bytes());
        }
        body.extend(full_box(b"ctts", version, 0, &p));
    }

    if !samples().all(|s| s.sync) {
        let sync: Vec<u32> = samples().enumerate().filter(|(_, s)| s.sync).map(|(i, _)| i as u32 + 1).collect();
        let mut p = (sync.len() as u32).to_be_bytes().to_vec();
        for n in sync {
            p.extend_from_slice(&n.to_be_bytes());
        }
        body.extend(full_box(b"stss", 0, 0, &p));
    }

    // Chunk n holds the samples of fragment n.
    let per_chunk = runs(track.chunks.iter().map(|c| c.samples.len() as u32));
    let mut p = (per_chunk.len() as u32).to_be_bytes().to_vec();
    let mut first = 1u32;
    for (n, samples_in_chunk) in &per_chunk {
        p.extend_from_slice(&first.to_be_bytes());
        p.extend_from_slice(&samples_in_chunk.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        first += n;
    }
    body.extend(full_box(b"stsc", 0, 0, &p));

    let mut p = 0u32.to_be_bytes().to_vec();
    p.extend_from_slice(&count.to_be_bytes());
    for s in samples() {
        p.extend_from_slice(&s.size.to_be_bytes());
    }
    body.extend(full_box(b"stsz", 0, 0, &p));

    let mut p = (chunk_offsets.len() as u32).to_be_bytes().to_vec();
    for o in chunk_offsets {
        p.extend_from_slice(&o.to_be_bytes());
    }
    body.extend(full_box(b"co64", 0, 0, &p));
    Ok(make_box(b"stbl", &body))
}

/// Rebuilds `container` (a whole box) with `path` replaced by `new_box`.
/// `stsd`-style boxes are never on the path, so children start right after headers.
fn replace(container: &[u8], path: &[&[u8; 4]], new_box: &[u8]) -> io::Result<Vec<u8>> {
    let (first, rest) = path.split_first().ok_or_else(|| invalid("empty path"))?;
    let header = 8;
    let mut payload = Vec::new();
    let mut found = false;
    for (kind, start, _, end) in children(container, header) {
        if &kind == *first && !found {
            found = true;
            if rest.is_empty() {
                payload.extend_from_slice(new_box);
            } else {
                payload.extend(replace(&container[start..end], rest, new_box)?);
            }
        } else {
            payload.extend_from_slice(&container[start..end]);
        }
    }
    if !found {
        return Err(invalid("box to replace is missing"));
    }
    Ok(make_box(container[4..8].try_into().unwrap(), &payload))
}

/// Track duration in its own timescale.
fn media_duration(track: &Track) -> u64 {
    track.chunks.iter().flat_map(|c| &c.samples).map(|s| u64::from(s.duration)).sum()
}

const MOVIE_TIMESCALE: u32 = 1000;

/// `trak` with new sample tables, durations and track ID.
fn new_trak(track: &Track, id: u32, chunk_offsets: &[u64]) -> io::Result<Vec<u8>> {
    let media = media_duration(track);
    let movie = media * u64::from(MOVIE_TIMESCALE) / u64::from(track.timescale);
    let mut trak = replace(&track.trak, &[b"mdia", b"minf", b"stbl"], &sample_table(track, chunk_offsets)?)?;

    if let Some((p, _)) = find(&trak, 8, &[b"tkhd"]) {
        // Enabled, in movie, in preview.
        trak[p + 3] |= 0x07;
        if trak[p] == 1 {
            put_u32(&mut trak, p + 20, id);
            trak[p + 28..p + 36].copy_from_slice(&movie.to_be_bytes());
        } else {
            put_u32(&mut trak, p + 12, id);
            put_u32(&mut trak, p + 20, movie.min(u64::from(u32::MAX)) as u32);
        }
    }
    if let Some((p, _)) = find(&trak, 8, &[b"mdia", b"mdhd"]) {
        if trak[p] == 1 {
            trak[p + 24..p + 32].copy_from_slice(&media.to_be_bytes());
        } else {
            put_u32(&mut trak, p + 16, media.min(u64::from(u32::MAX)) as u32);
        }
    }
    // A one-entry edit list covers the whole track (fragmented files often say 0).
    if let Some((p, end)) = find(&trak, 8, &[b"edts", b"elst"]) {
        if be32(&trak, p + 4)? == 1 {
            if trak[p] == 1 && end >= p + 16 {
                trak[p + 8..p + 16].copy_from_slice(&movie.to_be_bytes());
            } else if end >= p + 12 {
                put_u32(&mut trak, p + 8, movie.min(u64::from(u32::MAX)) as u32);
            }
        }
    }
    Ok(trak)
}

fn mvhd(duration: u64, next_track: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0u8; 16]); // creation and modification time
    p.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    p.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    p.extend_from_slice(&[0u8; 10]);
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        p.extend_from_slice(&v.to_be_bytes()); // unity matrix
    }
    p.extend_from_slice(&[0u8; 24]);
    p.extend_from_slice(&next_track.to_be_bytes());
    full_box(b"mvhd", 1, 0, &p)
}

fn ftyp(tracks: &[Track]) -> Vec<u8> {
    let mut p = b"isom".to_vec();
    p.extend_from_slice(&0x200u32.to_be_bytes());
    for brand in [b"isom", b"iso2", b"iso6", b"mp41"] {
        p.extend_from_slice(brand);
    }
    // Codec brands the inputs declared (av01, ...), not the DASH ones.
    for t in tracks {
        for b in t.ftyp.get(16..).unwrap_or_default().as_chunks::<4>().0 {
            if !matches!(b, b"isom" | b"iso2" | b"iso5" | b"iso6" | b"mp41" | b"mp42" | b"dash" | b"msdh" | b"msix") && !p[8..].chunks(4).any(|x| x == b) {
                p.extend_from_slice(b);
            }
        }
    }
    make_box(b"ftyp", &p)
}

/// True when the file starts like an MP4 (fragmented or not).
pub fn looks_like_mp4(path: &Path) -> bool {
    let mut head = [0u8; 8];
    File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && matches!(&head[4..8], b"ftyp" | b"styp" | b"moov" | b"moof" | b"sidx" | b"free")
}

/// Writes all tracks of `inputs` (the video first) as one regular MP4.
/// Inputs may be fragmented (DASH, CMAF, HLS) or regular MP4 files.
pub fn remux(inputs: &[&Path], output: &Path) -> io::Result<()> {
    let mut files = Vec::new();
    let mut tracks = Vec::new();
    for (i, path) in inputs.iter().enumerate() {
        let (file, found) = open_input(path, i)?;
        files.push(file);
        tracks.extend(found);
    }

    // Chunk order in the file: by decode time across tracks.
    let mut order: Vec<(usize, usize)> = tracks
        .iter()
        .enumerate()
        .flat_map(|(t, track)| (0..track.chunks.len()).map(move |c| (t, c)))
        .collect();
    order.sort_by(|a, b| {
        let time = |(t, c): &(usize, usize)| tracks[*t].chunks[*c].decode_time as f64 / f64::from(tracks[*t].timescale);
        time(a).total_cmp(&time(b)).then(a.0.cmp(&b.0))
    });

    // Chunk positions inside mdat; the moov size does not depend on them (co64).
    let mut rel_offsets: Vec<Vec<u64>> = tracks.iter().map(|t| vec![0; t.chunks.len()]).collect();
    let mut pos = 0u64;
    for &(t, c) in &order {
        rel_offsets[t][c] = pos;
        pos += tracks[t].chunks[c].samples.iter().map(|s| u64::from(s.size)).sum::<u64>();
    }
    let mdat_len = pos;

    let ftyp = ftyp(&tracks);
    let build_moov = |base: u64| -> io::Result<Vec<u8>> {
        let mut body = Vec::new();
        let duration = tracks
            .iter()
            .map(|t| media_duration(t) * u64::from(MOVIE_TIMESCALE) / u64::from(t.timescale))
            .max()
            .unwrap_or(0);
        body.extend(mvhd(duration, tracks.len() as u32 + 1));
        for (i, t) in tracks.iter().enumerate() {
            let offsets: Vec<u64> = rel_offsets[i].iter().map(|o| o + base).collect();
            body.extend(new_trak(t, i as u32 + 1, &offsets)?);
        }
        Ok(make_box(b"moov", &body))
    };
    let moov_len = build_moov(0)?.len() as u64;
    let mdat_header = 16u64;
    let moov = build_moov(ftyp.len() as u64 + moov_len + mdat_header)?;

    let mut out = BufWriter::with_capacity(1 << 20, File::create(output)?);
    out.write_all(&ftyp)?;
    out.write_all(&moov)?;
    out.write_all(&1u32.to_be_bytes())?;
    out.write_all(b"mdat")?;
    out.write_all(&(mdat_len + mdat_header).to_be_bytes())?;

    let mut buf = Vec::new();
    for &(t, c) in &order {
        let track = &tracks[t];
        let file = &mut files[track.input];
        let samples = &track.chunks[c].samples;
        // Samples of a fragment are usually one contiguous run.
        let mut i = 0;
        while i < samples.len() {
            let start = samples[i].offset;
            let mut end = start + u64::from(samples[i].size);
            let mut j = i + 1;
            while j < samples.len() && samples[j].offset == end {
                end += u64::from(samples[j].size);
                j += 1;
            }
            buf.resize(usize::try_from(end - start).map_err(|_| invalid("chunk too large"))?, 0);
            file.seek(SeekFrom::Start(start))?;
            file.read_exact(&mut buf)?;
            out.write_all(&buf)?;
            i = j;
        }
    }
    out.flush()?;
    Ok(())
}

/// Joins `video` and `audio` into `output` (a regular MP4).
pub fn merge(video: &Path, audio: &Path, output: &Path) -> io::Result<()> {
    remux(&[video, audio], output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny one-track fragmented MP4: `n` fragments of 2 samples, each
    /// `step` ticks apart; the first sample of a fragment is a key frame.
    fn sample(handler: &[u8; 4], timescale: u32, n: u32, step: u32, fill: u8) -> Vec<u8> {
        let mut tkhd = vec![0u8; 8];
        tkhd.extend_from_slice(&1u32.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 68]);
        let tkhd = full_box(b"tkhd", 0, 3, &tkhd);
        let mut mdhd = vec![0u8; 8];
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&[0u8; 8]);
        let mdhd = full_box(b"mdhd", 0, 0, &mdhd);
        let mut hdlr = vec![0u8; 4];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0u8; 13]);
        let hdlr = full_box(b"hdlr", 0, 0, &hdlr);
        let stsd = full_box(b"stsd", 0, 0, &[0, 0, 0, 1, 0, 0, 0, 8, b't', b'e', b's', b't']);
        let empty = |k: &[u8; 4]| full_box(k, 0, 0, &[0, 0, 0, 0]);
        let stbl = make_box(b"stbl", &[stsd, empty(b"stts"), empty(b"stsc"), empty(b"stco")].concat());
        let minf = make_box(b"minf", &stbl);
        let mdia = make_box(b"mdia", &[mdhd, hdlr, minf].concat());
        let trak = make_box(b"trak", &[tkhd, mdia].concat());
        let mut trex = 1u32.to_be_bytes().to_vec();
        trex.extend_from_slice(&1u32.to_be_bytes());
        trex.extend_from_slice(&(step / 2).to_be_bytes()); // default duration
        trex.extend_from_slice(&0u32.to_be_bytes());
        trex.extend_from_slice(&NON_SYNC.to_be_bytes()); // default: not a key frame
        let mvex = make_box(b"mvex", &full_box(b"trex", 0, 0, &trex));
        let mut file = make_box(b"ftyp", b"dash\0\0\0\0iso6av01");
        file.extend(make_box(b"moov", &[mvhd(0, 2), mvex, trak].concat()));
        file.extend(full_box(b"sidx", 0, 0, &[0u8; 20]));
        for i in 0..n {
            let mfhd = full_box(b"mfhd", 0, 0, &(i + 1).to_be_bytes());
            let tfhd = full_box(b"tfhd", 0, 0x020000, &1u32.to_be_bytes());
            let tfdt = full_box(b"tfdt", 0, 0, &(i * step).to_be_bytes());
            // trun: data offset, first-sample flags (key frame), sizes 3 and 2.
            let mut trun = 2u32.to_be_bytes().to_vec();
            trun.extend_from_slice(&0i32.to_be_bytes()); // patched below
            trun.extend_from_slice(&0u32.to_be_bytes());
            trun.extend_from_slice(&3u32.to_be_bytes());
            trun.extend_from_slice(&2u32.to_be_bytes());
            let trun_box = full_box(b"trun", 0, 0x205, &trun);
            let traf = make_box(b"traf", &[tfhd, tfdt, trun_box].concat());
            let mut moof = make_box(b"moof", &[mfhd, traf].concat());
            // trun is the last box: ... data_offset, first flags, size, size.
            let data_offset = moof.len() as u32 + 8;
            let at = moof.len() - 16;
            moof[at..at + 4].copy_from_slice(&data_offset.to_be_bytes());
            file.extend(moof);
            file.extend(make_box(b"mdat", &[fill + i as u8; 5]));
        }
        file
    }

    #[test]
    fn writes_a_regular_mp4_with_sample_tables() {
        let dir = std::env::temp_dir().join(format!("klippo_mux_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (v, a, o) = (dir.join("v.mp4"), dir.join("a.m4a"), dir.join("out.mp4"));
        std::fs::write(&v, sample(b"vide", 1000, 3, 2000, 0x10)).unwrap(); // 0 s, 2 s, 4 s
        std::fs::write(&a, sample(b"soun", 100, 2, 300, 0x20)).unwrap(); // 0 s, 3 s
        merge(&v, &a, &o).unwrap();

        let out = std::fs::read(&o).unwrap();
        let top: Vec<[u8; 4]> = children(&out, 0).iter().map(|c| c.0).collect();
        assert_eq!(top, vec![*b"ftyp", *b"moov", *b"mdat"], "no moof, no sidx");
        assert_eq!(&out[8..12], b"isom");
        assert!(out[16..children(&out, 0)[0].3].chunks(4).any(|b| b == b"av01"), "codec brand kept");

        let (moov, moov_end) = find(&out, 0, &[b"moov"]).unwrap();
        let moov = &out[moov - 8..moov_end];
        assert!(find(moov, 8, &[b"mvex"]).is_none());
        let traks: Vec<_> = children(moov, 8).into_iter().filter(|c| &c.0 == b"trak").collect();
        assert_eq!(traks.len(), 2);

        let video = &moov[traks[0].1..traks[0].3];
        let (tkhd, _) = find(video, 8, &[b"tkhd"]).unwrap();
        assert_eq!(be32(video, tkhd + 12).unwrap(), 1);
        assert_eq!(be32(video, tkhd + 20).unwrap(), 6000, "3 fragments x 2 samples x 1000 ticks = 6 s");
        let (stss, _) = find(video, 8, &[b"mdia", b"minf", b"stbl", b"stss"]).unwrap();
        assert_eq!((be32(video, stss + 4).unwrap(), be32(video, stss + 8).unwrap(), be32(video, stss + 12).unwrap()), (3, 1, 3));
        let (stsz, _) = find(video, 8, &[b"mdia", b"minf", b"stbl", b"stsz"]).unwrap();
        assert_eq!(be32(video, stsz + 8).unwrap(), 6);

        // Chunks in time order: v0 a0 v1 a1 v2 (video 0,2,4 s; audio 0,3 s).
        let audio = &moov[traks[1].1..traks[1].3];
        let co64 = |t: &[u8]| {
            let (p, _) = find(t, 8, &[b"mdia", b"minf", b"stbl", b"co64"]).unwrap();
            (0..be32(t, p + 4).unwrap() as usize).map(|i| be64(t, p + 8 + i * 8).unwrap()).collect::<Vec<_>>()
        };
        let (vo, ao) = (co64(video), co64(audio));
        assert_eq!(vo.len(), 3);
        assert_eq!(ao.len(), 2);
        assert!(vo[0] < ao[0] && ao[0] < vo[1] && vo[1] < ao[1] && ao[1] < vo[2]);
        assert_eq!(&out[vo[1] as usize..vo[1] as usize + 5], &[0x11; 5]);
        assert_eq!(&out[ao[1] as usize..ao[1] as usize + 5], &[0x21; 5]);
        let (mdat, mdat_end) = find(&out, 0, &[b"mdat"]).unwrap();
        assert_eq!(mdat_end - mdat, 25, "5 chunks of 5 bytes");

        // A regular MP4 with two tracks is a valid input too (direct files, muxed HLS).
        let again = dir.join("again.mp4");
        remux(&[o.as_path()], &again).unwrap();
        let out2 = std::fs::read(&again).unwrap();
        let (moov2, moov2_end) = find(&out2, 0, &[b"moov"]).unwrap();
        let moov2 = &out2[moov2 - 8..moov2_end];
        let traks2: Vec<_> = children(moov2, 8).into_iter().filter(|c| &c.0 == b"trak").collect();
        assert_eq!(traks2.len(), 2);
        let video2 = &moov2[traks2[0].1..traks2[0].3];
        let (stsz2, _) = find(video2, 8, &[b"mdia", b"minf", b"stbl", b"stsz"]).unwrap();
        assert_eq!(be32(video2, stsz2 + 8).unwrap(), 6);
        let (stss2, _) = find(video2, 8, &[b"mdia", b"minf", b"stbl", b"stss"]).unwrap();
        assert_eq!(be32(video2, stss2 + 4).unwrap(), 3, "key frames survive");
        let vo2 = co64(video2);
        assert_eq!(&out2[vo2[2] as usize..vo2[2] as usize + 5], &[0x12; 5]);

        // One track (YouTube "audio only") works the same way.
        let single = dir.join("single.m4a");
        remux(&[a.as_path()], &single).unwrap();
        let out = std::fs::read(&single).unwrap();
        let (moov, moov_end) = find(&out, 0, &[b"moov"]).unwrap();
        assert_eq!(children(&out[moov - 8..moov_end], 8).iter().filter(|c| &c.0 == b"trak").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
