use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Empty means "the system Downloads folder", filled in at startup.
    pub download_dir: PathBuf,
    /// Connections per download.
    pub connections: usize,
    /// Downloads running at the same time; the rest wait in the queue.
    pub max_concurrent: usize,
    /// Global limit across all downloads, KiB/s. 0 = unlimited.
    pub speed_limit_kib: u64,
    /// Save into Downloads\Video, Downloads\Music... by file type.
    pub sort_into_categories: bool,
    /// Show the Add dialog for downloads caught from the browser.
    pub ask_before_download: bool,
    pub notify_on_complete: bool,
    /// Ask installed browsers to offer the extension (they prompt once).
    pub offer_extension: bool,
    /// The user closed the "connect your browser" card.
    pub onboarding_dismissed: bool,
    /// Open a small progress window for each download as it starts.
    pub show_progress_window: bool,
    /// Closing the main window keeps MotlyDL running in the tray.
    pub close_to_tray: bool,
    /// "system", "light" or "dark".
    pub theme: String,
    /// After a torrent finishes: "stop", a share ratio such as "1" or "2",
    /// or "forever".
    pub seed_until: String,
    /// Once all downloads are done: "nothing", "close", "sleep" or "shutdown".
    /// Used once, then back to "nothing".
    pub when_done: String,
    /// Hold the queue until this time (ms since epoch). 0 = no schedule.
    pub start_at: u64,
    /// Stop/pause running and queued downloads at this time (ms since epoch). 0 = no schedule.
    pub stop_at: u64,
    /// Optional custom folder per category (overrides `download_dir/<Category>`).
    pub category_dirs: HashMap<String, PathBuf>,
    /// Proxy for downloads, e.g. "http://127.0.0.1:8080". Empty follows the
    /// system's. Downloads use a change after MotlyDL restarts.
    pub proxy: String,
    /// Send downloads out from this local address only (a VPN adapter's, say).
    pub bind_address: String,
    /// When switching to MotlyDL, offer to download a link on the clipboard.
    pub watch_clipboard: bool,
    /// Unpack finished .zip, .tar.gz and .7z files into a folder beside them.
    pub extract_archives: bool,
    /// Interface size in percent (90-150).
    pub ui_scale: u32,
    /// What a video link does: "ask" (the New download window), or start at once
    /// in "high" (best up to 1080p), "best" or "audio" (sound only).
    pub video_default: String,
    /// A local, aria2-compatible API for tools like AriaNg. Needs a restart.
    pub api_enabled: bool,
    pub api_port: u16,
    /// Every API call must carry this secret. The API stays off while it is empty.
    pub api_secret: String,
    /// Also let other devices on the local network reach the API and the web
    /// remote (a phone). Only used with a secret of 12 or more characters.
    pub api_lan: bool,
    /// Which debrid service the token is for: "realdebrid", "alldebrid" or "premiumize".
    pub debrid_provider: String,
    /// The service's API token. Empty = not used.
    pub debrid_token: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: PathBuf::new(),
            connections: 16,
            max_concurrent: 3,
            speed_limit_kib: 0,
            sort_into_categories: true,
            ask_before_download: true,
            notify_on_complete: true,
            offer_extension: true,
            onboarding_dismissed: false,
            show_progress_window: true,
            close_to_tray: true,
            theme: "system".into(),
            seed_until: "stop".into(),
            when_done: "nothing".into(),
            start_at: 0,
            stop_at: 0,
            category_dirs: HashMap::new(),
            proxy: String::new(),
            bind_address: String::new(),
            watch_clipboard: false,
            extract_archives: false,
            ui_scale: 100,
            video_default: "ask".into(),
            api_enabled: false,
            api_port: 6800,
            api_secret: String::new(),
            api_lan: false,
            debrid_provider: "realdebrid".into(),
            debrid_token: String::new(),
        }
    }
}

/// Loads `settings.json` from `dir`, falling back to defaults when missing or invalid.
pub fn load_from(dir: &Path) -> Settings {
    std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_proxy_and_bind_or_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("motlydl-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let missing = load_from(&dir);
        assert_eq!(missing.proxy, "");
        assert_eq!(missing.stop_at, 0);

        std::fs::write(
            dir.join("settings.json"),
            r#"{"proxy":"socks5h://127.0.0.1:1080","bindAddress":"10.0.0.2","stopAt":123456,"categoryDirs":{"Games":"/tmp/games"}}"#,
        )
        .unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.proxy, "socks5h://127.0.0.1:1080");
        assert_eq!(loaded.bind_address, "10.0.0.2");
        assert_eq!(loaded.stop_at, 123456);
        assert_eq!(loaded.category_dirs.get("Games").map(|p| p.to_string_lossy().into_owned()).as_deref(), Some("/tmp/games"));

        std::fs::write(dir.join("settings.json"), "{broken json").unwrap();
        assert_eq!(load_from(&dir).proxy, "");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
