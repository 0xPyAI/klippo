// Klippo browser integration.
//
// - Hands browser downloads to the Klippo native host. The host decides (from
//   Klippo's "File types" and exception list) whether to take a download;
//   when it does, the browser's own download is cancelled.
// - Finds the video and audio streams that pages load, like the "Download
//   this video" panel. content.js shows them over the video, popup.js in the
//   toolbar. Both send the chosen stream to Klippo.

const HOST = "org.klippo.downloader";
const api = globalThis.browser ?? globalThis.chrome;
const actionApi = api.action ?? api.browserAction;

const MAX_ITEMS = 40;
// Smaller media responses are nearly always stream segments or previews.
const MIN_FILE_SIZE = 300 * 1024;
const MIN_AUDIO_SIZE = 32 * 1024;
// DASH-style audio files are often sent as video/mp4; their names tell them apart.
const AUDIO_NAME = /audio/i;
// A navigation keeps media found this recently (it belongs to the new page).
const KEEP_ON_NAVIGATION_MS = 1500;

const SKIP_TYPES = new Set(["image", "imageset", "script", "stylesheet", "font", "ping", "csp_report", "beacon", "websocket", "web_manifest", "speculative"]);
const HLS_MIMES = new Set(["application/vnd.apple.mpegurl", "application/x-mpegurl", "application/mpegurl", "audio/mpegurl", "audio/x-mpegurl"]);
const SEGMENT_MIMES = new Set(["video/mp2t", "video/iso.segment", "audio/iso.segment"]);
const SEGMENT_EXTS = new Set(["ts", "m4s", "cmfv", "cmfa", "m4f"]);
const OCTET_MIMES = new Set(["", "application/octet-stream", "binary/octet-stream", "application/mp4", "application/force-download"]);
const MEDIA_EXTS = {
  mp4: "video/mp4", m4v: "video/mp4", webm: "video/webm", mkv: "video/x-matroska", mov: "video/quicktime",
  flv: "video/x-flv", avi: "video/x-msvideo", wmv: "video/x-ms-wmv", "3gp": "video/3gpp", ogv: "video/ogg",
  mpg: "video/mpeg", mpeg: "video/mpeg", mp3: "audio/mpeg", m4a: "audio/mp4", aac: "audio/aac",
  ogg: "audio/ogg", oga: "audio/ogg", opus: "audio/opus", wav: "audio/wav", flac: "audio/flac", wma: "audio/x-ms-wma",
};
const MIME_EXTS = {
  "video/mp4": "mp4", "video/webm": "webm", "video/x-matroska": "mkv", "video/quicktime": "mov", "video/x-flv": "flv",
  "video/x-msvideo": "avi", "video/x-ms-wmv": "wmv", "video/3gpp": "3gp", "video/ogg": "ogv", "video/mpeg": "mpg",
  "audio/mpeg": "mp3", "audio/mp3": "mp3", "audio/mp4": "m4a", "audio/x-m4a": "m4a", "audio/aac": "aac", "audio/ogg": "ogg",
  "audio/opus": "opus", "audio/wav": "wav", "audio/x-wav": "wav", "audio/webm": "weba", "audio/flac": "flac",
};

// ---------------------------------------------------------------------------
// Settings and native host
// ---------------------------------------------------------------------------

async function isEnabled() {
  const { enabled = true } = await api.storage.local.get("enabled");
  return enabled;
}

/** The cookies for `url`; a download of a private window uses that window's cookies. */
async function cookieHeader(url, incognito) {
  try {
    const query = { url };
    if (incognito) {
      // Chromium names its private store "1", Firefox "firefox-private".
      const stores = await api.cookies.getAllCookieStores();
      const store = stores.find((s) => s.incognito) || stores.find((s) => s.id === "1" || s.id === "firefox-private");
      if (store) query.storeId = store.id;
    }
    const cookies = await api.cookies.getAll(query);
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch (e) {
    return "";
  }
}

function sendToHost(message) {
  return api.runtime.sendNativeMessage(HOST, message);
}

/** Options > General of Klippo: special keys and context menu items. */
const hostConfig = { keys: null, menu: { download: true, last_video: true, videos: true, all_links: true }, panel: null, captureFetched: false, at: 0 };

async function loadConfig(force) {
  if (!force && Date.now() - hostConfig.at < 60000) return;
  hostConfig.at = Date.now();
  try {
    const r = await sendToHost({ action: "config" });
    if (!r || r.status !== "ok") return;
    hostConfig.keys = r.keys || null;
    hostConfig.panel = r.panel || null;
    hostConfig.captureFetched = r.capture_fetched === true;
    if (r.menu && JSON.stringify(r.menu) !== JSON.stringify(hostConfig.menu)) {
      hostConfig.menu = r.menu;
      buildMenus();
    }
  } catch (e) {}
}

/** The last link clicked in a page, with the keys held (content.js reports it). */
let lastClick = null;
/** Keys held right now in the active page (for "start in X seconds" pages). */
let keysNow = { alt: false, shift: false, ctrl: false, ins: false, time: 0 };

function sameUrl(a, b) {
  try {
    const x = new URL(a);
    const y = new URL(b);
    return x.href.split("#")[0] === y.href.split("#")[0];
  } catch (e) {
    return false;
  }
}

/** The browser's own fixed proxy as scheme://host:port ("" for direct or system). */
async function browserProxy() {
  try {
    if (!api.proxy || !api.proxy.settings) return "";
    const s = await api.proxy.settings.get({});
    const v = s && s.value;
    if (v && v.mode === "fixed_servers" && v.rules) {
      const p = v.rules.singleProxy || v.rules.proxyForHttps || v.rules.proxyForHttp;
      if (p && p.host) {
        const scheme = p.scheme === "socks5" ? "socks5h" : p.scheme === "socks4" ? "socks4" : "http";
        return `${scheme}://${p.host}:${p.port || (scheme === "http" ? 80 : 1080)}`;
      }
    }
    if (v && v.proxyType === "manual") {
      if (v.http) return "http://" + v.http.replace(/^https?:\/\//, "");
      if (v.socks) return (v.socksVersion === 4 ? "socks4://" : "socks5h://") + v.socks.replace(/^socks\d?:\/\//, "");
    }
  } catch (e) {}
  return "";
}

/** "Using special keys": "prevent", "force" or "" for this download. */
function keyDecision(item, url) {
  const cfg = hostConfig.keys;
  if (!cfg) return "";
  let held = null;
  if (cfg.check_click) {
    const click = lastClick && Date.now() - lastClick.time < 10000 ? lastClick : null;
    if (click && (sameUrl(click.url, url) || sameUrl(click.url, item.url))) held = click;
  } else if (Date.now() - keysNow.time < 30000) {
    held = keysNow;
  }
  if (!held) return "";
  const pressed = (set) => !!set && ((set.alt && held.alt) || (set.shift && held.shift) || (set.ctrl && held.ctrl) || (set.ins && held.ins));
  if (cfg.prevent && cfg.prevent.enabled && pressed(cfg.prevent)) return "prevent";
  if (cfg.force && cfg.force.enabled && pressed(cfg.force)) {
    const web = /^(text\/html|text\/css|image\/|application\/(x-)?javascript)/i.test(item.mime || "");
    if (!(cfg.skip_web && web)) return "force";
  }
  return "";
}

function baseName(path) {
  return path ? path.split(/[\\/]/).pop() : "";
}

// ---------------------------------------------------------------------------
// Download capture
// ---------------------------------------------------------------------------

// Forms that answer with a file (a POST): the form's body is kept for a while,
// so Klippo can send the same POST again for the file.
const FORM_KEEP_MS = 120000;
/** Request id -> body, until its headers tell the body's type. */
const formBodies = new Map();
/** Form address -> { data (base64), type, time }. */
const formPosts = new Map();

/** The body of a POST as the browser reports it: raw bytes, or the form's fields. */
function formBody(requestBody) {
  if (!requestBody) return null;
  if (requestBody.raw && requestBody.raw.length && requestBody.raw.every((p) => p.bytes)) {
    const parts = requestBody.raw.map((p) => new Uint8Array(p.bytes));
    const all = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of parts) {
      all.set(p, at);
      at += p.length;
    }
    return { raw: all };
  }
  if (requestBody.formData) return { fields: requestBody.formData };
  return null;
}

/** The fields again, in the encoding the form used. */
function encodeForm(body, type) {
  if (body.raw) return { data: toBase64(body.raw), type };
  const pairs = Object.entries(body.fields).flatMap(([k, vs]) => vs.map((v) => [k, v]));
  if (/^multipart\/form-data/i.test(type)) {
    // The browser does not report a file field's contents; only text fields go again.
    const boundary = "----KlippoForm" + Math.random().toString(16).slice(2);
    const text = pairs.map(([k, v]) => `--${boundary}\r\nContent-Disposition: form-data; name="${k.replace(/"/g, "%22")}"\r\n\r\n${v}\r\n`).join("") + `--${boundary}--\r\n`;
    return { data: toBase64(new TextEncoder().encode(text)), type: `multipart/form-data; boundary=${boundary}` };
  }
  return { data: toBase64(new TextEncoder().encode(new URLSearchParams(pairs).toString())), type: "application/x-www-form-urlencoded" };
}

const PAGE_REQUESTS = { urls: ["http://*/*", "https://*/*"], types: ["main_frame", "sub_frame"] };
// Firefox can report a download (downloads.onCreated) before it has delivered
// the request events of the same form. Listeners that hold the request
// (Firefox only; Chromium has no blocking listeners in MV3, and holds the
// download in onDeterminingFilename instead) see the form before its answer.
const HOLD_REQUEST = api.downloads.onDeterminingFilename ? [] : ["blocking"];
api.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (details.method !== "POST") return;
    const body = formBody(details.requestBody);
    if (!body) return;
    if (formBodies.size > 50) formBodies.delete(formBodies.keys().next().value); // a request that never sent its headers
    formBodies.set(details.requestId, { url: details.url, body });
  },
  PAGE_REQUESTS,
  ["requestBody", ...HOLD_REQUEST]
);
api.webRequest.onBeforeSendHeaders.addListener(
  (details) => {
    const pending = formBodies.get(details.requestId);
    if (!pending) return;
    formBodies.delete(details.requestId);
    const header = (details.requestHeaders || []).find((h) => h.name.toLowerCase() === "content-type");
    const now = Date.now();
    for (const [url, post] of formPosts) if (now - post.time > FORM_KEEP_MS) formPosts.delete(url);
    formPosts.set(pending.url, { ...encodeForm(pending.body, (header && header.value) || "application/x-www-form-urlencoded"), time: now });
  },
  PAGE_REQUESTS,
  ["requestHeaders", ...HOLD_REQUEST]
);

// Files that a page makes itself (blob: and data: addresses): only the browser
// can save them. When the browser has saved one, Klippo takes the file over.
/** Download id -> what Klippo needs to know about the file. */
const adopting = new Map();

async function adopt(id) {
  const info = adopting.get(id);
  adopting.delete(id);
  const [item] = await api.downloads.search({ id });
  if (!info || !item || !item.filename) return;
  let resp;
  try {
    resp = await sendToHost({ action: "adopt", path: item.filename, filename: baseName(item.filename), ...info, mime: item.mime || info.mime });
  } catch (e) {
    return; // no Klippo: the file stays where the browser saved it
  }
  if (resp && resp.status === "ok") {
    // Klippo moved the file; only the browser's list entry is left.
    try {
      await api.downloads.erase({ id });
    } catch (e) {}
  }
}

api.downloads.onChanged.addListener((delta) => {
  if (!adopting.has(delta.id) || !delta.state) return;
  if (delta.state.current === "complete") adopt(delta.id);
  else if (delta.state.current === "interrupted") adopting.delete(delta.id);
});

/** Offers a new browser download to Klippo; true when Klippo took it (the browser's is cancelled). */
async function offer(item) {
  if (!(await isEnabled())) return false;
  const url = item.finalUrl || item.url;
  if (item.state && item.state !== "in_progress") return false;
  const pageMade = /^(blob|data):/i.test(url);
  if (!pageMade && !/^(https?|ftp):/i.test(url)) return false; // file: addresses are on this computer already
  await loadConfig(false);
  const keys = keyDecision(item, url);
  if (keys === "prevent") return false; // the user held the "prevent" key: the browser keeps it
  // Download panels: "Don't capture downloads from web-players automatically" leaves
  // video and audio files to the browser; the panel still offers them.
  const panel = hostConfig.panel;
  if (panel && panel.no_auto_capture && keys !== "force" && /^(video|audio)\//i.test(item.mime || "")) return false;

  if (pageMade) {
    // A data: address holds the whole file; Klippo needs only its type.
    const shown = /^data:/i.test(url) ? url.slice(0, Math.min(80, (url.indexOf(",") + 1 || 80) - 1)) : url;
    adopting.set(item.id, { url: shown, referer: item.referrer || "", force: keys === "force", mime: item.mime || "" });
    // A small file can be complete already.
    const [now] = await api.downloads.search({ id: item.id });
    if (now && now.state === "complete") adopt(item.id);
    return false; // the browser saves it
  }

  // A form's POST, unless a redirect turned it into a plain address.
  const form = formPosts.get(item.url);
  const post = form && sameUrl(url, item.url) ? form : null;

  let resp;
  try {
    resp = await sendToHost({
      action: "download",
      force: keys === "force",
      browser_proxy: (await browserProxy()) || null,
      url,
      referer: item.referrer || "",
      cookies: await cookieHeader(url, item.incognito),
      user_agent: navigator.userAgent,
      filename: baseName(item.filename),
      file_size: item.fileSize > 0 ? item.fileSize : null,
      mime: item.mime || "",
      post_data: post ? post.data : null,
      post_type: post ? post.type : null,
    });
  } catch (e) {
    console.warn("Klippo native host is not available; the browser keeps the download.", e);
    return false;
  }

  if (resp && resp.status === "ok") {
    try {
      await api.downloads.cancel(item.id);
    } catch (e) {}
    // Without onDeterminingFilename (Firefox) a quick file can be complete
    // already: Klippo downloads it, so the browser's copy goes.
    try {
      const [now] = await api.downloads.search({ id: item.id });
      if (now && now.state === "complete" && now.exists !== false) await api.downloads.removeFile(item.id);
    } catch (e) {}
    try {
      await api.downloads.erase({ id: item.id });
    } catch (e) {}
    return true;
  }
  return false;
}

if (api.downloads.onDeterminingFilename) {
  // Chromium: the final file name is known here, which helps "download.php?id=1"
  // links. The browser waits for the name until Klippo has decided, so a quick
  // file does not end up in both places.
  api.downloads.onDeterminingFilename.addListener((item, suggest) => {
    offer(item).then(
      (taken) => {
        if (!taken) suggest();
      },
      () => suggest()
    );
    return true;
  });
} else {
  api.downloads.onCreated.addListener((item) => offer(item));
}

// ---------------------------------------------------------------------------
// Media found in tabs
// ---------------------------------------------------------------------------

/** tabId -> media items, oldest first. */
const media = new Map();
/** Playlists already fetched, so each one is read once. */
const inspected = new Set();

/** tabId -> { info, templates, ad } for YouTube pages (see the YouTube section). */
const youtube = new Map();

// The service worker can stop at any time; session storage keeps the lists.
const sessionStore = api.storage.session;
// Signature of the context menu now built, kept across restarts so the menu is
// not removed and rebuilt on every wake; that race sometimes left the browser
// with no "Download with Klippo" item.
let menuSignature = null;
const restored = sessionStore
  ? sessionStore
      .get(["media", "youtube", "menuSig"])
      .then(({ media: saved, youtube: savedYt, menuSig }) => {
        if (typeof menuSig === "string") menuSignature = menuSig;
        for (const [tabId, items] of Object.entries(saved || {})) {
          if (!media.has(Number(tabId))) media.set(Number(tabId), items);
        }
        for (const [tabId, yt] of Object.entries(savedYt || {})) {
          if (!youtube.has(Number(tabId))) youtube.set(Number(tabId), yt);
        }
      })
      .catch(() => {})
  : Promise.resolve();

let saveTimer = null;
function persist() {
  if (!sessionStore) return;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(
    () => sessionStore.set({ media: Object.fromEntries(media), youtube: Object.fromEntries(youtube) }).catch(() => {}),
    300
  );
}

function headerMap(headers) {
  const map = {};
  for (const { name, value } of headers || []) map[name.toLowerCase()] = value ?? "";
  return map;
}

function extOf(url) {
  try {
    const match = /\.([a-z0-9]{1,5})$/i.exec(new URL(url).pathname);
    return match ? match[1].toLowerCase() : "";
  } catch (e) {
    return "";
  }
}

/** Same stream, other byte range: one entry. */
function mediaKey(url) {
  try {
    const u = new URL(url);
    u.hash = "";
    if (u.hostname.endsWith(".googlevideo.com")) {
      for (const p of ["range", "rn", "rbuf"]) u.searchParams.delete(p);
    }
    return u.toString();
  } catch (e) {
    return url;
  }
}

function dispositionName(value) {
  if (!value) return "";
  const star = /filename\*\s*=\s*[^']*'[^']*'([^;]+)/i.exec(value);
  if (star) {
    try {
      return decodeURIComponent(star[1].trim());
    } catch (e) {}
  }
  const plain = /filename\s*=\s*"?([^";]+)"?/i.exec(value);
  return plain ? plain[1].trim() : "";
}

/** Returns a media item for a response, or null. */
function classify(details) {
  if (details.tabId < 0 || SKIP_TYPES.has(details.type)) return null;
  if (!/^https?:/i.test(details.url) || details.statusCode >= 300) return null;
  // YouTube stream addresses serve only their first megabyte to other programs;
  // YouTube videos are offered through the player's stream session instead.
  if (/^https?:\/\/[^/]*\.googlevideo\.com\//i.test(details.url)) return null;
  const h = headerMap(details.responseHeaders);
  const mime = (h["content-type"] || "").split(";")[0].trim().toLowerCase();
  const ext = extOf(details.url);
  const base = {
    key: mediaKey(details.url),
    url: mediaKey(details.url),
    referer: details.documentUrl || (details.frameId > 0 && details.initiator ? details.initiator + "/" : ""),
    name: dispositionName(h["content-disposition"]),
    time: Date.now(),
  };
  // Some players (TeraBox and other file hosts) fetch the HLS playlist from an
  // API address with a generic type, e.g. ".../share/streaming?...&type=M3U8_AUTO_720".
  // Treat an address that says "m3u8" as HLS too, unless the answer is clearly
  // not a playlist (a JSON or HTML API reply).
  const urlHls = /m3u8/i.test(details.url) && !/^(application\/json|text\/html|application\/xml)$/.test(mime);
  if (HLS_MIMES.has(mime) || ext === "m3u8" || urlHls) {
    return { ...base, kind: "hls", mime: "application/vnd.apple.mpegurl", size: null };
  }
  // DASH too: an API address may answer with a generic type and no ".mpd".
  const urlDash = (/\.mpd(?:[?&#]|$)/i.test(details.url) || /[?&](?:type|format|mime)=[^&]*(?:dash|mpd)/i.test(details.url)) && !/^(application\/json|text\/html)$/.test(mime);
  if (mime === "application/dash+xml" || mime === "video/vnd.mpeg.dash.mpd" || ext === "mpd" || urlDash) {
    return { ...base, kind: "dash", mime: "application/dash+xml", size: null };
  }
  if (SEGMENT_MIMES.has(mime) || SEGMENT_EXTS.has(ext)) return null;

  // Octet or "force download" media: name the type from the address extension,
  // or from the Content-Disposition file name when the address has none.
  const nameExt = (/\.([a-z0-9]{1,5})$/i.exec(base.name || "") || ["", ""])[1].toLowerCase();
  let type = "";
  if ((mime.startsWith("video/") || mime.startsWith("audio/")) && !HLS_MIMES.has(mime)) type = mime;
  else if (OCTET_MIMES.has(mime) && (MEDIA_EXTS[ext] || MEDIA_EXTS[nameExt])) type = MEDIA_EXTS[ext] || MEDIA_EXTS[nameExt];
  let params = null;
  try {
    params = new URL(details.url).searchParams;
  } catch (e) {}
  // googlevideo.com streams name their type and length in the address.
  const urlMime = params && params.get("mime");
  if (!type && urlMime && /^(video|audio)\//.test(urlMime)) type = urlMime;
  if (!type) return null;

  const range = /\/(\d+)\s*$/.exec(h["content-range"] || "");
  let size = range ? Number(range[1]) : null;
  if (size === null && details.statusCode === 200 && h["content-length"]) size = Number(h["content-length"]) || null;
  if (size === null && params && params.get("clen")) size = Number(params.get("clen")) || null;
  // Separate audio of a short video can be small.
  const minimum = type.startsWith("audio/") || AUDIO_NAME.test(fileName(details.url)) ? MIN_AUDIO_SIZE : MIN_FILE_SIZE;
  if (size !== null && size < minimum) return null;
  return { ...base, kind: "file", mime: type, size };
}

function attr(attrs, key) {
  const match = new RegExp(`(?:^|,)${key}=("[^"]*"|[^,]*)`).exec(attrs);
  return match ? match[1].replace(/^"|"$/g, "") : "";
}

/** tabId -> files that are parts of an HLS stream (hidden from the list). */
const segmentFiles = new Map();
/** tabId -> audio playlists named by a master playlist. */
const audioPlaylists = new Map();
/** tabId -> audio playlist -> { parent: master playlist, rank: bitrate guess }. */
const audioDetails = new Map();

/** Marks an HLS audio playlist and files it under its video, after the qualities. */
function markAudio(tabId, m) {
  if (m.kind !== "hls" || !audioPlaylists.has(tabId) || !audioPlaylists.get(tabId).has(m.key)) return;
  m.audioOnly = true;
  const d = audioDetails.has(tabId) && audioDetails.get(tabId).get(m.key);
  if (d) {
    m.parent = d.parent;
    m.audioRank = d.rank;
  }
}

function tabSet(map, tabId) {
  if (!map.has(tabId)) map.set(tabId, new Set());
  return map.get(tabId);
}

function withoutQuery(url) {
  return url.split(/[?#]/)[0];
}

function codecName(codecs) {
  const c = (codecs || "").toLowerCase();
  if (/(^|,)\s*(avc1|avc3)/.test(c)) return "H.264";
  if (/(^|,)\s*(hvc1|hev1)/.test(c)) return "HEVC";
  if (/(^|,)\s*(vp09|vp9)/.test(c)) return "VP9";
  if (/(^|,)\s*av01/.test(c)) return "AV1";
  return "";
}

/** Reads an HLS playlist: a master gives one entry per quality, each with its audio. */
async function inspectPlaylist(tabId, key) {
  if (inspected.has(key)) return;
  inspected.add(key);
  let text;
  try {
    const resp = await fetch(key, { credentials: "include" });
    if (!resp.ok) return;
    text = (await resp.text()).replace(/^﻿/, "");
  } catch (e) {
    return; // The server wants the page's referrer or cookies; keep the plain entry.
  }
  if (!text.startsWith("#EXTM3U")) return;
  const lines = text.split(/\r?\n/).map((l) => l.trim());

  if (lines.some((l) => l.startsWith("#EXT-X-STREAM-INF:"))) {
    // Separate audio playlists (Klippo joins them to the video).
    const audioByGroup = new Map();
    for (const l of lines) {
      if (!l.startsWith("#EXT-X-MEDIA:")) continue;
      const a = l.slice(13);
      if (attr(a, "TYPE") !== "AUDIO" || !attr(a, "URI")) continue;
      const url = mediaKey(new URL(attr(a, "URI"), key).toString());
      tabSet(audioPlaylists, tabId).add(url);
      const group = attr(a, "GROUP-ID");
      // "audio-128000" or "aac_128k": the group name usually carries the bitrate.
      const rank = Math.max(0, ...(group.match(/\d+/g) || ["0"]).map(Number));
      if (!audioDetails.has(tabId)) audioDetails.set(tabId, new Map());
      audioDetails.get(tabId).set(url, { parent: key, rank });
      const isDefault = attr(a, "DEFAULT") === "YES";
      if (!audioByGroup.has(group) || isDefault) {
        if (!(audioByGroup.has(group) && audioByGroup.get(group).isDefault)) audioByGroup.set(group, { url, isDefault });
      }
    }
    // The larger number in an audio playlist's name is usually its bitrate (AUDIO_128 > AUDIO_64).
    const audioScore = (url) => Math.max(0, ...(withoutQuery(url).split("/").pop().match(/\d+/g) || ["0"]).map(Number));
    const variants = [];
    const seen = new Map();
    for (let i = 0; i < lines.length; i++) {
      if (!lines[i].startsWith("#EXT-X-STREAM-INF:")) continue;
      const attrs = lines[i].slice(18);
      const uri = lines.slice(i + 1).find((l) => l && !l.startsWith("#"));
      if (!uri) continue;
      const vkey = mediaKey(new URL(uri, key).toString());
      const resolution = attr(attrs, "RESOLUTION");
      // Name by the short side, so a portrait 720x1280 video is "720p".
      const [w, h] = resolution.split("x").map(Number);
      const height = w && h ? Math.min(w, h) : 0;
      const audio = audioByGroup.get(attr(attrs, "AUDIO"));
      // AAC joins into MP4 and TS alike; AC-3/E-AC-3 copies of a quality come second.
      const codecs = attr(attrs, "CODECS").toLowerCase();
      const aac = !/(^|,)\s*(ac-3|ec-3)/.test(codecs) || codecs.includes("mp4a");
      const rank = (a, isAac) => (a ? (isAac ? 1e9 : 0) + audioScore(a.url || a) : -1);
      const known = seen.get(vkey);
      if (known) {
        // The same quality listed for another audio group: keep the better audio.
        if (audio && rank(audio, aac) > rank(known.audioUrl, known.aac)) {
          known.audioUrl = audio.url;
          known.aac = aac;
        }
        continue;
      }
      const hasAudioCodec = /mp4a|ac-3|ec-3|opus|vorbis|flac|alac/.test(codecs);
      const variant = {
        key: vkey,
        noAudio: !audio && codecs !== "" && !hasAudioCodec,
        bandwidth: Number(attr(attrs, "BANDWIDTH")) || 0,
        quality: height ? `${height}p` : "",
        codec: codecName(attr(attrs, "CODECS")),
        resolution,
        audioUrl: audio ? audio.url : "",
        aac,
      };
      seen.set(vkey, variant);
      variants.push(variant);
    }
    const list = media.get(tabId) || [];
    const master = list.find((m) => m.key === key);
    if (master) master.hidden = true; // the qualities below replace it
    for (const m of list) markAudio(tabId, m);
    variants.sort((a, b) => b.bandwidth - a.bandwidth);
    for (const v of variants) {
      await addMedia(tabId, {
        key: v.key,
        url: v.key,
        kind: "hls",
        mime: "application/vnd.apple.mpegurl",
        quality: v.quality,
        codec: v.codec,
        resolution: v.resolution,
        bandwidth: v.bandwidth,
        audioUrl: v.audioUrl,
        noAudio: v.noAudio,
        parent: key,
        referer: master ? master.referer : "",
        name: master ? master.name : "",
        time: Date.now(),
      });
    }
    return;
  }

  // Media playlist: length, container, live or encrypted.
  let duration = 0;
  const parts = tabSet(segmentFiles, tabId);
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    const m = /^#EXTINF:([\d.]+)/.exec(l);
    if (m) duration += Number(m[1]);
    const map = l.startsWith("#EXT-X-MAP:") && attr(l.slice(11), "URI");
    const uri = map || (l && !l.startsWith("#") ? l : "");
    if (uri) {
      try {
        parts.add(withoutQuery(new URL(uri, key).toString()));
      } catch (e) {}
    }
  }
  const fmp4 = lines.some((l) => l.startsWith("#EXT-X-MAP:"));
  // AES-128 with a plain key file is fine; SAMPLE-AES and key systems (FairPlay, Widevine) are DRM.
  const encrypted = lines.some((l) => {
    if (!l.startsWith("#EXT-X-KEY:")) return false;
    const method = attr(l.slice(11), "METHOD").toUpperCase();
    const format = attr(l.slice(11), "KEYFORMAT") || "identity";
    return method !== "NONE" && !(method === "AES-128" && format === "identity");
  });
  const live = !lines.includes("#EXT-X-ENDLIST");
  const list = media.get(tabId) || [];
  const item = list.find((m) => m.key === key);
  if (!item) return;
  item.duration = live ? null : duration;
  item.live = live;
  item.encrypted = encrypted;
  item.fmp4 = fmp4;
  markAudio(tabId, item);
  // The other qualities of the same video have the same length and container.
  if (item.parent) {
    for (const sibling of list) {
      if (sibling.parent !== item.parent) continue;
      if (!live && !sibling.duration) sibling.duration = duration;
      if (sibling.fmp4 === undefined) sibling.fmp4 = fmp4;
    }
  }
  changed(tabId);
}

function xmlAttr(attrs, key) {
  const m = new RegExp(`(?:^|\\s)(?:\\w+:)?${key}\\s*=\\s*"([^"]*)"`).exec(attrs);
  return m ? m[1] : "";
}

/** ISO 8601 duration (PT1H2M3.5S) in seconds. */
function isoSeconds(text) {
  const m = /P(?:(\d+(?:\.\d+)?)D)?(?:T(?:(\d+(?:\.\d+)?)H)?(?:(\d+(?:\.\d+)?)M)?(?:(\d+(?:\.\d+)?)S)?)?/.exec(text || "");
  if (!m) return 0;
  return (+m[1] || 0) * 86400 + (+m[2] || 0) * 3600 + (+m[3] || 0) * 60 + (+m[4] || 0);
}

/** Reads a DASH manifest: one entry per video height, each joined with the audio. */
async function inspectManifest(tabId, key) {
  if (inspected.has(key)) return;
  inspected.add(key);
  let text;
  try {
    const resp = await fetch(key, { credentials: "include" });
    if (!resp.ok) return;
    text = await resp.text();
  } catch (e) {
    return;
  }
  if (!/<(\w+:)?MPD[\s>]/.test(text)) return;
  const durationMatch = /mediaPresentationDuration\s*=\s*"([^"]+)"/.exec(text);
  const duration = durationMatch ? isoSeconds(durationMatch[1]) : 0;
  const drm = /<(\w+:)?ContentProtection[\s>/]/.test(text);
  const reps = [];
  for (const set of text.matchAll(/<(?:\w+:)?AdaptationSet\b([^>]*)>([\s\S]*?)<\/(?:\w+:)?AdaptationSet>/g)) {
    const setAttrs = set[1];
    const setMime = xmlAttr(setAttrs, "mimeType");
    const contentType = xmlAttr(setAttrs, "contentType");
    for (const rep of set[2].matchAll(/<(?:\w+:)?Representation\b([^>]*)>/g)) {
      const a = rep[1];
      const get = (k) => xmlAttr(a, k) || xmlAttr(setAttrs, k);
      reps.push({
        mime: xmlAttr(a, "mimeType") || setMime || (contentType ? `${contentType}/mp4` : ""),
        width: Number(get("width")) || 0,
        height: Number(get("height")) || 0,
        bandwidth: Number(get("bandwidth")) || 0,
        codecs: get("codecs"),
      });
    }
  }
  // MP4 first; a WebM-only manifest gives WebM video with WebM (Opus) audio, as Klippo joins it.
  const mp4 = (r) => !r.mime || r.mime.endsWith("/mp4");
  const webm = (r) => r.mime.endsWith("/webm");
  const isVideo = (r) => r.mime.startsWith("video/") || r.height > 0;
  const family = reps.some((r) => isVideo(r) && mp4(r)) ? mp4 : webm;
  const videos = reps.filter((r) => isVideo(r) && family(r));
  const good = (r) => r.codecs.startsWith("mp4a") || r.codecs.startsWith("opus");
  const audio = reps
    .filter((r) => r.mime.startsWith("audio/") && family(r))
    .sort((a, b) => good(b) - good(a) || b.bandwidth - a.bandwidth)[0];
  const best = new Map();
  for (const v of videos) {
    if (!best.has(v.height) || best.get(v.height).bandwidth < v.bandwidth) best.set(v.height, v);
  }
  const list = media.get(tabId) || [];
  const manifest = list.find((m) => m.key === key);
  if (manifest && best.size) manifest.hidden = true; // the heights below replace it
  for (const v of [...best.values()].sort((a, b) => b.height - a.height)) {
    const short = v.width && v.height ? Math.min(v.width, v.height) : v.height;
    const url = `${key}#height=${v.height}`;
    await addMedia(tabId, {
      key: url,
      url,
      kind: "dash",
      mime: "application/dash+xml",
      quality: short ? `${short}p` : "",
      codec: codecName(v.codecs),
      bandwidth: v.bandwidth + (audio ? audio.bandwidth : 0),
      duration: duration || null,
      withAudio: !!audio,
      encrypted: drm,
      parent: key,
      referer: manifest ? manifest.referer : "",
      name: manifest ? manifest.name : "",
      time: Date.now(),
    });
  }
}

async function addMedia(tabId, item) {
  await restored;
  markAudio(tabId, item);
  const list = media.get(tabId) || [];
  const existing = list.find((m) => m.key === item.key);
  if (existing) {
    for (const [k, v] of Object.entries(item)) {
      if (v !== null && v !== undefined && v !== "" && k !== "time") existing[k] = v;
    }
  } else {
    list.push(item);
    while (list.length > MAX_ITEMS) list.shift();
  }
  media.set(tabId, list);
  changed(tabId);
  // Qualities from a master playlist are read only when the player loads them.
  if (item.kind === "hls" && !item.parent) inspectPlaylist(tabId, item.key);
  if (item.kind === "dash" && !item.parent) inspectManifest(tabId, item.key);
}

function formatBytes(n) {
  if (!n) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

function formatDuration(s) {
  s = Math.round(s);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

function formatName(item) {
  if (item.kind === "hls") return "HLS";
  if (item.kind === "dash") return "DASH";
  const ext = MEDIA_EXTS[extOf(item.url)] ? extOf(item.url) : MIME_EXTS[item.mime] || item.mime.split("/")[1] || "";
  return ext.toUpperCase();
}

/** The file type named in the "Download panels" settings ("mp4", "m3u8", ...). */
function panelType(item) {
  if (item.kind === "hls") return "m3u8";
  if (item.kind === "dash") return "mpd";
  const ext = extOf(item.url);
  if (MEDIA_EXTS[ext]) return ext;
  return (MIME_EXTS[item.mime] || (item.mime || "").split("/")[1] || "").toLowerCase();
}

function fileName(url) {
  try {
    return decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
  } catch (e) {
    return "";
  }
}

function isAudioFile(m) {
  return m.kind === "file" && ((m.mime || "").startsWith("audio/") || AUDIO_NAME.test(fileName(m.url)));
}

/** An audio-only file from the same folder as a video file (DASH-style split streams). */
function pairedAudio(tabId, item) {
  if (item.kind !== "file" || !(item.mime || "").startsWith("video/") || isAudioFile(item)) return null;
  const folder = (u) => withoutQuery(u).replace(/[^/]*$/, "");
  const dir = folder(item.url);
  const audios = (media.get(tabId) || []).filter((m) => isAudioFile(m) && folder(m.url) === dir);
  return audios.sort((a, b) => (b.size || 0) - (a.size || 0))[0] || null;
}

/** What content.js and popup.js show for an item. */
function describe(item, tabId) {
  const parts = [];
  const paired = pairedAudio(tabId, item);
  if (item.audioOnly || isAudioFile(item)) parts.push("Audio only");
  if (item.quality) parts.push(item.quality);
  if (item.codec) parts.push(item.codec);
  parts.push(formatName(item));
  if (item.size) parts.push(formatBytes(item.size));
  else if (item.bandwidth && item.duration) parts.push("~" + formatBytes((item.bandwidth * item.duration) / 8));
  if (item.duration) parts.push(formatDuration(item.duration));
  if (item.audioUrl || item.withAudio || paired) parts.push("with audio");
  else if (item.noAudio) parts.push("no sound in this video");

  return {
    key: item.key,
    kind: item.kind,
    url: item.url,
    text: parts.join(" · "),
    type: panelType(item),
    size: item.size || null,
    disabled: !!item.encrypted,
    note: item.encrypted ? "DRM-protected stream, Klippo cannot download it" : "",
  };
}

function visibleItems(tabId) {
  // YouTube qualities first, then the newest video; the qualities of one video best first.
  const groups = new Map();
  // Files that are parts of an HLS stream are offered through the stream.
  // Live streams are not offered: Klippo does not record them.
  const parts = segmentFiles.get(tabId);
  for (const m of (media.get(tabId) || []).filter((m) => !m.hidden && !m.live && !(m.kind === "file" && parts && parts.has(withoutQuery(m.url))))) {
    const group = m.parent || m.key;
    if (!groups.has(group)) groups.set(group, []);
    groups.get(group).push(m);
  }
  const found = [...groups.values()]
    .reverse()
    .flatMap((g) => {
      const audio = g.filter((m) => m.audioOnly).sort((a, b) => (b.audioRank || 0) - (a.audioRank || 0));
      const videos = g.filter((m) => !m.audioOnly).sort((a, b) => (b.bandwidth || 0) - (a.bandwidth || 0));
      // A video's audio renditions differ only in bitrate: offer the best one.
      return videos.concat(videos.length ? audio.slice(0, 1) : audio);
    })
    .map((m) => describe(m, tabId));
  return youtubeItems(tabId).concat(found);
}

const notifyTimers = new Map();
function changed(tabId) {
  persist();
  clearTimeout(notifyTimers.get(tabId));
  notifyTimers.set(
    tabId,
    setTimeout(() => {
      notifyTimers.delete(tabId);
      refreshBadge(tabId);
      const items = visibleItems(tabId);
      Promise.resolve(api.tabs.sendMessage(tabId, { type: "klippo-media", items })).catch(() => {});
      Promise.resolve(api.runtime.sendMessage({ type: "klippo-popup-media", tabId, items })).catch(() => {});
    }, 200)
  );
}

function clearTab(tabId) {
  const yt = youtube.get(tabId);
  if (yt) {
    // content.js sends the new video's stream list after the page changes.
    yt.info = null;
    yt.templates = yt.templates.filter((t) => Date.now() - t.time < KEEP_ON_NAVIGATION_MS);
  }
  const list = media.get(tabId);
  if (list) {
    const keep = list.filter((m) => Date.now() - m.time < KEEP_ON_NAVIGATION_MS);
    if (keep.length) media.set(tabId, keep);
    else media.delete(tabId);
  }
  if (yt || list) changed(tabId);
}

// A file that a page fetches itself (fetch / XMLHttpRequest) and then saves
// without a normal browser download never reaches downloads.onDeterminingFilename,
// so Klippo would miss it; file hosts such as gofile deliver files this way.
// When such a response says it is an attachment, hand its address to Klippo,
// which gets its own copy. This watches headers only; it cannot stop the page.
const attachmentOffered = new Set(); // request ids already sent to Klippo

/** The file name in a Content-Disposition header (RFC 5987 `filename*` first). */
function dispositionName(value) {
  const star = /filename\*\s*=\s*[^']*''([^;]+)/i.exec(value);
  if (star) {
    try {
      return decodeURIComponent(star[1].trim().replace(/^"|"$/g, ""));
    } catch (e) {}
  }
  const plain = /filename\s*=\s*"([^"]*)"/i.exec(value) || /filename\s*=\s*([^;]+)/i.exec(value);
  return plain ? plain[1].trim() : "";
}

async function isIncognitoTab(tabId) {
  if (tabId < 0) return false;
  try {
    return !!(await api.tabs.get(tabId)).incognito;
  } catch (e) {
    return false;
  }
}

async function maybeCaptureAttachment(details) {
  if (details.type !== "xmlhttprequest" || details.method !== "GET" || details.tabId < 0) return;
  if (details.statusCode !== 200 && details.statusCode !== 206) return;
  if (!/^https?:/i.test(details.url) || attachmentOffered.has(details.requestId)) return;
  const headers = details.responseHeaders || [];
  const header = (name) => headers.find((h) => h.name.toLowerCase() === name);
  const cd = header("content-disposition");
  if (!cd || !/attachment/i.test(cd.value || "")) return; // only an explicit "save me" response
  const ct = header("content-type");
  const mime = ct ? (ct.value || "").split(";")[0].trim().toLowerCase() : "";
  // Web apps (Google's search suggestions, many JSON APIs) answer an XHR with
  // data and an "attachment" disposition for security. That is page data, not a
  // file the user wants, so take only real downloadable types.
  if (!mime || /json|javascript|ecmascript|^text\/(html|css|xml)$|^application\/(xml|x-www-form-urlencoded)$/.test(mime)) return;
  attachmentOffered.add(details.requestId);
  if (attachmentOffered.size > 300) attachmentOffered.delete(attachmentOffered.values().next().value);
  if (!(await isEnabled())) return;
  // Off by default: a web app such as webmail fetches files it does not mean to
  // save, so take page-fetched files only when the user turned this option on.
  await loadConfig(false);
  if (!hostConfig.captureFetched) return;
  const cl = header("content-length");
  let name = dispositionName(cd.value || "");
  if (!name) {
    try {
      name = baseName(new URL(details.url).pathname);
    } catch (e) {}
  }
  try {
    await sendToHost({
      action: "download",
      force: false,
      browser_proxy: (await browserProxy()) || null,
      url: details.url,
      referer: details.documentUrl || details.originUrl || details.initiator || "",
      cookies: await cookieHeader(details.url, await isIncognitoTab(details.tabId)),
      user_agent: navigator.userAgent,
      filename: baseName(name) || "download",
      file_size: cl ? Number(cl.value) || null : null,
      mime,
      post_data: null,
      post_type: null,
    });
  } catch (e) {
    // The native host is not running; the page keeps its own copy.
  }
}

api.webRequest.onHeadersReceived.addListener(
  (details) => {
    const item = classify(details);
    if (item) addMedia(details.tabId, item);
    maybeCaptureAttachment(details);
  },
  { urls: ["http://*/*", "https://*/*"] },
  ["responseHeaders"]
);

const tabUrls = new Map();
api.tabs.onUpdated.addListener((tabId, info) => {
  if (!info.url) return;
  const page = info.url.split("#")[0];
  const before = tabUrls.get(tabId);
  tabUrls.set(tabId, page);
  if (before !== page) restored.then(() => clearTab(tabId)); // not for "#anchor" changes
});

api.tabs.onRemoved.addListener((tabId) => {
  tabUrls.delete(tabId);
  segmentFiles.delete(tabId);
  audioPlaylists.delete(tabId);
  audioDetails.delete(tabId);
  restored.then(() => {
    media.delete(tabId);
    youtube.delete(tabId);
    persist();
  });
});

// ---------------------------------------------------------------------------
// YouTube
//
// YouTube's web player streams through SABR: it POSTs a protobuf request to a
// googlevideo.com "videoplayback?sabr=1" address. The address already has the
// player's "n" value and the body has the stream configuration and the
// proof-of-origin token, so Klippo can send the same request again and fetch
// every segment. content.js (through youtube_main.js) sends the stream list.
// ---------------------------------------------------------------------------

const MAX_TEMPLATES = 4;
// Multichannel and Dolby audio formats: not every player can play them.
const SURROUND_AUDIO = new Set([256, 258, 327, 328, 380]);

function ytState(tabId) {
  let yt = youtube.get(tabId);
  if (!yt) {
    yt = { info: null, templates: [], ad: false };
    youtube.set(tabId, yt);
  }
  return yt;
}

/** Base64 of an ArrayBuffer or a byte array (forms and YouTube request bodies). */
function toBase64(buffer) {
  const bytes = new Uint8Array(buffer);
  let text = "";
  for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(text);
}

api.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (details.tabId < 0 || details.method !== "POST" || !/[?&]sabr=1(&|$)/.test(details.url)) return;
    const raw = details.requestBody && details.requestBody.raw;
    if (!raw || !raw.length || !raw[0].bytes) return;
    const body = toBase64(raw[0].bytes);
    restored.then(() => {
      const yt = ytState(details.tabId);
      const first = !yt.templates.length;
      yt.templates.push({ url: details.url, body, time: Date.now(), ad: yt.ad });
      while (yt.templates.length > MAX_TEMPLATES) yt.templates.shift();
      persist();
      if (first) changed(details.tabId); // the qualities become available
    });
  },
  { urls: ["*://*.googlevideo.com/videoplayback*"] },
  ["requestBody"]
);

/** The newest stream session that was not recorded during an ad. */
function latestTemplate(yt) {
  const real = yt.templates.filter((t) => !t.ad);
  return real[real.length - 1] || null;
}

function codecOf(mime) {
  const m = /codecs="([^".]+)/.exec(mime || "");
  return m ? m[1] : "";
}

function pickAudio(formats) {
  const m4a = formats.filter((f) => f.mime.startsWith("audio/mp4") && codecOf(f.mime) === "mp4a" && f.audioDefault && !SURROUND_AUDIO.has(f.itag));
  return (
    m4a.find((f) => f.itag === 140 && !f.xtags) ||
    m4a.find((f) => f.itag === 140) ||
    m4a.sort((a, b) => b.bitrate - a.bitrate)[0] ||
    null
  );
}

/** Opus audio for WebM video (itag 251 when YouTube has it). */
function pickOpus(formats) {
  const opus = formats.filter((f) => f.mime.startsWith("audio/webm") && codecOf(f.mime) === "opus" && f.audioDefault && !SURROUND_AUDIO.has(f.itag));
  return opus.find((f) => f.itag === 251 && !f.xtags) || opus.find((f) => f.itag === 251) || opus.sort((a, b) => b.bitrate - a.bitrate)[0] || null;
}

const isWebm = (f) => !!f && f.mime.startsWith("video/webm");

/** The audio that joins `video`: AAC in MP4, Opus with WebM video. */
function audioFor(formats, video) {
  return isWebm(video) ? pickOpus(formats) : pickAudio(formats);
}

/** One video stream per quality: H.264 (MP4) when there is one, else AV1 (MP4), else VP9 (WebM). */
function pickVideos(formats) {
  const rank = (f) => {
    const codec = codecOf(f.mime);
    if (f.mime.startsWith("video/mp4")) return { avc1: 3, av01: 2 }[codec] || 0;
    return isWebm(f) && (codec === "vp09" || codec === "vp9") && pickOpus(formats) ? 1 : 0;
  };
  const byQuality = new Map();
  for (const f of formats) {
    if (!rank(f)) continue;
    const label = f.quality || `${f.height}p`;
    const have = byQuality.get(label);
    if (!have || rank(have) < rank(f)) byQuality.set(label, f);
  }
  return [...byQuality.values()].sort((a, b) => b.height - a.height || b.fps - a.fps);
}

function videoCodecLabel(f) {
  return { avc1: "H.264", av01: "AV1", vp09: "VP9", vp9: "VP9" }[codecOf(f.mime)] || "";
}

function streamSize(f, seconds) {
  if (!f) return 0;
  return f.size || Math.round(((f.bitrate || 0) * seconds) / 8);
}

function youtubeItems(tabId) {
  const yt = youtube.get(tabId);
  if (!yt || !yt.info || yt.info.isLive) return []; // no live recording
  const info = yt.info;
  if (info.drm) {
    // Paid or rented films: the streams are DRM-encrypted. Show one disabled
    // entry so the panel says why, like an encrypted HLS or DASH stream.
    return [{
      key: `yt:${info.videoId}:drm`,
      kind: "youtube",
      url: `https://www.youtube.com/watch?v=${info.videoId}`,
      text: ["Video", "MP4", "DRM-protected"].join(" · "),
      type: "mp4",
      size: null,
      disabled: true,
      note: "DRM-protected video, Klippo cannot download it",
    }];
  }
  const audio = pickAudio(info.formats);
  if (!audio) return [];
  const note = latestTemplate(yt) ? "" : "Play the video for a moment, then choose again";
  const url = `https://www.youtube.com/watch?v=${info.videoId}`;
  const items = pickVideos(info.formats).map((v) => ({
    key: `yt:${info.videoId}:${v.itag}`,
    kind: "youtube",
    url,
    text: [v.quality || `${v.height}p`, isWebm(v) ? "WebM" : "MP4", videoCodecLabel(v), formatBytes(streamSize(v, info.lengthSeconds) + streamSize(audioFor(info.formats, v), info.lengthSeconds))]
      .filter(Boolean)
      .join(" · "),
    type: isWebm(v) ? "webm" : "mp4",
    size: streamSize(v, info.lengthSeconds) + streamSize(audioFor(info.formats, v), info.lengthSeconds) || null,
    disabled: !!note,
    note,
  }));
  items.push({
    key: `yt:${info.videoId}:audio`,
    kind: "youtube",
    url,
    text: ["Audio only", "M4A", formatBytes(streamSize(audio, info.lengthSeconds))].filter(Boolean).join(" · "),
    type: "m4a",
    size: streamSize(audio, info.lengthSeconds) || null,
    disabled: !!note,
    note,
  });
  return items;
}

function ytFormat(f) {
  return { itag: f.itag, lmt: f.lmt, xtags: f.xtags || "", size: f.size || null, mime: f.mime.split(";")[0] };
}

async function downloadYoutube(tabId, key) {
  await restored;
  const [, videoId, which] = key.split(":");
  const yt = youtube.get(tabId);
  if (!yt || !yt.info || yt.info.videoId !== videoId) return { ok: false, error: "This video is no longer open in the tab." };
  if (yt.info.isLive) return { ok: false, error: "This is a live stream. Klippo does not record live streams." };
  if (yt.info.drm) return { ok: false, error: "This video is DRM-protected. Klippo cannot download it." };
  const tpl = latestTemplate(yt);
  if (!tpl) return { ok: false, error: "Play the video for a moment, then choose again." };
  const video = which === "audio" ? null : pickVideos(yt.info.formats).find((v) => String(v.itag) === which);
  const audio = audioFor(yt.info.formats, video);
  if (!audio || (which !== "audio" && !video)) return { ok: false, error: "YouTube no longer lists this quality." };

  const url = `https://www.youtube.com/watch?v=${videoId}`;
  const spec = {
    video_id: videoId,
    sabr_url: tpl.url,
    template: tpl.body,
    video: video ? ytFormat(video) : null,
    audio: ytFormat(audio),
    duration_ms: yt.info.lengthSeconds * 1000,
  };
  const name = cleanName(yt.info.title) || videoId;
  let resp;
  try {
    resp = await sendToHost({
      action: "download",
      force: true,
      url,
      referer: url,
      user_agent: navigator.userAgent,
      filename: `${name}.${!video ? "m4a" : isWebm(video) ? "webm" : "mp4"}`,
      media_type: "youtube",
      youtube: spec,
    });
  } catch (e) {
    return { ok: false, error: "Klippo is not reachable. Run \"klippo.exe install-host\" and reload the extension." };
  }
  if (resp && resp.status === "ok") return { ok: true };
  return { ok: false, error: (resp && resp.message) || "Klippo did not take the download." };
}

/** <video src="https://..."> seen on the page (service-worker requests have no tab). */
async function addElementMedia(tabId, msg) {
  const url = mediaKey(msg.url);
  const ext = extOf(url);
  const known = (media.get(tabId) || []).find((m) => m.key === url);
  const quality = msg.height ? `${msg.height}p` : "";
  if (known) {
    if (quality && !known.quality && known.kind === "file") {
      known.quality = quality;
      changed(tabId);
    }
    return;
  }
  const hls = ext === "m3u8";
  await addMedia(tabId, {
    key: url,
    url,
    kind: hls ? "hls" : "file",
    mime: hls ? "application/vnd.apple.mpegurl" : MEDIA_EXTS[ext] || (msg.audio ? "audio/mpeg" : "video/mp4"),
    size: null,
    quality: hls ? "" : quality,
    referer: msg.pageUrl || "",
    name: "",
    time: Date.now(),
  });
}

function cleanName(name) {
  return (name || "")
    .replace(/[<>:"/\\|?*\x00-\x1f]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/[. ]+$/, "")
    .slice(0, 150);
}

function mediaFileName(item, title) {
  if (item.name) return cleanName(item.name);
  let base = cleanName(title);
  if (!base) {
    try {
      base = cleanName(decodeURIComponent(baseName(new URL(item.url).pathname)).replace(/\.[^.]*$/, ""));
    } catch (e) {}
  }
  base = base || "video";
  const siblings = (media.get(item.tabId) || []).filter((m) => m.parent && m.parent === item.parent);
  if (item.quality && siblings.length > 1) base += ` (${item.quality})`;
  if (item.kind === "hls") return item.fmp4 ? `${base}.mp4` : base; // Klippo picks .mp4 or .ts by content
  if (item.kind === "dash") return `${base}.mp4`;
  const ext = MEDIA_EXTS[extOf(item.url)] ? extOf(item.url) : MIME_EXTS[item.mime] || "mp4";
  return `${base}.${ext}`;
}

// A double click (or two frames answering one click) must not add the file twice.
const recentSends = new Map();
const REPEAT_MS = 4000;

async function downloadMedia(tabId, key, frameUrl) {
  const id = `${tabId}|${key}`;
  if (Date.now() - (recentSends.get(id) || 0) < REPEAT_MS) return { ok: true };
  recentSends.set(id, Date.now());
  if (typeof key === "string" && key.startsWith("yt:")) return downloadYoutube(tabId, key);
  await restored;
  const item = (media.get(tabId) || []).find((m) => m.key === key);
  if (!item) return { ok: false, error: "This video is no longer in the list. Play it again." };
  if (item.encrypted) return { ok: false, error: "This stream is DRM-protected. Klippo cannot download it." };
  let tab = null;
  try {
    tab = await api.tabs.get(tabId);
  } catch (e) {}
  const filename = mediaFileName({ ...item, tabId }, tab && tab.title);
  let resp;
  try {
    resp = await sendToHost({
      action: "download",
      force: true,
      url: item.url,
      referer: item.referer || frameUrl || (tab && tab.url) || "",
      cookies: await cookieHeader(item.url),
      user_agent: navigator.userAgent,
      filename,
      file_size: item.size || null,
      mime: item.mime || "",
      media_type: item.kind === "hls" || item.kind === "dash" ? "hls" : "",
      audio_url: item.audioUrl || (pairedAudio(tabId, item) || {}).url || "",
    });
  } catch (e) {
    return { ok: false, error: "Klippo is not reachable. Run \"klippo.exe install-host\" and reload the extension." };
  }
  if (resp && resp.status === "ok") return { ok: true };
  return { ok: false, error: (resp && resp.message) || "Klippo did not take the download." };
}

// ---------------------------------------------------------------------------
// Messages from content.js and popup.js
// ---------------------------------------------------------------------------

async function handleMessage(msg, sender) {
  const tabId = msg.tabId ?? (sender.tab && sender.tab.id);
  switch (msg.type) {
    case "get-media":
      await restored;
      return { items: visibleItems(tabId) };
    case "get-panel-config":
      await loadConfig(false);
      return { panel: hostConfig.panel };
    case "download-links": {
      // The panel for selected links: the links go to Klippo's batch download list.
      const urls = (Array.isArray(msg.urls) ? msg.urls : []).filter((u) => /^(https?|ftp):/i.test(u));
      if (!urls.length) return { ok: false, error: "No links" };
      try {
        const r = await sendToHost({ action: "batch", urls });
        return r && r.status === "ok" ? { ok: true } : { ok: false, error: (r && r.message) || "Klippo did not take the links" };
      } catch (e) {
        return { ok: false, error: "Klippo is not reachable" };
      }
    }
    case "element-media":
      if (sender.tab && /^https?:/i.test(msg.url || "")) await addElementMedia(sender.tab.id, msg);
      return { ok: true };
    case "youtube-info": {
      const info = msg.info;
      if (!sender.tab || !info || typeof info.videoId !== "string" || !Array.isArray(info.formats)) return { ok: false };
      await restored;
      const yt = ytState(sender.tab.id);
      const changedVideo = !yt.info || yt.info.videoId !== info.videoId;
      yt.info = info;
      persist();
      if (changedVideo) changed(sender.tab.id);
      return { ok: true };
    }
    case "youtube-ad":
      if (sender.tab) {
        await restored;
        ytState(sender.tab.id).ad = !!msg.ad;
      }
      return { ok: true };
    case "link-click":
      lastClick = { url: msg.url, alt: !!msg.alt, shift: !!msg.shift, ctrl: !!msg.ctrl, ins: !!msg.ins, time: Date.now() };
      return { ok: true };
    case "keys-state":
      keysNow = { alt: !!msg.alt, shift: !!msg.shift, ctrl: !!msg.ctrl, ins: !!msg.ins, time: Date.now() };
      return { ok: true };
    case "download-media":
      // content.js offers a <video src> before any request for it was seen.
      if (msg.url && sender.tab && /^https?:/i.test(msg.url)) {
        await addElementMedia(tabId, { url: msg.url, pageUrl: msg.frameUrl });
        return downloadMedia(tabId, mediaKey(msg.url), msg.frameUrl);
      }
      return downloadMedia(tabId, msg.key, msg.frameUrl);
    case "status": {
      let host;
      try {
        const resp = await sendToHost({ action: "ping" });
        host = { ok: resp && resp.status === "ok", message: resp ? resp.message : "" };
      } catch (e) {
        host = { ok: false, message: String((e && e.message) || e) };
      }
      return { enabled: await isEnabled(), host };
    }
    case "set-enabled":
      await api.storage.local.set({ enabled: !!msg.enabled });
      await refreshAllBadges();
      return { ok: true };
    default:
      return { ok: false, error: "unknown message" };
  }
}

api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!msg || typeof msg.type !== "string") return false;
  handleMessage(msg, sender).then(sendResponse, (e) => sendResponse({ ok: false, error: String((e && e.message) || e) }));
  return true;
});

// ---------------------------------------------------------------------------
// Toolbar badge and context menu
// ---------------------------------------------------------------------------

// The toolbar number counts the video/audio found on a page. It follows the
// same "Show video and audio" switch as the popup list: off by default, so the
// icon shows no count unless the user turns that switch on.
let showMediaBadge = false;
Promise.resolve(api.storage.local.get("showMedia"))
  .then((r) => {
    showMediaBadge = r.showMedia === true;
  })
  .catch(() => {});
api.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.showMedia) {
    showMediaBadge = changes.showMedia.newValue === true;
    refreshAllBadges();
  }
});

async function refreshBadge(tabId) {
  if (tabId === activeMenuTab || activeMenuTab === -1) {
    Promise.resolve(api.tabs.query({ active: true, lastFocusedWindow: true }))
      .then(([t]) => t && t.id === tabId && refreshVideoMenu(tabId))
      .catch(() => {});
  }
  const enabled = await isEnabled();
  const count = showMediaBadge ? visibleItems(tabId).filter((i) => !i.disabled).length : 0;
  try {
    await actionApi.setBadgeText({ tabId, text: count ? String(count) : enabled ? "" : "OFF" });
  } catch (e) {} // the tab is gone
}

async function refreshAllBadges() {
  const enabled = await isEnabled();
  actionApi.setBadgeText({ text: enabled ? "" : "OFF" });
  actionApi.setTitle({
    title: enabled ? "Klippo: downloads and videos on this page" : "Klippo: download capture is off",
  });
  await restored;
  for (const tabId of media.keys()) refreshBadge(tabId);
}

actionApi.setBadgeBackgroundColor({ color: "#2e7d32" });

const VIDEO_SLOTS = 10;

/** The browser menu items, as chosen in Klippo's Options. */
let menuBuilding = null;
function buildMenus(force) {
  const sig = JSON.stringify(hostConfig.menu);
  // Build one at a time and only after the restored signature is known, so a
  // wake never tears the menu down while another build is mid-flight, and an
  // unchanged menu is left in place instead of removed and made again.
  const run = Promise.resolve(menuBuilding)
    .catch(() => {})
    .then(() => restored)
    .then(() => {
      if (!force && sig === menuSignature) return; // the right menu is already there
      return Promise.resolve(api.contextMenus.removeAll()).then(() => {
        const m = hostConfig.menu;
        const pageContexts = ["page", "frame", "video", "audio"];
        if (m.download) {
          api.contextMenus.create({ id: "klippo-download", title: "Download with Klippo", contexts: ["link", "video", "audio", "image"] });
        }
        if (m.last_video) {
          api.contextMenus.create({ id: "klippo-last-video", title: "Download last requested video with Klippo", contexts: pageContexts });
        }
        if (m.videos) {
          api.contextMenus.create({ id: "klippo-videos", title: "Download videos with Klippo from 10 last requested", contexts: pageContexts });
          for (let i = 0; i < VIDEO_SLOTS; i++) {
            api.contextMenus.create({ id: `klippo-video-${i}`, parentId: "klippo-videos", title: "-", contexts: pageContexts, visible: false });
          }
        }
        if (m.all_links) {
          api.contextMenus.create({ id: "klippo-all-links", title: "Download all links with Klippo", contexts: ["page", "frame", "selection", "link"] });
        }
        activeMenuTab = -1;
        menuSignature = sig;
        if (sessionStore) sessionStore.set({ menuSig: sig }).catch(() => {});
      });
    })
    .catch(() => {});
  menuBuilding = run;
  return run;
}

/** Fills "10 last requested" with the streams of the tab in front. */
let activeMenuTab = -1;
function refreshVideoMenu(tabId) {
  if (!hostConfig.menu.videos) return;
  activeMenuTab = tabId;
  const items = visibleItems(tabId).filter((i) => !i.disabled).slice(0, VIDEO_SLOTS);
  for (let i = 0; i < VIDEO_SLOTS; i++) {
    const it = items[i];
    Promise.resolve(api.contextMenus.update(`klippo-video-${i}`, it ? { title: it.text, visible: true } : { visible: false })).catch(() => {});
  }
}

// Build on install, update and browser start — not on every service-worker
// wake. Chromium keeps the menu items across worker restarts, so rebuilding
// each wake only risks the removeAll/create race that dropped the menu.
api.runtime.onInstalled.addListener(() => buildMenus(true));
if (api.runtime.onStartup) api.runtime.onStartup.addListener(() => buildMenus(true));
loadConfig(true);

api.tabs.onActivated.addListener(({ tabId }) => refreshVideoMenu(tabId));

async function downloadAllLinks(tabId) {
  let links = [];
  try {
    links = (await api.tabs.sendMessage(tabId, { type: "klippo-collect-links" })) || [];
  } catch (e) {}
  if (!links.length) return;
  try {
    await sendToHost({ action: "batch", urls: links });
  } catch (e) {
    console.warn("Klippo native host is not available.", e);
  }
}

api.contextMenus.onClicked.addListener(async (info, tab) => {
  const tabId = tab && tab.id;
  if (info.menuItemId === "klippo-all-links") return downloadAllLinks(tabId);
  if (info.menuItemId === "klippo-last-video" || String(info.menuItemId).startsWith("klippo-video-")) {
    await restored;
    const items = visibleItems(tabId).filter((i) => !i.disabled);
    const n = info.menuItemId === "klippo-last-video" ? 0 : Number(String(info.menuItemId).slice("klippo-video-".length));
    const item = items[n];
    if (item) {
      if (String(item.key).startsWith("yt:")) await downloadYoutube(tabId, item.key);
      else await downloadMedia(tabId, item.key, info.frameUrl || info.pageUrl);
    }
    return;
  }
  const url = info.linkUrl || info.srcUrl;
  if (!url || !/^https?:/i.test(url)) return;
  try {
    await sendToHost({
      action: "download",
      force: true,
      url,
      referer: info.frameUrl || info.pageUrl || (tab && tab.url) || "",
      cookies: await cookieHeader(url),
      user_agent: navigator.userAgent,
      media_type: extOf(url) === "m3u8" ? "hls" : "",
    });
  } catch (e) {
    console.warn("Klippo native host is not available.", e);
  }
});

refreshAllBadges();
