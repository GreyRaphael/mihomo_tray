#![windows_subsystem = "windows"]

mod app;
mod config;
mod core;
mod platform;
mod state;
mod text;
mod tray;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use single_instance::SingleInstance;
use tray_icon::TrayIconEvent;
use tray_icon::menu::MenuEvent;
use winit::event_loop::EventLoop;

use crate::app::MihomoTrayApp;
use crate::config::tray as tray_cfg;
use crate::core::process;
use crate::state::{AppEvent, Snapshot};

fn main() {
    // Handle elevated helper invocations before acquiring single-instance locks or GUI loops.
    if let Some(exit_code) =
        platform::try_run_helper(process::helper_stop_pid, process::helper_start_from_pid)
    {
        std::process::exit(exit_code);
    }

    let portable_root = platform::portable_root();
    let log_path = init_logging(&portable_root);

    let instance_key = single_instance_key(&portable_root);
    let instance = match SingleInstance::new(&instance_key) {
        Ok(inst) => inst,
        Err(err) => {
            platform::notify_error(
                text::APP_NAME,
                &format!("Single-instance check failed: {err}"),
            );
            std::process::exit(1);
        }
    };
    if !instance.is_single() {
        platform::notify_error(text::APP_NAME, text::ERR_ALREADY_RUNNING);
        return;
    }

    let settings_path = tray_cfg::settings_path(&portable_root);
    tray_cfg::ensure_sample_file(&settings_path);
    let (settings, startup_error) = match tray_cfg::load(&settings_path) {
        Ok(s) => (s, None),
        Err(err) => (tray_cfg::TraySettings::default(), Some(err)),
    };

    let shared = Arc::new(Mutex::new(Snapshot::default()));
    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .unwrap_or_else(|err| {
            platform::notify_error(
                text::APP_NAME,
                &format!("Failed to initialize event loop: {err}"),
            );
            std::process::exit(1);
        });

    let proxy = event_loop.create_proxy();

    let tray_proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = tray_proxy.send_event(AppEvent::TrayIcon(event));
    }));

    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(AppEvent::Menu(event));
    }));

    let mut app = MihomoTrayApp::new(
        portable_root,
        settings_path,
        log_path,
        settings,
        startup_error,
        shared,
        proxy,
    );

    if let Err(err) = event_loop.run_app(&mut app) {
        tracing::error!("Event loop terminated with error: {err}");
    }
    drop(instance);
}

fn init_logging(portable_root: &Path) -> PathBuf {
    let logs_dir = portable_root.join("logs");
    let _ = std::fs::create_dir_all(&logs_dir);
    let log_file = logs_dir.join("mihomo-tray.log");

    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&log_file) {
        let writer = Mutex::new(file);
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_target(false)
            .with_writer(writer)
            .try_init();
    }
    log_file
}

fn single_instance_key(portable_root: &Path) -> String {
    let normalized = portable_root.to_string_lossy().to_ascii_lowercase();
    // 64-bit FNV-1a hash keeps lock names unique per portable directory without extra crates.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalized.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("mihomo-tray-{hash:016x}")
}
