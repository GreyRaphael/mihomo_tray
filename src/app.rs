//! Main thread GUI orchestration (`winit 0.30` ApplicationHandler + `tray-icon 0.26`).

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::config::tray::TraySettings;
use crate::core::worker;
use crate::platform;
use crate::state::{AppEvent, SharedSnapshot, Snapshot, WorkerCommand};
use crate::tray::icon::{self, IconKey};
use crate::tray::menu::{self, MenuAction};

const CLICK_DEBOUNCE: Duration = Duration::from_millis(300);

pub struct MihomoTrayApp {
    portable_root: PathBuf,
    settings_path: PathBuf,
    log_path: PathBuf,
    initial_settings: TraySettings,
    startup_error: Option<String>,
    shared: SharedSnapshot,
    proxy: EventLoopProxy<AppEvent>,
    worker_tx: Option<Sender<WorkerCommand>>,
    tray_icon: Option<TrayIcon>,
    actions: Vec<MenuAction>,
    last_icon: Option<IconKey>,
    last_tooltip: String,
    last_click: Instant,
    quitting: bool,
}

impl MihomoTrayApp {
    pub fn new(
        portable_root: PathBuf,
        settings_path: PathBuf,
        log_path: PathBuf,
        initial_settings: TraySettings,
        startup_error: Option<String>,
        shared: SharedSnapshot,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Self {
        Self {
            portable_root,
            settings_path,
            log_path,
            initial_settings,
            startup_error,
            shared,
            proxy,
            worker_tx: None,
            tray_icon: None,
            actions: Vec::new(),
            last_icon: None,
            last_tooltip: String::new(),
            last_click: Instant::now() - CLICK_DEBOUNCE,
            quitting: false,
        }
    }

    fn init_tray_and_worker(&mut self) {
        if self.worker_tx.is_none() {
            let tx = worker::spawn(
                self.portable_root.clone(),
                self.settings_path.clone(),
                self.initial_settings.clone(),
                self.shared.clone(),
                self.proxy.clone(),
                self.startup_error.take(),
            );
            self.worker_tx = Some(tx);
        }

        if self.tray_icon.is_none() {
            let snap = self.shared.lock().unwrap().clone();
            let built = menu::build(&snap, snap.page_size);
            self.actions = built.actions;

            let icon_key = IconKey {
                state: snap.operational_state(),
                mode: snap.proxy_mode(),
            };
            let tooltip = snap.tooltip();

            let mut builder = TrayIconBuilder::new()
                .with_menu(Box::new(built.menu))
                .with_tooltip(&tooltip);

            if let Ok(icon) = icon::render_icon(icon_key) {
                builder = builder.with_icon(icon);
                self.last_icon = Some(icon_key);
            }

            #[cfg(not(target_os = "linux"))]
            {
                builder = builder.with_menu_on_left_click(false);
            }

            match builder.build() {
                Ok(tray) => {
                    self.tray_icon = Some(tray);
                    self.last_tooltip = tooltip;
                }
                Err(err) => {
                    tracing::warn!("Failed to create tray icon: {err}");
                }
            }
            platform::after_event_loop_ready();
        }
    }

    fn handle_tray_event(&mut self, event: TrayIconEvent) {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            if !self.debounce_pass() {
                return;
            }
            self.open_web_ui();
        }
    }

    fn handle_menu_event(&mut self, id_str: &str) {
        let Some(idx) = id_str
            .strip_prefix("act_")
            .and_then(|s| s.parse::<usize>().ok())
        else {
            return;
        };
        let Some(action) = self.actions.get(idx).cloned() else {
            return;
        };

        match action {
            MenuAction::OpenWebUi => self.open_web_ui(),
            MenuAction::OpenTrayConfig => {
                crate::config::tray::ensure_sample_file(&self.settings_path);
                self.open_target(&self.settings_path.display().to_string());
            }
            MenuAction::OpenMihomoConfig => {
                let target = self
                    .shared
                    .lock()
                    .unwrap()
                    .resolved_config
                    .clone()
                    .unwrap_or_else(|| self.portable_root.join("config"));
                let open_path = if target.exists() {
                    target
                } else {
                    target
                        .parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_else(|| self.portable_root.clone())
                };
                if !open_path.exists() {
                    let _ = std::fs::create_dir_all(&open_path);
                }
                self.open_target(&open_path.display().to_string());
            }
            MenuAction::OpenPortableDir => {
                self.open_target(&self.portable_root.display().to_string());
            }
            MenuAction::OpenUiDir => {
                let ui_dir = self.portable_root.join("ui");
                let _ = std::fs::create_dir_all(&ui_dir);
                self.open_target(&ui_dir.display().to_string());
            }
            MenuAction::OpenLogFile => {
                let target = if self.log_path.exists() {
                    self.log_path.clone()
                } else {
                    self.portable_root.join("logs")
                };
                self.open_target(&target.display().to_string());
            }
            MenuAction::SetMode(mode) => self.send_worker(WorkerCommand::SetMode(mode)),
            MenuAction::ToggleSystemProxy => self.send_worker(WorkerCommand::ToggleSystemProxy),
            MenuAction::ToggleTun => self.send_worker(WorkerCommand::ToggleTun),
            MenuAction::SelectProxy { group, member } => {
                self.send_worker(WorkerCommand::SelectProxy { group, member });
            }
            MenuAction::UnfixGroup(group) => self.send_worker(WorkerCommand::UnfixGroup(group)),
            MenuAction::TestGroupLatency(group) => {
                self.send_worker(WorkerCommand::TestGroupLatency(group));
            }
            MenuAction::RefreshProvider(name) => {
                self.send_worker(WorkerCommand::RefreshProvider(name));
            }
            MenuAction::StartCore => self.send_worker(WorkerCommand::StartCore),
            MenuAction::StopCore => self.send_worker(WorkerCommand::StopCore),
            MenuAction::RestartCore => self.send_worker(WorkerCommand::RestartCore),
            MenuAction::ForceRestartCore => self.send_worker(WorkerCommand::ForceRestartCore),
            MenuAction::ReloadConfig => self.send_worker(WorkerCommand::ReloadConfig),
            MenuAction::CloseConnections => self.send_worker(WorkerCommand::CloseConnections),
            MenuAction::RefreshNow => self.send_worker(WorkerCommand::RefreshNow),
            MenuAction::ToggleAutostart => self.send_worker(WorkerCommand::ToggleAutostart),
            MenuAction::Quit { stop_core } => {
                if !self.quitting {
                    self.quitting = true;
                    self.send_worker(WorkerCommand::Quit { stop_core });
                }
            }
        }
    }

    fn apply_snapshot(&mut self, rebuild_menu: bool) {
        if self.quitting {
            return;
        }
        let Some(tray) = self.tray_icon.as_ref() else {
            return;
        };
        let snap: Snapshot = self.shared.lock().unwrap().clone();

        let tooltip = snap.tooltip();
        if tooltip != self.last_tooltip {
            let _ = tray.set_tooltip(Some(&tooltip));
            self.last_tooltip = tooltip;
        }

        let icon_key = IconKey {
            state: snap.operational_state(),
            mode: snap.proxy_mode(),
        };
        if self.last_icon != Some(icon_key)
            && let Ok(icon) = icon::render_icon(icon_key)
            && tray.set_icon(Some(icon)).is_ok()
        {
            self.last_icon = Some(icon_key);
        }

        if rebuild_menu {
            let built = menu::build(&snap, snap.page_size);
            self.actions = built.actions;
            tray.set_menu(Some(Box::new(built.menu)));
        }
    }

    fn open_web_ui(&self) {
        let url = self.shared.lock().unwrap().web_ui_url.clone();
        if let Some(url) = url {
            self.open_target(&url);
        }
    }

    fn open_target(&self, target: &str) {
        if let Err(err) = open::that_detached(target) {
            let mut snap = self.shared.lock().unwrap();
            snap.action_error = Some(format!("Failed to open '{target}': {err}"));
            drop(snap);
            let _ = self
                .proxy
                .send_event(AppEvent::SnapshotUpdated { rebuild_menu: true });
        }
    }

    fn send_worker(&self, cmd: WorkerCommand) {
        if let Some(tx) = &self.worker_tx {
            let _ = tx.send(cmd);
        }
    }

    fn debounce_pass(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_click) < CLICK_DEBOUNCE {
            return false;
        }
        self.last_click = now;
        true
    }
}

impl ApplicationHandler<AppEvent> for MihomoTrayApp {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        self.init_tray_and_worker();
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::TrayIcon(ev) => self.handle_tray_event(ev),
            AppEvent::Menu(ev) => self.handle_menu_event(ev.id.as_ref()),
            AppEvent::SnapshotUpdated { rebuild_menu } => self.apply_snapshot(rebuild_menu),
            AppEvent::QuitCompleted => {
                self.tray_icon.take();
                event_loop.exit();
            }
        }
    }
}
