// Toolbar popup: capture switch and the streams found on the current tab.
const api = globalThis.browser ?? globalThis.chrome;

const list = document.getElementById("list");
const empty = document.getElementById("empty");
const hint = document.getElementById("hint");
const hostLabel = document.getElementById("host");
const enabledBox = document.getElementById("enabled");
const showMediaBox = document.getElementById("showMedia");
const mediaHead = document.getElementById("mediaHead");

let tabId = null;
let showMedia = false; // off by default: the page's video list stays hidden
let lastItems = [];

function applyMediaVisibility() {
  mediaHead.hidden = !showMedia;
  list.hidden = !showMedia;
  empty.hidden = !showMedia || lastItems.length > 0;
}

function send(message) {
  return Promise.resolve(api.runtime.sendMessage(message)).catch((e) => ({ ok: false, error: String((e && e.message) || e) }));
}

function render(items) {
  lastItems = items;
  list.replaceChildren();
  applyMediaVisibility();
  for (const item of items) {
    const li = document.createElement("li");
    const text = document.createElement("span");
    text.className = "text";
    const main = document.createElement("span");
    main.className = "main";
    main.textContent = item.text;
    const sub = document.createElement("span");
    sub.className = "sub";
    sub.textContent = item.url;
    sub.title = item.url;
    text.append(main, sub);
    if (item.note) {
      const note = document.createElement("span");
      note.className = "note";
      note.textContent = item.note;
      text.append(note);
    }
    const button = document.createElement("button");
    button.textContent = "Download";
    button.disabled = !!item.disabled;
    button.addEventListener("click", async () => {
      button.disabled = true;
      button.textContent = "Sending…";
      const resp = await send({ type: "download-media", tabId, key: item.key });
      if (resp && resp.ok) {
        window.close();
      } else {
        button.textContent = "Failed";
        button.title = (resp && resp.error) || "";
        const note = document.createElement("span");
        note.className = "note";
        note.textContent = (resp && resp.error) || "Klippo did not take the download.";
        text.append(note);
      }
    });
    li.append(text, button);
    list.append(li);
  }
}

async function init() {
  const store = await api.storage.local.get("showMedia");
  showMedia = store.showMedia === true;
  showMediaBox.checked = showMedia;
  applyMediaVisibility();

  const [tab] = await api.tabs.query({ active: true, currentWindow: true });
  tabId = tab ? tab.id : null;
  const media = await send({ type: "get-media", tabId });
  render((media && media.items) || []);

  const status = await send({ type: "status" });
  enabledBox.checked = !status || status.enabled !== false;
  const ok = status && status.host && status.host.ok;
  hostLabel.textContent = ok ? status.host.message || "Connected" : "Not connected";
  hostLabel.className = ok ? "ok" : "err";
  hostLabel.title = (status && status.host && status.host.message) || "";
  hint.hidden = !!ok;
}

enabledBox.addEventListener("change", () => send({ type: "set-enabled", enabled: enabledBox.checked }));

showMediaBox.addEventListener("change", () => {
  showMedia = showMediaBox.checked;
  api.storage.local.set({ showMedia });
  applyMediaVisibility();
});

// New streams while the popup is open.
api.runtime.onMessage.addListener((msg) => {
  if (msg && msg.type === "klippo-popup-media" && msg.tabId === tabId) render(msg.items);
});

init();
