// MotlyDL page helper.
// - Shows a "Download video" button over video players. Clicking it lists
//   the media files the page loaded and sends the chosen one to MotlyDL.
// - Alt+click on a link tells the background to let the browser handle
//   that one download itself.
// Everything lives in a closed shadow root so page styles can't touch it.

(() => {
  if (window.__motlydlLoaded) return;
  window.__motlydlLoaded = true;

  // Alt+click skips MotlyDL; Shift+Alt+click forces capture (ignoring thresholds/site skips).
  document.addEventListener("click", (e) => {
    if (!e.altKey || !e.target.closest?.("a[href]")) return;
    chrome.runtime.sendMessage({ type: e.shiftKey ? "force" : "bypass" }).catch(() => {});
  }, true);

  // Magnet links aren't downloads, so the browser would hand them to another
  // app. Send them to MotlyDL; if it can't take them, open the link as usual.
  document.addEventListener("click", async (e) => {
    const a = e.target.closest?.('a[href^="magnet:"]');
    if (!a || e.altKey || e.button !== 0 || e.defaultPrevented) return;
    e.preventDefault();
    e.stopPropagation();
    let res;
    try { res = await chrome.runtime.sendMessage({ type: "magnet", url: a.href }); } catch { res = null; }
    if (!res?.ok) location.href = a.href;
  }, true);

  const MIN_W = 140, MIN_H = 112, MIN_AUDIO_W = 120, MIN_AUDIO_H = 24;
  // Video/audio sites MotlyDL downloads through yt-dlp: the page itself is sent,
  // and the quality/format is chosen in MotlyDL.
  const VIDEO_SITE = /(^|\.)(youtube\.com|youtube-nocookie\.com|vimeo\.com|x\.com|twitter\.com|reddit\.com|instagram\.com|tiktok\.com|facebook\.com|twitch\.tv|dailymotion\.com|soundcloud\.com|bilibili\.com)$/i;
  const onVideoSite = () => VIDEO_SITE.test(location.hostname);
  const isSoundCloud = () => /(^|\.)soundcloud\.com$/i.test(location.hostname);
  const isTikTok = () => /(^|\.)tiktok\.com$/i.test(location.hostname);
  const sanitizeTitle = (raw) => (raw || "video").replace(/[\\/:*?"<>|]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 120);
  const LOGO = `<svg viewBox="0 0 64 64" width="16" height="16" aria-hidden="true"><defs><linearGradient id="mg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#1e1b4b"/><stop offset=".55" stop-color="#0f172a"/><stop offset="1" stop-color="#090d16"/></linearGradient><linearGradient id="mc" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#38bdf8"/><stop offset="1" stop-color="#818cf8"/></linearGradient></defs><rect width="64" height="64" rx="14.5" fill="url(#mg)"/><g fill="none" stroke-linecap="round" stroke-linejoin="round" transform="translate(13 13) scale(1.1875)"><path d="M5.6 21.4V5.8l4.8 5M26.4 21.4V5.8l-4.8 5" stroke="#f8fafc" stroke-width="4.2"/><path d="M16 4.8v15.4M11 16.4l5 4.8 5-4.8" stroke="url(#mc)" stroke-width="3.6"/><path d="M5.8 27.2h20.4" stroke="#f8fafc" stroke-width="3.6"/></g></svg>`;

  let host, root, wrap, btn, panel, video = null, hideTimer = 0, raf = 0, open = false;
  let mode = "anchored", docked = false, dismissedFor = "", activeMediaEl = null, baseLabel = "Download video";

  function build() {
    host = document.createElement("motlydl-overlay");
    host.style.cssText = "all:initial;position:fixed;inset:0 auto auto 0;width:0;height:0;z-index:2147483647;";
    root = host.attachShadow({ mode: "closed" });
    root.innerHTML = `<style>
      :host { all: initial; }
      * { box-sizing: border-box; font-family: "Segoe UI Variable Text", "Segoe UI", -apple-system, system-ui, sans-serif; }
      .wrap { position: fixed; width: max-content; display: flex; flex-direction: column; align-items: flex-end; gap: 6px; transform: translateX(-100%); opacity: 0; transition: opacity .15s; pointer-events: none; }
      .wrap.on { opacity: 1; pointer-events: auto; }
      .wrap.dock { left: auto !important; top: auto !important; right: 18px; bottom: 18px; transform: none; flex-direction: column-reverse; }
      .bar { display: inline-flex; align-items: center; gap: 2px; padding: 2px; border-radius: 9px; background: #141824;
        box-shadow: inset 0 0 0 1px rgba(148,163,184,.22), 0 4px 16px rgba(0,0,0,.42); }
      .btn { display: inline-flex; align-items: center; gap: 7px; height: 28px; padding: 0 10px 0 7px; white-space: nowrap; border: 0; border-radius: 7px;
        background: transparent; color: #f1f5f9; font-size: 12.5px; font-weight: 550; cursor: pointer; }
      .btn:hover { background: rgba(148,163,184,.14); }
      .btn .chev { opacity: .6; transition: transform .15s; }
      .btn .tick { display: none; }
      .btn.done .chev { display: none; }
      .btn.done .tick { display: block; color: #34d399; }
      .wrap.open .btn .chev { transform: rotate(180deg); }
      .dismiss { display: none; place-items: center; width: 22px; height: 26px; border: 0; border-radius: 6px; background: none; color: #94a3b8; font-size: 14px; line-height: 1; cursor: pointer; }
      .dismiss:hover { background: rgba(148,163,184,.16); color: #f1f5f9; }
      .wrap.dock .dismiss { display: grid; }
      .panel { width: 320px; padding: 5px; border-radius: 10px; background: #141824; color: #f1f5f9;
        box-shadow: inset 0 0 0 1px rgba(148,163,184,.18), 0 10px 28px rgba(0,0,0,.45); display: none; max-height: 360px; overflow-y: auto; }
      .panel.note { width: auto; max-width: 280px; padding: 8px 11px; }
      .wrap.open .panel { display: block; animation: pop .14s ease-out; }
      @keyframes pop { from { opacity: 0; transform: translateY(-4px); } }
      .head { padding: 6px 8px 6px; font-size: 11.5px; font-weight: 600; color: #94a3b8; }
      .item { display: flex; align-items: center; gap: 10px; width: 100%; padding: 8px; border: 0; border-radius: 8px; background: none; color: inherit; text-align: left; cursor: pointer; }
      .item:hover { background: rgba(148,163,184,.12); }
      .item[disabled] { cursor: default; opacity: .55; }
      .item[disabled]:hover { background: none; }
      .tag { flex: none; min-width: 44px; height: 20px; padding: 0 6px; border-radius: 5px; display: grid; place-items: center; font-size: 10.5px; font-weight: 700; letter-spacing: .03em;
        background: rgba(56,189,248,.16); color: #38bdf8; }
      .tag.stream { background: rgba(129,140,248,.18); color: #818cf8; }
      .meta { flex: 1; min-width: 0; }
      .meta b { display: block; font-size: 12.5px; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
      .meta span { display: block; margin-top: 1px; font-size: 11.5px; color: #94a3b8; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
      .panel.q { width: 210px; }
      .qrow { display: flex; align-items: center; gap: 12px; width: 100%; height: 30px; padding: 0 10px; border: 0; border-radius: 7px; background: none; color: #f1f5f9; cursor: pointer; font-size: 13px; font-weight: 600; font-variant-numeric: tabular-nums; }
      .qrow:hover { background: rgba(148,163,184,.12); }
      .qrow .sz { margin-left: auto; font-weight: 500; font-size: 12px; color: #94a3b8; }
      .qsep { height: 1px; margin: 4px 6px; background: rgba(148,163,184,.14); }
      .msg { font-size: 12.5px; color: #cbd5e1; line-height: 1.45; }
      .panel:not(.note) .msg { padding: 8px 8px 10px; color: #94a3b8; }
      .msg.err { color: #f87171; }
    </style>
    <div class="wrap"><div class="bar"><button class="btn" title="Download with MotlyDL">${LOGO}<span>Download video</span><svg class="chev" width="10" height="10" viewBox="0 0 10 10"><path d="M2 3.5l3 3 3-3" stroke="#fff" stroke-width="1.4" fill="none" stroke-linecap="round"/></svg><svg class="tick" width="11" height="11" viewBox="0 0 12 12"><path d="M2.4 6.3l2.4 2.4 4.8-5.2" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg></button><button class="dismiss" title="Hide bar on this page" aria-label="Hide">×</button></div><div class="panel"></div></div>`;
    wrap = root.querySelector(".wrap");
    btn = root.querySelector(".btn");
    panel = root.querySelector(".panel");
    btn.addEventListener("click", (e) => { e.stopPropagation(); e.preventDefault(); toggle(); });
    root.querySelector(".dismiss").addEventListener("click", (e) => {
      e.stopPropagation();
      dismissedFor = location.href;
      hide();
    });
    panel.addEventListener("click", (e) => {
      e.stopPropagation();
      const item = e.target.closest(".item:not([disabled])");
      if (!item) return;
      grab(item.dataset.url, item.dataset.name, {
        referer: item.dataset.referer || "",
        origin: item.dataset.origin || "",
        userAgent: item.dataset.ua || "",
      });
    });
    wrap.addEventListener("mouseenter", () => clearTimeout(hideTimer));
    wrap.addEventListener("mouseleave", () => scheduleHide());
    mount();
  }

  // News in the panel (looking, reading, a problem), sized to its words.
  function note(html) {
    panel.classList.remove("q");
    panel.classList.add("note");
    panel.innerHTML = `<div class="msg">${html}</div>`;
  }
  // The button says what is happening itself; "Sent" gets a tick.
  function label(text, done = false) {
    btn.querySelector("span").textContent = text || baseLabel;
    btn.classList.toggle("done", done);
  }

  function mount() {
    const parent = document.fullscreenElement || document.documentElement;
    if (host && host.parentNode !== parent) parent.append(host);
  }
  document.addEventListener("fullscreenchange", () => host && mount());
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      if (raf) { cancelAnimationFrame(raf); raf = 0; }
    } else if (mode === "anchored" && video && wrap?.classList.contains("on") && !raf) {
      raf = requestAnimationFrame(position);
    }
  });

  const bytes = (n) => {
    if (!n) return "";
    const u = ["B", "KB", "MB", "GB"]; let i = 0;
    while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
    return `${n < 10 && i ? n.toFixed(1) : Math.round(n)} ${u[i]}`;
  };
  const extOf = (url, mime) => (/\.([a-z0-9]{2,4})(?:$|[?#])/i.exec(new URL(url).pathname) || [])[1]?.toLowerCase() || (mime.split("/")[1] || "").replace(/^x-/, "").slice(0, 4) || "mp4";
  const title = () => (document.title || "video").replace(/[\\/:*?"<>|]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 120);

  function position() {
    if (mode !== "anchored" || !video || document.hidden || !wrap.classList.contains("on")) { raf = 0; return; }
    const r = video.getBoundingClientRect();
    const minW = video.tagName === "VIDEO" ? MIN_W : MIN_AUDIO_W;
    const minH = video.tagName === "VIDEO" ? MIN_H : MIN_AUDIO_H;
    if (r.width < minW || r.height < minH || r.bottom < 0 || r.top > innerHeight) { hide(); raf = 0; return; }
    wrap.style.left = `${Math.min(innerWidth - 8, r.right - 10)}px`;
    const topPos = video.tagName !== "VIDEO" && r.bottom > innerHeight - 72 ? r.top - 36 : r.top + 10;
    wrap.style.top = `${Math.max(8, topPos)}px`;
    raf = requestAnimationFrame(position);
  }
  // YouTube: once the pointer has rested on a video for a moment, ask the app
  // to look it up, so the download can start the moment a quality is picked.
  // Once per video; a quick pass of the mouse doesn't count.
  // leftAt: when the pointer last moved off the video (a resting pointer
  // sends no events, so "left" is what can be seen, not "still there").
  let prepTimer = 0, leftAt = 0;
  const prepared = new Set();
  function prepareSoon() {
    // Called on every mouse move: start the countdown once, don't restart it.
    if (prepTimer || !/(^|\.)youtube\.com$/i.test(location.hostname)) return;
    const started = performance.now();
    prepTimer = setTimeout(async () => {
      prepTimer = 0;
      // The pointer moved off during the countdown: it was only passing.
      if (leftAt > started && !open) return;
      const info = await askPage();
      if (!info?.id || info.live || prepared.has(info.id) || !video) return;
      prepared.add(info.id);
      chrome.runtime.sendMessage({ type: "prepare", url: `https://www.youtube.com/watch?v=${info.id}` }).catch(() => {});
    }, 1000);
  }

  function show(v, text = "Download video") {
    if (!host) build();
    mount();
    mode = "anchored";
    docked = false;
    wrap.classList.remove("dock");
    if (video !== v) { video = v; setOpen(false); }
    baseLabel = text;
    label(text);
    prepareSoon();
    clearTimeout(hideTimer);
    wrap.classList.add("on");
    if (!raf) raf = requestAnimationFrame(position);
  }
  function showDocked(text) {
    if (dismissedFor === location.href || document.hidden) return;
    if (!host) build();
    mount();
    mode = "docked";
    docked = true;
    video = null;
    if (raf) { cancelAnimationFrame(raf); raf = 0; }
    clearTimeout(hideTimer);
    baseLabel = text;
    label(text);
    wrap.classList.add("dock", "on");
  }
  function hide() {
    clearTimeout(prepTimer); prepTimer = 0;
    if (raf) { cancelAnimationFrame(raf); raf = 0; }
    wrap?.classList.remove("on", "dock");
    setOpen(false);
    docked = false;
    video = null;
  }
  function scheduleHide() { clearTimeout(hideTimer); if (!open && mode === "anchored") hideTimer = setTimeout(hide, 1600); }
  function setOpen(v) { open = v; wrap?.classList.toggle("open", v); }

  // Which captured stream is the video being watched. `items` is newest first.
  // A tab that stays open collects streams from every video played in it, and
  // their links expire, so the oldest is often stale. Prefer the newest playlist
  // that lists several qualities (the main one); failing that, the oldest of the
  // recent ones, since a player loads its main playlist first.
  function pickStream(items) {
    const streams = items.filter((m) => m.kind === "hls" || m.kind === "dash");
    const main = streams.find((m) => m.qualities?.length > 1) || streams.find((m) => m.qualities?.length);
    if (main || !streams.length) return main || null;
    return streams.filter((m) => streams[0].at - m.at < 60000).at(-1);
  }

  function pickActiveVideoElement() {
    const vids = [...document.getElementsByTagName("video")];
    const playing = vids.find((el) => !el.paused && !el.ended && el.readyState >= 2);
    if (playing) return playing;
    let best = null, bestArea = 0;
    for (const el of vids) {
      const r = el.getBoundingClientRect();
      const area = Math.max(0, Math.min(innerWidth, r.right) - Math.max(0, r.left)) * Math.max(0, Math.min(innerHeight, r.bottom) - Math.max(0, r.top));
      if (area > bestArea) { best = el; bestArea = area; }
    }
    return best;
  }

  function resolveTikTokTarget(v) {
    let cur = v || pickActiveVideoElement();
    for (let depth = 0; cur && depth < 12; depth++) {
      const a = cur.querySelector?.('a[href*="/video/"], a[href*="/photo/"]');
      if (a?.href) {
        const desc = cur.querySelector?.('[data-e2e="video-desc"], [data-e2e="browse-video-desc"]')?.textContent;
        return { url: a.href.split("?")[0], title: sanitizeTitle(desc || title()), kind: "video" };
      }
      cur = cur.parentElement;
    }
    if (/\/(?:video|photo)\//i.test(location.pathname)) return { url: location.href, title: title(), kind: "video" };
    const srcEl = v || pickActiveVideoElement();
    if (srcEl?.currentSrc && /^https?:/i.test(srcEl.currentSrc)) return { url: srcEl.currentSrc, title: `${title()}.mp4`, kind: "video" };
    return { url: location.href, title: title(), kind: "video" };
  }

  function resolveSoundCloudTarget(el) {
    const card = el?.closest?.(".sound, .soundList__item, .trackItem, .searchItem, .chartTrack");
    const cardLink = card?.querySelector?.("a.soundTitle__title[href], a.trackItem__trackTitle[href], a.chartTrack__titleLink[href]");
    const barLink = document.querySelector("a.playbackSoundBadge__titleLink[href], a.playbackSoundBadge__title[href]");
    const picked = cardLink || barLink;
    if (picked?.href) {
      const name = picked.getAttribute("title") || picked.textContent || title();
      return { url: picked.href.split("?")[0], title: sanitizeTitle(name), kind: "audio" };
    }
    return { url: location.href, title: title(), kind: "audio" };
  }

  function resolveActiveMedia(el) {
    if (isSoundCloud()) return resolveSoundCloudTarget(el);
    if (isTikTok()) return resolveTikTokTarget(el || activeMediaEl);
    return { url: location.href, title: title(), kind: (el || activeMediaEl)?.tagName === "AUDIO" ? "audio" : "video" };
  }

  const escAttr = (s) =>
    String(s || "")
      .replace(/&/g, "&amp;")
      .replace(/"/g, "&quot;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
  const headerAttrs = (m) =>
    `data-referer="${escAttr(m.referer || location.href)}" data-origin="${escAttr(m.origin || "")}" data-ua="${escAttr(m.userAgent || "")}"`;

  async function toggle() {
    if (open) { setOpen(false); return; }
    panel.classList.remove("q", "note");
    setOpen(true);
    if (/(^|\.)youtube\.com$/i.test(location.hostname) && (await youtubeMenu())) return;

    let res;
    try { res = await chrome.runtime.sendMessage({ type: "media" }); } catch { res = { items: [] }; }
    const items = [...(res?.items || [])];
    const mediaEl = video || activeMediaEl;
    const src = mediaEl?.currentSrc;
    if (src && /^https?:/i.test(src) && !items.some((m) => m.url === src)) {
      items.unshift({ url: src, kind: "file", mime: mediaEl.tagName === "AUDIO" || /\.mp3/i.test(src) ? "audio/mpeg" : "video/mp4", size: 0, referer: location.href });
    }
    const files = items.filter((m) => m.kind === "file");
    const stream = pickStream(items);

    if (onVideoSite()) {
      const active = resolveActiveMedia(mediaEl);
      if (!files.length && (!stream || isSoundCloud() || isTikTok())) {
        return grab(active.url, active.title);
      }
      const name = active.title || title();
      let html = `<div class="head">Download options</div>
        <button class="item" data-url="${escAttr(active.url)}" data-name="${escAttr(name)}" data-referer="${escAttr(location.href)}">
          <span class="tag">${isSoundCloud() ? "AUDIO" : "PAGE"}</span>
          <span class="meta"><b>${isSoundCloud() ? "This track (Best audio)" : "This video (Choose quality in MotlyDL)"}</b><span>${escAttr(name)}</span></span>
        </button>`;
      if (files.length) {
        html += `<div class="head">Captured files</div>` + files.map((m) => {
          const ext = extOf(m.url, m.mime || "");
          const kind = /^audio/.test(m.mime) || /mp3|m4a|aac|ogg|opus|flac|wav/.test(ext) ? "Audio" : "Video";
          return `<button class="item" data-url="${escAttr(m.url)}" data-name="${escAttr(`${name}.${ext}`)}" ${headerAttrs(m)}><span class="tag">${ext.toUpperCase()}</span><span class="meta"><b>${kind}${m.size ? ` · ${bytes(m.size)}` : ""}</b><span>${new URL(m.url).hostname}</span></span></button>`;
        }).join("");
      }
      if (stream && !isSoundCloud() && !isTikTok()) {
        html += `<div class="head">Stream</div><button class="item" data-url="${escAttr(stream.url)}" data-name="${escAttr(`${name}.mp4`)}" ${headerAttrs(stream)}><span class="tag stream">${stream.kind.toUpperCase()}</span><span class="meta"><b>Direct ${stream.kind.toUpperCase()} stream</b><span>${new URL(stream.url).hostname}</span></span></button>`;
      }
      panel.classList.remove("note");
      panel.innerHTML = html;
      return;
    }

    if (stream?.qualities?.length) {
      qualityMenu(stream.url, title(), stream.qualities, {
        referer: stream.referer || location.href,
        origin: stream.origin || "",
        userAgent: stream.userAgent || "",
      });
      return;
    }
    if (stream && (await streamMenu(stream))) return;
    note("Looking for downloadable files…");
    const name = title();
    let html = "";
    if (files.length) {
      html += `<div class="head">Video and audio on this page</div>`;
      html += files.map((m) => {
        const ext = extOf(m.url, m.mime || "");
        const kind = /^audio/.test(m.mime) || /mp3|m4a|aac|ogg|opus|flac|wav/.test(ext) ? "Audio" : "Video";
        return `<button class="item" data-url="${escAttr(m.url)}" data-name="${escAttr(`${name}.${ext}`)}" ${headerAttrs(m)}><span class="tag">${ext.toUpperCase()}</span><span class="meta"><b>${kind}${m.size ? ` · ${bytes(m.size)}` : ""}</b><span>${new URL(m.url).hostname}</span></span></button>`;
      }).join("");
    }
    if (stream) {
      html += `<div class="head">Stream</div><button class="item" data-url="${escAttr(stream.url)}" data-name="${escAttr(`${name}.mp4`)}" ${headerAttrs(stream)}><span class="tag stream">${stream.kind.toUpperCase()}</span><span class="meta"><b>This video</b><span>Choose the quality in MotlyDL</span></span></button>`;
    }
    if (!files.length && !stream) html = `<div class="msg">No downloadable file found yet. Play the video for a moment, then try again.</div>`;
    panel.classList.remove("note");
    panel.innerHTML = html;
  }

  // Say what actually went wrong. The commonest case isn't MotlyDL at all:
  // the extension was updated or reloaded, which cuts off pages that were
  // already open until they're reloaded.
  function failure(error) {
    const text = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
    if (/context invalidated|receiving end does not exist/i.test(error) || !chrome.runtime?.id) return "MotlyDL's extension was updated. Reload this page and try again.";
    if (/host not found|not allowed/i.test(error)) return "MotlyDL isn't set up in this browser yet. Open the MotlyDL app once, then try again.";
    return `MotlyDL couldn't take this download: ${text(error) || "no answer from the app"}.`;
  }

  // ---------- YouTube: qualities straight from the page ----------
  // yt.js (inside the page) reads what the player already has. Same choices
  // and format selectors as the app's own lookup, so either way matches.
  const askPage = () => new Promise((resolve) => {
    const on = (e) => { if (e.source === window && e.data?.motlydl === "yt-info") { removeEventListener("message", on); resolve(e.data); } };
    addEventListener("message", on);
    postMessage({ motlydl: "yt-ask" }, "*");
    setTimeout(() => { removeEventListener("message", on); resolve(null); }, 800);
  });
  function ytQualities(formats) {
    const videos = formats.filter((f) => f.mime.startsWith("video/") && f.height > 0);
    const audios = formats.filter((f) => f.mime.startsWith("audio/"));
    const m4a = audios.filter((f) => f.mime.startsWith("audio/mp4"));
    const audioSize = Math.max(0, ...(m4a.length ? m4a : audios).map((f) => f.size));
    const heights = [...new Set(videos.map((f) => f.height))].sort((a, b) => b - a);
    const list = [];
    // HDR is its own choice (same selectors as the app's own lookup): a plain
    // "2160p" would otherwise pick the HDR stream on a video that has one.
    const entry = (set, h, hdr, format) => {
      const best = set.reduce((a, b) => (b.bitrate > a.bitrate ? b : a));
      const fps = Math.max(...set.map((f) => f.fps));
      return { label: `${h}p${fps > 30.5 ? Math.round(fps) : ""}${hdr ? " HDR" : ""}`, format, ext: "mp4", size: best.size ? best.size + audioSize : null };
    };
    for (const h of heights) {
      const at = videos.filter((f) => f.height === h), hdr = at.filter((f) => f.hdr), sdr = at.filter((f) => !f.hdr);
      const plain = hdr.length
        ? `bv*[height=${h}][vcodec^=avc1]+ba[ext=m4a]/bv*[height=${h}][dynamic_range=?SDR]+ba[ext=m4a]/bv*[height=${h}][dynamic_range=?SDR]+ba/b[height=${h}]/bv*[height<=${h}][dynamic_range=?SDR]+ba/b`
        : `bv*[height=${h}][vcodec^=avc1]+ba[ext=m4a]/bv*[height=${h}]+ba[ext=m4a]/bv*[height=${h}]+ba/b[height=${h}]/bv*[height<=${h}]+ba/b`;
      if (hdr.length) list.push(entry(hdr, h, true, `bv*[height=${h}][dynamic_range~="HDR|HLG|DV"]+ba[ext=m4a]/bv*[height=${h}][dynamic_range~="HDR|HLG|DV"]+ba`));
      if (sdr.length || !hdr.length) list.push(entry(sdr.length ? sdr : at, h, false, plain));
    }
    if (audios.length) list.push({ label: "Audio only", format: "ba[ext=m4a]/ba", ext: m4a.length ? "m4a" : "webm", size: audioSize || null });
    return list;
  }
  async function youtubeMenu() {
    const info = await askPage();
    if (!info?.id || info.live || !info.formats.length) return false;
    const qualities = ytQualities(info.formats);
    if (!qualities.length) return false;
    const name = (info.title || title()).replace(/[\\/:*?"<>|]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 120);
    // Just the video: a "list=" mix would make yt-dlp think about the playlist.
    const url = `https://www.youtube.com/watch?v=${info.id}`;
    qualityMenu(url, name, qualities);
    // Start the slow part now, unless hovering already did.
    if (!prepared.has(info.id)) { prepared.add(info.id); chrome.runtime.sendMessage({ type: "prepare", url }).catch(() => {}); }
    return true;
  }

  function qualityMenu(url, name, qualities, headers = {}) {
    const row = (q, i) => `<button class="qrow" data-q="${i}"><span>${escAttr(q.label)}</span><span class="sz">${q.size ? bytes(q.size) : ""}</span></button>`;
    const audio = qualities.findIndex((q) => q.label === "Audio only");
    panel.classList.remove("note");
    panel.classList.add("q");
    panel.innerHTML = qualities.map((q, i) => (i === audio ? `<div class="qsep"></div>` : "") + row(q, i)).join("");
    panel.querySelectorAll("[data-q]").forEach((b) => b.addEventListener("click", (e) => {
      e.stopPropagation();
      const pick = Number(b.dataset.q);
      grab(url, `${name}.${qualities[pick].ext}`, { qualities, pick, ...headers });
    }));
  }
  async function streamMenu(stream) {
    note("Reading qualities…");
    let res;
    try { res = await chrome.runtime.sendMessage({ type: "streamQualities", url: stream.url }); } catch { res = null; }
    if (!res?.qualities?.length) return false;
    qualityMenu(stream.url, title(), res.qualities, {
      referer: stream.referer || location.href,
      origin: stream.origin || "",
      userAgent: stream.userAgent || "",
    });
    return true;
  }

  async function grab(url, filename, extra = {}) {
    // Close the menu; the button itself reports progress.
    setOpen(false);
    label("Sending…");
    let res;
    try { res = await chrome.runtime.sendMessage({ type: "grab", url, filename, ...extra }); } catch (e) { res = { ok: false, error: String(e?.message || e) }; }
    if (res?.ok) {
      label("Sent", true);
      setTimeout(() => { label(); scheduleHide(); }, 1800);
    } else {
      label();
      note(`<span class="err">${failure(res?.error || "")}</span>`);
      setOpen(true);
      setTimeout(() => { setOpen(false); scheduleHide(); }, 7000);
    }
  }

  function findMediaElement(x, y) {
    const stack = document.elementsFromPoint(x, y);
    const av = stack.find((el) => el.tagName === "VIDEO" || el.tagName === "AUDIO");
    if (av) return av;
    if (isSoundCloud()) {
      const sc = stack.find((el) => el.closest?.(".playControls, .sound__waveform, .soundList__item .sound"));
      if (sc) return sc.closest(".playControls, .soundList__item .sound, .sound__waveform") || sc;
      if (document.querySelector("a.playbackSoundBadge__titleLink[href]")) {
        const bar = document.querySelector(".playControls");
        if (bar) return bar;
      }
    }
    return null;
  }

  async function ensureEnabled() {
    if (disabled !== null) return !disabled;
    try { disabled = (await chrome.runtime.sendMessage({ type: "media" }))?.disabled ?? false; } catch { disabled = false; }
    return !disabled;
  }

  // Event-driven media playback detection (zero polling): catches <video>, <audio>,
  // and shadow-DOM players the moment playback starts.
  async function onMediaEvent(e) {
    const el = e.composedPath?.()[0] || e.target;
    if (!el || (el.tagName !== "VIDEO" && el.tagName !== "AUDIO")) return;
    activeMediaEl = el;
    if (!(await ensureEnabled())) return;
    const r = el.getBoundingClientRect();
    if (el.tagName === "VIDEO" && r.width >= MIN_W && r.height >= MIN_H && !video) {
      show(el, "Download video");
    } else if (!video && window === window.top) {
      showDocked(isSoundCloud() ? "Download track" : el.tagName === "AUDIO" ? "Download audio" : "Download video");
    }
  }
  document.addEventListener("play", onMediaEvent, true);
  document.addEventListener("loadedmetadata", onMediaEvent, true);

  // Hover detection over video/audio/SoundCloud players.
  let last = 0, disabled = null;
  document.addEventListener("mousemove", async (e) => {
    const now = performance.now();
    if (now - last < 120) return;
    last = now;
    const sc = isSoundCloud();
    if (!video && !sc && document.getElementsByTagName("video").length === 0 && document.getElementsByTagName("audio").length === 0) return;
    if (host && e.composedPath().includes(host)) return;
    const v = findMediaElement(e.clientX, e.clientY);
    if (v) {
      const r = v.getBoundingClientRect();
      const minW = v.tagName === "VIDEO" ? MIN_W : MIN_AUDIO_W;
      const minH = v.tagName === "VIDEO" ? MIN_H : MIN_AUDIO_H;
      if (r.width < minW || r.height < minH) return;
      if (!(await ensureEnabled())) return;
      show(v, v.tagName !== "VIDEO" && sc ? "Download track" : "Download video");
    } else if (video) { leftAt = now; scheduleHide(); }
  }, { passive: true });
  document.addEventListener("click", (e) => { if (open && host && !e.composedPath().includes(host)) setOpen(false); }, true);
  chrome.storage.onChanged.addListener((c) => { if (c.disabledSites) disabled = null; });
  chrome.runtime.onMessage.addListener((msg, _sender, respond) => {
    if (msg?.type === "activeMedia") {
      respond(resolveActiveMedia(video || activeMediaEl));
    } else if (msg?.type === "mediaFound" && window === window.top) {
      void ensureEnabled().then((ok) => {
        if (!ok || video || open) return;
        const text = isSoundCloud() ? "Download track" : msg.count > 1 ? `Download (${msg.count})` : "Download media";
        showDocked(text);
      });
    }
  });
})();
