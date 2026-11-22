# Firefox Add-ons listing

Everything the submission form on addons.mozilla.org asks for, ready to
paste. Keep it up to date when the add-on changes.

**Add-on ID:** `motlydl@motly-suana.github.io` (fixed; set by `app/src-tauri/build.rs`)
**Upload:** `motlydl-firefox.xpi` from a release, or build it locally (the
installer build writes `app/src-tauri/gen/extension-firefox/`; zip its
contents).

---

## Listing

**Name**

MotlyDL Integration

**Summary** (250 characters max)

Sends your downloads to MotlyDL, the free and open-source download manager, for faster multi-connection downloads that resume after anything. Adds a download button to videos and sends magnet links to MotlyDL.

**Description**

MotlyDL Integration connects Firefox to **MotlyDL**, a free and open-source
download manager for Windows, macOS and Linux.

Browsers download every file over a single connection. MotlyDL splits each
file across up to 32 connections and keeps all of them busy, so downloads from
servers that limit each connection finish many times faster. Progress is saved
every second, so a crash, a closed laptop or a dropped connection never means
starting over.

What the add-on does:

- Sends downloads you start in Firefox to MotlyDL, with the site's cookies, so
  downloads behind a login keep working.
- Adds a Download video button to video players. On YouTube and other video
  sites, pick a quality in MotlyDL, from 4K to audio only.
- Sends magnet links and .torrent files to MotlyDL's built-in torrent client.
- Hold Alt while clicking a link to let Firefox download it instead, or turn
  MotlyDL off for a single site from the toolbar button.

**Requires the MotlyDL desktop app**, free from
https://github.com/motly-suana/MotlyDL. The add-on talks only to the app on your own
computer (native messaging); nothing is sent to any server.

Open source (MIT): https://github.com/motly-suana/MotlyDL

**Categories:** Download Management

**Tags:** download manager, downloads, download accelerator, video downloader, torrent

**Support email or website:** https://github.com/motly-suana/MotlyDL/issues

**Homepage:** https://github.com/motly-suana/MotlyDL

**License:** MIT License

**Does this add-on require payment, non-free services or software, or
additional hardware?** No. (It needs the MotlyDL desktop app, which is free
and open source.)

---

## Privacy policy

MotlyDL Integration does not collect, store or send personal data to the
developer or to any third party.

When you start a download, the add-on passes the download's address, the page
it came from, the site's cookies for that address and your browser's user agent
to the MotlyDL desktop app running on your own computer, through Firefox's
native messaging. MotlyDL uses them only to download that file and deletes the
cookies when the download completes. The add-on also notes which video and
audio files a page loads, kept in memory for the current session, to offer them
in its video button.

Nothing leaves your computer except the requests MotlyDL makes to download the
files you chose. There are no analytics, accounts or telemetry.

---

## Notes to reviewer

MotlyDL Integration is the browser half of MotlyDL, an open-source desktop
download manager (https://github.com/motly-suana/MotlyDL). The add-on's code
is in `extension/`; it is plain JavaScript with no build step, minification or
remote code, so the uploaded files are the source.

Why each permission is needed:

- `nativeMessaging`: hands downloads to the MotlyDL desktop app
  (host `com.motlydl.host`). This is the add-on's core purpose.
- `downloads`: notices downloads started in Firefox, pauses them and cancels
  the Firefox copy once MotlyDL has taken over (or resumes it if the app isn't
  available).
- `cookies` and `<all_urls>`: reads the cookies for the download's own address
  so downloads behind a login work in MotlyDL. They are passed only to the
  local app.
- `webRequest` and `webRequestBlocking`: inspects response headers (`Content-Disposition`, `Content-Type`, `Content-Length`) so file downloads are handed to MotlyDL before Firefox adds an entry to its download list (only cancels the request after MotlyDL accepts it; fails open otherwise), and watches responses for video and audio streams.
- `contextMenus`: adds "Download with MotlyDL" to links and media.
- `storage`: the add-on's settings (on/off, per-site switch, size threshold)
  and the per-tab list of media found.

The content script (`content.js`) shows the video button in a closed shadow
root and sends magnet-link clicks to the app. `innerHTML` is only used to build
the add-on's own markup from escaped values.

To test: install MotlyDL for Windows, macOS or Linux from
https://github.com/motly-suana/MotlyDL/releases/latest and open it once (it
registers the native messaging host for Firefox). Then start any download in
Firefox: MotlyDL opens a "New download" window. Without the app, the add-on
leaves downloads to Firefox and its popup says MotlyDL can't be reached.

Data collection is declared as `none`: everything the add-on reads goes only
to the app on the same computer.
