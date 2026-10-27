//! YouTube and stream checks against real files and servers, so they are
//! ignored by default (Klippo does not record live streams):
//!
//! KLIPPO_MUX_VIDEO=v.mp4 KLIPPO_MUX_AUDIO=a.m4a KLIPPO_MUX_OUT=o.mp4 cargo test --test youtube_live -- --ignored mux
//! KLIPPO_YT_SPEC=spec.json KLIPPO_YT_OUT=o.mp4 cargo test --test youtube_live -- --ignored sabr

use klippo::core::mp4mux;
use klippo::core::{YoutubeGrabber, YoutubeSpec};

#[test]
#[ignore]
fn mux_real_streams() {
    let v = std::env::var("KLIPPO_MUX_VIDEO").unwrap();
    let a = std::env::var("KLIPPO_MUX_AUDIO").unwrap();
    let o = std::env::var("KLIPPO_MUX_OUT").unwrap();
    mp4mux::merge(v.as_ref(), a.as_ref(), o.as_ref()).unwrap();
}

/// KLIPPO_MUX_VIDEO=v.webm KLIPPO_MUX_AUDIO=a.webm KLIPPO_MUX_OUT=o.webm
#[test]
#[ignore]
fn mux_real_webm() {
    let v = std::env::var("KLIPPO_MUX_VIDEO").unwrap();
    let a = std::env::var("KLIPPO_MUX_AUDIO").unwrap();
    let o = std::env::var("KLIPPO_MUX_OUT").unwrap();
    klippo::core::mkvmux::merge(v.as_ref(), a.as_ref(), o.as_ref()).unwrap();
}

#[tokio::test]
#[ignore]
async fn sabr_real_download() {
    let spec: YoutubeSpec = serde_json::from_str(&std::fs::read_to_string(std::env::var("KLIPPO_YT_SPEC").unwrap()).unwrap()).unwrap();
    let out = std::env::var("KLIPPO_YT_OUT").unwrap();
    let mut g = YoutubeGrabber::new(spec, &out);
    g.set_request_options(klippo::core::RequestOptions {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36".to_string(),
        ..Default::default()
    });
    let started = std::time::Instant::now();
    g.run().await.unwrap();
    println!("done in {:?}", started.elapsed());
}

#[tokio::test]
#[ignore]
async fn sabr_pause_and_resume() {
    let spec: YoutubeSpec = serde_json::from_str(&std::fs::read_to_string(std::env::var("KLIPPO_YT_SPEC").unwrap()).unwrap()).unwrap();
    let out = std::env::var("KLIPPO_YT_OUT").unwrap();
    let options = klippo::core::RequestOptions {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36".to_string(),
        ..Default::default()
    };
    let mut first = YoutubeGrabber::new(spec.clone(), &out);
    first.set_request_options(options.clone());
    let control = first.control();
    let progress = first.progress();
    let stopper = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if progress.bytes.load(std::sync::atomic::Ordering::Relaxed) > 20 * 1024 * 1024 {
                control.stop();
                return;
            }
        }
    });
    let err = first.run().await.unwrap_err();
    assert!(err.is::<klippo::core::DownloadStopped>(), "{}", err);
    stopper.await.unwrap();
    let paused_at = first.progress().bytes.load(std::sync::atomic::Ordering::Relaxed);
    println!("paused at {} bytes", paused_at);

    let mut second = YoutubeGrabber::new(spec, &out);
    second.set_request_options(options);
    second.run().await.unwrap();
    println!("resumed and finished");
}

/// KLIPPO_HLS_VIDEO=<variant or master> [KLIPPO_HLS_AUDIO=<audio playlist>] KLIPPO_HLS_OUT=<file>
#[tokio::test]
#[ignore]
async fn hls_with_separate_audio() {
    let video = std::env::var("KLIPPO_HLS_VIDEO").unwrap();
    let out = std::env::var("KLIPPO_HLS_OUT").unwrap();
    let mut g = klippo::core::HLSGrabber::new(&video, &out, 8);
    g.set_audio_url(std::env::var("KLIPPO_HLS_AUDIO").ok());
    let saved = g.run().await.unwrap();
    println!("saved {}", saved.display());
}
