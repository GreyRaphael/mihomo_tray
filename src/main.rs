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
use std::time::{Duration, Instant};

use file_rotate::{ContentLimit, FileRotate, compression::Compression, suffix::AppendCount};
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

    let after_restart_pid = parse_after_restart_pid();
    if let Some(old_pid) = after_restart_pid {
        let mut inspector = process::ProcessInspector::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && inspector.is_pid_alive(old_pid) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let instance = acquire_single_instance(text::APP_NAME, after_restart_pid.is_some());
    let Some(instance) = instance else {
        return;
    };

    let settings_path = tray_cfg::settings_path(&portable_root);
    tray_cfg::ensure_sample_file(&settings_path);
    let (settings, startup_error) = match tray_cfg::load(&settings_path) {
        Ok(s) => (s, None),
        Err(err) => {
            let mut defaults = tray_cfg::TraySettings::default();
            // Do not auto-launch a default core when user settings failed to parse.
            defaults.mihomo.auto_start = false;
            (defaults, Some(err))
        }
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
    let should_restart = app.should_restart();
    drop(instance);
    if should_restart {
        let args = vec![
            platform::AFTER_RESTART_FLAG.to_string(),
            std::process::id().to_string(),
        ];
        if let Err(err) = platform::spawn_detached_self(&args) {
            platform::notify_error(text::APP_NAME, &err);
        }
    }
}

fn parse_after_restart_pid() -> Option<u32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(platform::AFTER_RESTART_FLAG) {
        return args.get(2).and_then(|s| s.parse::<u32>().ok());
    }
    None
}

fn acquire_single_instance(
    instance_key: &str,
    retry_after_restart: bool,
) -> Option<SingleInstance> {
    let deadline = if retry_after_restart {
        Instant::now() + Duration::from_secs(3)
    } else {
        Instant::now()
    };

    loop {
        match SingleInstance::new(instance_key) {
            Ok(inst) if inst.is_single() => return Some(inst),
            Ok(inst) => {
                drop(inst);
                if Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                platform::notify_error(text::APP_NAME, text::ERR_ALREADY_RUNNING);
                return None;
            }
            Err(err) => {
                platform::notify_error(
                    text::APP_NAME,
                    &format!("Single-instance check failed: {err}"),
                );
                std::process::exit(1);
            }
        }
    }
}

const MAX_LOG_BYTES: usize = 2 * 1024 * 1024;

fn init_logging(portable_root: &Path) -> PathBuf {
    let logs_dir = portable_root.join("logs");
    let log_file = logs_dir.join("mihomo-tray.log");
    if let Err(err) = std::fs::create_dir_all(&logs_dir) {
        eprintln!(
            "Failed to create log directory {}: {err}",
            logs_dir.display()
        );
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_target(false)
            .try_init();
        return log_file;
    }

    match OpenOptions::new().create(true).append(true).open(&log_file) {
        Ok(probe) => {
            drop(probe);
            let rotator = FileRotate::new(
                &log_file,
                AppendCount::new(1),
                ContentLimit::Bytes(MAX_LOG_BYTES),
                Compression::None,
                None,
            );
            let _ = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_target(false)
                .with_writer(Mutex::new(rotator))
                .try_init();
        }
        Err(err) => {
            eprintln!("Failed to open log file {}: {err}", log_file.display());
            let _ = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_target(false)
                .try_init();
        }
    }
    log_file
}
