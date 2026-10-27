//! Engine behavior against servers that misbehave or lack features.

use std::fs;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use klippo::core::engine::{DownloadEngine, EngineStatus};
use klippo::core::rate_limiter::TokenBucketLimiter;
use klippo::core::resume::ResumeManager;
use klippo::core::segment::Segment;
use klippo::core::{DownloadStopped, HLSGrabber};

fn payload(size: usize) -> Arc<Vec<u8>> {
    Arc::new((0..size).map(|i| (i * 7 % 251) as u8).collect())
}

fn sha(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

#[derive(Clone, Default)]
struct Behavior {
    /// Answer range requests with 206.
    ranges: bool,
    /// Send Content-Length (otherwise the body is close-delimited).
    content_length: bool,
    /// Extra raw header lines, each ending in CRLF.
    extra_headers: String,
    /// The first N body requests (not the probe) get HTTP 503.
    fail_first: usize,
    /// The next N body requests are cut after half of their body.
    truncate_next: usize,
    /// Fixed status code for every request.
    status_override: Option<u16>,
    /// The first N probes (`bytes=0-0`) get HTTP 503.
    fail_probes: usize,
}

/// Serves `data`; returns the address and a counter of body (non-probe) requests.
async fn serve(data: Arc<Vec<u8>>, behavior: Behavior) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_srv = counter.clone();
    let probes = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let data = data.clone();
            let b = behavior.clone();
            let counter = counter_srv.clone();
            let probes = probes.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let total = data.len();

                if let Some(code) = b.status_override {
                    let resp = format!("HTTP/1.1 {} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", code);
                    let _ = socket.write_all(resp.as_bytes()).await;
                    return;
                }

                let range = req.find("range: bytes=").map(|pos| {
                    let line = req[pos + 13..].split("\r\n").next().unwrap_or("").to_string();
                    let (s, e) = line.split_once('-').unwrap_or(("0", ""));
                    let start = s.parse::<usize>().unwrap_or(0);
                    let end = if e.is_empty() { total.saturating_sub(1) } else { e.parse::<usize>().unwrap_or(0) };
                    (start, end)
                });
                let is_probe = range == Some((0, 0));
                if is_probe && probes.fetch_add(1, Ordering::SeqCst) < b.fail_probes {
                    let resp = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(resp.as_bytes()).await;
                    return;
                }

                let mut truncate = false;
                if !is_probe {
                    let nth = counter.fetch_add(1, Ordering::SeqCst);
                    if nth < b.fail_first {
                        let resp = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = socket.write_all(resp.as_bytes()).await;
                        return;
                    }
                    truncate = nth < b.fail_first + b.truncate_next;
                }

                let (status, body, content_range) = match range {
                    Some(_) if b.ranges && total == 0 => ("416 Range Not Satisfiable", Vec::new(), Some("bytes */0".to_string())),
                    Some((s, e)) if b.ranges && s <= e && s < total => {
                        let e = e.min(total - 1);
                        ("206 Partial Content", data[s..=e].to_vec(), Some(format!("bytes {}-{}/{}", s, e, total)))
                    }
                    _ => ("200 OK", data.to_vec(), None),
                };

                let mut head = format!("HTTP/1.1 {}\r\nConnection: close\r\n{}", status, b.extra_headers);
                if b.ranges {
                    head.push_str("Accept-Ranges: bytes\r\n");
                }
                if let Some(cr) = content_range {
                    head.push_str(&format!("Content-Range: {}\r\n", cr));
                }
                if b.content_length {
                    head.push_str(&format!("Content-Length: {}\r\n", body.len()));
                }
                head.push_str("\r\n");
                let _ = socket.write_all(head.as_bytes()).await;
                let send = if truncate { &body[..body.len() / 2] } else { &body[..] };
                let _ = socket.write_all(send).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (addr, counter)
}

fn ranged() -> Behavior {
    Behavior { ranges: true, content_length: true, ..Default::default() }
}

#[tokio::test]
async fn tiny_file_with_many_connections() {
    // Fewer bytes than connections used to underflow the part size.
    let (addr, _) = serve(Arc::new(b"abc".to_vec()), ranged()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/tiny.txt", addr), Some(dir.path()), None, 8, 0);
    engine.run().await.expect("tiny download");
    assert_eq!(fs::read(dir.path().join("tiny.txt")).unwrap(), b"abc");
}

#[tokio::test]
async fn empty_file_completes() {
    let (addr, _) = serve(Arc::new(Vec::new()), ranged()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/empty.bin", addr), Some(dir.path()), None, 4, 0);
    engine.run().await.expect("empty download");
    assert_eq!(fs::metadata(dir.path().join("empty.bin")).unwrap().len(), 0);
    assert_eq!(*engine.status.read().await, EngineStatus::Completed);
}

#[tokio::test]
async fn unknown_size_stream_downloads_fully() {
    // No Content-Length and no range support: a single close-delimited stream.
    let data = payload(3 * 1024 * 1024);
    let (addr, _) = serve(data.clone(), Behavior::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/stream", addr), Some(dir.path()), Some("stream.bin"), 8, 0);
    engine.run().await.expect("unknown size download");
    let got = fs::read(dir.path().join("stream.bin")).unwrap();
    assert_eq!(got.len(), data.len());
    assert_eq!(sha(&got), sha(&data));
    assert!(!engine.resumable);
    assert_eq!(engine.total_size, Some(data.len() as u64));
}

/// A form's address: the file comes only for a POST with the form's fields;
/// a GET gets HTTP 405. Returns the address and the number of POSTs served.
async fn serve_form(data: Arc<Vec<u8>>, fields: &'static str) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let posts = Arc::new(AtomicUsize::new(0));
    let posts_srv = posts.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let data = data.clone();
            let posts = posts_srv.clone();
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = vec![0u8; 8192];
                // Headers, then the body that Content-Length announces.
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(n) if n > 0 => n,
                        _ => return,
                    };
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let want = text.to_lowercase().split("content-length:").nth(1).and_then(|v| v.split("\r\n").next()).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                        if req.len() >= end + 4 + want {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&req).to_string();
                let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
                let form = text.to_lowercase().contains("content-type: application/x-www-form-urlencoded");
                if !(text.starts_with("POST ") && form && body == fields) {
                    let _ = socket.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    return;
                }
                posts.fetch_add(1, Ordering::SeqCst);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nContent-Disposition: attachment; filename=\"export.csv\"\r\n\r\n",
                    data.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&data).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (addr, posts)
}

#[tokio::test]
async fn a_form_download_sends_the_post_again() {
    let data = payload(700_000);
    let (addr, posts) = serve_form(data.clone(), "token=abc&kind=csv").await;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://{}/export", addr);
    let mut get = DownloadEngine::new(&url, Some(dir.path()), None, 4, 0);
    assert!(get.run().await.is_err(), "a GET of a form's address fails");
    let mut engine = DownloadEngine::new(&url, Some(dir.path()), None, 4, 0);
    engine.set_request_options(klippo::core::http::RequestOptions {
        post: Some(klippo::core::http::PostBody { data: b"token=abc&kind=csv".to_vec(), content_type: "application/x-www-form-urlencoded".to_string() }),
        ..Default::default()
    });
    engine.run().await.expect("form download");
    assert_eq!(sha(&fs::read(dir.path().join("export.csv")).unwrap()), sha(&data), "named by the server, byte for byte");
    assert!(posts.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn server_without_ranges_uses_one_stream() {
    let data = payload(2 * 1024 * 1024);
    let (addr, _) = serve(data.clone(), Behavior { content_length: true, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/single.bin", addr), Some(dir.path()), None, 8, 0);
    engine.run().await.expect("single stream download");
    assert_eq!(engine.segments.read().await.len(), 1);
    assert_eq!(sha(&fs::read(dir.path().join("single.bin")).unwrap()), sha(&data));
}

#[tokio::test]
async fn retries_after_server_errors_and_cut_connections() {
    let data = payload(3 * 1024 * 1024);
    let behavior = Behavior { fail_first: 3, truncate_next: 2, ..ranged() };
    let (addr, requests) = serve(data.clone(), behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/flaky.bin", addr), Some(dir.path()), None, 4, 0);
    engine.run().await.expect("download should survive transient errors");
    assert_eq!(sha(&fs::read(dir.path().join("flaky.bin")).unwrap()), sha(&data));
    assert!(requests.load(Ordering::SeqCst) > 4 + 3);
}

#[tokio::test]
async fn busy_server_at_the_first_request_is_tried_again() {
    // The engine keeps trying when the server is busy (503) before the download starts.
    let data = payload(512 * 1024);
    let behavior = Behavior { fail_probes: 2, ..ranged() };
    let (addr, _) = serve(data.clone(), behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/busy.bin", addr), Some(dir.path()), None, 4, 0);
    engine.run().await.expect("the probe should be tried again");
    assert_eq!(sha(&fs::read(dir.path().join("busy.bin")).unwrap()), sha(&data));
}

#[tokio::test]
async fn cut_stream_without_ranges_restarts_from_zero() {
    let data = payload(1024 * 1024);
    let behavior = Behavior { content_length: true, truncate_next: 1, ..Default::default() };
    let (addr, _) = serve(data.clone(), behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/norange.bin", addr), Some(dir.path()), None, 4, 0);
    engine.run().await.expect("download restarts after the cut");
    assert_eq!(sha(&fs::read(dir.path().join("norange.bin")).unwrap()), sha(&data));
}

#[tokio::test]
async fn http_404_fails_with_message() {
    let behavior = Behavior { status_override: Some(404), ..Default::default() };
    let (addr, _) = serve(Arc::new(Vec::new()), behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/missing.zip", addr), Some(dir.path()), None, 4, 0);
    let err = engine.run().await.expect_err("404 must fail");
    assert!(err.to_string().contains("404"), "{}", err);
    assert_eq!(*engine.status.read().await, EngineStatus::Failed);
}

#[tokio::test]
async fn content_disposition_names_the_file() {
    let behavior = Behavior {
        extra_headers: "Content-Disposition: attachment; filename=\"Q3 report.pdf\"; size=11\r\n".to_string(),
        ..ranged()
    };
    let (addr, _) = serve(Arc::new(b"report body".to_vec()), behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/download.php?id=7", addr), Some(dir.path()), None, 2, 0);
    engine.run().await.expect("download");
    assert_eq!(engine.suggested_filename(), "Q3 report.pdf");
    assert_eq!(fs::read(dir.path().join("Q3 report.pdf")).unwrap(), b"report body");
}

#[tokio::test]
async fn explicit_filename_beats_content_disposition() {
    let behavior = Behavior {
        extra_headers: "Content-Disposition: attachment; filename=\"server.bin\"\r\n".to_string(),
        ..ranged()
    };
    let (addr, _) = serve(Arc::new(b"xyz".to_vec()), behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/a", addr), Some(dir.path()), Some("mine.bin"), 2, 0);
    engine.run().await.expect("download");
    assert!(dir.path().join("mine.bin").exists());
    assert!(!dir.path().join("server.bin").exists());
}

#[tokio::test]
async fn pause_then_resume_keeps_progress() {
    let data = payload(3 * 1024 * 1024);
    let (addr, requests) = serve(data.clone(), ranged()).await;
    let url = format!("http://{}/pausable.bin", addr);
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("pausable.bin");

    // 1 MB/s for 3 MB: stop after about 1.2 s.
    let mut engine = DownloadEngine::new(&url, Some(dir.path()), None, 4, 1024 * 1024);
    let control = engine.control();
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        control.stop();
    });
    let err = engine.run().await.expect_err("a stopped download returns an error");
    stopper.await.unwrap();
    assert!(err.is::<DownloadStopped>());
    assert_eq!(*engine.status.read().await, EngineStatus::Paused);

    let partial = engine.downloaded_bytes().await;
    assert!(partial > 0 && partial < data.len() as u64, "partial = {}", partial);
    let state = ResumeManager::new(&target);
    assert!(state.exists(), "resume state must be saved on pause");
    let saved: u64 = state.restore_segments().iter().map(|s| s.downloaded_bytes()).sum();
    assert_eq!(saved, partial, "saved state matches the bytes on disk");

    // No worker may still be writing after run() returned.
    let before = requests.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(requests.load(Ordering::SeqCst), before);

    let mut resumed = DownloadEngine::new(&url, Some(dir.path()), None, 4, 0);
    resumed.run().await.expect("resume");
    assert_eq!(sha(&fs::read(&target).unwrap()), sha(&data));
    assert!(!state.exists(), "state file is removed after completion");
}

#[tokio::test]
async fn fresh_start_ignores_saved_progress() {
    let data = payload(1024 * 1024);
    let (addr, _) = serve(data.clone(), ranged()).await;
    let url = format!("http://{}/fresh.bin", addr);
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("fresh.bin");

    // Valid-looking state that claims everything is done, over garbage bytes.
    fs::write(&target, vec![0u8; data.len()]).unwrap();
    let mut seg = Segment::new(0, 0, data.len() as u64 - 1);
    seg.current_offset = data.len() as u64;
    ResumeManager::new(&target).save_state(&url, Some(data.len() as u64), None, None, &[seg]).unwrap();

    let mut engine = DownloadEngine::new(&url, Some(dir.path()), None, 4, 0);
    engine.set_fresh_start(true);
    engine.run().await.expect("redownload");
    assert_eq!(sha(&fs::read(&target).unwrap()), sha(&data));
}

#[tokio::test]
async fn changed_remote_file_restarts() {
    let data = payload(3 * 1024 * 1024);
    let (addr, _) = serve(data.clone(), ranged()).await;
    let url = format!("http://{}/changed.bin", addr);
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("changed.bin");

    // Stale state for a different (smaller) file with bogus progress.
    fs::write(&target, vec![0xAAu8; 1000]).unwrap();
    let mut seg = Segment::new(0, 0, 999);
    seg.current_offset = 1000;
    ResumeManager::new(&target).save_state(&url, Some(1000), None, None, &[seg]).unwrap();

    let mut engine = DownloadEngine::new(&url, Some(dir.path()), None, 4, 0);
    engine.run().await.expect("download");
    assert_eq!(sha(&fs::read(&target).unwrap()), sha(&data));
}

#[tokio::test]
async fn live_speed_limit_change_applies() {
    let data = payload(2 * 1024 * 1024);
    let (addr, _) = serve(data.clone(), ranged()).await;
    let dir = tempfile::tempdir().unwrap();
    // Starts at 64 KB/s (would take 32 s), then the limit is lifted.
    let mut engine = DownloadEngine::new(&format!("http://{}/live.bin", addr), Some(dir.path()), None, 4, 64 * 1024);
    let control = engine.control();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        control.set_speed_limit(0);
    });
    tokio::time::timeout(Duration::from_secs(10), engine.run())
        .await
        .expect("limit change must take effect")
        .expect("download");
    assert_eq!(sha(&fs::read(dir.path().join("live.bin")).unwrap()), sha(&data));
}

#[tokio::test]
async fn shared_global_limiter_throttles() {
    let data = payload(512 * 1024);
    let (addr, _) = serve(data.clone(), ranged()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/global.bin", addr), Some(dir.path()), None, 4, 0);
    engine.set_global_limiter(Some(TokenBucketLimiter::new(256 * 1024)));
    let t0 = std::time::Instant::now();
    engine.run().await.expect("download");
    // 512 KB at 256 KB/s with a 256 KB burst: at least ~1 s.
    assert!(t0.elapsed() >= Duration::from_millis(800), "{:?}", t0.elapsed());
}

#[tokio::test]
async fn limiter_handles_chunks_larger_than_bucket() {
    let limiter = TokenBucketLimiter::new(16 * 1024);
    // 256 KB in one call is far beyond the 64 KB bucket; it must not hang.
    tokio::time::timeout(Duration::from_secs(5), limiter.consume(256 * 1024, None))
        .await
        .expect("consume must return");
}

#[tokio::test]
async fn hls_grabber_end_to_end() {
    // A master playlist, a media playlist and three segments.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body: Vec<u8> = match path.as_str() {
                    "/master.m3u8" => b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=900\nhigh.m3u8\n".to_vec(),
                    "/high.m3u8" => b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\ns0.ts\n#EXTINF:4,\ns1.ts\n#EXTINF:4,\ns2.ts\n#EXT-X-ENDLIST\n".to_vec(),
                    "/s0.ts" => vec![b'A'; 1000],
                    "/s1.ts" => vec![b'B'; 1000],
                    "/s2.ts" => vec![b'C'; 500],
                    _ => Vec::new(),
                };
                let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
                let head = format!("HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", status, body.len());
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("video.ts");
    let grabber = HLSGrabber::new(&format!("http://{}/master.m3u8", addr), &out, 4);
    let progress = grabber.progress();
    grabber.run().await.expect("hls download");
    let got = fs::read(&out).unwrap();
    assert_eq!(got.len(), 2500);
    assert!(got[..1000].iter().all(|&b| b == b'A'));
    assert!(got[1000..2000].iter().all(|&b| b == b'B'));
    assert!(got[2000..].iter().all(|&b| b == b'C'));
    assert_eq!(progress.fraction(), 1.0);
    assert!(!HLSGrabber::temp_dir_for(&out).exists());
}

#[tokio::test]
async fn hls_live_stream_is_refused() {
    // A media playlist without #EXT-X-ENDLIST is a live stream: no recording.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let segment_requests = Arc::new(AtomicUsize::new(0));
    let seen = segment_requests.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body: Vec<u8> = if req.contains("/live.m3u8") {
                    b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:40\n#EXTINF:2,\ns40.ts\n#EXTINF:2,\ns41.ts\n".to_vec()
                } else {
                    seen.fetch_add(1, Ordering::SeqCst);
                    vec![b'L'; 100]
                };
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("live.ts");
    let grabber = HLSGrabber::new(&format!("http://{}/live.m3u8", addr), &out, 4);
    let err = tokio::time::timeout(Duration::from_secs(10), grabber.run()).await.expect("must not wait for the stream").expect_err("live must fail");
    assert_eq!(err.to_string(), klippo::core::hls::LIVE_STREAM);
    assert_eq!(segment_requests.load(Ordering::SeqCst), 0);
    assert!(!out.exists());
}

#[tokio::test]
async fn file_date_comes_from_last_modified() {
    let behavior = Behavior { extra_headers: "Last-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\n".to_string(), ..ranged() };
    let (addr, _) = serve(Arc::new(b"dated".to_vec()), behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/dated.txt", addr), Some(dir.path()), None, 1, 0);
    engine.set_file_time_from_server(true);
    engine.run().await.expect("download");
    let modified = fs::metadata(dir.path().join("dated.txt")).unwrap().modified().unwrap();
    let secs = modified.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert_eq!(secs, 1_445_412_480);
}

#[tokio::test]
async fn unauthorized_reports_realm() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let resp = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"Members area\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut engine = DownloadEngine::new(&format!("http://{}/secret.zip", addr), Some(dir.path()), None, 2, 0);
    let err = engine.run().await.expect_err("401 must fail");
    let msg = err.to_string();
    assert!(msg.contains("HTTP 401") && msg.contains("realm \"Members area\""), "{}", msg);
}
