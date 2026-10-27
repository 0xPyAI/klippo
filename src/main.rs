#![windows_subsystem = "windows"]

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use klippo::core::{DownloadEngine, HLSGrabber};
use klippo::gui::ipc::IpcMessage;
use klippo::gui::system::attach_console;
use klippo::native_host;
use klippo::ui::{format_bytes, TerminalVisualizer};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser, Debug)]
#[command(name = "klippo")]
#[command(author = "Klippo Team")]
#[command(version)]
#[command(about = "High-performance multi-connection download accelerator in Rust")]
#[command(after_help = "Classic download-manager switches are accepted too: /d URL /p PATH /f NAME /a /s /n /q\n\
Commands: gui, install-host, uninstall-host, native-host")]
struct Cli {
    /// Download URL (/d)
    #[arg(short = 'd', long = "url")]
    url: Option<String>,

    /// Destination directory (/p)
    #[arg(short = 'p', long = "path")]
    path: Option<String>,

    /// Destination file name (/f)
    #[arg(short = 'f', long = "filename")]
    filename: Option<String>,

    /// Number of connections (1 to 32)
    #[arg(short = 'c', long = "connections", default_value_t = 8)]
    connections: usize,

    /// Speed limit, e.g. 2M, 500K or 300 (KB/s)
    #[arg(short = 'l', long = "limit")]
    limit: Option<String>,

    /// /q: exit when the download is done (in --cli mode: no live display)
    #[arg(short = 'q', long = "quit")]
    quit: bool,

    /// /n: silent, start without asking
    #[arg(short = 'n', long = "silent")]
    silent: bool,

    /// /a: add to the main queue without starting
    #[arg(short = 'a', long = "add-to-queue")]
    add_to_queue: bool,

    /// /s: start the main queue
    #[arg(short = 's', long = "start-queue")]
    start_queue: bool,

    /// Treat the URL as an HLS (.m3u8) stream
    #[arg(long = "hls")]
    hls: bool,

    /// Download in the console instead of the window
    #[arg(long = "cli")]
    cli_mode: bool,

    /// Open the window (default)
    #[arg(long = "gui")]
    gui_mode: bool,

    /// Started by Windows at logon: open minimized
    #[arg(long = "startup")]
    startup: bool,

    /// HTTP Referer header
    #[arg(long = "referer")]
    referer: Option<String>,

    /// Cookie header value
    #[arg(long = "cookie")]
    cookie: Option<String>,

    /// User-Agent header value
    #[arg(long = "user-agent")]
    user_agent: Option<String>,

    /// Extension ID allowed to use the native host (install-host)
    #[arg(long = "extension-id")]
    extension_id: Vec<String>,

    /// Command (gui, install-host, uninstall-host, native-host) or a URL
    #[arg(index = 1)]
    command: Option<String>,
}

/// Turns the classic `/d URL /p PATH /f NAME /a /s /n /q` switches into regular flags.
fn normalize_idm_args(args: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut iter = args.into_iter();
    if let Some(program) = iter.next() {
        normalized.push(program);
    }
    while let Some(arg) = iter.next() {
        let lower = arg.to_ascii_lowercase();
        match lower.as_str() {
            "/d" | "/p" | "/f" | "/c" | "/l" => {
                normalized.push(format!("-{}", &lower[1..]));
                if let Some(value) = iter.next() {
                    normalized.push(value);
                }
            }
            "/a" | "/s" | "/n" | "/q" => normalized.push(format!("-{}", &lower[1..])),
            // /h (hang up when done) has no meaning without dial-up.
            "/h" => {}
            _ => normalized.push(arg),
        }
    }
    normalized
}

fn looks_like_url(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// "2M" -> 2 MB/s, "500K" -> 500 KB/s, "300" -> 300 KB/s (bytes per second).
fn parse_limit(s: &str) -> Result<u64, BoxError> {
    let s = s.trim().to_ascii_uppercase();
    let (num, mult) = match s.chars().last() {
        Some('G') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        Some('M') => (&s[..s.len() - 1], 1024 * 1024),
        Some('K') => (&s[..s.len() - 1], 1024),
        _ => (s.as_str(), 1024),
    };
    let value: f64 = num.trim().parse().map_err(|_| format!("invalid speed limit: {}", s))?;
    Ok((value * mult as f64) as u64)
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();

    // Browsers start the host with their own arguments (extension origin, window handle).
    if native_host::is_native_host_invocation(&raw) {
        if let Err(e) = native_host::run_native_host() {
            eprintln!("native host error: {}", e);
        }
        return;
    }

    let cli = Cli::parse_from(normalize_idm_args(raw));

    let result: Result<(), BoxError> = match cli.command.as_deref() {
        Some("install-host") => {
            attach_console();
            install_host(&cli.extension_id).map_err(Into::into)
        }
        Some("uninstall-host") => {
            attach_console();
            native_host::unregister_native_host().map_err(Into::into)
        }
        Some("native-host") => native_host::run_native_host().map_err(Into::into),
        _ if cli.cli_mode => {
            attach_console();
            run_cli(&cli)
        }
        Some(cmd) if !looks_like_url(cmd) && cmd != "gui" => {
            attach_console();
            Err(format!("unknown command: {} (try --help)", cmd).into())
        }
        _ => run_gui(&cli),
    };

    if let Err(e) = result {
        attach_console();
        eprintln!("Klippo: {}", e);
        std::process::exit(1);
    }
}

fn install_host(extra_ids: &[String]) -> std::io::Result<()> {
    if extra_ids.is_empty() {
        return native_host::register_native_host();
    }
    let mut settings = klippo::gui::state::load_settings();
    for id in extra_ids {
        if !settings.extension_ids.contains(id) {
            settings.extension_ids.push(id.clone());
        }
    }
    let report = native_host::register_native_host_for(&settings.extension_ids, native_host::BrowserTargets::all())?;
    println!("{}", report);
    Ok(())
}

fn run_gui(cli: &Cli) -> Result<(), BoxError> {
    let url = cli
        .url
        .clone()
        .or_else(|| cli.command.clone().filter(|c| looks_like_url(c)))
        .unwrap_or_default();
    let msg = IpcMessage {
        url,
        filename: cli.filename.clone(),
        dir: cli.path.clone(),
        referer: cli.referer.clone(),
        cookies: cli.cookie.clone(),
        user_agent: cli.user_agent.clone(),
        silent: cli.silent,
        queue_only: cli.add_to_queue,
        start_queue: cli.start_queue,
        quit_when_done: cli.quit,
        ..Default::default()
    };
    klippo::gui::run_gui_with_message(msg, cli.startup).map_err(|e| e.to_string().into())
}

fn run_cli(cli: &Cli) -> Result<(), BoxError> {
    let url = cli
        .url
        .clone()
        .or_else(|| cli.command.clone().filter(|c| looks_like_url(c)))
        .ok_or("a download URL is required (-d URL)")?;
    let limit = match cli.limit {
        Some(ref l) => parse_limit(l)?,
        None => 0,
    };
    let quiet = cli.quit || cli.silent || !std::io::stdout().is_terminal();
    let dir = cli.path.clone().map(PathBuf::from);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        if cli.hls || klippo::gui::state::is_hls_url(&url) {
            let name = cli.filename.clone().unwrap_or_else(|| "video.ts".to_string());
            let out = dir.unwrap_or_else(|| PathBuf::from(".")).join(name);
            let grabber = HLSGrabber::new(&url, &out, cli.connections);
            let control = grabber.control();
            let progress = grabber.progress();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    control.stop();
                }
            });
            let reporter = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let done = progress.done_parts.load(std::sync::atomic::Ordering::Relaxed);
                    let total = progress.total_parts.load(std::sync::atomic::Ordering::Relaxed);
                    let bytes = progress.bytes.load(std::sync::atomic::Ordering::Relaxed);
                    println!("HLS parts {}/{}  ({})", done, total, format_bytes(bytes));
                }
            });
            let res = grabber.run().await;
            reporter.abort();
            println!("Saved {}", res?.display());
            return Ok(());
        }

        let mut engine = DownloadEngine::new(&url, dir.as_deref(), cli.filename.as_deref(), cli.connections, limit);
        if let Some(ref r) = cli.referer {
            engine.add_header("Referer", r)?;
        }
        if let Some(ref c) = cli.cookie {
            engine.add_header("Cookie", c)?;
        }
        if let Some(ref ua) = cli.user_agent {
            engine.set_user_agent(ua);
        }

        let control = engine.control();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                control.stop();
            }
        });

        let segments = engine.segments.clone();
        let speed = engine.current_speed_bps.clone();
        let info = engine.info.clone();
        let reporter = tokio::spawn(async move {
            #[cfg(windows)]
            let ansi = !quiet && crossterm::ansi_support::supports_ansi();
            #[cfg(not(windows))]
            let ansi = !quiet;
            loop {
                tokio::time::sleep(Duration::from_millis(if ansi { 500 } else { 2000 })).await;
                let i = info.lock().unwrap().clone();
                if !i.probed {
                    continue;
                }
                if ansi {
                    TerminalVisualizer::render(&i.filename, i.total_size, i.resumable, &segments, &speed).await;
                } else {
                    let done: u64 = segments.read().await.iter().map(|s| s.downloaded_bytes()).sum();
                    let total = i.total_size.map(format_bytes).unwrap_or_else(|| "?".to_string());
                    println!("{}: {} / {} at {}/s", i.filename, format_bytes(done), total, format_bytes(*speed.lock().await as u64));
                }
            }
        });

        let res = engine.run().await;
        reporter.abort();
        match res {
            Ok(()) => {
                println!("Saved {} ({})", engine.output_path().display(), format_bytes(engine.total_size.unwrap_or(0)));
                Ok(())
            }
            Err(e) if e.is::<klippo::core::DownloadStopped>() => {
                println!("Stopped. Run the same command again to resume.");
                Ok(())
            }
            Err(e) => Err(e),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn idm_switches_are_normalized() {
        let out = normalize_idm_args(s(&["klippo", "/d", "https://a.com/x.zip", "/P", "C:\\dl", "/f", "y.zip", "/a", "/n", "/h"]));
        assert_eq!(out, s(&["klippo", "-d", "https://a.com/x.zip", "-p", "C:\\dl", "-f", "y.zip", "-a", "-n"]));
        let cli = Cli::parse_from(out);
        assert!(cli.add_to_queue && cli.silent && !cli.quit);
        assert_eq!(cli.path.as_deref(), Some("C:\\dl"));
    }

    #[test]
    fn speed_limits() {
        assert_eq!(parse_limit("2M").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_limit("500k").unwrap(), 500 * 1024);
        assert_eq!(parse_limit("300").unwrap(), 300 * 1024);
        assert!(parse_limit("fast").is_err());
    }
}
