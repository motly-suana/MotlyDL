// Shared download capture rules & header metadata store used by both
// Chromium MV3 (importScripts) and Firefox MV3 (background.scripts).
// Tested directly in Node via extension/capture.test.mjs.

((root) => {
  const BROWSER_UPDATE_HOSTS = new Set([
    "clients2.google.com",
    "update.googleapis.com",
    "edgedl.me.gvt1.com",
    "aus5.mozilla.org",
    "download-installer.cdn.mozilla.net",
    "brave-core-ext.s3.brave.com",
  ]);

  const NON_DOWNLOAD_MIME = /^(?:text\/(?:html|plain|css|javascript|xml)|application\/(?:xhtml\+xml|javascript|x-javascript|json|xml)|image\/)/i;
  const DOWNLOAD_MIME = new Set([
    "application/octet-stream",
    "application/force-download",
    "application/zip",
    "application/x-zip-compressed",
    "application/x-rar-compressed",
    "application/vnd.rar",
    "application/x-7z-compressed",
    "application/gzip",
    "application/x-gzip",
    "application/x-tar",
    "application/x-xz",
    "application/x-bzip2",
    "application/zstd",
    "application/x-iso9660-image",
    "application/x-msdownload",
    "application/vnd.microsoft.portable-executable",
    "application/x-msi",
    "application/x-apple-diskimage",
    "application/vnd.debian.binary-package",
    "application/x-rpm",
    "application/vnd.android.package-archive",
    "application/x-executable",
    "application/x-bittorrent",
    "application/metalink4+xml",
    "application/metalink+xml",
  ]);

  function headerValue(headers, name) {
    if (!Array.isArray(headers)) return "";
    const target = name.toLowerCase();
    return headers.find((h) => h.name?.toLowerCase() === target)?.value || "";
  }

  function hostOf(url) {
    try {
      return new URL(url).hostname.replace(/^www\./, "").toLowerCase();
    } catch {
      return "";
    }
  }

  function baseUrl(url) {
    return typeof url === "string" ? url.split("#")[0] : "";
  }

  /**
   * Parses RFC 6266 / 5987 Content-Disposition filename (preferring filename*=).
   */
  function parseContentDisposition(value) {
    if (!value || typeof value !== "string") return "";
    const parts = value.split(";").map((s) => s.trim());
    for (const p of parts) {
      if (/^filename\*=/i.test(p)) {
        const raw = p.slice(p.indexOf("=") + 1).trim().replace(/^"|"$/g, "");
        const idx1 = raw.indexOf("'");
        const idx2 = idx1 >= 0 ? raw.indexOf("'", idx1 + 1) : -1;
        const encoded = idx2 >= 0 ? raw.slice(idx2 + 1) : raw;
        try {
          return decodeURIComponent(encoded).split(/[\\/]/).pop() || "";
        } catch {
          return encoded.split(/[\\/]/).pop() || "";
        }
      }
    }
    for (const p of parts) {
      if (/^filename=/i.test(p)) {
        const raw = p.slice(p.indexOf("=") + 1).trim().replace(/^"|"$/g, "");
        return raw.split(/[\\/]/).pop() || "";
      }
    }
    return "";
  }

  /**
   * Returns true for browser-internal URLs, extension packages, and browser updaters.
   */
  function isBrowserInternalUrl(url, byExtensionId) {
    if (byExtensionId) return true;
    if (typeof url !== "string" || !/^https?:/i.test(url)) return true;
    let parsed;
    try {
      parsed = new URL(url);
    } catch {
      return true;
    }
    const host = parsed.hostname.toLowerCase();
    const path = parsed.pathname.toLowerCase();
    if (path.endsWith(".crx") || path.endsWith(".xpi")) return true;
    if (BROWSER_UPDATE_HOSTS.has(host)) return true;
    return false;
  }

  /**
   * Evaluates webRequest.onHeadersReceived details for Firefox blocking capture.
   */
  function isDownloadableResponse({ method, statusCode, type, url, headers }) {
    if (method && method.toUpperCase() !== "GET") return { capture: false };
    if (statusCode !== 200 && statusCode !== 206) return { capture: false };
    if (type !== "main_frame" && type !== "sub_frame") return { capture: false };
    if (isBrowserInternalUrl(url, null)) return { capture: false };

    const disp = headerValue(headers, "content-disposition");
    const ctypeRaw = headerValue(headers, "content-type");
    const contentType = ctypeRaw.split(";")[0].trim().toLowerCase();
    const lenRaw = Number(headerValue(headers, "content-length"));
    const contentLength = Number.isFinite(lenRaw) && lenRaw > 0 ? lenRaw : 0;
    const filename = parseContentDisposition(disp);

    if (/^attachment\b/i.test(disp.trim())) {
      return { capture: true, filename, contentType, contentLength };
    }
    if (!contentType || NON_DOWNLOAD_MIME.test(contentType) || contentType === "application/pdf") {
      return { capture: false };
    }
    if (DOWNLOAD_MIME.has(contentType)) {
      return { capture: true, filename, contentType, contentLength };
    }
    return { capture: false };
  }

  /**
   * Shared skip-rule evaluation for both Chromium onCreated and Firefox onHeadersReceived.
   */
  function shouldSkipCapture({
    url,
    finalUrl,
    rawUrl,
    referrer,
    pageUrl,
    sizeBytes = 0,
    byExtensionId = null,
    settings = {},
    now = Date.now(),
    bypassUntil = 0,
    forceUntil = 0,
    handedBack = null,
    browserOnce = null,
  }) {
    if (isBrowserInternalUrl(url, byExtensionId)) {
      return { skip: true, reason: "browser-internal" };
    }
    if (now < bypassUntil) {
      return { skip: true, reason: "alt-bypass", clearBypass: true };
    }
    const forced = now < forceUntil;
    if (!settings.enabled && !forced) return { skip: true, reason: "disabled" };

    const candidates = [rawUrl, finalUrl, url].map(baseUrl).filter(Boolean);
    if (browserOnce && candidates.some((u) => (browserOnce.get(u) || 0) > now)) {
      return { skip: true, reason: "download-in-browser" };
    }
    if (handedBack && candidates.some((u) => {
      const until = handedBack instanceof Map ? handedBack.get(u) : handedBack[u];
      return (until || 0) > now;
    })) {
      return { skip: true, reason: "anti-loop" };
    }

    const disabledSites = Array.isArray(settings.disabledSites) ? settings.disabledSites : [];
    const hosts = [pageUrl, referrer, url].map(hostOf).filter(Boolean);
    if (!forced && hosts.some((h) => disabledSites.includes(h))) {
      return { skip: true, reason: "site-disabled" };
    }

    const minBytes = (Number(settings.minSizeKB) || 0) * 1024;
    if (!forced && minBytes > 0 && sizeBytes > 0 && sizeBytes < minBytes) {
      return { skip: true, reason: "below-threshold" };
    }

    return { skip: false, reason: forced ? "forced" : "capture", clearForce: forced };
  }

  /**
   * Bounded TTL store correlating request/redirect/response metadata per URL & requestId.
   */
  function createMetaStore({ maxEntries = 300, ttlMs = 60_000 } = {}) {
    const byReqId = new Map();
    const byUrl = new Map();

    function prune(now = Date.now()) {
      for (const [k, v] of byReqId) {
        if (now - v.at > ttlMs) byReqId.delete(k);
      }
      for (const [k, v] of byUrl) {
        if (now - v.at > ttlMs) byUrl.delete(k);
      }
      while (byReqId.size > maxEntries) {
        byReqId.delete(byReqId.keys().next().value);
      }
      while (byUrl.size > maxEntries) {
        byUrl.delete(byUrl.keys().next().value);
      }
    }

    function upsert(requestId, url, patch, now = Date.now()) {
      prune(now);
      const prev = (requestId && byReqId.get(requestId)) || byUrl.get(url) || {};
      const next = { ...prev, ...patch, url, at: now };
      if (requestId) byReqId.set(requestId, next);
      if (url) {
        byUrl.set(url, next);
        const stripped = baseUrl(url);
        if (stripped && stripped !== url) byUrl.set(stripped, next);
      }
      return next;
    }

    return {
      recordRequestHeaders(requestId, url, headers, pageUrl = "", now = Date.now()) {
        const referer = headerValue(headers, "referer");
        const origin = headerValue(headers, "origin");
        const userAgent = headerValue(headers, "user-agent");
        const patch = {};
        if (referer) patch.referer = referer;
        if (origin && /^https?:/i.test(origin)) patch.origin = origin;
        if (userAgent) patch.userAgent = userAgent;
        if (pageUrl && /^https?:/i.test(pageUrl)) patch.pageUrl = pageUrl;
        upsert(requestId, url, patch, now);
      },

      recordRedirect(requestId, fromUrl, toUrl, now = Date.now()) {
        const prev = (requestId && byReqId.get(requestId)) || byUrl.get(fromUrl) || {};
        let sameOrigin = false;
        try {
          sameOrigin = new URL(fromUrl).origin === new URL(toUrl).origin;
        } catch {}
        const nextPatch = {
          referer: prev.referer,
          userAgent: prev.userAgent,
          pageUrl: prev.pageUrl,
          origin: sameOrigin ? prev.origin : undefined,
        };
        upsert(requestId, toUrl, nextPatch, now);
      },

      recordResponseHeaders(requestId, url, headers, now = Date.now(), statusCode = 0) {
        const disp = headerValue(headers, "content-disposition");
        const filename = parseContentDisposition(disp);
        const contentType = headerValue(headers, "content-type").split(";")[0].trim().toLowerCase();
        const len = Number(headerValue(headers, "content-length"));
        const patch = {};
        if (filename) patch.filename = filename;
        if (contentType) patch.contentType = contentType;
        if (Number.isFinite(len) && len > 0) patch.contentLength = len;
        if (statusCode) patch.statusCode = statusCode;
        if (
          (statusCode === 200 || statusCode === 206) &&
          (/^attachment\b/i.test(disp.trim()) || DOWNLOAD_MIME.has(contentType) || /^(video|audio)\//i.test(contentType))
        ) {
          patch.confirmedDownload = true;
        }
        upsert(requestId, url, patch, now);
      },

      lookup(url, finalUrl, requestId = null, now = Date.now()) {
        prune(now);
        const candidates = [
          requestId ? byReqId.get(requestId) : null,
          finalUrl ? byUrl.get(finalUrl) : null,
          finalUrl ? byUrl.get(baseUrl(finalUrl)) : null,
          url ? byUrl.get(url) : null,
          url ? byUrl.get(baseUrl(url)) : null,
        ].filter(Boolean);
        return candidates[0] || null;
      },

      size() {
        return { reqs: byReqId.size, urls: byUrl.size };
      },
    };
  }

  const MEDIA_EXT = /\.(mp4|m4v|webm|mkv|mov|avi|flv|wmv|mp3|m4a|aac|ogg|oga|opus|flac|wav)(?:$|[?#])/i;
  const CHUNK_EXT = /\.(ts|m4s|aac|vtt|srt)(?:$|[?#])/i;
  const RANGE_QUERY = /[?&](?:range|bytestart|byteend|rn|rbuf)=[^&#]*/gi;
  const MIME_HINT_PARAMS = ["mime", "mime_type", "type", "format", "ct"];
  const SENSITIVE_PARAMS = /^(?:token|sig|signature|expires|expiry|key|auth|policy|credential|cookie|x-amz-signature|x-amz-security-token)$/i;
  const MIN_MEDIA_BYTES = 128 * 1024;

  function redactUrl(url) {
    if (typeof url !== "string") return "";
    try {
      const u = new URL(url);
      for (const [k] of [...u.searchParams.entries()]) {
        if (SENSITIVE_PARAMS.test(k)) u.searchParams.set(k, "***");
      }
      return u.toString();
    } catch {
      return url;
    }
  }

  function stripRangeParams(url) {
    if (typeof url !== "string") return "";
    return url
      .replace(/([?&])(range|bytestart|byteend|rn|rbuf)=[^&#]*/gi, "$1")
      .replace(/[?&]+$/, "")
      .replace(/\?&/, "?");
  }

  function queryMimeHint(url) {
    try {
      const u = new URL(url);
      for (const p of MIME_HINT_PARAMS) {
        const val = (u.searchParams.get(p) || "").trim().toLowerCase();
        if (!val) continue;
        if (/m3u8|mpegurl/.test(val)) return "hls";
        if (/mpd|dash/.test(val)) return "dash";
        if (/^(?:video|audio)(?:\/|_)[a-z0-9.+-]+$/i.test(val) || /^(?:mp4|m4v|webm|mp3|m4a|opus|ogg|aac|flac)$/.test(val)) {
          return "file";
        }
      }
      if ([...u.searchParams.values()].some((v) => /\.m3u8(?:$|[?#])/i.test(v))) return "hls";
      if ([...u.searchParams.values()].some((v) => /\.mpd(?:$|[?#])/i.test(v))) return "dash";
    } catch {}
    return null;
  }

  function classifyMedia(url, mime = "", size = 0, hasContentRange = false) {
    if (typeof url !== "string" || !/^https?:/i.test(url)) return null;
    if (/mpegurl/i.test(mime) || /\.m3u8(?:$|[?#])/i.test(url)) return "hls";
    if (/dash\+xml/i.test(mime) || /\.mpd(?:$|[?#])/i.test(url)) return "dash";
    if (/mp2t|^text\/vtt/i.test(mime) || CHUNK_EXT.test(url)) return null;
    const hint = (!mime || mime === "application/octet-stream" || mime === "binary/octet-stream") ? queryMimeHint(url) : null;
    if (hint === "hls" || hint === "dash") return hint;
    const isMedia = /^(video|audio)\//i.test(mime) || MEDIA_EXT.test(url) || hint === "file";
    if (!isMedia) return null;
    if (RANGE_QUERY.test(url)) {
      RANGE_QUERY.lastIndex = 0;
      if (!hasContentRange || (size > 0 && size < MIN_MEDIA_BYTES)) return null;
    }
    RANGE_QUERY.lastIndex = 0;
    if (size && size < MIN_MEDIA_BYTES) return null;
    return "file";
  }

  const api = {
    parseContentDisposition,
    isBrowserInternalUrl,
    isDownloadableResponse,
    shouldSkipCapture,
    createMetaStore,
    stripRangeParams,
    classifyMedia,
    redactUrl,
  };

  root.MotlyDLCapture = api;
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
})(typeof globalThis !== "undefined" ? globalThis : self);
