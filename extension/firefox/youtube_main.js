// Runs in YouTube's own page (MAIN world) because content scripts cannot see
// page variables. It answers content.js with the player's stream list and
// tells it when the page moves to another video or an ad starts or ends.
(() => {
  if (window.__klippoYoutube) return;
  window.__klippoYoutube = true;

  function pageVideoId() {
    const u = new URL(location.href);
    const shorts = /^\/(?:shorts|embed|live)\/([\w-]{11})/.exec(u.pathname);
    return u.searchParams.get("v") || (shorts && shorts[1]) || "";
  }

  function playerResponse() {
    for (const id of ["movie_player", "shorts-player"]) {
      const el = document.getElementById(id);
      try {
        const r = el && el.getPlayerResponse && el.getPlayerResponse();
        if (r && r.videoDetails) return r;
      } catch (e) {}
    }
    return window.ytInitialPlayerResponse || null;
  }

  function info() {
    const r = playerResponse();
    const want = pageVideoId();
    if (!r || !r.videoDetails || !r.streamingData) return null;
    if (want && r.videoDetails.videoId !== want) return null; // still the previous video
    // Paid films and other DRM videos: YouTube marks the stream with drmParams,
    // or each format carries a drmFamilies list (Widevine, PlayReady). Klippo
    // cannot download these.
    const adaptive = r.streamingData.adaptiveFormats || [];
    const drm = !!r.streamingData.drmParams || adaptive.some((f) => Array.isArray(f.drmFamilies) && f.drmFamilies.length);
    const formats = adaptive.map((f) => ({
      itag: f.itag,
      mime: f.mimeType || "",
      quality: f.qualityLabel || "",
      height: f.height || 0,
      fps: f.fps || 0,
      bitrate: f.bitrate || 0,
      lmt: String(f.lastModified || "0"),
      xtags: f.xtags || "",
      size: f.contentLength ? Number(f.contentLength) : null,
      durationMs: f.approxDurationMs ? Number(f.approxDurationMs) : null,
      audioDefault: f.audioTrack ? !!f.audioTrack.audioIsDefault : true,
    }));
    return {
      videoId: r.videoDetails.videoId,
      title: r.videoDetails.title || "",
      lengthSeconds: Number(r.videoDetails.lengthSeconds) || 0,
      isLive: !!r.videoDetails.isLive,
      drm,
      formats,
    };
  }

  function adShowing() {
    const el = document.getElementById("movie_player");
    return !!(el && el.classList.contains("ad-showing"));
  }

  function post(data) {
    window.postMessage({ klippo: "youtube", ...data }, location.origin);
  }

  window.addEventListener("message", (e) => {
    if (e.source === window && e.data && e.data.klippo === "youtube-request") {
      post({ kind: "info", info: info(), ad: adShowing() });
    }
  });
  document.addEventListener("yt-navigate-finish", () => post({ kind: "changed" }));

  // Ads play through the same player; content.js tells the background so it
  // keeps the stream session of the real video.
  let lastAd = null;
  setInterval(() => {
    const ad = adShowing();
    if (ad !== lastAd) {
      lastAd = ad;
      post({ kind: "ad", ad });
    }
  }, 1000);
})();
