// Release builds are GUI apps: no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod batch;
mod browser;
mod checksum;
mod commands;
mod debrid;
mod extract;
mod ftp;
mod grabber;
mod log;
mod manager;
mod media;
mod metalink;
mod native_host;
mod rpc;
mod settings;
mod stream;
mod torrent;
mod update;
mod windows;

use manager::DownloadManager;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

static MAIN_CREATING: AtomicBool = AtomicBool::new(false);

struct ClearFlag(&'static AtomicBool);

impl Drop for ClearFlag {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let Some(mut cfg) = app.config().app.windows.first().cloned() else {
        return;
    };
    if MAIN_CREATING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return;
    }
    cfg.visible = true;
    let app = app.clone();
    std::thread::spawn(move || {
        let _guard = ClearFlag(&MAIN_CREATING);
        match tauri::WebviewWindowBuilder::from_config(&app, &cfg).and_then(|b| b.build()) {
            Ok(w) => {
                windows::polish(&w);
                let _ = w.show();
                let _ = w.set_focus();
            }
            Err(e) => crate::log::line(&format!("could not open main window: {e:#}")),
        }
    });
}

/// MotlyDL lives in the tray like IDM: closing the main window keeps
/// downloads (and browser integration) running.
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open MotlyDL", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit MotlyDL", true, None::<&str>)?;
    let nolimit = tauri::menu::CheckMenuItem::with_id(app, "nolimit", "Ignore speed limit", true, false, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &nolimit, &PredefinedMenuItem::separator(app)?, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main").tooltip("MotlyDL").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "open" => show_main_window(app),
        "nolimit" => {
            app.state::<Arc<DownloadManager>>().toggle_limit();
        }
        "quit" => {
            app.state::<Arc<DownloadManager>>().save();
            app.exit(0);
        }
        _ => {}
    })
    .on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
            show_main_window(tray.app_handle());
        }
    })
    .build(app)?;
    Ok(())
}

fn main() {
    log::install_panic_hook();
    let args: Vec<String> = std::env::args().collect();
    if native_host::is_native_host_launch(&args) {
        native_host::run();
        return;
    }

    tauri::Builder::default()
        // Must be first: a second launch (e.g. from the extension) hands its
        // args to this window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if let Some(inc) = native_host::parse_prepare_arg(&args) {
                app.state::<Arc<DownloadManager>>().prepare(&inc);
            } else if let Some(inc) = native_host::parse_add_arg(&args).or_else(|| torrent::parse_open_arg(&args)) {
                app.state::<Arc<DownloadManager>>().incoming(inc);
            } else {
                show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            log::line(&format!("MotlyDL {} starting on {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH));
            let data_dir = app.path().app_data_dir()?;
            let downloads = app.path().download_dir().unwrap_or_else(|_| data_dir.join("Downloads"));
            let mgr = DownloadManager::new(app.handle().clone(), data_dir.clone(), downloads)?;
            if let Err(e) = browser::register(&data_dir) {
                crate::log::line(&format!("browser integration not registered: {e:#}"));
            }
            if let Err(e) = torrent::register_handlers() {
                crate::log::line(&format!("magnet and .torrent handlers not registered: {e:#}"));
            }
            if let Err(e) = browser::offer_extension(mgr.settings().offer_extension) {
                crate::log::line(&format!("could not offer the browser extension: {e:#}"));
            }
            app.manage(mgr.clone());
            if let Some(main) = app.get_webview_window("main") {
                windows::polish(&main);
            }
            if let Err(e) = build_tray(app.handle()) {
                crate::log::line(&format!("tray icon not available: {e:#}"));
            }
            mgr.start_ticker();
            let api = mgr.settings();
            if api.api_enabled && !api.api_secret.is_empty() {
                // The network only gets in with a long secret.
                let lan = api.api_lan && api.api_secret.len() >= 12;
                if api.api_lan && !lan {
                    crate::log::line("local API: network access needs a secret of 12 or more characters; staying on this PC");
                }
                let addr = format!("{}:{}", if lan { "0.0.0.0" } else { "127.0.0.1" }, api.api_port);
                match rpc::serve(&addr, Arc::new(mgr.clone()), api.api_secret, lan) {
                    Ok(_) => crate::log::line(&format!("local API listening on {addr}")),
                    Err(e) => crate::log::line(&format!("local API couldn't start on {addr}: {e}")),
                }
            }
            // Launched by the browser extension: show only the small
            // download window, like IDM. Otherwise open the main window.
            // Started only to look a video up: stay out of sight (tray).
            if let Some(inc) = native_host::parse_prepare_arg(&args) {
                mgr.prepare(&inc);
                return Ok(());
            }
            match native_host::parse_add_arg(&args).or_else(|| torrent::parse_open_arg(&args)) {
                Some(inc) => mgr.incoming(inc),
                None => show_main_window(app.handle()),
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::probe_url,
            commands::probe_media,
            commands::probe_note,
            commands::add_links,
            commands::is_playlist_url,
            commands::probe_playlist,
            commands::add_playlist,
            commands::set_url,
            commands::start_refresh,
            commands::cancel_refresh,
            commands::grab_page,
            commands::add_metalink,
            commands::download_next,
            commands::open_in_browser,
            commands::is_media_url,
            commands::is_torrent_url,
            commands::probe_torrent,
            commands::open_new_download,
            commands::default_dir,
            commands::default_dir_for_category,
            commands::add_download,
            commands::pause,
            commands::resume,
            commands::remove,
            commands::open_file,
            commands::open_folder,
            commands::set_settings,
            commands::open_url,
            commands::extension_dir,
            commands::open_extension_folder,
            commands::open_extensions_page,
            commands::open_log,
            commands::pick_folder,
            commands::integration_status,
            commands::open_store,
            commands::get_item,
            commands::confirm_info,
            commands::forget_confirm,
            commands::show_main,
            commands::check_update,
            commands::install_update,
        ])
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { .. } if window.label() == "main" => {
                // Allow the main WebView to close and release its renderer/GPU memory;
                // `RunEvent::ExitRequested` keeps the tray process alive when `close_to_tray` is on.
                if let Some(mgr) = window.app_handle().try_state::<Arc<DownloadManager>>() {
                    mgr.save();
                }
            }
            tauri::WindowEvent::Destroyed => {
                if let Some(mgr) = window.app_handle().try_state::<Arc<DownloadManager>>() {
                    mgr.on_window_destroyed(window.label());
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while starting MotlyDL")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { code, api, .. } => {
                if code.is_none()
                    && (MAIN_CREATING.load(Ordering::Acquire)
                        || app.try_state::<Arc<DownloadManager>>().is_some_and(|m| m.settings().close_to_tray))
                {
                    api.prevent_exit();
                }
            }
            // macOS hands over magnet links and .torrent files as an event,
            // not as arguments.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Opened { urls } => {
                for url in urls {
                    let link = if url.scheme() == "file" {
                        url.to_file_path().ok().map(|p| p.to_string_lossy().into_owned())
                    } else {
                        Some(url.to_string())
                    };
                    if let (Some(link), Some(mgr)) = (link, app.try_state::<Arc<DownloadManager>>()) {
                        if torrent::is_torrent(&link, None) {
                            mgr.incoming(manager::Incoming { url: link, ..Default::default() });
                        }
                    }
                }
            }
            _ => {}
        });
}
