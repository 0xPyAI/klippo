//! The GUI's download supervisor, driven without a window.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use klippo::core::http::RequestOptions;
use klippo::core::resume::ResumeManager;
use klippo::gui::manager::{DownloadManagerBridge, DownloadRequest, GuiEvent};
use klippo::gui::state::{DownloadKind, DownloadStatus};

/// Blocking range server; each response is sent in 16 KB pieces with a pause.
fn serve(data: Arc<Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let data = data.clone();
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range: Option<(usize, usize)> = None;
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    return;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
                        break;
                    }
                    let lower = line.to_lowercase();
                    if let Some(spec) = lower.strip_prefix("range: bytes=") {
                        let (s, e) = spec.trim().split_once('-').unwrap();
                        let s: usize = s.parse().unwrap();
                        let e: usize = if e.is_empty() { data.len() - 1 } else { e.parse().unwrap() };
                        range = Some((s, e.min(data.len() - 1)));
                    }
                }
                let mut out = stream;
                let disposition = if first.contains("/named") {
                    "Content-Disposition: attachment; filename=\"Server Name.bin\"\r\n"
                } else {
                    ""
                };
                let (head, body) = match range {
                    Some((s, e)) => (
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n{}Connection: close\r\n\r\n",
                            s, e, data.len(), e - s + 1, disposition
                        ),
                        &data[s..=e],
                    ),
                    None => (
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n", data.len(), disposition),
                        &data[..],
                    ),
                };
                if out.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for chunk in body.chunks(16 * 1024) {
                    if out.write_all(chunk).is_err() {
                        return;
                    }
                    thread::sleep(Duration::from_millis(15));
                }
            });
        }
    });
    format!("http://{}", addr)
}

fn request(id: u64, url: &str, path: PathBuf) -> DownloadRequest {
    DownloadRequest {
        id,
        kind: DownloadKind::Http,
        url: url.to_string(),
        save_path: path,
        filename_auto: false,
        connections: 4,
        speed_limit_bps: 0,
        request: RequestOptions::default(),
        fresh: false,
        max_retries: 3,
        ignore_time_changes: true,
        set_file_time: false,
        youtube: None,
        audio_url: None,
        ftp: Default::default(),
    }
}

/// Collects events until `done` returns true or the timeout passes.
fn wait(bridge: &DownloadManagerBridge, timeout: Duration, mut done: impl FnMut(&GuiEvent) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        while let Some(ev) = bridge.try_recv_event() {
            if done(&ev) {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn bridge_reports_speed_pauses_resumes_and_removes() {
    let data: Arc<Vec<u8>> = Arc::new((0..4 * 1024 * 1024).map(|i| (i % 253) as u8).collect());
    let base = serve(data.clone());
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("bridge.bin");
    let url = format!("{}/bridge.bin", base);

    let bridge = DownloadManagerBridge::new();
    bridge.start_download(request(1, &url, target.clone()));

    // Live progress with a measured speed and the "Downloading" status.
    let mut downloading = false;
    let mut speed_seen = false;
    assert!(wait(&bridge, Duration::from_secs(10), |ev| {
        match ev {
            GuiEvent::StatusChanged { status: DownloadStatus::Downloading, .. } => downloading = true,
            GuiEvent::Progress { speed_bps, total, .. } if *speed_bps > 0.0 && *total == Some(4 * 1024 * 1024) => speed_seen = true,
            _ => {}
        }
        downloading && speed_seen
    }), "expected Downloading status and a non-zero speed");

    // Pause really stops the engine and keeps resume data.
    bridge.pause_download(1);
    assert!(wait(&bridge, Duration::from_secs(5), |ev| matches!(ev, GuiEvent::StatusChanged { status: DownloadStatus::Paused, .. })));
    assert!(ResumeManager::new(&target).exists());

    // Resume to completion.
    bridge.start_download(request(1, &url, target.clone()));
    assert!(wait(&bridge, Duration::from_secs(30), |ev| matches!(ev, GuiEvent::Completed { .. })), "download should complete");
    assert_eq!(std::fs::read(&target).unwrap(), *data);
    assert!(!ResumeManager::new(&target).exists());

    // Remove deletes the given files after the task stopped.
    bridge.remove_download(1, vec![target.clone()]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while target.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!target.exists());
}

#[test]
fn bridge_restart_waits_for_previous_run() {
    // Starting the same id twice must not run two engines on one file.
    let data: Arc<Vec<u8>> = Arc::new((0..2 * 1024 * 1024).map(|i| (i % 241) as u8).collect());
    let base = serve(data.clone());
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("twice.bin");
    let url = format!("{}/twice.bin", base);

    let bridge = DownloadManagerBridge::new();
    bridge.start_download(request(7, &url, target.clone()));
    thread::sleep(Duration::from_millis(300));
    let mut fresh = request(7, &url, target.clone());
    fresh.fresh = true;
    bridge.start_download(fresh);

    let mut completed = 0;
    let mut paused = 0;
    wait(&bridge, Duration::from_secs(30), |ev| {
        match ev {
            GuiEvent::Completed { .. } => completed += 1,
            GuiEvent::StatusChanged { status: DownloadStatus::Paused, .. } => paused += 1,
            _ => {}
        }
        completed >= 1
    });
    assert_eq!(completed, 1);
    assert_eq!(paused, 1, "the first run is stopped before the second starts");
    assert_eq!(std::fs::read(&target).unwrap(), *data);
}

#[test]
fn bridge_probe_reports_name_size_and_resume_support() {
    let data: Arc<Vec<u8>> = Arc::new(vec![7u8; 123_456]);
    let base = serve(data);
    let dir = tempfile::tempdir().unwrap();
    let bridge = DownloadManagerBridge::new();
    bridge.probe(5, format!("{}/named?id=1", base), dir.path().to_path_buf(), RequestOptions::default());

    let mut info = None;
    assert!(wait(&bridge, Duration::from_secs(10), |ev| {
        if let GuiEvent::ProbeResult { probe_id: 5, result } = ev {
            info = Some(result.clone());
            return true;
        }
        false
    }));
    let info = info.unwrap().expect("probe should succeed");
    assert_eq!(info.filename, "Server Name.bin");
    assert_eq!(info.total_size, Some(123_456));
    assert!(info.resumable);
}

#[test]
fn bridge_runs_hls_streams() {
    // Playlist plus two parts served by a tiny blocking server.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                let _ = reader.read_line(&mut first);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body: Vec<u8> = match path.as_str() {
                    "/live/index.m3u8" => b"#EXTM3U\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n".to_vec(),
                    "/live/a.ts" => vec![1u8; 4000],
                    "/live/b.ts" => vec![2u8; 3000],
                    _ => Vec::new(),
                };
                let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
                let mut out = stream;
                let _ = out.write_all(format!("HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", status, body.len()).as_bytes());
                let _ = out.write_all(&body);
            });
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("stream.ts");
    let mut req = request(9, &format!("http://{}/live/index.m3u8", addr), target.clone());
    req.kind = DownloadKind::Hls;

    let bridge = DownloadManagerBridge::new();
    bridge.start_download(req);
    let mut final_total = None;
    assert!(wait(&bridge, Duration::from_secs(15), |ev| {
        if let GuiEvent::Progress { total, .. } = ev {
            final_total = *total;
        }
        matches!(ev, GuiEvent::Completed { .. })
    }));
    let data = std::fs::read(&target).unwrap();
    assert_eq!(data.len(), 7000);
    assert_eq!(final_total, Some(7000));
    assert!(data[..4000].iter().all(|&b| b == 1) && data[4000..].iter().all(|&b| b == 2));
}
