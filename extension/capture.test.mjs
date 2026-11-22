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
