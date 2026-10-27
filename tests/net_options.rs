//! Network options checked against real servers, so they are ignored by default:
//!
//! KLIPPO_SOCKS=127.0.0.1:1080 KLIPPO_SOCKS_URL=http://host/file KLIPPO_OUT=dir cargo test --test net_options -- --ignored socks
//! KLIPPO_PAC=http://127.0.0.1:8766/proxy.pac KLIPPO_PAC_URL=http://example.com/ KLIPPO_PAC_EXPECT=127.0.0.1:8899 cargo test --test net_options -- --ignored pac
//! KLIPPO_FTP_URL=ftp://speedtest.tele2.net/1KB.zip KLIPPO_OUT=dir [KLIPPO_FTP_SOCKS=127.0.0.1:1080] [KLIPPO_FTP_LOGIN=user:pass] cargo test --test net_options -- --ignored ftp

use klippo::core::http::{ProxyConfig, RequestOptions};

#[tokio::test]
#[ignore]
async fn socks_download() {
    let proxy = std::env::var("KLIPPO_SOCKS").unwrap();
    let url = std::env::var("KLIPPO_SOCKS_URL").unwrap();
    let out = std::env::var("KLIPPO_OUT").unwrap();
    let socks = format!("socks5h://{}", proxy);
    let mut engine = klippo::core::engine::DownloadEngine::new(&url, Some(std::path::Path::new(&out)), Some("socks.bin"), 4, 0);
    engine.set_request_options(RequestOptions {
        proxy: Some(ProxyConfig { http: Some(socks.clone()), https: Some(socks), ..Default::default() }),
        ..Default::default()
    });
    engine.run().await.unwrap();
    let len = std::fs::metadata(std::path::Path::new(&out).join("socks.bin")).unwrap().len();
    println!("downloaded {} bytes through {}", len, proxy);
    assert!(len > 0);
}

#[test]
#[ignore]
fn pac_script() {
    let pac = std::env::var("KLIPPO_PAC").unwrap();
    let url = std::env::var("KLIPPO_PAC_URL").unwrap();
    let got = klippo::gui::system::pac_proxy_for(&url, &pac);
    println!("PAC picked {:?} for {}", got, url);
    assert_eq!(got.as_deref(), std::env::var("KLIPPO_PAC_EXPECT").ok().as_deref());
}

#[tokio::test]
#[ignore]
async fn ftp_download() {
    let url = std::env::var("KLIPPO_FTP_URL").unwrap();
    let out = std::path::Path::new(&std::env::var("KLIPPO_OUT").unwrap()).join("ftp.bin");
    let _ = std::fs::remove_file(&out);
    for passive in [true, false] {
        let mut g = klippo::core::ftp::FtpGrabber::new(&url, &out);
        let socks = std::env::var("KLIPPO_FTP_SOCKS").ok();
        let login = std::env::var("KLIPPO_FTP_LOGIN").ok().and_then(|l| l.split_once(':').map(|(u, p)| (u.to_string(), p.to_string())));
        g.set_options(klippo::core::ftp::FtpOptions { passive, socks, login, ..Default::default() });
        match g.run().await {
            Ok(path) => println!("passive={} saved {} ({} bytes)", passive, path.display(), std::fs::metadata(&path).unwrap().len()),
            Err(e) => println!("passive={} failed: {}", passive, e),
        }
        let _ = std::fs::remove_file(&out);
    }
    // Resume: write half, then let the grabber finish it.
    let g = klippo::core::ftp::FtpGrabber::new(&url, &out);
    g.run().await.unwrap();
    let full = std::fs::read(&out).unwrap();
    std::fs::write(&out, &full[..full.len() / 2]).unwrap();
    let g = klippo::core::ftp::FtpGrabber::new(&url, &out);
    g.run().await.unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), full, "resumed file matches");
    println!("resume ok ({} bytes)", full.len());
}
