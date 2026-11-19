//! Main thread GUI orchestration (`winit 0.30` ApplicationHandler + `tray-icon 0.26`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::config::mihomo;
use crate::config::tray::TraySettings;
use crate::core::worker::{self, WorkerHandle};
use crate::platform;
use crate::state::{AppEvent, SharedSnapshot, WorkerCommand};
use crate::tray::icon::{self, IconKey};
use crate::tray::menu::{self, MenuAction, MenuFingerprint};

const CLICK_DEBOUNCE: Duration = Duration::from_millis(300);

pub struct MihomoTrayApp {
    portable_root: PathBuf,
    settings_path: PathBuf,
    log_path: PathBuf,
    initial_settings: TraySettings,
    startup_error: Option<String>,
    shared: SharedSnapshot,
    proxy: EventLoopProxy<AppEvent>,
    worker: Option<WorkerHandle>,
    tray_icon: Option<TrayIcon>,
    actions: Vec<MenuAction>,
    menu_generation: u64,
    last_icon: Option<IconKey>,
    last_tooltip: String,
    last_menu_fingerprint: Option<MenuFingerprint>,
    pending_menu_rebuild: bool,
    last_click: Option<Instant>,
    version_dialog_open: Arc<AtomicBool>,
    prompt_dialog_open: Arc<AtomicBool>,
    delete_dialog_open: Arc<AtomicBool>,
    open_error_dialog_open: Arc<AtomicBool>,
    quitting: bool,
    restart_on_exit: bool,
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
            worker: None,
            tray_icon: None,
            actions: Vec::new(),
            menu_generation: 1,
            last_icon: None,
            last_tooltip: String::new(),
            last_menu_fingerprint: None,
            pending_menu_rebuild: false,
            last_click: None,
            version_dialog_open: Arc::new(AtomicBool::new(false)),
            prompt_dialog_open: Arc::new(AtomicBool::new(false)),
            delete_dialog_open: Arc::new(AtomicBool::new(false)),
            open_error_dialog_open: Arc::new(AtomicBool::new(false)),
            quitting: false,
            restart_on_exit: false,
        }
    }

    pub fn should_restart(&self) -> bool {
        self.restart_on_exit
    }

    fn init_tray_and_worker(&mut self) {
        if self.worker.is_none() {
            let handle = worker::spawn(
                self.portable_root.clone(),
                self.settings_path.clone(),
                self.initial_settings.clone(),
                self.shared.clone(),
                self.proxy.clone(),
                self.startup_error.take(),
            );
            self.worker = Some(handle);
        }

        if self.tray_icon.is_none() {
            let snap = self.shared.lock().unwrap().clone();
            let now_unix = menu::current_unix_secs();
            let built = menu::build(&snap, self.menu_generation, now_unix);
            self.actions = built.actions;
            self.last_menu_fingerprint = Some(MenuFingerprint::from_snapshot(&snap, now_unix));

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
        }
    }

    fn handle_tray_event(&mut self, event: &TrayIconEvent) {
        if self.quitting {
            return;
        }
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
        if self.quitting {
            return;
        }
        let Some((generation, idx)) = menu::parse_action_id(id_str) else {
            return;
        };
        if generation != self.menu_generation {
            tracing::debug!(
                "Ignoring stale menu event from generation {generation} (current {})",
                self.menu_generation
            );
            return;
        }
        let Some(action) = self.actions.get(idx).cloned() else {
            return;
        };

        match action {
            MenuAction::OpenWebUi => self.open_web_ui(),
            MenuAction::SetSubscription => self.prompt_and_set_subscription(),
            MenuAction::SwitchProfile(name) => self.send_worker(WorkerCommand::SwitchProfile(name)),
            MenuAction::UpdateSubscription(name) => {
                self.send_worker(WorkerCommand::UpdateSubscription(name));
            }
            MenuAction::CopyProfileUrl { name, url } => self.copy_profile_url(&name, &url),
            MenuAction::DeleteProfile(name) => self.confirm_and_delete_profile(name),
            MenuAction::OpenTrayConfig => {
                crate::config::tray::ensure_sample_file(&self.settings_path);
                self.open_target(self.settings_path.clone());
            }
            MenuAction::OpenMihomoConfig => {
                let target = self.resolved_config_or_default();
                mihomo::ensure_default_config(&target);
                self.open_target(target);
            }
            MenuAction::OpenPortableDir => {
                self.open_target(self.portable_root.clone());
            }
            MenuAction::OpenUiDir => {
                let ui_dir = self.portable_root.join("ui");
                let _ = std::fs::create_dir_all(&ui_dir);
                self.open_target(ui_dir);
            }
            MenuAction::OpenLogFile => {
                let target = if self.log_path.exists() {
                    self.log_path.clone()
                } else {
                    self.portable_root.join("logs")
                };
                self.open_target(target);
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
            MenuAction::CopyEnvVars => self.send_worker(WorkerCommand::CopyEnvVars),
            MenuAction::RestartCore => self.send_worker(WorkerCommand::RestartCore),
            MenuAction::RestartApp => self.begin_quit(true),
            MenuAction::ToggleAutostart => self.send_worker(WorkerCommand::ToggleAutostart),
            MenuAction::ShowVersion => self.show_version_dialog(),
            MenuAction::Quit => self.begin_quit(false),
        }
    }

    fn begin_quit(&mut self, restart_on_exit: bool) {
        if !self.quitting {
            self.quitting = true;
            self.restart_on_exit = restart_on_exit;
            self.send_worker(WorkerCommand::Quit { interactive: true });
        }
    }

    fn resolved_config_or_default(&self) -> PathBuf {
        self.shared
            .lock()
            .unwrap()
            .resolved_config
            .clone()
            .unwrap_or_else(|| {
                self.portable_root
                    .join("config")
                    .join(crate::text::MIHOMO_CONFIG_NAME)
            })
    }

    fn spawn_singleton_dialog(
        flag: &Arc<AtomicBool>,
        thread_name: &str,
        f: impl FnOnce() + Send + 'static,
    ) {
        if flag
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let open_flag = Arc::clone(flag);
        if std::thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                struct ResetFlag(Arc<AtomicBool>);
                impl Drop for ResetFlag {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _reset = ResetFlag(open_flag);
                f();
            })
            .is_err()
        {
            flag.store(false, Ordering::Release);
        }
    }

    fn show_version_dialog(&self) {
        let (core_ver, cfg_path) = {
            let snap = self.shared.lock().unwrap();
            (snap.version.clone(), snap.resolved_config.clone())
        };
        let portable = self.portable_root.clone();
        Self::spawn_singleton_dialog(&self.version_dialog_open, "version-dialog", move || {
            let msg = crate::text::format_version_dialog(&core_ver, &portable, cfg_path.as_deref());
            platform::notify_info(crate::text::TITLE_VERSION, &msg);
        });
    }

    fn prompt_and_set_subscription(&self) {
        let Some(tx) = self.worker.as_ref().map(|w| w.tx.clone()) else {
            return;
        };
        Self::spawn_singleton_dialog(&self.prompt_dialog_open, "sub-prompt", move || {
            if let Some((name, url)) = platform::prompt_subscription() {
                let _ = tx.send(WorkerCommand::SetSubscription { name, url });
            }
        });
    }

    fn confirm_and_delete_profile(&self, name: String) {
        let Some(tx) = self.worker.as_ref().map(|w| w.tx.clone()) else {
            return;
        };
        Self::spawn_singleton_dialog(&self.delete_dialog_open, "delete-profile", move || {
            let msg = crate::text::format_confirm_delete_profile(&name);
            if platform::confirm_yes_no(crate::text::TITLE_DELETE_PROFILE, &msg) {
                let _ = tx.send(WorkerCommand::DeleteProfile(name));
            }
        });
    }

    fn copy_profile_url(&self, name: &str, url: &str) {
        if let Err(err) = platform::copy_to_clipboard(url) {
            self.spawn_notice_dialog(crate::text::MENU_COPY_PROFILE_URL.to_string(), err, true);
        } else {
            tracing::info!("Copied subscription URL for profile '{name}'");
        }
    }

    fn spawn_notice_dialog(&self, title: String, message: String, is_error: bool) {
        let _ = std::thread::Builder::new()
            .name("notice-dialog".to_string())
            .spawn(move || {
                if is_error {
                    platform::notify_error(&title, &message);
                } else {
                    platform::notify_info(&title, &message);
                }
            });
    }

    fn apply_snapshot(&mut self, rebuild_menu: bool) {
        if self.quitting {
            return;
        }
        let Some(tray) = self.tray_icon.as_ref() else {
            return;
        };
        // Never call `set_menu` while TrackPopupMenu is displaying the live HMENU,
        // otherwise Windows destroys the active popup menu mid-hover.
        let menu_open = rebuild_menu && platform::is_tray_menu_open();

        let (tooltip, icon_key, menu_rebuild) = {
            let snap = self.shared.lock().unwrap();
            let tooltip = snap.tooltip();
            let icon_key = IconKey {
                state: snap.operational_state(),
                mode: snap.proxy_mode(),
            };
            let menu_rebuild = if rebuild_menu {
                let now_unix = menu::current_unix_secs();
                let fp = MenuFingerprint::from_snapshot(&snap, now_unix);
                if self.last_menu_fingerprint.as_ref() != Some(&fp) {
                    if menu_open {
                        self.pending_menu_rebuild = true;
                        None
                    } else {
                        Some((now_unix, fp, snap.clone()))
                    }
                } else {
                    self.pending_menu_rebuild = false;
                    None
                }
            } else {
                None
            };
            (tooltip, icon_key, menu_rebuild)
        };

        if tooltip != self.last_tooltip {
            let _ = tray.set_tooltip(Some(&tooltip));
            self.last_tooltip = tooltip;
        }

        if self.last_icon != Some(icon_key)
            && let Ok(icon) = icon::render_icon(icon_key)
            && tray.set_icon(Some(icon)).is_ok()
        {
            self.last_icon = Some(icon_key);
        }

        if let Some((now_unix, fp, snap)) = menu_rebuild {
            let next_gen = self.menu_generation.wrapping_add(1).max(1);
            let built = menu::build(&snap, next_gen, now_unix);
            self.actions = built.actions;
            self.menu_generation = next_gen;
            tray.set_menu(Some(Box::new(built.menu)));
            self.last_menu_fingerprint = Some(fp);
            self.pending_menu_rebuild = false;
        }
    }

    fn flush_pending_menu(&mut self) {
        if !self.pending_menu_rebuild || self.quitting || platform::is_tray_menu_open() {
            return;
        }
        self.apply_snapshot(true);
    }

    fn open_web_ui(&self) {
        let url = self.shared.lock().unwrap().web_ui_url.clone();
        if let Some(url) = url {
            self.open_target(&url);
        }
    }

    fn open_target(&self, target: impl AsRef<std::ffi::OsStr>) {
        let target = target.as_ref();
        if let Err(err) = open::that_detached(target) {
            let display = target.to_string_lossy();
            let msg = crate::text::format_open_target_failed(&display, &err);
            tracing::warn!("{msg}");
            Self::spawn_singleton_dialog(
                &self.open_error_dialog_open,
                "open-err-dialog",
                move || {
                    platform::notify_error(crate::text::APP_NAME, &msg);
                },
            );
        }
    }

    fn send_worker(&self, cmd: WorkerCommand) {
        if let Some(worker) = &self.worker {
            worker.send(cmd);
        }
    }

    fn shutdown_on_exit(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            self.quitting = true;
            // Always enqueue a non-interactive Quit so exiting while a QuitCancelled
            // event is still pending in the UI queue never deadlocks `worker.join()`.
            worker.send(WorkerCommand::Quit { interactive: false });
            worker.join();
        }
        self.tray_icon.take();
    }

    fn debounce_pass(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last_click
            .is_some_and(|prev| now.duration_since(prev) < CLICK_DEBOUNCE)
        {
            return false;
        }
        self.last_click = Some(now);
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
            AppEvent::TrayIcon(ev) => self.handle_tray_event(&ev),
            AppEvent::Menu(ev) => self.handle_menu_event(ev.id.as_ref()),
            AppEvent::SnapshotUpdated { rebuild_menu } => self.apply_snapshot(rebuild_menu),
            AppEvent::ShowNotice {
                title,
                message,
                is_error,
            } => self.spawn_notice_dialog(title, message, is_error),
            AppEvent::QuitCompleted => {
                event_loop.exit();
            }
            AppEvent::QuitCancelled => {
                self.quitting = false;
                self.restart_on_exit = false;
                self.apply_snapshot(true);
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.flush_pending_menu();
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutdown_on_exit();
    }
}

impl Drop for MihomoTrayApp {
    fn drop(&mut self) {
        self.shutdown_on_exit();
    }
}
