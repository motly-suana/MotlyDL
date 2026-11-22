import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const CAP = require("./capture.js");

test("parses Content-Disposition filenames (RFC 5987 and quoted)", () => {
  assert.equal(CAP.parseContentDisposition('attachment; filename="installer.exe"'), "installer.exe");
  assert.equal(
    CAP.parseContentDisposition("attachment; filename=fallback.bin; filename*=UTF-8''my%20file%20(1).zip"),
    "my file (1).zip",
  );
  assert.equal(CAP.parseContentDisposition('attachment; filename="../evil/payload.7z"'), "payload.7z");
  assert.equal(CAP.parseContentDisposition("inline"), "");
});

test("skips browser-internal URLs and extension packages", () => {
  assert.equal(CAP.isBrowserInternalUrl("https://clients2.google.com/service/update2/crx", null), true);
  assert.equal(CAP.isBrowserInternalUrl("https://addons.mozilla.org/firefox/downloads/file/123/ext.xpi", null), true);
  assert.equal(CAP.isBrowserInternalUrl("https://example.com/plugin.crx", null), true);
  assert.equal(CAP.isBrowserInternalUrl("https://example.com/archive.zip", "some-extension-id"), true);
  assert.equal(CAP.isBrowserInternalUrl("chrome://extensions", null), true);
  assert.equal(CAP.isBrowserInternalUrl("blob:https://example.com/123", null), true);
  assert.equal(CAP.isBrowserInternalUrl("https://example.com/release.iso", null), false);
});

test("detects downloadable responses for Firefox onHeadersReceived without hijacking web pages", () => {
  const html = CAP.isDownloadableResponse({
    method: "GET",
    statusCode: 200,
    type: "main_frame",
    url: "https://example.com/page",
    headers: [{ name: "Content-Type", value: "text/html; charset=utf-8" }],
  });
  assert.equal(html.capture, false);

  const xhrBin = CAP.isDownloadableResponse({
    method: "GET",
    statusCode: 200,
    type: "xmlhttprequest",
    url: "https://example.com/api/blob",
    headers: [{ name: "Content-Type", value: "application/octet-stream" }],
  });
  assert.equal(xhrBin.capture, false);

  const attached = CAP.isDownloadableResponse({
    method: "GET",
    statusCode: 200,
    type: "main_frame",
    url: "https://example.com/dl?id=42",
    headers: [
      { name: "Content-Type", value: "application/octet-stream" },
      { name: "Content-Disposition", value: 'attachment; filename="game-patch.zip"' },
      { name: "Content-Length", value: "5242880" },
    ],
  });
  assert.deepEqual(attached, {
    capture: true,
    filename: "game-patch.zip",
    contentType: "application/octet-stream",
    contentLength: 5242880,
  });
});

test("enforces size threshold, disabled sites, and 30s anti-loop guard", () => {
  const now = 1_000_000;
  const settings = { enabled: true, minSizeKB: 1024, disabledSites: ["skip.example"] };

  assert.equal(
    CAP.shouldSkipCapture({ url: "https://cdn.example/small.zip", sizeBytes: 512 * 1024, settings, now }).reason,
    "below-threshold",
  );
  assert.equal(
    CAP.shouldSkipCapture({ url: "https://cdn.example/big.zip", sizeBytes: 2 * 1024 * 1024, settings, now }).skip,
    false,
  );
  assert.equal(
    CAP.shouldSkipCapture({
      url: "https://cdn.example/file.zip",
      pageUrl: "https://www.skip.example/releases",
      sizeBytes: 5 * 1024 * 1024,
      settings,
      now,
    }).reason,
    "site-disabled",
  );

  const handedBack = new Map([["https://cdn.example/fail.iso", now + 30_000]]);
  assert.equal(
    CAP.shouldSkipCapture({
      url: "https://cdn.example/fail.iso",
      sizeBytes: 5 * 1024 * 1024,
      settings,
      now: now + 10_000,
      handedBack,
    }).reason,
    "anti-loop",
  );
  assert.equal(
    CAP.shouldSkipCapture({
      url: "https://cdn.example/fail.iso",
      sizeBytes: 5 * 1024 * 1024,
      settings,
      now: now + 31_000,
      handedBack,
    }).skip,
    false,
  );
});

test("correlates request headers, redirects, and response headers within TTL and bounds", () => {
  const store = CAP.createMetaStore({ maxEntries: 3, ttlMs: 10_000 });
  store.recordRequestHeaders(
    "req-1",
    "https://site.example/download/start",
    [
      { name: "Referer", value: "https://site.example/page" },
      { name: "User-Agent", value: "CustomBrowser/1.0" },
    ],
    "https://site.example/page",
    1000,
  );
  store.recordRedirect("req-1", "https://site.example/download/start", "https://cdn.example/token123", 1050);
  store.recordResponseHeaders(
    "req-1",
    "https://cdn.example/token123",
    [
      { name: "Content-Disposition", value: 'attachment; filename="setup.exe"' },
      { name: "Content-Type", value: "application/x-msdownload" },
      { name: "Content-Length", value: "4096000" },
    ],
    1100,
  );

  const found = store.lookup("https://site.example/download/start", "https://cdn.example/token123", null, 1200);
  assert.equal(found.referer, "https://site.example/page");
  assert.equal(found.userAgent, "CustomBrowser/1.0");
  assert.equal(found.filename, "setup.exe");
  assert.equal(found.contentType, "application/x-msdownload");
  assert.equal(found.contentLength, 4096000);

  // Expires after TTL
  assert.equal(store.lookup("https://cdn.example/token123", null, null, 20_000), null);
});

test("classifies SoundCloud HLS/MP3 and TikTok range-fetched MP4 without dropping chunks prematurely", () => {
  assert.equal(
    CAP.classifyMedia("https://cf-hls-media.sndcdn.com/playlist/abc.128.m3u8?Policy=1", "application/vnd.apple.mpegurl", 0, false),
    "hls",
  );
  assert.equal(
    CAP.classifyMedia("https://cf-media.sndcdn.com/track.mp3", "audio/mpeg", 320_000, false),
    "file",
  );
  assert.equal(
    CAP.classifyMedia("https://v16-webapp-prime.tiktok.com/video/tos/useast2a/v1/?mime_type=video_mp4&bytestart=0&byteend=65535", "video/mp4", 2_400_000, true),
    "file",
  );
  assert.equal(
    CAP.stripRangeParams("https://v16-webapp-prime.tiktok.com/video/tos/?mime=video_mp4&bytestart=0&rn=2"),
    "https://v16-webapp-prime.tiktok.com/video/tos/?mime=video_mp4",
  );
  assert.equal(
    CAP.classifyMedia("https://cdn.example/seg-12.m4s", "video/iso.segment", 500_000, false),
    null,
  );
  assert.equal(
    CAP.classifyMedia("https://cdn.example/ping.mp3", "audio/mpeg", 16_000, false),
    null,
  );
  assert.equal(
    CAP.classifyMedia("https://cdn.example/blob?mime_type=video_mp4&id=1", "application/octet-stream", 2_000_000, false),
    "file",
  );
  assert.equal(
    CAP.classifyMedia("https://cdn.example/play?src=master.m3u8", "text/plain", 0, false),
    "hls",
  );
});

test("tracks origin/confirmedDownload in metaStore, supports forceUntil, and redacts sensitive query tokens", () => {
  const store = CAP.createMetaStore({ maxEntries: 10, ttlMs: 10_000 });
  store.recordRequestHeaders(
    "req-m",
    "https://origin-a.example/media",
    [{ name: "Origin", value: "https://site.example" }, { name: "Referer", value: "https://site.example/" }],
    "https://site.example/watch",
    1000,
  );
  store.recordResponseHeaders(
    "req-m",
    "https://origin-a.example/media",
    [{ name: "Content-Type", value: "application/zip" }, { name: "Content-Disposition", value: 'attachment; filename="pack.zip"' }],
    1050,
    200,
  );
  const metaA = store.lookup("https://origin-a.example/media", null, "req-m", 1100);
  assert.equal(metaA.origin, "https://site.example");
  assert.equal(metaA.confirmedDownload, true);

  store.recordRedirect("req-m", "https://origin-a.example/media", "https://cdn-b.other/file.zip", 1200);
  const metaB = store.lookup("https://cdn-b.other/file.zip", null, "req-m", 1250);
  assert.equal(metaB.origin, undefined, "cross-origin redirect drops previous Origin header");
  assert.equal(metaB.referer, "https://site.example/");

  const forced = CAP.shouldSkipCapture({
    url: "https://cdn.example/tiny.zip",
    pageUrl: "https://skip.example/dl",
    sizeBytes: 10_000,
    settings: { enabled: false, minSizeKB: 1024, disabledSites: ["skip.example"] },
    now: 2000,
    forceUntil: 5000,
  });
  assert.equal(forced.skip, false);
  assert.equal(forced.clearForce, true);

  assert.equal(
    CAP.redactUrl("https://cdn.example/v.mp4?token=secret123&sig=abc&itag=22"),
    "https://cdn.example/v.mp4?token=***&sig=***&itag=22",
  );
});
