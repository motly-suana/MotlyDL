//! The download list: queueing, persistence, live progress, and the bridge
//! between the engine and the UI. All state sits behind one mutex that is
//! never held across an `.await`.

use crate::settings::Settings;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use motlydl_engine::header::{HeaderMap, HeaderName, HeaderValue};
use motlydl_engine::{CancellationToken, Client, Outcome, Progress, Request, SpeedLimiter};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Downloading,
    Paused,
    Completed,
    Error,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u64,
    pub url: String,
    pub filename: String,
    pub path: PathBuf,
    pub size: Option<u64>,
    pub downloaded: u64,
    pub status: Status,
    pub error: Option<String>,
    pub category: String,
    pub added_at: u64,
    pub completed_at: Option<u64>,
    pub resumable: bool,
    /// Cookie/Referer/User-Agent captured by the browser extension. Kept so
    /// resuming still works on sites that need a login; cleared on completion.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Missing byte ranges, for the connection map. Empty when complete or
    /// when the server can't do ranges.
    #[serde(default)]
    pub remaining: Vec<(u64, u64)>,
    /// Live `(start, pos, end)` per connection segment, for the download
    /// window's connection view.
    #[serde(default, skip_deserializing)]
    pub segments: Vec<(u64, u64, u64)>,
    /// Bytes per second, smoothed. Live only, not meaningful on disk.
    #[serde(default, skip_deserializing)]
    pub speed: f64,
    #[serde(default, skip_deserializing)]
    pub connections: usize,
    /// yt-dlp format for downloads from video sites (YouTube and others),
    /// which go through yt-dlp instead of the engine.
    #[serde(default)]
    pub format: Option<String>,
    /// For a torrent or magnet link: the files chosen (empty = all). Its
    /// presence marks the download as a torrent.
    #[serde(default)]
    pub torrent_files: Option<Vec<usize>>,
    /// Live status text, such as "Merging video and audio".
    #[serde(default, skip_deserializing)]
    pub note: Option<String>,
    /// Checksum (MD5, SHA-1, SHA-256, or SHA-512) to verify the finished file against.
    #[serde(default)]
    pub checksum: Option<String>,
    /// Other addresses of the same file (from a Metalink).
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// The finished file matched `checksum`.
    #[serde(default)]
    pub verified: bool,
    /// Page URL where the download originated (kept across pause/error for link refresh).
    #[serde(default)]
    pub page_url: Option<String>,
}

/// A download sent by the browser extension, waiting for the user to
/// confirm it in the Add dialog.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Incoming {
    pub url: String,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub page_url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub content_length: Option<u64>,
    /// A video's qualities, read by the extension from the page itself, so
    /// the New download window can show them without asking yt-dlp.
    #[serde(default)]
    pub qualities: Option<Vec<crate::media::Quality>>,
    /// Which of `qualities` the user clicked.
    #[serde(default)]
    pub pick: Option<usize>,
}

impl Incoming {
    /// A usable file name sent by the extension (e.g. a video page's title),
    /// if it has an extension; otherwise the server's name is better.
    pub fn suggested_name(&self) -> Option<String> {
        let name: String = self.filename.as_deref()?.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
        let name = name.trim().trim_end_matches('.').to_string();
        Path::new(&name).extension().is_some().then_some(name)
    }

    pub fn headers(&self) -> Vec<(String, String)> {
        let mut h = vec![];
        if let Some(c) = self.cookies.as_ref().filter(|c| !c.is_empty()) {
            h.push(("Cookie".into(), c.clone()));
        }
        if let Some(r) = self.referrer.as_ref().filter(|r| !r.is_empty()) {
            h.push(("Referer".into(), r.clone()));
        }
        if let Some(u) = self.user_agent.as_ref().filter(|u| !u.is_empty()) {
            h.push(("User-Agent".into(), u.clone()));
        }
        h
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddRequest {
    pub url: String,
    pub filename: String,
    pub dir: PathBuf,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// False adds it paused ("Download later").
    pub start: bool,
    /// Label of the window this was added from (the "New download"
    /// window), which then becomes the download's progress window.
    #[serde(default)]
    pub window: Option<String>,
    /// yt-dlp format, for a video-site download (see `Item::format`).
    #[serde(default)]
    pub format: Option<String>,
    /// Expected size, when known up front (video qualities).
    #[serde(default)]
    pub size: Option<u64>,
    /// Checksum (MD5, SHA-1, SHA-256, or SHA-512) to verify the finished file against.
    #[serde(default)]
    pub checksum: Option<String>,
    /// Other addresses of the same file.
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// A torrent: the files to download (see `Item::torrent_files`).
    #[serde(default)]
    pub torrent_files: Option<Vec<usize>>,
    /// Explicit category chosen by the user in the New download window.
    #[serde(default)]
    pub category: Option<String>,
    /// Server MIME type for fallback categorization when filename lacks an extension.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Source page URL stored for expired-link refresh.
    #[serde(default)]
    pub page_url: Option<String>,
}

struct RefreshTarget {
    item_id: u64,
    filename: String,
    size: Option<u64>,
    started_at: Instant,
}

struct Running {
    path: PathBuf,
    cancel: CancellationToken,
    progress: Arc<Progress>,
    last: (u64, Instant),
    speed: f64,
    note: crate::media::Note,
}

struct Inner {
    items: Vec<Item>,
    settings: Settings,
    running: HashMap<u64, Running>,
    pending: Vec<Incoming>,
    /// Downloads caught from the browser, waiting in a "New download" window.
    confirms: HashMap<u64, Incoming>,
    next_confirm: u64,
    /// Download id -> label of the window showing its progress.
    windows: HashMap<u64, String>,
    refresh_target: Option<RefreshTarget>,
    next_id: u64,
    dirty: bool,
}

pub struct DownloadManager {
    app: AppHandle,
    client: Client,
    limiter: Arc<SpeedLimiter>,
    /// The tray's "Ignore speed limit" is on.
    limit_off: std::sync::atomic::AtomicBool,
    wake: tokio::sync::Notify,
    data_dir: PathBuf,
    inner: Mutex<Inner>,
}

impl DownloadManager {
    pub fn new(app: AppHandle, data_dir: PathBuf, default_download_dir: PathBuf) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&data_dir)?;
        let mut settings: Settings = load_json(&data_dir.join("settings.json")).unwrap_or_default();
        if settings.download_dir.as_os_str().is_empty() {
            settings.download_dir = default_download_dir;
        }
        let mut items: Vec<Item> = load_json(&data_dir.join("downloads.json")).unwrap_or_default();
        for item in &mut items {
            // The app was closed mid-download: leave it paused, like IDM.
            if item.status == Status::Downloading {
                item.status = Status::Paused;
            }
            if item.status == Status::Completed && !item.segments.is_empty() {
                item.segments.clear();
            }
            if item.status != Status::Completed && item.remaining.is_empty() {
                item.remaining = motlydl_engine::saved_remaining(&item.path).unwrap_or_default();
            }
        }
        let next_id = items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        Ok(Arc::new(Self {
            app,
            client: {
                crate::media::set_proxy(&settings.proxy);
                crate::media::set_bind(&settings.bind_address);
                motlydl_engine::build_client_opts(&settings.proxy, &settings.bind_address)?
            },
            limit_off: std::sync::atomic::AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
            limiter: Arc::new(SpeedLimiter::new(settings.speed_limit_kib * 1024)),
            data_dir,
            inner: Mutex::new(Inner {
                items,
                settings,
                running: HashMap::new(),
                pending: vec![],
                confirms: HashMap::new(),
                next_confirm: 1,
                windows: HashMap::new(),
                refresh_target: None,
                next_id,
                dirty: false,
            }),
        }))
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn app_handle(&self) -> &AppHandle {
        &self.app
    }

    pub fn settings(&self) -> Settings {
        self.inner.lock().unwrap().settings.clone()
    }

    pub fn snapshot(&self) -> Vec<Item> {
        self.inner.lock().unwrap().items.clone()
    }

    /// Where a file of this category goes by default.
    pub fn default_dir(&self, category: &str) -> PathBuf {
        default_dir_for_settings(&self.settings(), category)
    }

    pub fn add(self: &Arc<Self>, req: AddRequest) -> u64 {
        let filename = req.filename.trim().to_string();
        let path = motlydl_engine::resolve_dest(&req.dir, &filename);
        let category = req
            .category
            .as_deref()
            .map(str::trim)
            .filter(|c| is_valid_category(c))
            .map(str::to_string)
            .unwrap_or_else(|| category_for_with_mime(&filename, req.content_type.as_deref()).to_string());
        let checksum = req
            .checksum
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase);
        let page_url = req
            .page_url
            .filter(|s| !s.trim().is_empty())
            .or_else(|| req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("referer")).map(|(_, v)| v.clone()));
        let id = {
            let mut g = self.inner.lock().unwrap();
            let id = g.next_id;
            g.next_id += 1;
            g.items.push(Item {
                id,
                url: req.url,
                filename: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(filename.clone()),
                category,
                path,
                size: req.size,
                downloaded: 0,
                status: if req.start { Status::Queued } else { Status::Paused },
                error: None,
                added_at: now_ms(),
                completed_at: None,
                resumable: true,
                headers: req.headers,
                remaining: vec![],
                segments: vec![],
                speed: 0.0,
                connections: 0,
                format: req.format,
                torrent_files: req.torrent_files,
                note: None,
                checksum,
                mirrors: req.mirrors,
                verified: false,
                page_url,
            });
            if let Some(label) = req.window {
                g.windows.insert(id, label);
            }
            id
        };
        self.changed();
        self.schedule();
        id
    }

    pub fn pause(self: &Arc<Self>, ids: &[u64]) {
        {
            let mut g = self.inner.lock().unwrap();
            for id in ids {
                if let Some(r) = g.running.get(id) {
                    r.cancel.cancel(); // the task marks it paused when it stops
                } else if let Some(item) = g.items.iter_mut().find(|i| i.id == *id && i.status == Status::Queued) {
                    item.status = Status::Paused;
                }
            }
        }
        self.changed();
    }

    pub fn resume(self: &Arc<Self>, ids: &[u64]) {
        {
            let mut g = self.inner.lock().unwrap();
            for item in g.items.iter_mut().filter(|i| ids.contains(&i.id)) {
                if matches!(item.status, Status::Paused | Status::Error) {
                    item.status = Status::Queued;
                    item.error = None;
                }
            }
        }
        self.changed();
        self.schedule();
    }

    pub fn remove(self: &Arc<Self>, ids: &[u64], delete_files: bool) {
        let removed: Vec<Item> = {
            let mut g = self.inner.lock().unwrap();
            for id in ids {
                if let Some(r) = g.running.get(id) {
                    r.cancel.cancel();
                }
            }
            let (gone, keep) = std::mem::take(&mut g.items).into_iter().partition(|i| ids.contains(&i.id));
            g.items = keep;
            gone
        };
        for item in &removed {
            // Running ones clean up their own temp files when their task ends.
            if item.status != Status::Downloading {
                remove_temp_files(&item.path);
            }
            if delete_files && item.status == Status::Completed {
                let _ = std::fs::remove_file(&item.path);
            }
        }
        self.changed();
    }

    pub fn set_settings(self: &Arc<Self>, settings: Settings) {
        crate::media::set_proxy(&settings.proxy);
        crate::media::set_bind(&settings.bind_address);
        self.apply_limit(settings.speed_limit_kib);
        if let Err(e) = crate::browser::offer_extension(settings.offer_extension) {
            crate::log::line(&format!("could not update browser extension offer: {e:#}"));
        }
        // Open windows (download windows) follow the theme at once.
        let _ = self.app.emit("settings", &settings);
        self.inner.lock().unwrap().settings = settings;
        self.save();
        self.schedule();
        self.wake.notify_one();
    }

    fn apply_limit(&self, kib: u64) {
        let off = self.limit_off.load(Ordering::Relaxed);
        self.limiter.set_rate(if off { 0 } else { kib * 1024 });
    }

    /// The tray's "Ignore speed limit": flips it, and returns whether it is on.
    pub fn toggle_limit(&self) -> bool {
        let off = !self.limit_off.fetch_xor(true, Ordering::Relaxed);
        let kib = self.inner.lock().unwrap().settings.speed_limit_kib;
        self.apply_limit(kib);
        off
    }

    /// A Metalink link: fetches the list, then downloads the file from all its
    /// mirrors, checking the checksum it lists. Returns the file's name and
    /// how many addresses it has.
    pub async fn add_metalink(self: &Arc<Self>, url: &str) -> Result<(String, usize), String> {
        let text = self.client.get(url).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
        let m = crate::metalink::parse(&text).map_err(|e| format!("{e:#}"))?;
        let dir = self.default_dir(category_for(&m.name));
        let n = m.urls.len();
        self.add(AddRequest {
            url: m.urls[0].clone(),
            filename: m.name.clone(),
            dir,
            start: true,
            size: m.size,
            checksum: m.checksum,
            mirrors: m.urls[1..].to_vec(),
            ..Default::default()
        });
        Ok((m.name, n))
    }

    /// Moves a waiting download to the front of the queue.
    pub fn download_next(&self, id: u64) -> bool {
        let moved = {
            let mut g = self.inner.lock().unwrap();
            let pos = g.items.iter().position(|i| i.id == id && i.status == Status::Queued);
            let first = g.items.iter().position(|i| i.status == Status::Queued);
            match (pos, first) {
                (Some(pos), Some(first)) => {
                    let item = g.items.remove(pos);
                    g.items.insert(first, item);
                    true
                }
                _ => false,
            }
        };
        if moved {
            self.changed();
            self.save();
        }
        moved
    }

    /// The chosen debrid service and its token.
    pub fn debrid(&self) -> (String, String) {
        let g = self.inner.lock().unwrap();
        (g.settings.debrid_provider.clone(), g.settings.debrid_token.clone())
    }

    pub fn item(&self, id: u64) -> Option<Item> {
        self.inner.lock().unwrap().items.iter().find(|i| i.id == id).cloned()
    }

    /// A download arrived from the browser extension. Like IDM, it gets its
    /// own small "New download" window rather than the main window.
    /// Starts looking a YouTube video up, so its download starts at once.
    pub fn prepare(self: &Arc<Self>, inc: &Incoming) {
        let (client, url, headers) = (self.client.clone(), inc.url.clone(), inc.headers());
        tauri::async_runtime::spawn(async move { crate::media::prepare(&client, &url, headers).await });
    }

    pub fn incoming(self: &Arc<Self>, inc: Incoming) {
        let pending_refresh = {
            let mut g = self.inner.lock().unwrap();
            match g.refresh_target.take() {
                Some(t) if t.started_at.elapsed() < Duration::from_secs(600) => Some(t),
                _ => None,
            }
        };
        if let Some(target) = pending_refresh {
            if !crate::media::is_media_url(&inc.url) && !crate::torrent::is_torrent(&inc.url, inc.filename.as_deref()) {
                let this = self.clone();
                tauri::async_runtime::spawn(async move {
                    let headers = inc.headers();
                    let probed = motlydl_engine::probe(&this.client, &inc.url, &to_header_map(&headers)).await.ok();
                    let incoming_size = probed.as_ref().and_then(|r| r.size).or(inc.content_length);
                    let incoming_name = probed.as_ref().map(|r| r.filename.clone()).or_else(|| inc.suggested_name());
                    match matches_refresh_target(&target.filename, target.size, incoming_name.as_deref(), incoming_size) {
                        Ok(()) => match this.set_url_with_headers(target.item_id, &inc.url, Some(headers)) {
                            Ok(()) => {
                                let _ = this.app.emit(
                                    "refresh-state",
                                    serde_json::json!({ "itemId": target.item_id, "filename": target.filename, "status": "matched" }),
                                );
                            }
                            Err(err) => {
                                let _ = this.app.emit(
                                    "refresh-state",
                                    serde_json::json!({ "itemId": target.item_id, "filename": target.filename, "status": "mismatch", "error": err }),
                                );
                                this.dispatch_incoming(inc);
                            }
                        },
                        Err(reason) => {
                            let _ = this.app.emit(
                                "refresh-state",
                                serde_json::json!({ "itemId": target.item_id, "filename": target.filename, "status": "mismatch", "error": reason }),
                            );
                            this.dispatch_incoming(inc);
                        }
                    }
                });
                return;
            }
            // Keep refresh target active if the user captured a torrent/video page in the meantime.
            self.inner.lock().unwrap().refresh_target = Some(target);
        }
        self.dispatch_incoming(inc);
    }

    fn dispatch_incoming(self: &Arc<Self>, inc: Incoming) {
        self.prepare(&inc);
        // Video sites always ask: there's a quality to choose.
        if self.settings().ask_before_download || crate::media::is_media_url(&inc.url) || crate::torrent::is_torrent(&inc.url, inc.filename.as_deref()) {
            self.new_download_window(inc);
            return;
        }
        self.add_quiet(inc);
    }

    /// Links pasted in bulk (a list, or a pattern like `file[01-50].zip`):
    /// each is probed for its name and started, without a window per file.
    pub fn add_links(self: &Arc<Self>, text: &str) -> usize {
        let links = crate::batch::expand_links(text);
        for url in &links {
            let inc = Incoming { url: url.clone(), ..Default::default() };
            if crate::media::is_media_url(url) || crate::torrent::is_torrent(url, None) {
                self.new_download_window(inc);
            } else {
                self.add_quiet(inc);
            }
        }
        links.len()
    }

    /// Probes for the real file name, then starts the download right away.
    fn add_quiet(self: &Arc<Self>, inc: Incoming) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            let headers = inc.headers();
            let probed = motlydl_engine::probe(&this.client, &inc.url, &to_header_map(&headers)).await.ok();
            let content_type = probed.as_ref().and_then(|r| r.content_type.clone()).or_else(|| inc.content_type.clone());
            let filename = inc.suggested_name().or_else(|| probed.map(|r| r.filename)).unwrap_or_else(|| "download.bin".into());
            let category = category_for_with_mime(&filename, content_type.as_deref());
            let dir = this.default_dir(category);
            let page_url = inc.page_url.clone().or_else(|| inc.referrer.clone());
            this.add(AddRequest {
                url: inc.url,
                filename,
                dir,
                headers,
                start: true,
                category: Some(category.to_string()),
                content_type,
                page_url,
                ..Default::default()
            });
        });
    }

    /// Opens a "New download" window for `inc`.
    /// The videos picked from a playlist, each as its own download in the chosen
    /// shape ("high", "best" or "audio"). Returns how many were added.
    pub fn add_playlist(self: &Arc<Self>, entries: &[(String, String)], mode: &str) -> usize {
        let (format, ext, category) = crate::media::playlist_format(mode);
        for (url, title) in entries {
            self.add(AddRequest {
                url: url.clone(),
                filename: format!("{}.{ext}", clean_name(title)),
                dir: self.default_dir(category),
                start: true,
                format: Some(format.to_string()),
                category: Some(category.to_string()),
                ..Default::default()
            });
        }
        entries.len()
    }

    /// A video link: opens the New download window to choose a quality, unless
    /// Settings says to start straight away with a default.
    pub fn new_download_window(self: &Arc<Self>, inc: Incoming) {
        let mode = self.settings().video_default;
        if mode != "ask" && crate::media::is_media_url(&inc.url) && !crate::torrent::is_torrent(&inc.url, inc.filename.as_deref()) {
            self.add_video_quietly(inc, mode);
            return;
        }
        self.open_confirm_window(inc);
    }

    /// Looks up the video's qualities (the extension may have sent them), picks
    /// one by the default, and starts. If anything goes wrong, the window opens
    /// so the person can see why and choose.
    fn add_video_quietly(self: &Arc<Self>, inc: Incoming, mode: String) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            let found: Result<(String, Vec<crate::media::Quality>), String> = match inc.qualities.clone().filter(|q| !q.is_empty()) {
                Some(q) => Ok((inc.filename.clone().unwrap_or_default(), q)),
                None => crate::media::probe(&this.client, &inc.url, inc.headers()).await.map(|i| (i.title, i.qualities)).map_err(|e| format!("{e:#}")),
            };
            let picked = found.ok().and_then(|(title, qs)| {
                // A quality chosen on the page beats the default.
                let at = inc.pick.filter(|&n| n < qs.len()).or_else(|| pick_quality(&qs, &mode))?;
                Some((title, qs[at].clone()))
            });
            let Some((title, q)) = picked else {
                this.open_confirm_window(inc);
                return;
            };
            let name = format!("{}.{}", clean_name(if title.trim().is_empty() { "video" } else { &title }), q.ext);
            let category = if matches!(q.ext.as_str(), "m4a" | "mp3" | "opus" | "ogg" | "webm" | "aac" | "flac") && q.label == "Audio only" { "Music" } else { "Video" };
            let dir = this.default_dir(category);
            this.add(AddRequest {
                url: inc.url.clone(),
                filename: name,
                dir,
                headers: inc.headers(),
                start: true,
                format: Some(q.format),
                size: q.size,
                category: Some(category.to_string()),
                page_url: inc.page_url.clone().or_else(|| inc.referrer.clone()),
                ..Default::default()
            });
        });
    }

    fn open_confirm_window(&self, inc: Incoming) {
        let token = {
            let mut g = self.inner.lock().unwrap();
            let token = g.next_confirm;
            g.next_confirm += 1;
            g.confirms.insert(token, inc);
            token
        };
        if let Err(e) = crate::windows::open(&self.app, &format!("new-{token}"), &format!("confirm={token}"), "New download") {
            crate::log::line(&format!("could not open the New download window: {e:#}"));
        }
    }

    pub fn take_pending(&self) -> Vec<Incoming> {
        std::mem::take(&mut self.inner.lock().unwrap().pending)
    }

    /// What a "New download" window was opened for.
    pub fn confirm_info(&self, token: u64) -> Option<Incoming> {
        self.inner.lock().unwrap().confirms.get(&token).cloned()
    }

    pub fn forget_confirm(&self, token: u64) {
        self.inner.lock().unwrap().confirms.remove(&token);
    }

    /// Cleans tracked progress/confirmation state when a window closes or is destroyed.
    pub fn on_window_destroyed(&self, label: &str) {
        let mut g = self.inner.lock().unwrap();
        let Inner { windows, confirms, .. } = &mut *g;
        prune_destroyed_window(windows, confirms, label);
    }

    /// Opens (or keeps) the progress window for each newly started
    /// download, unless the user turned that off.
    fn open_progress_windows(&self, ids: &[u64]) {
        let to_open: Vec<(u64, String)> = {
            let mut g = self.inner.lock().unwrap();
            if !g.settings.show_progress_window {
                return;
            }
            let missing: Vec<u64> = ids
                .iter()
                .copied()
                .filter(|id| g.windows.get(id).is_none_or(|label| self.app.get_webview_window(label).is_none()))
                .collect();
            missing
                .into_iter()
                .map(|id| {
                    g.windows.insert(id, format!("dl-{id}"));
                    let title = g.items.iter().find(|i| i.id == id).map(|i| i.filename.clone()).unwrap_or_default();
                    (id, title)
                })
                .collect()
        };
        for (id, title) in to_open {
            if let Err(e) = crate::windows::open(&self.app, &format!("dl-{id}"), &format!("id={id}"), &title) {
                crate::log::line(&format!("could not open the download window: {e:#}"));
            }
        }
    }

    /// Starts queued downloads until `max_concurrent` are running.
    fn schedule(self: &Arc<Self>) {
        let to_start: Vec<(u64, Request, Kind, Arc<Progress>, crate::media::Note, CancellationToken)> = {
            let mut g = self.inner.lock().unwrap();
            let Inner { items, running, settings, .. } = &mut *g;
            // A start time in the future holds the whole queue.
            let held = settings.start_at > now_ms();
            let free = if held { 0 } else { settings.max_concurrent.max(1).saturating_sub(running.len()) };
            let connections = settings.connections;
            items
                .iter_mut()
                .filter(|i| i.status == Status::Queued)
                .take(free)
                .map(|item| {
                    let id = item.id;
                    item.status = Status::Downloading;
                    let req = Request {
                        url: item.url.clone(),
                        dest: item.path.clone(),
                        connections,
                        headers: to_header_map(&item.headers),
                        restart: false,
                        mirrors: item.mirrors.clone(),
                    };
                    let progress = Arc::new(Progress::default());
                    progress.downloaded.store(item.downloaded, Ordering::Relaxed);
                    progress.total.store(item.size.unwrap_or(0), Ordering::Relaxed);
                    let cancel = CancellationToken::new();
                    let note = crate::media::Note::default();
                    running.insert(
                        id,
                        Running {
                            path: item.path.clone(),
                            cancel: cancel.clone(),
                            progress: progress.clone(),
                            last: (item.downloaded, Instant::now()),
                            speed: 0.0,
                            note: note.clone(),
                        },
                    );
                    let kind = match (&item.torrent_files, &item.format) {
                        _ if crate::ftp::is_ftp(&item.url) => Kind::Ftp,
                        (Some(files), _) => Kind::Torrent(files.clone()),
                        (None, Some(format)) => Kind::Media(format.clone()),
                        (None, None) => Kind::File,
                    };
                    (id, req, kind, progress, note, cancel)
                })
                .collect()
        };
        if to_start.is_empty() {
            return;
        }
        self.open_progress_windows(&to_start.iter().map(|t| t.0).collect::<Vec<_>>());
        for (id, req, kind, progress, note, cancel) in to_start {
            let this = self.clone();
            tauri::async_runtime::spawn(async move {
                let check_note = note.clone();
                let result = match kind {
                    // A stream picked in the extension: our own downloader.
                    Kind::Media(format) if crate::stream::parse_format(&format).is_some() => {
                        let (video, audio) = match crate::stream::parse_format(&format).unwrap_or_default() {
                            // Audio only: the sound track is the one to download.
                            (None, Some(a)) => (Some(a), None),
                            other => other,
                        };
                        let job = crate::stream::Job { video: video.unwrap_or_else(|| req.url.clone()), audio, dest: req.dest.clone(), headers: req.headers.clone() };
                        crate::stream::run(&this.client, job, progress, note, cancel).await
                    }
                    Kind::Media(format) => {
                        let headers = req.headers.iter()
                            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
                            .collect();
                        let job = crate::media::Job { url: req.url.clone(), format, dest: req.dest.clone(), headers };
                        crate::media::run(&this.client, job, progress, note, cancel).await
                    }
                    Kind::Torrent(files) => {
                        // A torrent saves into a folder; `dest` is its name inside it.
                        let dir = req.dest.parent().map(Path::to_path_buf).unwrap_or_default();
                        let seed = crate::torrent::Seed::from_setting(&this.settings().seed_until);
                        let job = crate::torrent::Job { url: req.url.clone(), headers: req.headers.clone(), dir, files, seed };
                        crate::torrent::run(&this.client, job, progress, note, cancel).await
                    }
                    Kind::Ftp => crate::ftp::run(req.url.clone(), req.dest.clone(), progress, cancel).await,
                    Kind::File => {
                        // A file-hosting link goes through Real-Debrid, when set up.
                        let mut req = req;
                        let (service, token) = this.debrid();
                        match crate::debrid::resolve(&this.client, &service, &token, &req.url).await {
                            Ok(Some(direct)) => {
                                req.url = direct.url;
                                req.headers = Default::default();
                                motlydl_engine::run(&this.client, &req, progress, cancel, this.limiter.clone()).await
                            }
                            Ok(None) => motlydl_engine::run(&this.client, &req, progress, cancel, this.limiter.clone()).await,
                            Err(e) => Err(e),
                        }
                    }
                };
                let result = this.verify(id, result, &check_note).await;
                this.finish(id, result);
            });
        }
        self.changed();
    }

    fn finish(self: &Arc<Self>, id: u64, result: Result<Outcome>) {
        let mut notify = None;
        let mut completed = false;
        let mut unpack: Option<PathBuf> = None;
        {
            let mut g = self.inner.lock().unwrap();
            let running = g.running.remove(&id);
            let notify_enabled = g.settings.notify_on_complete;
            let extract_enabled = g.settings.extract_archives;
            match g.items.iter_mut().find(|i| i.id == id) {
                None => {
                    // Removed while running: drop what it left behind.
                    if let Some(r) = &running {
                        remove_temp_files(&r.path);
                    }
                }
                Some(item) => {
                    if let Some(r) = &running {
                        item.downloaded = r.progress.downloaded.load(Ordering::Relaxed);
                        let total = r.progress.total.load(Ordering::Relaxed);
                        if total > 0 {
                            item.size = Some(total);
                        }
                        item.resumable = r.progress.resumable.load(Ordering::Relaxed);
                        item.remaining = r.progress.remaining.lock().unwrap().clone();
                        item.segments = r.progress.segments.lock().unwrap().clone();
                    }
                    item.speed = 0.0;
                    item.connections = 0;
                    item.note = None;
                    match result {
                        Ok(Outcome::Completed) => {
                            completed = true;
                            if extract_enabled && item.torrent_files.is_none() && crate::extract::is_archive(&item.filename) {
                                unpack = Some(item.path.clone());
                            }
                            item.status = Status::Completed;
                            item.completed_at = Some(now_ms());
                            item.size = item.size.or(Some(item.downloaded));
                            item.downloaded = item.size.unwrap_or(item.downloaded);
                            item.headers.clear();
                            item.remaining.clear();
                            item.segments.clear();
                            if notify_enabled {
                                notify = Some(item.filename.clone());
                            }
                        }
                        Ok(Outcome::Paused) => item.status = Status::Paused,
                        Err(e) => {
                            item.status = Status::Error;
                            item.error = Some(format!("{e:#}"));
                            crate::log::line(&format!("download {} failed ({}): {e:#}", item.filename, item.url));
                        }
                    }
                }
            }
        }
        if let Some(name) = notify {
            use tauri_plugin_notification::NotificationExt;
            let _ = self.app.notification().builder().title("Download complete").body(name).show();
        }
        if let Some(path) = unpack {
            let app = self.app.clone();
            std::thread::spawn(move || {
                use tauri_plugin_notification::NotificationExt;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                match crate::extract::unpack(&path) {
                    Ok(dir) => {
                        crate::log::line(&format!("unpacked {name} into {}", dir.display()));
                        let _ = app.notification().builder().title("Unpacked").body(format!("{name} is in {}", dir.display())).show();
                    }
                    Err(e) => {
                        crate::log::line(&format!("couldn't unpack {name}: {e:#}"));
                        let _ = app.notification().builder().title("Couldn't unpack").body(format!("{name}: {e}")).show();
                    }
                }
            });
        }
        self.changed();
        self.save();
        self.schedule();
        // "When all downloads finish": once nothing is running or waiting.
        let action = {
            let mut g = self.inner.lock().unwrap();
            let idle = g.running.is_empty() && !g.items.iter().any(|i| i.status == Status::Queued);
            if completed && idle && g.settings.when_done != "nothing" {
                Some(std::mem::replace(&mut g.settings.when_done, "nothing".into()))
            } else {
                None
            }
        };
        if let Some(kind) = action {
            let settings = self.inner.lock().unwrap().settings.clone();
            let _ = self.app.emit("settings", &settings);
            self.save();
            self.power(&kind);
        }
    }

    /// Checks a finished file against `checksum` (MD5/SHA-1/SHA-256/SHA-512) or
    /// an adjacent sidecar file (.md5, .sha1, .sha256, .sha512).
    async fn verify(self: &Arc<Self>, id: u64, result: Result<Outcome>, note: &crate::media::Note) -> Result<Outcome> {
        if !matches!(result, Ok(Outcome::Completed)) {
            return result;
        }
        let Some(item) = self.item(id) else { return result };
        if item.torrent_files.is_some() {
            return result;
        }
        let want = item.checksum.clone();
        let path = item.path.clone();
        let note_clone = note.clone();
        match tauri::async_runtime::spawn_blocking(move || {
            let has_explicit = want.as_deref().map(str::trim).is_some_and(|s| !s.is_empty());
            if has_explicit {
                *note_clone.lock().unwrap() = Some("Checking checksum".into());
            } else if let Some((algo, _)) = crate::checksum::find_sidecar(&path) {
                *note_clone.lock().unwrap() = Some(format!("Checking {}", algo.label()));
            }
            crate::checksum::verify_file(&path, want.as_deref())
        })
        .await
        {
            Ok(Ok(Some((_algo, matched_hex)))) => {
                if let Some(i) = self.inner.lock().unwrap().items.iter_mut().find(|i| i.id == id) {
                    i.verified = true;
                    if i.checksum.as_deref().map(str::trim).is_none_or(str::is_empty) {
                        i.checksum = Some(matched_hex);
                    }
                }
                result
            }
            Ok(Ok(None)) => result,
            Ok(Err(msg)) => Err(anyhow::anyhow!(msg)),
            Err(_) => Err(anyhow::anyhow!("The file finished, but couldn't be read to check it.")),
        }
    }

    /// Replaces a stopped download's address (an expired link, refreshed) and
    /// resumes it where it left off.
    pub fn set_url(self: &Arc<Self>, id: u64, url: &str) -> Result<(), String> {
        self.set_url_with_headers(id, url, None)
    }

    pub fn set_url_with_headers(
        self: &Arc<Self>,
        id: u64,
        url: &str,
        headers: Option<Vec<(String, String)>>,
    ) -> Result<(), String> {
        let url = url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("That isn't a web link".into());
        }
        {
            let mut g = self.inner.lock().unwrap();
            let Some(item) = g.items.iter_mut().find(|i| i.id == id) else { return Err("Download not found".into()) };
            if item.format.is_some() || item.torrent_files.is_some() {
                return Err("Only regular file downloads can change address".into());
            }
            if matches!(item.status, Status::Completed | Status::Downloading) {
                return Err("Pause the download first".into());
            }
            item.url = url.to_string();
            if let Some(h) = headers.filter(|v| !v.is_empty()) {
                item.headers = h;
            }
            item.error = None;
            motlydl_engine::rebase_saved(&item.path, url);
        }
        self.resume(&[id]);
        Ok(())
    }

    /// Opens the source page for `id` in the browser and arms auto-matching for
    /// the next captured regular file download with matching filename & size.
    pub fn start_refresh(self: &Arc<Self>, id: u64) -> Result<String, String> {
        use tauri_plugin_opener::OpenerExt;
        let (filename, open_url, needs_pause) = {
            let mut g = self.inner.lock().unwrap();
            let Some(item) = g.items.iter().find(|i| i.id == id).cloned() else {
                return Err("Download not found".into());
            };
            if item.format.is_some() || item.torrent_files.is_some() {
                return Err("Only regular file downloads can refresh their address".into());
            }
            if item.status == Status::Completed {
                return Err("Download is already completed".into());
            }
            let open_url = item
                .page_url
                .clone()
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                .or_else(|| {
                    item.headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("referer"))
                        .map(|(_, v)| v.clone())
                        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                })
                .unwrap_or_else(|| item.url.clone());
            if !(open_url.starts_with("http://") || open_url.starts_with("https://")) {
                return Err("No web page address recorded for this download".into());
            }
            let needs_pause = matches!(item.status, Status::Downloading | Status::Queued);
            g.refresh_target = Some(RefreshTarget {
                item_id: id,
                filename: item.filename.clone(),
                size: item.size,
                started_at: Instant::now(),
            });
            (item.filename, open_url, needs_pause)
        };
        if needs_pause {
            self.pause(&[id]);
        }
        self.app
            .opener()
            .open_url(&open_url, None::<&str>)
            .map_err(|e| e.to_string())?;
        let _ = self.app.emit(
            "refresh-state",
            serde_json::json!({ "itemId": id, "filename": filename, "status": "waiting", "pageUrl": open_url }),
        );
        Ok(open_url)
    }

    pub fn cancel_refresh(&self) {
        self.inner.lock().unwrap().refresh_target = None;
        let _ = self.app.emit("refresh-state", serde_json::Value::Null);
    }

    /// Quit, sleep or shut down, with a notice first.
    fn power(&self, kind: &str) {
        use tauri_plugin_notification::NotificationExt;
        crate::log::line(&format!("all downloads finished: {kind}"));
        if kind == "close" {
            self.app.exit(0);
            return;
        }
        let body = if kind == "sleep" { "Going to sleep in a few seconds." } else { "Shutting down in 60 seconds. To cancel, run: shutdown /a" };
        let _ = self.app.notification().builder().title("Downloads finished").body(body).show();
        let kind = kind.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(5));
            let mut cmd;
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd = if kind == "sleep" {
                    let mut c = std::process::Command::new("rundll32.exe");
                    c.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
                    c
                } else {
                    let mut c = std::process::Command::new("shutdown");
                    c.args(["/s", "/t", "60"]);
                    c
                };
                cmd.creation_flags(0x0800_0000);
            }
            #[cfg(target_os = "macos")]
            {
                cmd = if kind == "sleep" {
                    let mut c = std::process::Command::new("pmset");
                    c.arg("sleepnow");
                    c
                } else {
                    let mut c = std::process::Command::new("osascript");
                    c.args(["-e", "tell app \"System Events\" to shut down"]);
                    c
                };
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                cmd = std::process::Command::new("systemctl");
                cmd.arg(if kind == "sleep" { "suspend" } else { "poweroff" });
            }
            if let Err(e) = cmd.spawn() {
                crate::log::line(&format!("could not {kind}: {e}"));
            }
        });
    }

    /// Starts the held queue once `start_at` arrives, and pauses running/queued
    /// downloads once `stop_at` arrives.
    fn check_schedule(self: &Arc<Self>) {
        let now = now_ms();
        let (started, to_pause) = {
            let mut g = self.inner.lock().unwrap();
            let mut started = false;
            if g.settings.start_at != 0 && now >= g.settings.start_at {
                g.settings.start_at = 0;
                started = true;
            }
            let mut to_pause = Vec::new();
            if g.settings.stop_at != 0 && now >= g.settings.stop_at {
                g.settings.stop_at = 0;
                to_pause = g
                    .items
                    .iter()
                    .filter(|i| matches!(i.status, Status::Downloading | Status::Queued))
                    .map(|i| i.id)
                    .collect();
            }
            (started, to_pause)
        };
        if started || !to_pause.is_empty() {
            let settings = self.inner.lock().unwrap().settings.clone();
            let _ = self.app.emit("settings", &settings);
            self.save();
            if !to_pause.is_empty() {
                crate::log::line(&format!("scheduled stop reached: pausing {} download(s)", to_pause.len()));
                self.pause(&to_pause);
            }
            if started && to_pause.is_empty() {
                self.schedule();
            }
        }
    }

    /// Copies live progress into items, computes speeds, and pushes a snapshot
    /// to open UI windows when something changed.
    pub fn tick(&self, n: u64) {
        let has_windows = !self.app.webview_windows().is_empty();
        let (snapshot, active) = {
            let mut g = self.inner.lock().unwrap();
            let Inner { items, running, dirty, .. } = &mut *g;
            let now = Instant::now();
            for (id, r) in running.iter_mut() {
                let done = r.progress.downloaded.load(Ordering::Relaxed);
                let dt = now.duration_since(r.last.1).as_secs_f64();
                if dt > 0.0 {
                    let instant = done.saturating_sub(r.last.0) as f64 / dt;
                    r.speed = if r.speed == 0.0 { instant } else { r.speed * 0.6 + instant * 0.4 };
                }
                r.last = (done, now);
                if let Some(item) = items.iter_mut().find(|i| i.id == *id) {
                    item.downloaded = done;
                    let total = r.progress.total.load(Ordering::Relaxed);
                    if total > 0 {
                        item.size = Some(total);
                    }
                    item.speed = r.speed;
                    item.connections = r.progress.connections.load(Ordering::Relaxed);
                    item.remaining = r.progress.remaining.lock().unwrap().clone();
                    item.segments = r.progress.segments.lock().unwrap().clone();
                    item.note = r.note.lock().unwrap().clone();
                }
            }
            let active = !running.is_empty();
            if !active && !*dirty {
                return;
            }
            *dirty = false;
            (has_windows.then(|| items.clone()), active)
        };
        if let Some(snapshot) = snapshot {
            let _ = self.app.emit("downloads", snapshot);
        }
        if active && n % 10 == 0 {
            self.save();
        }
    }

    fn changed(&self) {
        self.inner.lock().unwrap().dirty = true;
        self.wake.notify_one();
    }

    pub fn save(&self) {
        let (items, settings) = {
            let g = self.inner.lock().unwrap();
            (g.items.clone(), g.settings.clone())
        };
        let _ = save_json(&self.data_dir.join("downloads.json"), &items);
        let _ = save_json(&self.data_dir.join("settings.json"), &settings);
    }

    fn next_tick_wait(&self) -> Option<Duration> {
        let g = self.inner.lock().unwrap();
        compute_next_tick_wait(g.running.len(), g.dirty, g.settings.start_at, g.settings.stop_at, now_ms())
    }

    pub fn start_ticker(self: &Arc<Self>) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut n = 0u64;
            loop {
                match this.next_tick_wait() {
                    None => {
                        this.wake.notified().await;
                    }
                    Some(wait) if wait.is_zero() => {}
                    Some(wait) => {
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {}
                            _ = this.wake.notified() => {}
                        }
                    }
                }
                this.check_schedule();
                this.tick(n);
                n = n.wrapping_add(1);
            }
        });
        self.schedule();
    }

}

fn compute_next_tick_wait(running: usize, dirty: bool, start_at: u64, stop_at: u64, now: u64) -> Option<Duration> {
    if running > 0 {
        return Some(Duration::from_millis(500));
    }
    if dirty {
        return Some(Duration::ZERO);
    }
    [start_at, stop_at]
        .into_iter()
        .filter(|&t| t > 0)
        .min()
        .map(|target| Duration::from_millis(target.saturating_sub(now).min(60_000)))
}

fn prune_destroyed_window(windows: &mut HashMap<u64, String>, confirms: &mut HashMap<u64, Incoming>, label: &str) {
    windows.retain(|_, v| v != label);
    if let Some(token) = label.strip_prefix("new-").and_then(|s| s.parse::<u64>().ok()) {
        confirms.remove(&token);
    }
}

/// How a download is fetched.
enum Kind {
    File,
    /// An ftp:// address.
    Ftp,
    /// A video site, through yt-dlp, with this format.
    Media(String),
    /// A torrent or magnet link, with these files (empty = all).
    Torrent(Vec<usize>),
}

pub fn to_header_map(headers: &[(String, String)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            map.insert(k, v);
        }
    }
    map
}

pub const CATEGORIES: [&str; 8] = [
    "Games",
    "Video",
    "Music",
    "Programs",
    "Compressed",
    "Documents",
    "Images",
    "Other",
];

pub fn is_valid_category(c: &str) -> bool {
    CATEGORIES.contains(&c)
}

pub fn category_for(filename: &str) -> &'static str {
    let ext = Path::new(filename).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "nsp" | "xci" | "nsz" | "cia" | "3ds" | "nds" | "gba" | "gbc" | "gb" | "sfc" | "smc" | "nes" | "n64" | "z64"
        | "v64" | "gcm" | "wbfs" | "rvz" | "chd" | "cso" | "pbp" | "vpk" | "xbe" | "xex" | "wad" | "pak" | "vdf" | "rpf" => {
            "Games"
        }
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "iso" | "img" => "Compressed",
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "txt" | "rtf" | "epub" | "csv" | "md" => {
            "Documents"
        }
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "opus" | "wma" => "Music",
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "m4v" | "mpg" | "mpeg" | "3gp" | "ts" => "Video",
        "exe" | "msi" | "dmg" | "pkg" | "deb" | "rpm" | "apk" | "appimage" | "msix" | "bat" | "jar" => "Programs",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "heic" | "avif" => "Images",
        _ => "Other",
    }
}

/// Assigns a category by filename extension first, falling back to HTTP Content-Type
/// when the extension is missing or unrecognized.
pub fn category_for_with_mime(filename: &str, content_type: Option<&str>) -> &'static str {
    let by_ext = category_for(filename);
    if by_ext != "Other" {
        return by_ext;
    }
    let Some(raw) = content_type else {
        return "Other";
    };
    let mime = raw.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if mime.starts_with("video/") {
        "Video"
    } else if mime.starts_with("audio/") {
        "Music"
    } else if mime.starts_with("image/") {
        "Images"
    } else if matches!(
        mime.as_str(),
        "application/zip"
            | "application/x-zip-compressed"
            | "application/x-rar-compressed"
            | "application/vnd.rar"
            | "application/x-7z-compressed"
            | "application/gzip"
            | "application/x-gzip"
            | "application/x-tar"
            | "application/x-xz"
            | "application/x-bzip2"
            | "application/zstd"
            | "application/x-iso9660-image"
    ) {
        "Compressed"
    } else if matches!(
        mime.as_str(),
        "application/pdf"
            | "application/msword"
            | "application/rtf"
            | "application/epub+zip"
            | "text/plain"
            | "text/csv"
            | "text/markdown"
    ) || mime.starts_with("application/vnd.openxmlformats-officedocument.")
        || mime.starts_with("application/vnd.ms-")
        || mime.starts_with("application/vnd.oasis.opendocument.")
    {
        "Documents"
    } else if matches!(
        mime.as_str(),
        "application/vnd.microsoft.portable-executable"
            | "application/x-msdownload"
            | "application/x-msi"
            | "application/x-apple-diskimage"
            | "application/vnd.debian.binary-package"
            | "application/x-rpm"
            | "application/vnd.android.package-archive"
            | "application/x-executable"
    ) {
        "Programs"
    } else {
        "Other"
    }
}

/// Precedence: explicit `settings.category_dirs[category]` > `download_dir/<Category>`
/// (when `sort_into_categories` and not `"Other"`) > `download_dir`.
pub fn default_dir_for_settings(s: &Settings, category: &str) -> PathBuf {
    if let Some(custom) = s.category_dirs.get(category).filter(|p| !p.as_os_str().is_empty()) {
        return custom.clone();
    }
    if s.sort_into_categories && category != "Other" {
        s.download_dir.join(category)
    } else {
        s.download_dir.clone()
    }
}

#[allow(dead_code)]
pub fn is_expired_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("403")
        || lower.contains("410")
        || lower.contains("web page")
        || lower.contains("expired")
}

/// Validates that a newly captured download matches the partial file being refreshed.
pub fn matches_refresh_target(
    target_filename: &str,
    target_size: Option<u64>,
    incoming_filename: Option<&str>,
    incoming_size: Option<u64>,
) -> std::result::Result<(), String> {
    if let Some(expected_size) = target_size.filter(|&n| n > 0) {
        if let Some(got_size) = incoming_size {
            if got_size != expected_size {
                return Err(format!(
                    "The refreshed file size ({got_size} bytes) doesn't match the partial file ({expected_size} bytes)."
                ));
            }
            return Ok(());
        }
    }
    let Some(got_name) = incoming_filename.map(str::trim).filter(|n| !n.is_empty()) else {
        return Err("Couldn't verify that the new download matches the partial file's name or size.".into());
    };
    if got_name.eq_ignore_ascii_case(target_filename.trim()) {
        Ok(())
    } else {
        Err(format!("Captured '{got_name}', expected '{target_filename}'."))
    }
}

fn remove_temp_files(path: &Path) {
    crate::media::cleanup(path);
    let _ = std::fs::remove_file(motlydl_engine::part_path(path));
    let mut state = path.as_os_str().to_owned();
    state.push(".motlydl");
    let _ = std::fs::remove_file(PathBuf::from(state));
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod checksum_tests {
    use crate::checksum::{Algo, hash_file};

    #[test]
    fn hashes_a_file() {
        let p = std::env::temp_dir().join(format!("motlydl-hash-test-{}.txt", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(hash_file(&p, Algo::Md5).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hash_file(&p, Algo::Sha1).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hash_file(&p, Algo::Sha256).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hash_file(&p, Algo::Sha512).unwrap(),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod category_and_refresh_tests {
    use super::*;

    #[test]
    fn categorizes_by_extension_and_mime_fallback() {
        assert_eq!(category_for("game.nsp"), "Games");
        assert_eq!(category_for_with_mime("download.bin", Some("application/zip; charset=binary")), "Compressed");
        assert_eq!(category_for_with_mime("stream", Some("video/mp4")), "Video");
        assert_eq!(category_for_with_mime("manual", Some("application/pdf")), "Documents");
        assert_eq!(category_for_with_mime("photo.png", Some("application/octet-stream")), "Images");
    }

    #[test]
    fn default_dir_respects_custom_category_folder_precedence() {
        let mut s = Settings::default();
        s.download_dir = PathBuf::from("/dl");
        s.sort_into_categories = true;
        assert_eq!(default_dir_for_settings(&s, "Games"), PathBuf::from("/dl/Games"));
        assert_eq!(default_dir_for_settings(&s, "Other"), PathBuf::from("/dl"));
        s.category_dirs.insert("Games".into(), PathBuf::from("/ssd/Games"));
        assert_eq!(default_dir_for_settings(&s, "Games"), PathBuf::from("/ssd/Games"));
    }

    #[test]
    fn validates_refresh_matching_and_expired_errors() {
        assert!(is_expired_error("server returned 403 Forbidden"));
        assert!(is_expired_error("server returned 410 Gone"));
        assert!(is_expired_error("This link opens a web page, not a file"));
        assert!(!is_expired_error("connection timed out"));

        assert!(matches_refresh_target("ubuntu.iso", Some(1024), Some("different.iso"), Some(1024)).is_ok());
        assert!(matches_refresh_target("ubuntu.iso", Some(1024), Some("ubuntu.iso"), Some(2048)).is_err());
        assert!(matches_refresh_target("ubuntu.iso", Some(1024), Some("ubuntu.iso"), None).is_ok());
        assert!(matches_refresh_target("ubuntu.iso", None, Some("UBUNTU.ISO"), None).is_ok());
        assert!(matches_refresh_target("ubuntu.iso", None, Some("other.iso"), None).is_err());
    }
}

/// The quality a default means: "audio" is sound only, "best" the first (highest)
/// video, "high" the best up to 1080p that isn't HDR.
fn pick_quality(qs: &[crate::media::Quality], mode: &str) -> Option<usize> {
    let height = |q: &crate::media::Quality| q.label.split(|c: char| !c.is_ascii_digit()).next().and_then(|d| d.parse::<u32>().ok());
    let video = |q: &crate::media::Quality| q.label != "Audio only";
    match mode {
        "audio" => qs.iter().position(|q| q.label == "Audio only"),
        "best" => qs.iter().position(video),
        _ => qs.iter().position(|q| video(q) && !q.label.contains("HDR") && height(q).is_some_and(|h| h <= 1080)).or_else(|| qs.iter().position(video)),
    }
}

/// A file name from a video's title: no characters Windows refuses, no extension left over.
pub fn clean_name(title: &str) -> String {
    let t = title.trim();
    let t = match t.rsplit_once('.') {
        Some((stem, ext)) if (2..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && !stem.is_empty() => stem,
        _ => t,
    };
    let cleaned: String = t.chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { ' ' } else { c }).collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = cleaned.chars().take(120).collect();
    let cut = cut.trim().trim_end_matches('.').to_string();
    if cut.is_empty() { "video".into() } else { cut }
}

#[cfg(test)]
mod video_default_tests {
    use super::*;
    use crate::media::Quality;

    fn q(label: &str, ext: &str) -> Quality {
        Quality { label: label.into(), format: format!("f-{label}"), ext: ext.into(), size: None }
    }

    #[test]
    fn picks_by_default() {
        let qs = [q("2160p HDR", "mp4"), q("2160p", "mp4"), q("1080p60", "mp4"), q("1080p HDR", "mp4"), q("720p", "mp4"), q("Audio only", "m4a")];
        assert_eq!(pick_quality(&qs, "high"), Some(2), "best up to 1080p, never HDR by default");
        assert_eq!(pick_quality(&qs, "best"), Some(0));
        assert_eq!(pick_quality(&qs, "audio"), Some(5));
        assert_eq!(pick_quality(&[q("4320p", "mp4")], "high"), Some(0), "nothing at or under 1080p: take the first video");
        assert_eq!(pick_quality(&[q("720p", "mp4")], "audio"), None);
    }

    #[test]
    fn names_from_titles() {
        assert_eq!(clean_name("Noor, Khan - Aarzu (Official Music Video).mp4"), "Noor, Khan - Aarzu (Official Music Video)");
        assert_eq!(clean_name("a/b: c*d?"), "a b c d");
        assert_eq!(clean_name("Version 2.0"), "Version 2.0", "a long tail after the dot isn't an extension");
        assert_eq!(clean_name("  "), "video");
    }
}

#[cfg(test)]
mod background_opt_tests {
    use super::*;

    #[test]
    fn computes_adaptive_tick_waits() {
        assert_eq!(compute_next_tick_wait(0, false, 0, 0, 1_000), None);
        assert_eq!(compute_next_tick_wait(0, true, 0, 0, 1_000), Some(Duration::ZERO));
        assert_eq!(compute_next_tick_wait(1, false, 0, 0, 1_000), Some(Duration::from_millis(500)));
        assert_eq!(compute_next_tick_wait(1, true, 5_000, 9_000, 1_000), Some(Duration::from_millis(500)));
        assert_eq!(compute_next_tick_wait(0, false, 6_000, 0, 1_000), Some(Duration::from_secs(5)));
        assert_eq!(compute_next_tick_wait(0, false, 600_000, 0, 1_000), Some(Duration::from_secs(60)));
        assert_eq!(compute_next_tick_wait(0, false, 90_000, 11_000, 1_000), Some(Duration::from_secs(10)));
        assert_eq!(compute_next_tick_wait(0, false, 500, 0, 1_000), Some(Duration::ZERO));
    }

    #[test]
    fn prunes_destroyed_window_maps() {
        let mut windows = HashMap::from([(1, "dl-1".into()), (2, "dl-2".into())]);
        let mut confirms = HashMap::from([(42, Incoming::default()), (43, Incoming::default())]);
        prune_destroyed_window(&mut windows, &mut confirms, "dl-1");
        prune_destroyed_window(&mut windows, &mut confirms, "new-42");
        assert!(!windows.contains_key(&1));
        assert!(windows.contains_key(&2));
        assert!(!confirms.contains_key(&42));
        assert!(confirms.contains_key(&43));
    }
}
