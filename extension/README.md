# Klippo browser extension

The extension sends downloads and videos from the browser to Klippo through
the native messaging host `org.klippo.downloader`.

Klippo is an independent project. The extension gives the cookies, referrer
and user agent of a download only to the Klippo program on the same PC, which
uses them only to fetch that file from its own server; there is no tracking
and nothing goes to anyone else.

## Chrome, Edge, Brave, Opera, Vivaldi

1. Run `klippo.exe install-host` once (or click **Options > General >
   Install / repair browser integration** in Klippo).
2. Open `chrome://extensions` (or `edge://extensions`, `brave://extensions`),
   turn on **Developer mode**, click **Load unpacked** and select the
   `chromium` folder.
3. The extension ID must be `hkfpllhmbmikkeobmbnggbjloggpibef`. The `key` in
   `manifest.json` fixes this ID, and the host manifest allows it.
4. After you update the files, click the reload icon on the extension's card.

## Firefox

1. Run `klippo.exe install-host`.
2. Open `about:debugging#/runtime/this-firefox`, click **Load Temporary
   Add-on** and select `firefox/manifest.json`. (Unsigned add-ons stay only
   until Firefox restarts, except in Developer Edition / Nightly.)

The `firefox` folder has the same scripts as `chromium`; only the manifest
differs (Manifest V2). Copy the scripts again after you change them.

Firefox has no way to hold a download until Klippo decides, so the extension
sees each download when it starts: a file that Klippo takes is removed from
the browser's folder if the browser finished it first. Firefox can also
report a form's download before the form's own request, so the extension
holds page requests (`webRequestBlocking`) for the moment it takes to note a
form's fields.

## Use

- **Downloads:** the browser's downloads of every file type go to Klippo
  (Options > File types > "Take browser downloads of all file types"; turn
  it off to take only the types in the list). Klippo opens only its
  "Download File Info" dialog, in front of the browser (the main window
  stays in the notification area). Sites in the exception list and web
  pages that the browser saves stay in the browser.
- **Forms that answer with a file** (POST, for example "Export to CSV"): the
  extension keeps the form's fields for two minutes, and Klippo sends the
  same POST again for the file.
- **Files that a page makes** (`blob:` and `data:` addresses, for example a
  file that a web app builds in the page): only the browser can save them.
  When the browser has saved one, Klippo moves it to the folder of its
  category and lists it as complete; the browser's list keeps no entry.
- A download in a private window uses that window's cookies (allow the
  extension in private windows first: "Allow in Incognito" in Chromium, "Run
  in Private Windows" in Firefox). When Chromium asks where to save a file in
  a private window ("Save As"), a file that a page makes goes to Klippo after
  you answer that dialog.
- **Videos:** when a page plays a video, a **Download this video** button
  shows above the video's top-right corner. Move the pointer over the video
  to show it again. With more than one stream (for example 1080p, 720p and
  360p of an HLS video), the button opens a list. The **×** hides the button
  for that video.
- **Toolbar button:** shows the streams found on the current tab (the badge
  shows how many) and the switch that turns download capture on or off.
- Right-click a link, video, audio or image and pick **Download with Klippo**
  to send it in all cases.
- When Klippo cannot be reached, the browser keeps the download.

## What the video button finds

- Direct files (MP4, WebM, MKV, MOV, FLV, MP3, M4A, ...) that a page plays,
  with their size and height (for example "720p · MP4 · 45.3 MB").
- HLS streams (`.m3u8`): each quality of a master playlist (with its codec),
  the length and an estimated size. When the quality has its sound in a
  separate audio playlist (`#EXT-X-MEDIA:TYPE=AUDIO`, as Reddit, X and many
  players do), the entry says "with audio" and Klippo downloads both and
  joins them. The qualities of a video come first and its best
  audio rendition last ("Audio only"). A quality whose playlist names no
  audio at all says "no sound in this video": the site's own file is silent,
  so the download is silent too. Fragmented-MP4 (CMAF) streams become one `.mp4`;
  MPEG-TS streams become one `.ts` (separate TS audio, or packed AAC, AC-3
  or E-AC-3 audio, is merged in time order).
- DASH manifests (`.mpd`): one entry per video height, "with audio"; Klippo
  reads the manifest (SegmentTemplate, SegmentTimeline, SegmentList or one
  file per stream), downloads the chosen MP4 video and the AAC audio, and
  joins them into one MP4. A WebM-only manifest gives WebM video with Opus
  (or Vorbis) audio, joined into one WebM (`core/mkvmux.rs`).
- Split files: a video-only MP4 and an audio-only file from the same folder
  (DASH-style files such as `DASH_720.mp4` and `DASH_AUDIO_128.mp4`) are
  offered together as "with audio" and joined into one MP4.
- YouTube videos (watch pages, Shorts, YouTube Music): every quality with
  the size of video plus audio, and "Audio only · M4A". A quality is H.264
  or AV1 in MP4 (joined with AAC audio into one MP4), or VP9 in WebM where
  YouTube has nothing else (joined with Opus audio into one WebM).

### How YouTube works

YouTube's web player gets no plain file addresses: it streams with SABR, a
protocol where it POSTs a protobuf request to `googlevideo.com/videoplayback`
and receives a UMP stream. A direct `videoplayback` address serves only its
first megabyte to other programs. So the extension records the player's own
SABR request (`background.js`, `webRequest.onBeforeRequest` with the request
body); that request already carries the player's `n` value, the stream
configuration and the proof-of-origin token. `youtube_main.js` reads the
format list from the player in the page. Klippo (`src/core/youtube.rs`) then
sends the request again with the chosen formats and the playback position
moved forward until all segments have arrived, and `src/core/mp4mux.rs`
joins the two streams into a regular MP4 (full sample tables, so VLC and
other players seek and play the sound correctly).

- Start playing the video (a second is enough) before you choose a quality;
  until then the qualities are listed but marked.
- The recorded session expires after some hours. If a paused YouTube
  download fails with "session has expired", open the video page again and
  start the download from the video button.

Limits:

- Live streams (YouTube live and live HLS) are not listed: Klippo does not
  record live streams.
- DRM-protected HLS (SAMPLE-AES, FairPlay) and DRM-protected DASH streams
  are not supported; they are listed but
  marked.
  AES-128 HLS streams are decrypted.
- Klippo prefers a stream's AAC audio; AC-3 and E-AC-3 audio (TS or packed)
  is joined too.
