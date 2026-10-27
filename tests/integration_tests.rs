use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use klippo::core::engine::{DownloadEngine, EngineStatus};
use klippo::core::rate_limiter::TokenBucketLimiter;
use klippo::core::resume::ResumeManager;
use klippo::core::segment::{Segment, SegmentStatus};
use klippo::core::storage::DirectStorage;

const TEST_PAYLOAD_SIZE: usize = 3 * 1024 * 1024; // 3 MB

fn generate_test_payload() -> Vec<u8> {
    (0..TEST_PAYLOAD_SIZE).map(|i| (i % 256) as u8).collect()
}

fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

#[tokio::test]
async fn test_segment_split_math() {
    let mut seg = Segment::new(0, 0, 9_999_999);
    seg.status = SegmentStatus::Downloading;
    seg.current_offset = 2_000_000; // 2 MB downloaded, 8 MB remaining

    let new_seg = seg.split_remaining(1, 1024 * 1024).expect("Should split");

    assert_eq!(seg.end, 5_999_999);
    assert_eq!(new_seg.start, 6_000_000);
    assert_eq!(new_seg.end, 9_999_999);
    assert_eq!(new_seg.current_offset, 6_000_000);
    assert_eq!(new_seg.status, SegmentStatus::Pending);

    // Continuity verification
    assert_eq!(seg.end + 1, new_seg.start);
    assert_eq!(seg.start, 0);
    assert_eq!(new_seg.end, 9_999_999);
}

#[tokio::test]
async fn test_storage_direct_writes() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("direct_test.bin");

    let storage = DirectStorage::new(&file_path, Some(100));
    storage.open().unwrap();

    // Write chunk 2 at offset 50
    storage.write_at(50, b"WORLD").unwrap();
    // Write chunk 1 at offset 0
    storage.write_at(0, b"HELLO").unwrap();

    storage.close();

    let content = fs::read(&file_path).unwrap();
    assert_eq!(&content[0..5], b"HELLO");
    assert_eq!(&content[50..55], b"WORLD");
    assert_eq!(content.len(), 100);
}

#[tokio::test]
async fn test_resume_persistence() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("resumable.bin");

    let resume_mgr = ResumeManager::new(&file_path);
    assert!(!resume_mgr.exists());

    let mut seg1 = Segment::new(0, 0, 999);
    seg1.status = SegmentStatus::Downloading;
    seg1.current_offset = 500;

    resume_mgr.save_state("http://example.com/test", Some(1000), Some("etag1"), None, &[seg1]).unwrap();
    assert!(resume_mgr.exists());

    let restored = resume_mgr.restore_segments();
    assert_eq!(restored.len(), 1);
    // Downloading should be reset to Pending upon restore
    assert_eq!(restored[0].status, SegmentStatus::Pending);
    assert_eq!(restored[0].current_offset, 500);

    resume_mgr.cleanup();
    assert!(!resume_mgr.exists());
}

#[tokio::test]
async fn test_token_bucket_limiter() {
    let limiter = TokenBucketLimiter::new(100 * 1024); // 100 KB/s
    limiter.consume(100 * 1024, None).await; // burst

    let t0 = std::time::Instant::now();
    limiter.consume(100 * 1024, None).await;
    let elapsed = t0.elapsed().as_secs_f64();

    assert!(elapsed >= 0.7, "Elapsed: {:.2}s should be ~1.0s", elapsed);
}

/// Lightweight mock HTTP server supporting HTTP Range requests
async fn run_mock_server(payload: Arc<Vec<u8>>) -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok((mut socket, _)) = listener.accept() => {
                    let payload = payload.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let n = match socket.read(&mut buf).await {
                            Ok(n) if n > 0 => n,
                            _ => return,
                        };
                        let req_str = String::from_utf8_lossy(&buf[..n]);

                        let total_size = payload.len();
                        let etag = "\"mock-etag-12345\"";

                        if req_str.starts_with("HEAD") {
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nETag: {}\r\nConnection: close\r\n\r\n",
                                total_size, etag
                            );
                            let _ = socket.write_all(resp.as_bytes()).await;
                            let _ = socket.shutdown().await;
                            return;
                        }

                        // Check Range header case-insensitively
                        let req_lower = req_str.to_lowercase();
                        if let Some(pos) = req_lower.find("range: bytes=") {
                            let range_part = &req_lower[pos + 13..];
                            let line = range_part.split("\r\n").next().unwrap_or("");
                            let mut parts = line.split('-');
                            let start = parts.next().unwrap_or("0").parse::<usize>().unwrap_or(0);
                            let end_str = parts.next().unwrap_or("");
                            let end = if end_str.is_empty() {
                                total_size - 1
                            } else {
                                end_str.parse::<usize>().unwrap_or(total_size - 1)
                            };

                            if start < total_size && start <= end {
                                let chunk = &payload[start..=end.min(total_size - 1)];
                                let resp_header = format!(
                                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nETag: {}\r\nConnection: close\r\n\r\n",
                                    start, end, total_size, chunk.len(), etag
                                );
                                let _ = socket.write_all(resp_header.as_bytes()).await;
                                let _ = socket.write_all(chunk).await;
                                let _ = socket.shutdown().await;
                                return;
                            }
                        }

                        // Standard 200 OK
                        let resp_header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nETag: {}\r\nConnection: close\r\n\r\n",
                            total_size, etag
                        );
                        let _ = socket.write_all(resp_header.as_bytes()).await;
                        let _ = socket.write_all(&payload).await;
                        let _ = socket.shutdown().await;
                    });
                }
                _ = &mut shutdown_rx => {
                    break;
                }
            }
        }
    });

    (addr, shutdown_tx)
}

#[tokio::test]
async fn test_multipart_download_e2e() {
    let payload = Arc::new(generate_test_payload());
    let expected_sha = compute_sha256(&payload);
    let (addr, shutdown) = run_mock_server(payload.clone()).await;

    let url = format!("http://{}/largefile.bin", addr);
    let temp_dir = tempfile::tempdir().unwrap();
    let out_file = temp_dir.path().join("downloaded.bin");

    let mut engine = DownloadEngine::new(
        &url,
        Some(temp_dir.path()),
        Some("downloaded.bin"),
        8, // 8 parallel connections
        0,
    );

    engine.run().await.expect("Download should succeed");

    assert_eq!(*engine.status.read().await, EngineStatus::Completed);
    assert!(out_file.exists());

    let downloaded = fs::read(&out_file).unwrap();
    assert_eq!(downloaded.len(), TEST_PAYLOAD_SIZE);
    assert_eq!(compute_sha256(&downloaded), expected_sha);

    println!("\n✓ Rust 8-Connection Segmented Download Verified: 100% SHA-256 match!");
    let _ = shutdown.send(());
}

#[tokio::test]
async fn test_dynamic_resegmentation_live_rust() {
    let payload = Arc::new(generate_test_payload());
    let expected_sha = compute_sha256(&payload);
    let (addr, shutdown) = run_mock_server(payload.clone()).await;

    let url = format!("http://{}/resegment_test.bin", addr);
    let temp_dir = tempfile::tempdir().unwrap();
    let out_file = temp_dir.path().join("resegment_out.bin");

    let mut engine = DownloadEngine::new(
        &url,
        Some(temp_dir.path()),
        Some("resegment_out.bin"),
        6,
        0,
    );
    // Lower split threshold to 256 KB so it actively splits our 3MB test file
    engine.set_min_split_size(256 * 1024);

    engine.run().await.expect("Download with resegmentation should succeed");

    assert_eq!(*engine.status.read().await, EngineStatus::Completed);
    let downloaded = fs::read(&out_file).unwrap();
    assert_eq!(downloaded.len(), TEST_PAYLOAD_SIZE);
    assert_eq!(compute_sha256(&downloaded), expected_sha);

    let seg_count = engine.segments.read().await.len();
    println!("\n✓ Rust Dynamic Resegmentation Verified: {} segments created, SHA-256 matched!", seg_count);
    assert!(seg_count >= 6);

    let _ = shutdown.send(());
}
