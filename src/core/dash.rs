//! MPEG-DASH manifests (`.mpd`): picks one video and one audio
//! representation and lists their segment addresses, so the stream engine can
//! download them like HLS parts and join them into one MP4.
//!
//! Supported: `SegmentTemplate` (`$Number$`, `$Time$` with `SegmentTimeline`,
//! `$RepresentationID$`, `$Bandwidth$`), `SegmentList`, and one file per
//! representation (`BaseURL`, with or without `SegmentBase`). MP4
//! representations are preferred; WebM-only manifests give WebM video with
//! WebM (Opus or Vorbis) audio.

use url::Url;

use super::hls::{HlsSegment, MediaPlaylist};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// One XML element start: name and attributes (MPDs need nothing more).
#[derive(Debug, Clone, Default)]
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    self_closing: bool,
}

impl Tag {
    fn get(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
enum Token {
    Open(Tag),
    Close(String),
    Text(String),
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Element and text tokens; comments, declarations and processing
/// instructions are skipped. Namespace prefixes are removed from names.
fn tokens(xml: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        let text = rest[..lt].trim();
        if !text.is_empty() {
            out.push(Token::Text(unescape(text)));
        }
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map(|e| &rest[e + 3..]).unwrap_or("");
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let inner = &rest[1..gt];
        rest = &rest[gt + 1..];
        if inner.starts_with('?') || inner.starts_with('!') {
            continue;
        }
        let local = |n: &str| n.rsplit(':').next().unwrap_or(n).to_string();
        if let Some(name) = inner.strip_prefix('/') {
            out.push(Token::Close(local(name.trim())));
            continue;
        }
        let self_closing = inner.ends_with('/');
        let inner = inner.trim_end_matches('/');
        let (name, mut attrs_src) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
        let mut attrs = Vec::new();
        while let Some(eq) = attrs_src.find('=') {
            let key = attrs_src[..eq].trim().to_string();
            let after = attrs_src[eq + 1..].trim_start();
            let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
            let Some(end) = after[1..].find(quote) else { break };
            attrs.push((local(&key), unescape(&after[1..1 + end])));
            attrs_src = &after[end + 2..];
        }
        out.push(Token::Open(Tag { name: local(name), attrs, self_closing }));
    }
    out
}

/// ISO 8601 duration such as `PT1H2M3.5S` (days included), in seconds.
fn iso_duration(s: &str) -> Option<f64> {
    let s = s.strip_prefix('P')?;
    let (days, time) = match s.split_once('T') {
        Some((d, t)) => (d, t),
        None => (s, ""),
    };
    let mut total = 0.0;
    let mut num = String::new();
    for c in days.chars() {
        if c == 'D' {
            total += num.parse::<f64>().ok()? * 86400.0;
            num.clear();
        } else {
            num.push(c);
        }
    }
    num.clear();
    for c in time.chars() {
        let unit = match c {
            'H' => 3600.0,
            'M' => 60.0,
            'S' => 1.0,
            _ => {
                num.push(c);
                continue;
            }
        };
        total += num.parse::<f64>().ok()? * unit;
        num.clear();
    }
    Some(total)
}

#[derive(Debug, Clone, Default)]
struct Template {
    media: Option<String>,
    init: Option<String>,
    start_number: u64,
    timescale: u64,
    duration: Option<u64>,
    /// (t, d, r) entries of a SegmentTimeline.
    timeline: Vec<(Option<u64>, u64, i64)>,
}

#[derive(Debug, Clone, Default)]
struct SegList {
    init: Option<(String, Option<String>)>,
    media: Vec<(String, Option<String>)>,
}

/// One representation with everything inherited from its parents.
#[derive(Debug, Clone, Default)]
pub struct Representation {
    pub id: String,
    pub bandwidth: u64,
    pub width: u32,
    pub height: u32,
    pub codecs: String,
    pub mime: String,
    base: String,
    template: Option<Template>,
    list: Option<SegList>,
}

impl Representation {
    pub fn is_video(&self) -> bool {
        self.mime.starts_with("video/") || self.height > 0
    }

    pub fn is_audio(&self) -> bool {
        self.mime.starts_with("audio/")
    }

    fn is_mp4(&self) -> bool {
        self.mime.ends_with("/mp4") || self.mime.is_empty()
    }

    fn is_webm(&self) -> bool {
        self.mime.ends_with("/webm")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Mpd {
    pub duration: f64,
    pub representations: Vec<Representation>,
}

fn byte_range(spec: Option<&str>) -> Option<(u64, u64)> {
    let (a, b) = spec?.split_once('-')?;
    let start: u64 = a.trim().parse().ok()?;
    let end: u64 = b.trim().parse().ok()?;
    Some((start, end.checked_sub(start)? + 1))
}

/// Parses an MPD; `base` is the manifest address.
pub fn parse(xml: &str, base: &Url) -> Result<Mpd, BoxError> {
    let toks = tokens(xml);
    if !toks.iter().any(|t| matches!(t, Token::Open(tag) if tag.name == "MPD")) {
        return Err("The address is not a DASH manifest".into());
    }
    if toks.iter().any(|t| matches!(t, Token::Open(tag) if tag.name == "ContentProtection")) {
        return Err("This DASH stream is protected (DRM) and cannot be downloaded".into());
    }

    let mut mpd = Mpd::default();
    // Context stack: (element name, base URL, template, list, adaptation-set attributes).
    struct Level {
        name: String,
        base: String,
        template: Option<Template>,
        list: Option<SegList>,
        set: Tag,
    }
    let mut stack: Vec<Level> = vec![Level { name: String::new(), base: base.to_string(), template: None, list: None, set: Tag::default() }];
    let mut current: Option<Representation> = None;
    let mut in_base_url = false;
    let mut in_timeline = false;

    for tok in toks {
        match tok {
            Token::Open(tag) => {
                let parent = stack.last().unwrap();
                let (base_now, template_now, list_now, set_now) = (parent.base.clone(), parent.template.clone(), parent.list.clone(), parent.set.clone());
                match tag.name.as_str() {
                    "MPD" => {
                        mpd.duration = tag.get("mediaPresentationDuration").and_then(iso_duration).unwrap_or(0.0);
                    }
                    "Period" => {
                        if mpd.duration == 0.0 {
                            mpd.duration = tag.get("duration").and_then(iso_duration).unwrap_or(0.0);
                        }
                    }
                    "BaseURL" => in_base_url = true,
                    "SegmentTimeline" => in_timeline = true,
                    "S" if in_timeline => {
                        let t = tag.get("t").and_then(|v| v.parse().ok());
                        let d = tag.get("d").and_then(|v| v.parse().ok()).unwrap_or(0);
                        let r = tag.get("r").and_then(|v| v.parse().ok()).unwrap_or(0);
                        if let Some(tpl) = current.as_mut().and_then(|c| c.template.as_mut()).or(stack.last_mut().unwrap().template.as_mut()) {
                            tpl.timeline.push((t, d, r));
                        }
                    }
                    "SegmentTemplate" => {
                        let inherited = current.as_ref().and_then(|c| c.template.clone()).or_else(|| template_now.clone()).unwrap_or_default();
                        let tpl = Template {
                            media: tag.get("media").map(str::to_string).or(inherited.media),
                            init: tag.get("initialization").map(str::to_string).or(inherited.init),
                            start_number: tag.get("startNumber").and_then(|v| v.parse().ok()).unwrap_or(if inherited.start_number == 0 { 1 } else { inherited.start_number }),
                            timescale: tag.get("timescale").and_then(|v| v.parse().ok()).unwrap_or(if inherited.timescale == 0 { 1 } else { inherited.timescale }),
                            duration: tag.get("duration").and_then(|v| v.parse().ok()).or(inherited.duration),
                            timeline: Vec::new(),
                        };
                        match current.as_mut() {
                            Some(rep) => rep.template = Some(tpl),
                            None => stack.last_mut().unwrap().template = Some(tpl),
                        }
                    }
                    "SegmentList" => {
                        let list = SegList::default();
                        match current.as_mut() {
                            Some(rep) => rep.list = Some(list),
                            None => stack.last_mut().unwrap().list = Some(list),
                        }
                    }
                    "Initialization" => {
                        let entry = tag.get("sourceURL").map(|u| (u.to_string(), tag.get("range").map(str::to_string)));
                        let list = current.as_mut().and_then(|c| c.list.as_mut()).or(stack.last_mut().unwrap().list.as_mut());
                        if let (Some(list), Some(entry)) = (list, entry) {
                            list.init = Some(entry);
                        }
                    }
                    "SegmentURL" => {
                        let entry = (tag.get("media").unwrap_or("").to_string(), tag.get("mediaRange").map(str::to_string));
                        if let Some(list) = current.as_mut().and_then(|c| c.list.as_mut()).or(stack.last_mut().unwrap().list.as_mut()) {
                            list.media.push(entry);
                        }
                    }
                    "AdaptationSet" => {
                        if !tag.self_closing {
                            stack.push(Level { name: tag.name.clone(), base: base_now, template: template_now, list: list_now, set: tag.clone() });
                        }
                        continue;
                    }
                    "Representation" => {
                        let attr = |k: &str| tag.get(k).or(set_now.get(k)).unwrap_or("").to_string();
                        let mut mime = attr("mimeType");
                        if mime.is_empty() {
                            mime = match set_now.get("contentType") {
                                Some("video") => "video/mp4".into(),
                                Some("audio") => "audio/mp4".into(),
                                _ => String::new(),
                            };
                        }
                        current = Some(Representation {
                            id: attr("id"),
                            bandwidth: attr("bandwidth").parse().unwrap_or(0),
                            width: attr("width").parse().unwrap_or(0),
                            height: attr("height").parse().unwrap_or(0),
                            codecs: attr("codecs"),
                            mime,
                            base: base_now,
                            template: template_now,
                            list: list_now,
                        });
                        if tag.self_closing {
                            mpd.representations.extend(current.take());
                        }
                        continue;
                    }
                    _ => {}
                }
                if !tag.self_closing && matches!(tag.name.as_str(), "MPD" | "Period") {
                    stack.push(Level { name: tag.name.clone(), base: base_now, template: template_now, list: list_now, set: set_now });
                }
            }
            Token::Text(text) if in_base_url => {
                let joined = |b: &str| Url::parse(b).and_then(|b| b.join(&text)).map(|u| u.to_string()).unwrap_or(text.clone());
                match current.as_mut() {
                    Some(rep) => rep.base = joined(&rep.base),
                    None => {
                        let level = stack.last_mut().unwrap();
                        level.base = joined(&level.base);
                    }
                }
            }
            Token::Text(_) => {}
            Token::Close(name) => match name.as_str() {
                "BaseURL" => in_base_url = false,
                "SegmentTimeline" => in_timeline = false,
                "Representation" => mpd.representations.extend(current.take()),
                "AdaptationSet" | "Period" | "MPD" if stack.len() > 1 && stack.last().is_some_and(|l| l.name == name) => {
                    stack.pop();
                }
                _ => {}
            },
        }
    }
    Ok(mpd)
}

/// Fills `$Identifier$` and `$Identifier%0Nd$` placeholders.
fn fill(template: &str, rep: &Representation, number: u64, time: u64) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('$') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = &after[..end];
        let (name, format) = token.split_once('%').unwrap_or((token, ""));
        let value = match name {
            "" => Some("$".to_string()),
            "RepresentationID" => Some(rep.id.clone()),
            "Bandwidth" => Some(rep.bandwidth.to_string()),
            "Number" => Some(number.to_string()),
            "Time" => Some(time.to_string()),
            _ => None,
        };
        match value {
            Some(v) => {
                let width: usize = format.trim_start_matches('0').trim_end_matches('d').parse().unwrap_or(0);
                out.push_str(&format!("{:0>width$}", v, width = width));
            }
            None => out.push_str(&format!("${}$", token)),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

impl Mpd {
    /// Segment addresses of one representation.
    pub fn playlist(&self, rep: &Representation) -> Result<MediaPlaylist, BoxError> {
        let base = Url::parse(&rep.base)?;
        let abs = |u: &str| base.join(u).map(|x| x.to_string());
        if let Some(ref t) = rep.template {
            let media = t.media.as_deref().ok_or("The DASH template has no media address")?;
            let init = match t.init.as_deref() {
                Some(i) => Some(HlsSegment { url: abs(&fill(i, rep, 0, 0))?, byte_range: None, key: None }),
                None => None,
            };
            let mut segments = Vec::new();
            if !t.timeline.is_empty() {
                let mut time = 0u64;
                let mut number = t.start_number;
                for &(start, d, r) in &t.timeline {
                    if let Some(s) = start {
                        time = s;
                    }
                    let repeat = if r < 0 {
                        // Repeat until the end of the period.
                        let end = (self.duration * t.timescale as f64) as u64;
                        if d == 0 { 0 } else { end.saturating_sub(time).div_ceil(d).saturating_sub(1) }
                    } else {
                        r as u64
                    };
                    for _ in 0..=repeat {
                        segments.push(HlsSegment { url: abs(&fill(media, rep, number, time))?, byte_range: None, key: None });
                        time += d;
                        number += 1;
                    }
                }
            } else {
                let d = t.duration.ok_or("The DASH template has no segment duration")?;
                let count = ((self.duration * t.timescale as f64) / d as f64).ceil().max(1.0) as u64;
                for i in 0..count {
                    let number = t.start_number + i;
                    segments.push(HlsSegment { url: abs(&fill(media, rep, number, i * d))?, byte_range: None, key: None });
                }
            }
            return Ok(MediaPlaylist { init, segments, ..Default::default() });
        }
        if let Some(ref list) = rep.list {
            let seg = |(u, r): &(String, Option<String>)| -> Result<HlsSegment, BoxError> {
                let url = if u.is_empty() { rep.base.clone() } else { abs(u)? };
                Ok(HlsSegment { url, byte_range: byte_range(r.as_deref()), key: None })
            };
            return Ok(MediaPlaylist {
                init: list.init.as_ref().map(seg).transpose()?,
                segments: list.media.iter().map(seg).collect::<Result<_, _>>()?,
                ..Default::default()
            });
        }
        // One file for the whole representation.
        Ok(MediaPlaylist { segments: vec![HlsSegment { url: rep.base.clone(), byte_range: None, key: None }], ..Default::default() })
    }

    /// Best MP4 video (closest to `height` when given) and best MP4 audio;
    /// WebM video and audio when the manifest has no MP4 video.
    pub fn choose(&self, height: Option<u32>) -> Result<(&Representation, Option<&Representation>), BoxError> {
        let mut webm = false;
        let mut videos: Vec<&Representation> = self.representations.iter().filter(|r| r.is_video() && r.is_mp4()).collect();
        if videos.is_empty() {
            webm = true;
            videos = self.representations.iter().filter(|r| r.is_video() && r.is_webm()).collect();
        }
        if videos.is_empty() {
            return Err(if self.representations.iter().any(|r| r.is_video()) {
                "The DASH video is neither MP4 nor WebM".into()
            } else {
                "The DASH manifest has no video".into()
            });
        }
        // The closest height (the best bitrate among equals), or the largest.
        let video = match height {
            Some(h) => videos.iter().min_by_key(|r| (r.height.abs_diff(h), std::cmp::Reverse(r.bandwidth))),
            None => videos.iter().max_by_key(|r| (r.height, r.bandwidth)),
        }
        .copied()
        .ok_or("The DASH manifest has no video")?;
        let audio = self
            .representations
            .iter()
            .filter(|r| r.is_audio() && if webm { r.is_webm() } else { r.is_mp4() })
            .max_by_key(|r| (r.codecs.starts_with("mp4a") || r.codecs.starts_with("opus"), r.bandwidth));
        Ok((video, audio))
    }
}

/// True when the text is a DASH manifest.
pub fn looks_like_mpd(text: &str) -> bool {
    let head: String = text.chars().take(2048).collect();
    head.contains("<MPD") || head.contains(":MPD")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE_MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT10S" type="static">
  <!-- comment -->
  <Period>
    <BaseURL>media/</BaseURL>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" startNumber="1"
        initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/seg-$Number%05d$.m4s"/>
      <Representation id="v720" bandwidth="2000000" width="1280" height="720" codecs="avc1.64001f"/>
      <Representation id="v360" bandwidth="600000" width="640" height="360" codecs="avc1.4d401e"/>
    </AdaptationSet>
    <AdaptationSet contentType="audio" mimeType="audio/mp4" lang="en">
      <Representation id="a128" bandwidth="128000" codecs="mp4a.40.2">
        <SegmentTemplate timescale="48000" initialization="a/init.mp4" media="a/$Time$.m4s">
          <SegmentTimeline><S t="0" d="192000" r="1"/><S d="96000"/></SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn template_numbers_and_timeline() {
        let base = Url::parse("https://cdn.example.com/vod/manifest.mpd").unwrap();
        let mpd = parse(TEMPLATE_MPD, &base).unwrap();
        assert_eq!(mpd.duration, 10.0);
        assert_eq!(mpd.representations.len(), 3);
        let (video, audio) = mpd.choose(None).unwrap();
        assert_eq!(video.id, "v720");
        let v = mpd.playlist(video).unwrap();
        assert_eq!(v.init.unwrap().url, "https://cdn.example.com/vod/media/v720/init.mp4");
        assert_eq!(v.segments.len(), 3, "10 s in 4 s segments");
        assert_eq!(v.segments[2].url, "https://cdn.example.com/vod/media/v720/seg-00003.m4s");
        let a = mpd.playlist(audio.unwrap()).unwrap();
        let urls: Vec<&str> = a.segments.iter().map(|s| s.url.rsplit('/').next().unwrap()).collect();
        assert_eq!(urls, vec!["0.m4s", "192000.m4s", "384000.m4s"]);
        assert_eq!(mpd.choose(Some(360)).unwrap().0.id, "v360");
    }

    #[test]
    fn single_file_and_segment_list() {
        let xml = r#"<MPD mediaPresentationDuration="PT1M0.5S"><Period>
          <AdaptationSet mimeType="video/mp4"><Representation id="1" bandwidth="1" height="480">
            <BaseURL>https://x.example/v480.mp4</BaseURL><SegmentBase indexRange="800-1500"><Initialization range="0-799"/></SegmentBase>
          </Representation></AdaptationSet>
          <AdaptationSet mimeType="audio/mp4"><Representation id="2" bandwidth="1">
            <BaseURL>audio.mp4</BaseURL>
            <SegmentList><Initialization sourceURL="audio.mp4" range="0-99"/>
              <SegmentURL mediaRange="100-199"/><SegmentURL mediaRange="200-299"/></SegmentList>
          </Representation></AdaptationSet></Period></MPD>"#;
        let mpd = parse(xml, &Url::parse("https://x.example/m/a.mpd").unwrap()).unwrap();
        assert_eq!(mpd.duration, 60.5);
        let (video, audio) = mpd.choose(None).unwrap();
        let v = mpd.playlist(video).unwrap();
        assert!(v.init.is_none());
        assert_eq!(v.segments[0].url, "https://x.example/v480.mp4");
        let a = mpd.playlist(audio.unwrap()).unwrap();
        assert_eq!(a.init.as_ref().unwrap().byte_range, Some((0, 100)));
        assert_eq!(a.segments.len(), 2);
        assert_eq!(a.segments[1].url, "https://x.example/m/audio.mp4");
        assert_eq!(a.segments[1].byte_range, Some((200, 100)));
    }

    #[test]
    fn drm_is_refused_and_webm_pairs_with_webm_audio() {
        let base = Url::parse("https://x/a.mpd").unwrap();
        assert!(parse(r#"<MPD><Period><AdaptationSet><ContentProtection schemeIdUri="urn:x"/></AdaptationSet></Period></MPD>"#, &base).is_err());
        let webm = parse(
            r#"<MPD><Period><AdaptationSet mimeType="video/webm"><Representation id="1" height="720"/></AdaptationSet><AdaptationSet mimeType="audio/mp4"><Representation id="a" codecs="mp4a.40.2"/></AdaptationSet><AdaptationSet mimeType="audio/webm"><Representation id="o" codecs="opus" bandwidth="1"/><Representation id="v" codecs="vorbis" bandwidth="9"/></AdaptationSet></Period></MPD>"#,
            &base,
        )
        .unwrap();
        let (video, audio) = webm.choose(None).unwrap();
        assert_eq!((video.id.as_str(), audio.unwrap().id.as_str()), ("1", "o"));
    }
}
