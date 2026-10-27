//! FTP downloads (`ftp://user:password@host:port/path`):
//! one connection, resumed with REST, passive (PASV) or active mode, and
//! optionally through a SOCKS5 proxy (control and data connections).

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use percent_encoding::percent_decode_str;
use suppaftp::types::FileType;
use suppaftp::{FtpStream, Mode};
use tokio::sync::watch;

use super::engine::DownloadStopped;
use super::hls::{HlsControl, HlsProgress};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// How the FTP connection is made.
#[derive(Debug, Clone)]
pub struct FtpOptions {
    /// PASV (the "Use FTP in PASV mode" option); active mode otherwise.
    pub passive: bool,
    /// SOCKS5 proxy `host:port` for both connections.
    pub socks: Option<String>,
    /// Login for the SOCKS5 proxy.
    pub socks_login: Option<(String, String)>,
    /// Server login when the address has none ("Use authorization", Site Logins).
    pub login: Option<(String, String)>,
}

impl Default for FtpOptions {
    fn default() -> Self {
        Self { passive: true, socks: None, socks_login: None, login: None }
    }
}

pub struct FtpGrabber {
    url: String,
    output_path: PathBuf,
    options: FtpOptions,
    progress: Arc<HlsProgress>,
    cancel_tx: Arc<watch::Sender<bool>>,
    cancel_rx: watch::Receiver<bool>,
}

/// True for `ftp://` addresses.
pub fn is_ftp_url(url: &str) -> bool {
    url.trim().get(..6).is_some_and(|s| s.eq_ignore_ascii_case("ftp://"))
}

impl FtpGrabber {
    pub fn new<P: AsRef<Path>>(url: &str, output_path: P) -> Self {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        Self {
            url: url.trim().to_string(),
            output_path: output_path.as_ref().to_path_buf(),
            options: FtpOptions::default(),
            progress: Arc::new(HlsProgress::default()),
            cancel_tx: Arc::new(cancel_tx),
            cancel_rx,
        }
    }

    pub fn set_options(&mut self, options: FtpOptions) {
        self.options = options;
    }

    pub fn progress(&self) -> Arc<HlsProgress> {
        self.progress.clone()
    }

    pub fn control(&self) -> HlsControl {
        HlsControl::new(self.cancel_tx.clone())
    }

    /// Downloads (or resumes) the file; returns where it was saved.
    pub async fn run(&self) -> Result<PathBuf, BoxError> {
        let (url, out, options, progress, cancel) =
            (self.url.clone(), self.output_path.clone(), self.options.clone(), self.progress.clone(), self.cancel_rx.clone());
        tokio::task::spawn_blocking(move || download(&url, &out, &options, &progress, &cancel))
            .await
            .map_err(|e| format!("Internal error: {}", e))?
    }
}

/// A TCP connection to `target`, through the SOCKS5 proxy when there is one.
fn connect(target: &str, options: &FtpOptions) -> std::io::Result<TcpStream> {
    let stream = match (&options.socks, &options.socks_login) {
        (Some(proxy), Some((user, pass))) => socks::Socks5Stream::connect_with_password(proxy.as_str(), target, user, pass)?.into_inner(),
        (Some(proxy), None) => socks::Socks5Stream::connect(proxy.as_str(), target)?.into_inner(),
        (None, _) => {
            // Every address of the host in turn, like a browser: an IPv6
            // address first is common, and the network may not route it.
            use std::net::ToSocketAddrs;
            let mut last = std::io::Error::other("host not found");
            let mut found = None;
            for addr in target.to_socket_addrs()? {
                match TcpStream::connect_timeout(&addr, Duration::from_secs(30)) {
                    Ok(s) => {
                        found = Some(s);
                        break;
                    }
                    Err(e) => last = e,
                }
            }
            found.ok_or(last)?
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    Ok(stream)
}

fn download(url: &str, out: &Path, options: &FtpOptions, progress: &HlsProgress, cancel: &watch::Receiver<bool>) -> Result<PathBuf, BoxError> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Bad FTP address: {}", e))?;
    let host = parsed.host_str().ok_or("The FTP address has no server name")?.to_string();
    let port = parsed.port().unwrap_or(21);
    let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().to_string();
    let (user, pass) = match (parsed.username(), &options.login) {
        ("", Some((u, p))) if !u.is_empty() => (u.clone(), p.clone()),
        ("", _) => ("anonymous".to_string(), "klippo@".to_string()),
        (u, _) => (decode(u), parsed.password().map(decode).unwrap_or_else(|| "klippo@".to_string())),
    };
    let path = decode(parsed.path());
    if path.is_empty() || path.ends_with('/') {
        return Err("The FTP address names a folder, not a file".into());
    }
    let target = format!("{}:{}", host, port);

    let control = connect(&target, options).map_err(|e| format!("Cannot connect to {}: {}", target, e))?;
    let mut ftp = FtpStream::connect_with_stream(control).map_err(|e| format!("FTP server did not answer: {}", e))?;
    if options.socks.is_some() {
        let opts = options.clone();
        ftp = ftp.passive_stream_builder(move |addr| connect(&addr.to_string(), &opts).map_err(suppaftp::FtpError::ConnectionError));
    }
    if !options.passive {
        ftp = ftp.active_mode(Duration::from_secs(30));
    }
    ftp.set_mode(if options.passive { Mode::Passive } else { Mode::Active });
    ftp.login(user.as_str(), pass.as_str()).map_err(|e| format!("FTP login failed: {}", e))?;
    ftp.transfer_type(FileType::Binary)?;

    let total = ftp.size(&path).ok().map(|s| s as u64);
    progress.total_parts.store(1, Ordering::Relaxed);
    progress.known_total.store(total.unwrap_or(0), Ordering::Relaxed);

    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let existing = fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    let mut offset = 0;
    if existing > 0 && total.is_some_and(|t| existing <= t) && ftp.resume_transfer(existing as usize).is_ok() {
        offset = existing;
    }
    if total.is_some_and(|t| t == offset && t > 0) {
        progress.bytes.store(offset, Ordering::Relaxed);
        progress.done_parts.store(1, Ordering::Relaxed);
        let _ = ftp.quit();
        return Ok(out.to_path_buf());
    }
    let mut file = if offset > 0 {
        OpenOptions::new().append(true).open(out)?
    } else {
        fs::File::create(out)?
    };
    progress.bytes.store(offset, Ordering::Relaxed);

    let mut stream = ftp.retr_as_stream(&path).map_err(|e| format!("The server refused the file: {}", e))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = offset;
    loop {
        if *cancel.borrow() {
            drop(stream);
            let _ = ftp.quit();
            return Err(Box::new(DownloadStopped));
        }
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress.bytes.store(done, Ordering::Relaxed);
    }
    file.flush()?;
    stream.finish().map_err(|e| format!("The transfer did not finish: {}", e))?;
    let _ = ftp.quit();
    if let Some(t) = total.filter(|t| done < *t) {
        return Err(format!("The server sent {} of {} bytes", done, t).into());
    }
    progress.done_parts.store(1, Ordering::Relaxed);
    Ok(out.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ftp_addresses() {
        assert!(is_ftp_url("ftp://ftp.example.com/pub/file.zip"));
        assert!(is_ftp_url("FTP://host/a"));
        assert!(!is_ftp_url("https://example.com/a"));
    }

    #[test]
    fn connect_tries_every_address() {
        // "localhost" gives ::1 first on Windows; the server is on 127.0.0.1 only.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = format!("localhost:{}", server.local_addr().unwrap().port());
        let stream = connect(&target, &FtpOptions::default()).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), server.local_addr().unwrap());
    }
}
