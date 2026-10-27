//! The Site Grabber: explores a web site from a start page, follows its links
//! to a set depth and collects the files of the chosen types (images, audio,
//! video, documents, ...). The pages are read one after the other.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use percent_encoding::percent_decode_str;
use reqwest::header::CONTENT_TYPE;
use url::Url;

use super::http::{describe_error, extension_for_mime, wildcard_match, RequestOptions};

/// File types of pages: the grabber reads these and follows their links.
const PAGE_TYPES: [&str; 12] = ["html", "htm", "shtml", "xhtml", "php", "asp", "aspx", "jsp", "jspx", "cgi", "cfm", "pl"];
/// Parts of web pages that are not files to download, even for "All files".
const WEB_TYPES: [&str; 14] = ["css", "js", "mjs", "json", "xml", "rss", "atom", "ico", "woff", "woff2", "ttf", "eot", "otf", "map"];
/// A page larger than this is read only up to here.
const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// The project templates of the first Site Grabber step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrabTemplate {
    #[default]
    Images,
    Audio,
    Video,
    Documents,
    Archives,
    /// Every file that is not part of a page.
    AllFiles,
    /// The file types the user types.
    Custom,
}

impl GrabTemplate {
    pub const ALL: [GrabTemplate; 7] =
        [GrabTemplate::Images, GrabTemplate::Audio, GrabTemplate::Video, GrabTemplate::Documents, GrabTemplate::Archives, GrabTemplate::AllFiles, GrabTemplate::Custom];

    pub fn label(self) -> &'static str {
        match self {
            GrabTemplate::Images => "Download all images from a web site",
            GrabTemplate::Audio => "Download all audio files from a web site",
            GrabTemplate::Video => "Download all video files from a web site",
            GrabTemplate::Documents => "Download all documents from a web site",
            GrabTemplate::Archives => "Download all archives and programs from a web site",
            GrabTemplate::AllFiles => "Download all files linked from the pages",
            GrabTemplate::Custom => "Download the files of the types below",
        }
    }

    /// The file types of the template (space separated; empty: every file).
    pub fn types(self) -> &'static str {
        match self {
            GrabTemplate::Images => "JPG JPEG PNG GIF BMP WEBP SVG TIF TIFF AVIF",
            GrabTemplate::Audio => "MP3 WAV FLAC AAC OGG WMA M4A OPUS AIF AIFF MPA RA",
            GrabTemplate::Video => "MP4 MKV AVI MOV WMV FLV WEBM M4V 3GP MPG MPEG OGV ASF RM RMVB QT",
            GrabTemplate::Documents => "PDF DOC DOCX TXT RTF XLS XLSX PPT PPTX PPS CSV EPUB ODT ODS ODP MOBI",
            GrabTemplate::Archives => "ZIP RAR 7Z TAR GZ BZ2 XZ TGZ ISO CAB ZST EXE MSI APK DMG",
            GrabTemplate::AllFiles | GrabTemplate::Custom => "",
        }
    }
}

/// What to explore and which files to collect.
#[derive(Debug, Clone)]
pub struct GrabSettings {
    pub start: String,
    /// Link levels to follow from the start page (0: the start page only).
    pub depth: u32,
    /// Explore only pages on the start page's server.
    pub same_server: bool,
    /// Explore only pages in the start page's folder and below it.
    pub same_folder: bool,
    /// Collect files from other servers too (pictures often come from another server).
    pub other_server_files: bool,
    /// The grabber stops after this number of pages.
    pub max_pages: usize,
    /// Patterns for the file types to collect ("jpg", "mp*"); empty: every file that is not a page.
    pub file_types: Vec<String>,
    /// Login for the start page's server only.
    pub login: Option<(String, String)>,
}

impl Default for GrabSettings {
    fn default() -> Self {
        Self {
            start: String::new(),
            depth: 1,
            same_server: true,
            same_folder: false,
            other_server_files: true,
            max_pages: 200,
            file_types: Vec::new(),
            login: None,
        }
    }
}

/// Splits "JPG png  mp*" into lowercase patterns.
pub fn parse_types(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(|t| t.trim().trim_start_matches("*.").trim_start_matches('.').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundFile {
    pub url: String,
    /// The page that links to the file (sent as the Referer).
    pub page: String,
}

/// Shared with the window while the grabber works.
#[derive(Debug, Default)]
pub struct GrabProgress {
    pub pages_done: AtomicUsize,
    /// Pages read and pages that wait to be read.
    pub pages_found: AtomicUsize,
    pub stop: AtomicBool,
    pub finished: AtomicBool,
    pub files: Mutex<Vec<FoundFile>>,
    pub current: Mutex<String>,
    pub error: Mutex<Option<String>>,
}

impl GrabProgress {
    pub fn file_count(&self) -> usize {
        self.files.lock().map(|f| f.len()).unwrap_or(0)
    }
}

/// Explores the site and puts the files it finds into `progress`. It sets
/// `progress.finished` at the end, also after an error or a stop.
pub async fn explore(settings: GrabSettings, request: RequestOptions, progress: Arc<GrabProgress>) {
    if let Err(e) = run(&settings, &request, &progress).await {
        if let Ok(mut error) = progress.error.lock() {
            *error = Some(e);
        }
    }
    progress.finished.store(true, Ordering::SeqCst);
}

enum Fetched {
    Page(String, Url),
    /// Not a page: a file with this type (from its Content-Type).
    File(Option<&'static str>),
}

async fn run(settings: &GrabSettings, request: &RequestOptions, progress: &GrabProgress) -> Result<(), String> {
    let mut start = Url::parse(settings.start.trim()).map_err(|_| "The start page address is not correct".to_string())?;
    if !matches!(start.scheme(), "http" | "https") {
        return Err("The start page address must start with http:// or https://".to_string());
    }
    let mut request = request.clone();
    request.connect_timeout = Duration::from_secs(15);
    request.read_timeout = Duration::from_secs(20);
    request.basic_auth = None;
    let client = request.build_client().map_err(|e| describe_error(&e))?;

    // A page to read, its link level and the page that links to it.
    let mut queue = VecDeque::from([(start.clone(), 0u32, start.clone())]);
    let mut seen_pages = HashSet::from([key(&start)]);
    let mut seen_files = HashSet::new();
    progress.pages_found.store(1, Ordering::Relaxed);
    let mut done = 0usize;
    while let Some((page, level, parent)) = queue.pop_front() {
        if progress.stop.load(Ordering::Relaxed) || done >= settings.max_pages.max(1) {
            break;
        }
        if let Ok(mut current) = progress.current.lock() {
            *current = page.to_string();
        }
        let login = settings.login.as_ref().filter(|_| same_server(&page, &start));
        let fetched = tokio::select! {
            r = fetch(&client, &request, &page, login) => r,
            _ = stopped(progress) => break,
        };
        done += 1;
        progress.pages_done.store(done, Ordering::Relaxed);
        let (html, base) = match fetched {
            Ok(Fetched::Page(html, base)) => (html, base),
            Ok(Fetched::File(ext)) => {
                if done == 1 {
                    return Err("The start page is not a web page".to_string());
                }
                // A link without a file type that is a file after all ("/download/12").
                if ext.is_some_and(|e| type_wanted(e, &settings.file_types)) && seen_files.insert(key(&page)) {
                    add_file(progress, &page, &parent);
                }
                continue;
            }
            Err(e) if done == 1 => return Err(format!("Cannot open the start page: {}", e)),
            Err(_) => continue,
        };
        if done == 1 {
            // The start page moved (http to https, another host name): compare with its new address.
            start = base.clone();
        }
        for link in page_links(&html, &base) {
            if file_wanted(&link, &settings.file_types)
                && (settings.other_server_files || same_server(&link, &start))
                && seen_files.insert(key(&link))
            {
                add_file(progress, &link, &base);
            }
            if level < settings.depth && is_page(&link) && page_allowed(&link, &start, settings) && seen_pages.insert(key(&link)) {
                queue.push_back((link, level + 1, base.clone()));
                progress.pages_found.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}

fn add_file(progress: &GrabProgress, file: &Url, page: &Url) {
    if let Ok(mut files) = progress.files.lock() {
        files.push(FoundFile { url: file.to_string(), page: page.to_string() });
    }
}

/// Returns when the user stops the grabber.
async fn stopped(progress: &GrabProgress) {
    while !progress.stop.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn fetch(client: &reqwest::Client, request: &RequestOptions, url: &Url, login: Option<&(String, String)>) -> Result<Fetched, String> {
    let mut req = request.get(client, url.as_str());
    if let Some((user, pass)) = login {
        req = req.basic_auth(user, Some(pass));
    }
    let mut resp = req.send().await.map_err(|e| describe_error(&e))?;
    if !resp.status().is_success() {
        return Err(format!("the server answered {}", resp.status()));
    }
    let content_type = resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if !content_type.is_empty() && !content_type.contains("html") {
        return Ok(Fetched::File(extension_for_mime(&content_type)));
    }
    let base = resp.url().clone();
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| describe_error(&e))? {
        body.extend_from_slice(&chunk);
        if body.len() >= MAX_PAGE_BYTES {
            break;
        }
    }
    Ok(Fetched::Page(String::from_utf8_lossy(&body).into_owned(), base))
}

/// The address without its #fragment, to find pages and files seen before.
fn key(url: &Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    u.to_string()
}

/// The file type of the address's last part ("jpg" for ".../a.JPG?x=1").
pub fn url_type(url: &Url) -> String {
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let name = percent_decode_str(last).decode_utf8_lossy();
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 10 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn is_page(url: &Url) -> bool {
    let t = url_type(url);
    t.is_empty() || PAGE_TYPES.contains(&t.as_str())
}

fn type_wanted(ext: &str, types: &[String]) -> bool {
    if types.is_empty() {
        return !PAGE_TYPES.contains(&ext) && !WEB_TYPES.contains(&ext);
    }
    types.iter().any(|t| wildcard_match(t, ext))
}

fn file_wanted(url: &Url, types: &[String]) -> bool {
    let t = url_type(url);
    !t.is_empty() && type_wanted(&t, types)
}

/// The same server: the same host (with or without "www.") and port.
fn same_server(a: &Url, b: &Url) -> bool {
    let host = |u: &Url| u.host_str().map(|h| h.trim_start_matches("www.").to_ascii_lowercase());
    host(a) == host(b) && a.port_or_known_default() == b.port_or_known_default()
}

fn page_allowed(page: &Url, start: &Url, settings: &GrabSettings) -> bool {
    if settings.same_server && !same_server(page, start) {
        return false;
    }
    if settings.same_folder {
        let folder = &start.path()[..start.path().rfind('/').map_or(0, |i| i + 1)];
        return same_server(page, start) && page.path().starts_with(folder);
    }
    true
}

/// The http(s) addresses a page links to (pages, pictures, media, files), in
/// page order, made absolute with the page's address or its `<base href>`.
pub fn page_links(html: &str, page: &Url) -> Vec<Url> {
    let mut base = page.clone();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut i = 0;
    while let Some(off) = html[i..].find('<') {
        let start = i + off;
        if html[start..].starts_with("<!--") {
            i = html[start + 4..].find("-->").map_or(html.len(), |e| start + 4 + e + 3);
            continue;
        }
        let name_start = start + 1;
        let name_end = html[name_start..].find(|c: char| !c.is_ascii_alphanumeric()).map_or(html.len(), |e| name_start + e);
        let tag = html[name_start..name_end].to_ascii_lowercase();
        if tag.is_empty() || !tag.starts_with(|c: char| c.is_ascii_alphabetic()) {
            i = name_start;
            continue;
        }
        let (attrs, end) = attributes(html, name_end);
        i = end;
        if tag == "script" || tag == "style" {
            // Their text is code, not HTML.
            i = find_ignore_case(&html[i..], &format!("</{}", tag)).map_or(html.len(), |e| i + e);
        }
        let mut values: Vec<&str> = Vec::new();
        for (name, value) in &attrs {
            match name.as_str() {
                "href" | "src" | "data" | "poster" | "data-src" | "data-original" | "data-lazy-src" | "data-href" => values.push(value),
                "srcset" | "data-srcset" => values.extend(value.split(',').filter_map(|c| c.split_whitespace().next())),
                _ => {}
            }
        }
        if tag == "base" {
            if let Some(b) = values.first().and_then(|v| page.join(v.trim()).ok()) {
                base = b;
            }
            continue;
        }
        for value in values {
            let Ok(mut url) = base.join(value.trim()) else { continue };
            if !matches!(url.scheme(), "http" | "https") {
                continue;
            }
            url.set_fragment(None);
            if seen.insert(url.to_string()) {
                out.push(url);
            }
        }
    }
    out
}

/// Position of the ASCII text `needle` in `hay`, in upper or lower case.
fn find_ignore_case(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// The attributes of a tag that starts at `pos`, and the position after its `>`.
fn attributes(html: &str, mut pos: usize) -> (Vec<(String, String)>, usize) {
    let b = html.as_bytes();
    let mut out = Vec::new();
    loop {
        while pos < b.len() && (b[pos].is_ascii_whitespace() || b[pos] == b'/') {
            pos += 1;
        }
        if pos >= b.len() {
            return (out, pos);
        }
        if b[pos] == b'>' {
            return (out, pos + 1);
        }
        let name_start = pos;
        while pos < b.len() && !b[pos].is_ascii_whitespace() && !matches!(b[pos], b'=' | b'>' | b'/') {
            pos += 1;
        }
        let name = html[name_start..pos].to_ascii_lowercase();
        while pos < b.len() && b[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos < b.len() && b[pos] == b'=' {
            pos += 1;
            while pos < b.len() && b[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let value = if pos < b.len() && (b[pos] == b'"' || b[pos] == b'\'') {
                let quote = b[pos];
                pos += 1;
                let value_start = pos;
                while pos < b.len() && b[pos] != quote {
                    pos += 1;
                }
                let value = &html[value_start..pos];
                pos = (pos + 1).min(b.len());
                value
            } else {
                let value_start = pos;
                while pos < b.len() && !b[pos].is_ascii_whitespace() && b[pos] != b'>' {
                    pos += 1;
                }
                &html[value_start..pos]
            };
            out.push((name, decode_entities(value)));
        } else {
            out.push((name, String::new()));
        }
    }
}

/// `&amp;`, `&quot;`, `&#39;`, `&#x2F;` and the like in an attribute value.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest[1..].find(';').map(|e| e + 1).filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn links_of_a_page() {
        let html = r#"<html><head><base href="https://a.org/dir/"><link rel=stylesheet href="s.css"></head>
            <!-- <a href="hidden.zip"> -->
            <a href='page2.html#top'>2</a> <A HREF=/root.pdf>pdf</A>
            <img src="i.png" srcset="i-2x.png 2x, https://cdn.org/i-3x.png 3x">
            <img data-src="lazy.jpg"><a href="mailto:x@a.org">m</a><a href="javascript:void(0)">j</a>
            <a href="q.zip?a=1&amp;b=2">q</a><video poster="p.jpg"><source src="v.mp4"></video>
            <script>var s = "<a href='code.zip'>";</script><a href="page2.html">again</a>"#;
        let got: Vec<String> = page_links(html, &url("https://a.org/start/index.html")).iter().map(|u| u.to_string()).collect();
        assert_eq!(
            got,
            [
                "https://a.org/dir/s.css",
                "https://a.org/dir/page2.html",
                "https://a.org/root.pdf",
                "https://a.org/dir/i.png",
                "https://a.org/dir/i-2x.png",
                "https://cdn.org/i-3x.png",
                "https://a.org/dir/lazy.jpg",
                "https://a.org/dir/q.zip?a=1&b=2",
                "https://a.org/dir/p.jpg",
                "https://a.org/dir/v.mp4",
            ]
        );
    }

    #[test]
    fn types_pages_and_servers() {
        assert_eq!(parse_types("JPG, *.png .Gif  mp*"), ["jpg", "png", "gif", "mp*"]);
        assert_eq!(url_type(&url("https://a.org/x/Photo%201.JPG?size=2")), "jpg");
        assert_eq!(url_type(&url("https://a.org/x/")), "");
        assert_eq!(url_type(&url("https://a.org/.htaccess")), "");
        assert!(is_page(&url("https://a.org/x/")) && is_page(&url("https://a.org/a.php?id=2")) && !is_page(&url("https://a.org/a.zip")));
        let images = parse_types(GrabTemplate::Images.types());
        assert!(file_wanted(&url("https://a.org/a.webp"), &images) && !file_wanted(&url("https://a.org/a.mp3"), &images));
        assert!(file_wanted(&url("https://a.org/a.iso"), &[]) && !file_wanted(&url("https://a.org/a.css"), &[]) && !file_wanted(&url("https://a.org/a.html"), &[]));
        assert!(same_server(&url("https://www.a.org/x"), &url("https://a.org/y")) && !same_server(&url("https://a.org:8080/"), &url("https://a.org/")));
        let s = GrabSettings { same_folder: true, ..Default::default() };
        let start = url("https://a.org/gallery/index.html");
        assert!(page_allowed(&url("https://a.org/gallery/2/p.html"), &start, &s) && !page_allowed(&url("https://a.org/other/p.html"), &start, &s));
        assert!(!page_allowed(&url("https://b.org/gallery/p.html"), &start, &GrabSettings::default()));
        assert_eq!(decode_entities("a&amp;b&#47;c&#x41;&bogus;&"), "a&b/cA&bogus;&");
    }

    /// A small web site on 127.0.0.1: path -> (content type, body).
    fn site(pages: HashMap<&'static str, (&'static str, String)>) -> String {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in server.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut header = String::new();
                while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
                    header.clear();
                }
                let (status, kind, body) = match pages.get(path.as_str()) {
                    Some((kind, body)) => ("200 OK", *kind, body.clone()),
                    None => ("404 Not Found", "text/plain", "missing".to_string()),
                };
                let mut stream = reader.into_inner();
                let _ = write!(stream, "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", status, kind, body.len(), body);
            }
        });
        base
    }

    fn grab(settings: GrabSettings) -> (Vec<String>, usize, Option<String>) {
        let progress = Arc::new(GrabProgress::default());
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(explore(settings, RequestOptions::default(), progress.clone()));
        assert!(progress.finished.load(Ordering::SeqCst));
        let files = progress.files.lock().unwrap().iter().map(|f| f.url.rsplit('/').next().unwrap().to_string()).collect();
        let error = progress.error.lock().unwrap().clone();
        (files, progress.pages_done.load(Ordering::SeqCst), error)
    }

    #[test]
    fn explores_a_site_to_the_set_depth() {
        let html = |s: &str| ("text/html; charset=utf-8", s.to_string());
        let base = site(HashMap::from([
            ("/gallery/", html(r#"<a href="a.html">a</a> <img src="1.jpg"> <a href="/other/b.html">b</a> <a href="/dl/7">file</a> <a href="song.mp3">s</a>"#)),
            ("/gallery/a.html", html(r#"<img src="2.png"><a href="deep.html">d</a><img src="1.jpg">"#)),
            ("/gallery/deep.html", html(r#"<img src="3.gif">"#)),
            ("/other/b.html", html(r#"<img src="4.jpg">"#)),
            ("/dl/7", ("image/jpeg", "jpeg".to_string())),
        ]));
        let images = |depth, same_folder| GrabSettings {
            start: format!("{}/gallery/", base),
            depth,
            same_folder,
            file_types: parse_types(GrabTemplate::Images.types()),
            ..Default::default()
        };
        let (files, pages, error) = grab(images(0, false));
        assert_eq!((files, pages, error), (vec!["1.jpg".to_string()], 1, None), "depth 0: the start page only");
        let (files, pages, _) = grab(images(1, false));
        assert_eq!(files, ["1.jpg", "2.png", "4.jpg", "7"], "a link without a type is a picture after all");
        assert_eq!(pages, 4);
        let (files, _, _) = grab(images(2, true));
        assert_eq!(files, ["1.jpg", "2.png", "3.gif"], "the other folders are not explored");
        let (files, _, _) = grab(GrabSettings { max_pages: 2, ..images(2, false) });
        assert_eq!(files, ["1.jpg", "2.png"], "the grabber stops after the set number of pages");
        let (files, _, _) = grab(GrabSettings { file_types: parse_types("mp3"), ..images(1, false) });
        assert_eq!(files, ["song.mp3"]);
        let (_, _, error) = grab(GrabSettings { start: format!("{}/nothing/", base), ..Default::default() });
        assert!(error.is_some_and(|e| e.starts_with("Cannot open the start page")));
    }
}
