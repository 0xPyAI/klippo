//! HTTP client construction and response-metadata helpers shared by the
//! segmented engine, the HLS grabber and the GUI probe.

use std::path::{Path, PathBuf};
use std::time::Duration;

use percent_encoding::percent_decode_str;
use reqwest::header::{HeaderMap, ACCEPT_ENCODING, CONTENT_TYPE, USER_AGENT};
use reqwest::{Client, RequestBuilder};

/// Browser-like agent string; some servers refuse unknown agents.
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Klippo/1.1";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    /// Proxy for `http://` URLs, e.g. `http://host:8080`.
    pub http: Option<String>,
    /// Proxy for `https://` URLs.
    pub https: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Comma or semicolon separated hosts that bypass the proxy.
    pub bypass: Option<String>,
}

/// The body of a form that answered with a file (a POST download).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostBody {
    pub data: Vec<u8>,
    /// `application/x-www-form-urlencoded`, `multipart/form-data; boundary=...`, ...
    pub content_type: String,
}

/// Everything needed to build a request: client options plus per-download headers.
#[derive(Debug, Clone)]
pub struct RequestOptions {
    pub user_agent: String,
    pub headers: HeaderMap,
    pub basic_auth: Option<(String, String)>,
    pub proxy: Option<ProxyConfig>,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    /// The download came from a form: `download` sends this POST again.
    pub post: Option<PostBody>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            headers: HeaderMap::new(),
            basic_auth: None,
            proxy: None,
            connect_timeout: Duration::from_secs(20),
            read_timeout: Duration::from_secs(30),
            post: None,
        }
    }
}

impl RequestOptions {
    pub fn build_client(&self) -> reqwest::Result<Client> {
        // No total timeout: a large segment can stream for hours. The read
        // timeout catches stalled connections instead.
        let mut builder = Client::builder()
            .connect_timeout(self.connect_timeout)
            .read_timeout(self.read_timeout)
            .redirect(reqwest::redirect::Policy::limited(15));

        if let Some(ref proxy) = self.proxy {
            let bypass = proxy
                .bypass
                .as_deref()
                .map(|b| b.replace(';', ","))
                .and_then(|b| reqwest::NoProxy::from_string(&b));
            let add = |builder: reqwest::ClientBuilder, p: reqwest::Result<reqwest::Proxy>| -> reqwest::Result<reqwest::ClientBuilder> {
                let mut p = p?;
                if let Some(ref user) = proxy.username {
                    if !user.is_empty() {
                        p = p.basic_auth(user, proxy.password.as_deref().unwrap_or(""));
                    }
                }
                p = p.no_proxy(bypass.clone());
                Ok(builder.proxy(p))
            };
            if let Some(ref http) = proxy.http {
                builder = add(builder, reqwest::Proxy::http(http))?;
            }
            if let Some(ref https) = proxy.https {
                builder = add(builder, reqwest::Proxy::https(https))?;
            }
        }

        builder.build()
    }

    /// GET request with the agent, identity encoding, custom headers and credentials.
    pub fn get(&self, client: &Client, url: &str) -> RequestBuilder {
        self.finish(client.get(url))
    }

    /// The request for the file itself: a GET, or the form's POST again for a
    /// download that a form started.
    pub fn download(&self, client: &Client, url: &str) -> RequestBuilder {
        match self.post {
            Some(ref post) => self.finish(client.post(url)).header(CONTENT_TYPE, &post.content_type).body(post.data.clone()),
            None => self.get(client, url),
        }
    }

    fn finish(&self, req: RequestBuilder) -> RequestBuilder {
        // Identity encoding keeps byte offsets valid for range requests.
        let mut req = req.header(USER_AGENT, &self.user_agent).header(ACCEPT_ENCODING, "identity");
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        if let Some((ref user, ref pass)) = self.basic_auth {
            req = req.basic_auth(user, Some(pass));
        }
        req
    }
}

/// Short readable text for a reqwest error (timeouts, refused connections, ...).
pub fn describe_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "Connection timed out".to_string()
    } else if e.is_connect() {
        let mut msg = "Cannot connect to the server".to_string();
        let mut source = std::error::Error::source(e);
        while let Some(s) = source {
            msg = format!("Cannot connect to the server: {}", s);
            source = s.source();
        }
        msg
    } else if e.is_body() || e.is_decode() {
        "The connection was interrupted".to_string()
    } else if e.is_redirect() {
        "Too many redirects".to_string()
    } else {
        e.to_string()
    }
}

/// Parses `Content-Range: bytes START-END/TOTAL` (TOTAL may be `*`).
/// (served range, total size) of a Content-Range header.
pub type ContentRange = (Option<(u64, u64)>, Option<u64>);

pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let total = total.trim().parse::<u64>().ok();
    let range = range.trim();
    if range == "*" {
        return Some((None, total));
    }
    let (s, e) = range.split_once('-')?;
    Some((Some((s.trim().parse().ok()?, e.trim().parse().ok()?)), total))
}

/// Extracts the file name from a Content-Disposition header.
/// RFC 6266 `filename*` (RFC 5987 encoded) wins over plain `filename`.
pub fn parse_content_disposition(value: &str) -> Option<String> {
    let mut plain: Option<String> = None;
    let mut extended: Option<String> = None;

    for part in split_header_params(value) {
        let Some((key, val)) = part.split_once('=') else { continue };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim();
        if key == "filename*" {
            // charset'lang'percent-encoded
            let encoded = val.trim_matches('"');
            let mut pieces = encoded.splitn(3, '\'');
            let charset = pieces.next().unwrap_or("");
            let _lang = pieces.next();
            if let Some(data) = pieces.next() {
                let bytes: Vec<u8> = percent_decode_str(data).collect();
                let decoded = if charset.eq_ignore_ascii_case("utf-8") || charset.is_empty() {
                    String::from_utf8_lossy(&bytes).into_owned()
                } else {
                    bytes.iter().map(|&b| b as char).collect()
                };
                if !decoded.trim().is_empty() {
                    extended = Some(decoded);
                }
            }
        } else if key == "filename" {
            let unquoted = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                val[1..val.len() - 1].replace("\\\"", "\"")
            } else {
                val.trim_matches('"').to_string()
            };
            // Some servers percent-encode the plain form as well.
            let decoded = percent_decode_str(&unquoted).decode_utf8_lossy().into_owned();
            if !decoded.trim().is_empty() {
                plain = Some(decoded);
            }
        }
    }

    extended.or(plain).map(|n| sanitize_filename(&n)).filter(|n| !n.is_empty())
}

/// Splits `a; b="x;y"; c` on semicolons outside quotes.
fn split_header_params(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => {
                current.push(c);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ';' if !in_quotes => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    parts.push(current);
    parts
}

/// Last path segment of a URL, percent-decoded and made safe for the file system.
pub fn filename_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let last = parsed.path_segments()?.rfind(|s| !s.is_empty())?.to_string();
    let decoded = percent_decode_str(&last).decode_utf8_lossy().into_owned();
    let clean = sanitize_filename(&decoded);
    if clean.is_empty() { None } else { Some(clean) }
}

/// Removes characters Windows does not allow and blocks path traversal.
pub fn sanitize_filename(name: &str) -> String {
    // Keep only the final component of anything that looks like a path.
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut out: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .collect();
    // Windows strips trailing dots and spaces; leading dots hide files.
    out = out.trim().trim_end_matches(['.', ' ']).trim_start_matches('.').to_string();

    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out = format!("_{}", out);
    }
    if out.chars().count() > 200 {
        let ext = Path::new(&out).extension().and_then(|e| e.to_str()).map(|e| e.to_string());
        let mut short: String = out.chars().take(180).collect();
        if let Some(ext) = ext {
            if ext.len() < 16 {
                short = format!("{}.{}", short, ext);
            }
        }
        out = short;
    }
    out
}

/// File extension for common Content-Type values (used when the URL has none).
pub fn extension_for_mime(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "text/html" => "html",
        "text/plain" => "txt",
        "text/css" => "css",
        "text/csv" => "csv",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        "application/pdf" => "pdf",
        "application/zip" | "application/x-zip-compressed" => "zip",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-7z-compressed" => "7z",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-tar" => "tar",
        "application/x-msdownload" | "application/x-dosexec" => "exe",
        "application/x-msi" => "msi",
        "application/vnd.android.package-archive" => "apk",
        "application/x-iso9660-image" => "iso",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "video/quicktime" => "mov",
        "video/mp2t" => "ts",
        "application/vnd.apple.mpegurl" | "application/x-mpegurl" => "m3u8",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => return None,
    })
}

/// Returns `path`, or `name (2).ext`, `name (3).ext`, ... when `taken` says it is used.
pub fn unique_path(path: &Path, mut taken: impl FnMut(&Path) -> bool) -> PathBuf {
    if !taken(path) {
        return path.to_path_buf();
    }
    let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("download").to_string();
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_string());
    for n in 2..10_000 {
        let name = match ext {
            Some(ref e) => format!("{} ({}).{}", stem, n, e),
            None => format!("{} ({})", stem, n),
        };
        let candidate = parent.join(name);
        if !taken(&candidate) {
            return candidate;
        }
    }
    path.to_path_buf()
}

/// Case-insensitive glob match supporting `*` and `?`.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_variants() {
        assert_eq!(parse_content_disposition("attachment; filename=\"a b.zip\"; size=3").as_deref(), Some("a b.zip"));
        assert_eq!(parse_content_disposition("attachment; filename=plain.iso").as_deref(), Some("plain.iso"));
        assert_eq!(
            parse_content_disposition("attachment; filename=\"fallback.txt\"; filename*=UTF-8''%E2%82%AC%20rates.txt").as_deref(),
            Some("€ rates.txt")
        );
        assert_eq!(parse_content_disposition("attachment; filename=\"../../evil.exe\"").as_deref(), Some("evil.exe"));
        assert_eq!(parse_content_disposition("inline").as_deref(), None);
        assert_eq!(parse_content_disposition("attachment; filename=\"semi;colon.zip\"").as_deref(), Some("semi;colon.zip"));
    }

    #[test]
    fn url_filenames_are_decoded_and_sanitized() {
        assert_eq!(filename_from_url("https://x.org/a/my%20file.zip?x=1").as_deref(), Some("my file.zip"));
        assert_eq!(filename_from_url("https://x.org/dir/").as_deref(), Some("dir"));
        assert_eq!(filename_from_url("https://x.org/").as_deref(), None);
        assert_eq!(sanitize_filename("a<b>:c?.txt"), "a_b__c_.txt");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
    }

    #[test]
    fn content_range_parsing() {
        assert_eq!(parse_content_range("bytes 0-0/1234"), Some((Some((0, 0)), Some(1234))));
        assert_eq!(parse_content_range("bytes */1234"), Some((None, Some(1234))));
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((Some((5, 9)), None)));
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("R0*", "r01"));
        assert!(wildcard_match("*.update.microsoft.com", "dl.update.microsoft.com"));
        assert!(!wildcard_match("ZIP", "zipx"));
        assert!(wildcard_match("f?le", "FILE"));
    }

    #[test]
    fn unique_paths() {
        let taken = ["a.zip", "a (2).zip"];
        let p = unique_path(Path::new("a.zip"), |p| taken.contains(&p.to_str().unwrap()));
        assert_eq!(p, PathBuf::from("a (3).zip"));
    }
}
