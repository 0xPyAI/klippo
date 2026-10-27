// Klippo "Download this video" button over playing videos, like a classic download manager's video
// panel. background.js finds the streams the page loads and sends the list;
// this script only draws the button and the stream menu.
(() => {
  if (window.__klippoPanel) return;
  window.__klippoPanel = true;

  const api = globalThis.browser ?? globalThis.chrome;
  const MIN_WIDTH = 200;
  const MIN_HEIGHT = 110;
  const SHOW_MS = 5000; // after a video starts or a new stream is found
  const LINGER_MS = 1500; // after the pointer leaves the video

  let items = [];
  let target = null; // the <video> the button belongs to
  let shownUntil = 0;
  let lastHover = 0;
  let pointer = null;
  let menuOpen = false;
  let statusUntil = 0;
  let frame = 0;
  const dismissed = new WeakSet();
  const reported = new WeakMap();
  const videos = document.getElementsByTagName("video");

  function alive() {
    try {
      return !!api.runtime.id;
    } catch (e) {
      return false;
    }
  }

  function send(message) {
    if (!alive()) return Promise.resolve(null);
    try {
      return Promise.resolve(api.runtime.sendMessage(message)).catch(() => null);
    } catch (e) {
      return Promise.resolve(null);
    }
  }

  // ---- panel (closed shadow root, built without innerHTML for Trusted Types pages) ----

  const CSS = `
    :host { all: initial; }
    .wrap { position: relative; font: 12px "Segoe UI", system-ui, sans-serif; color: #12376b; }
    .bar { display: flex; align-items: stretch; border: 1px solid #7a9cc6; border-radius: 3px; overflow: hidden;
           background: linear-gradient(#ffffff, #e3ebf6); box-shadow: 0 2px 6px rgba(0,0,0,.35); }
    button { all: unset; cursor: pointer; display: flex; align-items: center; gap: 6px; padding: 4px 8px; white-space: nowrap; }
    button:hover { background: rgba(122,156,198,.25); }
    .main { font-weight: 600; }
    .close { border-left: 1px solid #b7c9e0; padding: 4px 7px; font-size: 13px; color: #44597a; }
    .arrow { font-size: 10px; }
    .ok { color: #1f7a33; }
    .err { color: #b3261e; }
    /* Mini mode: the arrow icon only. */
    .mini .main { padding: 3px 4px; gap: 3px; }
    .mini .close { display: none; }
    /* The list looks like a Windows pop-up menu. */
    .menu { position: absolute; right: 0; top: calc(100% + 2px); min-width: 250px; max-width: 440px; max-height: 360px;
            overflow: auto; padding: 3px 0; background: #f9f9f9; color: #000; border: 1px solid #c9c9c9;
            box-shadow: 2px 3px 8px rgba(0,0,0,.3); font: 12px "Segoe UI", system-ui, sans-serif; }
    .menu[hidden] { display: none; }
    .row { display: block; width: 100%; box-sizing: border-box; padding: 4px 24px 4px 28px; line-height: 16px; }
    .row:hover { background: #cce8ff; }
    .row .note { display: block; color: #8a5a00; font-size: 11px; white-space: normal; }
    .row[aria-disabled="true"] { cursor: default; color: #8c8c8c; }
    .row[aria-disabled="true"]:hover { background: none; }
    @media (prefers-color-scheme: dark) {
      .menu { background: #2b2b2b; color: #fff; border-color: #454545; }
      .row:hover { background: #414141; }
      .row[aria-disabled="true"] { color: #8c8c8c; }
      .row .note { color: #e0b060; }
    }
  `;

  function el(tag, cls, text) {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text) node.textContent = text;
    return node;
  }

  function arrowIcon() {
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    svg.setAttribute("width", "14");
    svg.setAttribute("height", "14");
    svg.setAttribute("viewBox", "0 0 16 16");
    const rect = document.createElementNS(ns, "rect");
    for (const [k, v] of Object.entries({ x: "0", y: "0", width: "16", height: "16", rx: "3", fill: "#2e9b47" })) rect.setAttribute(k, v);
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", "M6.5 3h3v5h2.5L8 12.5 4 8h2.5z");
    path.setAttribute("fill", "#fff");
    svg.append(rect, path);
    return svg;
  }

  const host = document.createElement("klippo-video-panel");
  host.style.setProperty("all", "initial", "important");
  host.style.setProperty("position", "fixed", "important");
  host.style.setProperty("z-index", "2147483647", "important");
  host.style.setProperty("top", "0", "important");
  host.style.setProperty("left", "0", "important");
  host.style.setProperty("display", "none", "important");
  const root = host.attachShadow({ mode: "closed" });
  function style(shadow) {
    try {
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(CSS);
      shadow.adoptedStyleSheets = [sheet];
    } catch (e) {
      shadow.append(el("style", "", CSS));
    }
  }
  style(root);
  const wrap = el("div", "wrap");
  const bar = el("div", "bar");
  const main = el("button", "main");
  const label = el("span", "", "Download this video");
  const arrow = el("span", "arrow", "▼");
  main.append(arrowIcon(), label, arrow);
  const close = el("button", "close", "×");
  close.title = "Hide for this video";
  bar.append(main, close);
  const menu = el("div", "menu");
  menu.hidden = true;
  wrap.append(bar, menu);
  root.append(wrap);

  // The panel stays while the pointer rests on it.
  let overPanel = false;
  host.addEventListener("pointerenter", () => (overPanel = true));
  host.addEventListener("pointerleave", () => {
    overPanel = false;
    lastHover = Date.now();
    schedule();
  });
  for (const type of ["mousedown", "mouseup", "click", "dblclick", "pointerdown", "pointerup", "keydown"]) {
    // Keep clicks on the panel away from the player (pause, fullscreen).
    host.addEventListener(type, (e) => e.stopPropagation());
  }

  function attach() {
    const fs = document.fullscreenElement;
    // Inside a fullscreen player (but never inside <video>, which shows no children).
    const parent = fs && fs.tagName !== "VIDEO" ? fs : document.documentElement;
    if (parent && host.parentNode !== parent) parent.append(host);
  }

  // ---- what to show ----

  function httpSrc(video) {
    const src = video && (video.currentSrc || video.src);
    return src && /^https?:/i.test(src) ? src : "";
  }

  function bigEnough(rect) {
    return rect.width >= MIN_WIDTH && rect.height >= MIN_HEIGHT && rect.bottom > 0 && rect.top < innerHeight && rect.right > 0 && rect.left < innerWidth;
  }

  function videoAt(x, y) {
    let best = null;
    let bestArea = 0;
    for (const v of videos) {
      const r = v.getBoundingClientRect();
      if (!bigEnough(r) || x < r.left || x > r.right || y < r.top || y > r.bottom) continue;
      if (r.width * r.height > bestArea) {
        best = v;
        bestArea = r.width * r.height;
      }
    }
    return best;
  }

  function playingVideo() {
    let best = null;
    let bestArea = 0;
    for (const v of videos) {
      if (v.paused) continue;
      const r = v.getBoundingClientRect();
      if (bigEnough(r) && r.width * r.height > bestArea) {
        best = v;
        bestArea = r.width * r.height;
      }
    }
    return best;
  }

  /** Long ids in the video's poster address (X: amplify_video_thumb/<id>/img/...). */
  function posterIds(video) {
    const poster = video && video.getAttribute && video.getAttribute("poster");
    if (!poster) return [];
    try {
      return new URL(poster, location.href).pathname
        .split("/")
        .map((s) => s.replace(/\.\w+$/, ""))
        .filter((s) => s.length >= 10 && /\d/.test(s));
    } catch (e) {
      return [];
    }
  }

  // ---- Options > General > "Customize download panels in browsers" ----

  let panelCfg = null;

  function loadPanelConfig() {
    send({ type: "get-panel-config" }).then((resp) => {
      if (resp && resp.panel) {
        panelCfg = resp.panel;
        wrap.classList.toggle("mini", !!panelCfg.mini);
        linksWrap.classList.toggle("mini", !!panelCfg.links_mini);
        render();
        schedule();
      }
    });
  }

  /** Site names: separated by spaces, * is a wildcard; "site.com" also covers its subdomains. */
  function siteMatches(patterns) {
    const host = location.hostname.toLowerCase();
    return (patterns || []).some((p) => {
      p = String(p).trim().toLowerCase();
      if (!p) return false;
      if (!p.includes("*")) return host === p || host.endsWith("." + p) || location.href.toLowerCase().startsWith(p);
      const re = new RegExp("^" + p.split("*").map((s) => s.replace(/[.+?^${}()|[\]\\/]/g, "\\$&")).join(".*") + "$");
      return re.test(host) || re.test(location.href.toLowerCase());
    });
  }

  function typeOfUrl(url) {
    try {
      const m = /\.([a-z0-9]{1,5})$/i.exec(new URL(url).pathname);
      return m ? m[1].toLowerCase() : "";
    } catch (e) {
      return "";
    }
  }

  /** The checked file types, the minimum size and protected content. */
  function allowed(item) {
    const cfg = panelCfg;
    if (!cfg) return true;
    if (item.disabled && !cfg.protected) return false;
    if (item.type && Array.isArray(cfg.types) && !cfg.types.includes(item.type)) return false;
    if (item.size && cfg.min_size && item.size < cfg.min_size) return false;
    return true;
  }

  function choices() {
    const found = items.filter(allowed);
    if (found.length) {
      // A page with many videos (an X timeline): offer the streams of this one.
      const ids = posterIds(target);
      const mine = ids.length ? found.filter((i) => ids.some((id) => (i.url || "").includes(id))) : [];
      return mine.length ? mine : found;
    }
    if (items.length) return [];
    const src = httpSrc(target);
    const own = src ? [{ key: src, url: src, text: "This video", fromElement: true, type: typeOfUrl(src) }] : [];
    return own.filter(allowed);
  }

  function wanted(now) {
    if (panelCfg && siteMatches(panelCfg.exceptions)) return false;
    if (!target || !target.isConnected || dismissed.has(target) || !choices().length) return false;
    if (!bigEnough(target.getBoundingClientRect())) return false;
    return overPanel || menuOpen || now < shownUntil || now < statusUntil || now - lastHover < LINGER_MS;
  }

  function place() {
    const r = target.getBoundingClientRect();
    const w = bar.offsetWidth;
    const h = bar.offsetHeight;
    let top = r.top - h - 4; // The panel sits above the video's top right corner
    if (top < 2) top = r.top + 6;
    const left = Math.max(2, Math.min(r.right - w - 6, innerWidth - w - 2));
    host.style.setProperty("transform", `translate(${Math.round(left)}px, ${Math.round(top)}px)`, "important");
  }

  function tick() {
    frame = 0;
    const now = Date.now();
    if (pointer) {
      const v = videoAt(pointer.x, pointer.y);
      if (v) {
        if (v !== target && !menuOpen) setTarget(v);
        if (v === target) lastHover = now;
      }
    }
    if (!wanted(now)) {
      hide();
      return;
    }
    attach();
    if (host.style.getPropertyValue("display") === "none") {
      host.style.setProperty("display", "block", "important");
      render();
    }
    place();
    frame = requestAnimationFrame(tick);
  }

  function schedule() {
    if (!frame) frame = requestAnimationFrame(tick);
  }

  function hide() {
    host.style.setProperty("display", "none", "important");
    closeMenu();
  }

  function setTarget(video) {
    target = video;
    report(video);
    render();
  }

  function render() {
    const list = choices();
    arrow.style.display = list.length > 1 ? "" : "none";
    if (Date.now() >= statusUntil) {
      label.className = "";
      label.textContent = target && target.tagName === "AUDIO" ? "Download this audio" : "Download this video";
      main.title = list.length === 1 ? list[0].text : `${list.length} streams found`;
    }
    // Mini mode shows the text only for a status ("Sent to Klippo").
    label.style.display = panelCfg && panelCfg.mini && Date.now() >= statusUntil ? "none" : "";
    if (menuOpen) fillMenu();
  }

  function fillMenu() {
    menu.replaceChildren();
    menu.title = "Download with Klippo";
    for (const item of choices()) {
      const row = el("button", "row", item.text);
      row.title = item.url;
      if (item.disabled) {
        row.setAttribute("aria-disabled", "true");
        if (item.note) row.append(el("span", "note", item.note));
      } else {
        row.addEventListener("click", () => {
          closeMenu();
          download(item);
        });
      }
      menu.append(row);
    }
  }

  function openMenu() {
    menuOpen = true;
    fillMenu();
    menu.hidden = false;
  }

  function closeMenu() {
    menuOpen = false;
    menu.hidden = true;
  }

  function showStatus(text, cls, ms) {
    label.textContent = text;
    label.className = cls;
    statusUntil = Date.now() + ms;
    setTimeout(() => {
      render();
      schedule();
    }, ms + 50);
    schedule();
  }

  let lastSend = { key: "", time: 0 };

  async function download(item) {
    if (lastSend.key === item.key && Date.now() - lastSend.time < 4000) return;
    lastSend = { key: item.key, time: Date.now() };
    showStatus("Sending to Klippo…", "", 15000);
    const resp = await send({
      type: "download-media",
      key: item.key,
      url: item.fromElement ? item.url : undefined,
      frameUrl: location.href,
    });
    if (resp && resp.ok) showStatus("Sent to Klippo", "ok", 2500);
    else showStatus((resp && resp.error) || "Klippo is not reachable", "err", 5000);
  }

  main.addEventListener("click", () => {
    const list = choices().filter((i) => !i.disabled);
    if (list.length === 1 && choices().length === 1) download(list[0]);
    else if (menuOpen) closeMenu();
    else openMenu();
  });
  close.addEventListener("click", () => {
    if (target) dismissed.add(target);
    hide();
  });

  // ---- page events ----

  function report(video) {
    const src = httpSrc(video);
    if (!src || reported.get(video) === src) return;
    reported.set(video, src);
    send({
      type: "element-media",
      url: src,
      height: video.videoHeight || 0,
      audio: video.tagName === "AUDIO",
      pageUrl: location.href,
    });
  }

  document.addEventListener(
    "pointermove",
    (e) => {
      if (!videos.length && !target) return;
      pointer = { x: e.clientX, y: e.clientY };
      schedule();
    },
    { capture: true, passive: true }
  );
  document.documentElement.addEventListener("mouseleave", () => (pointer = null));

  for (const type of ["play", "loadedmetadata"]) {
    document.addEventListener(
      type,
      (e) => {
        const media = e.target;
        if (!(media instanceof HTMLMediaElement)) return;
        if (media.tagName === "AUDIO") {
          report(media);
          return;
        }
        if (type === "loadedmetadata") reported.delete(media); // new source or new size
        report(media);
        if (type === "play" && bigEnough(media.getBoundingClientRect())) {
          if (!menuOpen) setTarget(media);
          shownUntil = Date.now() + SHOW_MS;
          schedule();
        }
      },
      true
    );
  }
  document.addEventListener("fullscreenchange", schedule);
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key === "Escape" && menuOpen) closeMenu();
    },
    true
  );
  document.addEventListener("mousedown", (e) => {
    if (menuOpen && !e.composedPath().includes(host)) closeMenu();
  }, true);

  function setItems(next) {
    const grew = next.length > items.length;
    items = next;
    if (grew) {
      // A new stream was found: show the button on the playing video for a moment.
      const v = (target && !target.paused && target.isConnected && target) || playingVideo();
      if (v) {
        if (!menuOpen) setTarget(v);
        shownUntil = Date.now() + SHOW_MS;
      }
    }
    render();
    schedule();
  }

  // ---- Panel for selected links ----

  const linksHost = document.createElement("klippo-links-panel");
  for (const [k, v] of [["all", "initial"], ["position", "fixed"], ["z-index", "2147483647"], ["top", "0"], ["left", "0"], ["display", "none"]]) {
    linksHost.style.setProperty(k, v, "important");
  }
  const linksRoot = linksHost.attachShadow({ mode: "closed" });
  style(linksRoot);
  const linksWrap = el("div", "wrap");
  const linksBar = el("div", "bar");
  const linksMain = el("button", "main");
  const linksLabel = el("span", "", "Download with Klippo");
  linksMain.append(arrowIcon(), linksLabel);
  const linksClose = el("button", "close", "×");
  linksClose.title = "Hide";
  linksBar.append(linksMain, linksClose);
  linksWrap.append(linksBar);
  linksRoot.append(linksWrap);
  for (const type of ["mousedown", "mouseup", "click", "pointerdown", "pointerup"]) {
    linksHost.addEventListener(type, (e) => e.stopPropagation());
  }
  let selectedLinks = [];
  let linksTimer = 0;
  let linksStatusUntil = 0;

  function linksWanted() {
    const cfg = panelCfg;
    if (!cfg) return true; // Default: any selected links
    if (cfg.links === "never") return false;
    if (cfg.links === "sites") return siteMatches(cfg.links_sites);
    return true;
  }

  /** Links in the selection: <a> elements and addresses written in the text. */
  function linksInSelection() {
    const sel = window.getSelection && window.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) return [];
    const range = sel.getRangeAt(0);
    let top = range.commonAncestorContainer;
    if (top.nodeType !== 1) top = top.parentElement;
    if (!top) return [];
    const seen = new Set();
    const add = (href) => {
      if (/^(https?|ftp):/i.test(href)) seen.add(href.split("#")[0]);
    };
    const outer = top.closest && top.closest("a[href]");
    if (outer) add(outer.href);
    for (const a of top.querySelectorAll("a[href]")) {
      if (sel.containsNode(a, true)) add(a.href);
    }
    for (const m of sel.toString().matchAll(/\b(?:https?|ftp):\/\/[^\s<>"']+/gi)) add(m[0].replace(/[).,;:!?]+$/, ""));
    return [...seen];
  }

  function hideLinks() {
    linksHost.style.setProperty("display", "none", "important");
  }

  function showLinks() {
    const sel = window.getSelection();
    if (!sel || !sel.rangeCount) return hideLinks();
    const r = sel.getRangeAt(0).getBoundingClientRect();
    if (!r.width && !r.height) return hideLinks();
    const n = selectedLinks.length;
    if (Date.now() >= linksStatusUntil) {
      linksLabel.className = "";
      linksLabel.textContent = n === 1 ? "Download link with Klippo" : `Download ${n} links with Klippo`;
    }
    linksMain.title = selectedLinks.slice(0, 10).join("\n") + (n > 10 ? `\n… ${n - 10} more` : "");
    linksLabel.style.display = panelCfg && panelCfg.links_mini && Date.now() >= linksStatusUntil ? "none" : "";
    const fs = document.fullscreenElement;
    const parent = fs && fs.tagName !== "VIDEO" ? fs : document.documentElement;
    if (linksHost.parentNode !== parent) parent.append(linksHost);
    linksHost.style.setProperty("display", "block", "important");
    const w = linksBar.offsetWidth;
    const h = linksBar.offsetHeight;
    let top = r.bottom + 6;
    if (top + h > innerHeight - 2) top = r.top - h - 6;
    const left = Math.max(2, Math.min(r.right - w, innerWidth - w - 2));
    linksHost.style.setProperty("transform", `translate(${Math.round(left)}px, ${Math.round(Math.max(2, top))}px)`, "important");
  }

  function checkSelection() {
    clearTimeout(linksTimer);
    linksTimer = setTimeout(() => {
      const active = document.activeElement;
      if (active && (active.tagName === "INPUT" || active.tagName === "TEXTAREA" || active.isContentEditable)) return hideLinks();
      selectedLinks = linksWanted() ? linksInSelection() : [];
      if (selectedLinks.length) showLinks();
      else if (Date.now() >= linksStatusUntil) hideLinks();
    }, 250);
  }

  linksMain.addEventListener("click", async () => {
    if (!selectedLinks.length) return;
    linksStatusUntil = Date.now() + 15000;
    linksLabel.style.display = "";
    linksLabel.textContent = "Sending to Klippo…";
    const resp = await send({ type: "download-links", urls: selectedLinks });
    linksLabel.className = resp && resp.ok ? "ok" : "err";
    linksLabel.textContent = resp && resp.ok ? "Sent to Klippo" : (resp && resp.error) || "Klippo is not reachable";
    linksStatusUntil = Date.now() + 2500;
    setTimeout(() => {
      linksStatusUntil = 0;
      hideLinks();
    }, 2500);
  });
  linksClose.addEventListener("click", hideLinks);
  document.addEventListener("mouseup", (e) => {
    if (!e.composedPath().includes(linksHost)) checkSelection();
  }, true);
  document.addEventListener("keyup", (e) => {
    if (e.key === "Escape") hideLinks();
    else if (e.shiftKey || e.key === "Shift" || ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a")) checkSelection();
  }, true);
  document.addEventListener("mousedown", (e) => {
    if (!e.composedPath().includes(linksHost) && Date.now() >= linksStatusUntil) hideLinks();
  }, true);
  window.addEventListener("scroll", () => {
    if (linksHost.style.getPropertyValue("display") !== "none" && selectedLinks.length) showLinks();
  }, { passive: true, capture: true });

  loadPanelConfig();
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") loadPanelConfig();
  });

  api.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (msg && msg.type === "klippo-media" && Array.isArray(msg.items)) setItems(msg.items);
    if (msg && msg.type === "klippo-collect-links" && window === window.top) {
      // "Download all links": the selected links, or every link of the page.
      const sel = window.getSelection && window.getSelection();
      let anchors = [...document.querySelectorAll("a[href]")];
      if (sel && !sel.isCollapsed) anchors = anchors.filter((a) => sel.containsNode(a, true));
      const seen = new Set();
      for (const a of anchors) {
        if (/^(https?|ftp):/i.test(a.href)) seen.add(a.href.split("#")[0]);
      }
      sendResponse([...seen]);
    }
  });

  // "Using special keys": report the keys held when a link is clicked,
  // and the keys held now (for pages that start the download later).
  let insDown = false;
  const keyState = (e) => ({ alt: e.altKey, shift: e.shiftKey, ctrl: e.ctrlKey, ins: insDown });
  document.addEventListener(
    "mousedown",
    (e) => {
      if (e.button !== 0) return;
      const a = e.target && e.target.closest && e.target.closest("a[href]");
      if (a && /^(https?|ftp):/i.test(a.href)) send({ type: "link-click", url: a.href, ...keyState(e) });
    },
    true
  );
  const keyWatch = (e) => {
    if (e.key === "Insert") insDown = e.type === "keydown";
    if (["Alt", "Shift", "Control", "Insert"].includes(e.key)) send({ type: "keys-state", ...keyState(e) });
  };
  document.addEventListener("keydown", keyWatch, true);
  document.addEventListener("keyup", keyWatch, true);
  // Media that loaded before this script ran (document_idle, restored tabs).
  for (const media of document.querySelectorAll("video, audio")) {
    if (media.readyState >= 1) report(media);
  }

  // ---- YouTube: youtube_main.js reads the player's stream list in the page ----

  if (/(^|\.)youtube(-nocookie)?\.com$/i.test(location.hostname)) {
    let infoTimer = 0;
    const askInfo = (ms) => {
      clearTimeout(infoTimer);
      infoTimer = setTimeout(() => window.postMessage({ klippo: "youtube-request" }, location.origin), ms);
    };
    window.addEventListener("message", (e) => {
      const d = e.source === window && e.data;
      if (!d || d.klippo !== "youtube") return;
      if (d.kind === "info") {
        if (d.info) send({ type: "youtube-info", info: d.info });
        else askInfo(1500); // the player has not loaded the new video yet
        send({ type: "youtube-ad", ad: !!d.ad });
      } else if (d.kind === "changed") {
        askInfo(500);
      } else if (d.kind === "ad") {
        send({ type: "youtube-ad", ad: !!d.ad });
      }
    });
    document.addEventListener("play", () => askInfo(300), true);
    try {
      // Firefox (Manifest V2) has no MAIN-world content scripts: add the script to the page.
      if (api.runtime.getManifest().manifest_version === 2) {
        const s = document.createElement("script");
        s.src = api.runtime.getURL("youtube_main.js");
        s.onload = () => s.remove();
        (document.head || document.documentElement).append(s);
      }
    } catch (e) {}
    askInfo(800);
  }
  send({ type: "get-media" }).then((resp) => {
    if (resp && Array.isArray(resp.items)) setItems(resp.items);
  });
})();
