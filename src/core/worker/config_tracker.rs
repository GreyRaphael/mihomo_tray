//! Configuration, portable path resolution, and autostart state tracking for [`super::Worker`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::config::tray::{self, TraySettings};
use crate::core::discover;
use crate::platform;

const AUTOSTART_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SettingsReload {
    changed: bool,
    controller_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PollChanges {
    pub settings_changed: bool,
    pub controller_changed: bool,
    pub config_file_changed: bool,
    pub error_changed: bool,
}

pub(super) struct ConfigTracker {
    portable_root: PathBuf,
    settings_path: PathBuf,
    settings: TraySettings,
    last_settings_mtime: Option<SystemTime>,
    last_config_mtime: Option<SystemTime>,
    settings_error: Option<String>,
    kernel_path_error: Option<String>,
    known_kernel_path: Option<PathBuf>,
    home_dir: PathBuf,
    config_file: PathBuf,
    cached_autostart: bool,
    last_autostart_check: Option<Instant>,
    pending_path_refresh: bool,
}

impl ConfigTracker {
    pub fn new(
        portable_root: PathBuf,
        settings_path: PathBuf,
        settings: TraySettings,
        startup_error: Option<String>,
    ) -> Self {
        let (home_dir, config_file) =
            discover::resolve_home_and_config(&portable_root, &settings, None);
        let last_settings_mtime = file_mtime(&settings_path);
        let last_config_mtime = file_mtime(&config_file);
        Self {
            portable_root,
            settings_path,
            settings,
            last_settings_mtime,
            last_config_mtime,
            settings_error: startup_error,
            kernel_path_error: None,
            known_kernel_path: None,
            home_dir,
            config_file,
            cached_autostart: false,
            last_autostart_check: None,
            pending_path_refresh: false,
        }
    }

    pub fn portable_root(&self) -> &Path {
        &self.portable_root
    }

    pub fn settings(&self) -> &TraySettings {
        &self.settings
    }

    pub fn known_kernel_path(&self) -> Option<&Path> {
        self.known_kernel_path.as_deref()
    }

    pub fn set_known_kernel_path_if_none(&mut self, path: PathBuf) {
        if self.known_kernel_path.is_none() {
            self.known_kernel_path = Some(path);
        }
    }

    pub fn kernel_path_error(&self) -> Option<&str> {
        self.kernel_path_error.as_deref()
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn cached_autostart(&self) -> bool {
        self.cached_autostart
    }

    pub fn current_config_error(&self) -> Option<String> {
        self.settings_error
            .clone()
            .or_else(|| self.kernel_path_error.clone())
    }

    pub fn poll_changes(&mut self, tracked_alive: bool) -> PollChanges {
        let prev_error = self.current_config_error();
        let mut settings_changed = false;
        let mut controller_changed = false;
        let mut path_switched = false;

        if file_mtime(&self.settings_path) != self.last_settings_mtime {
            let reload = self.load_settings_from_disk();
            settings_changed = reload.changed;
            controller_changed = reload.controller_changed;
            if settings_changed {
                self.pending_path_refresh = true;
            }
        }
        if self.pending_path_refresh && !tracked_alive {
            path_switched = self.refresh_paths();
        }

        let config_file_changed = path_switched || self.config_file_changed();
        let error_changed = self.current_config_error() != prev_error;
        PollChanges {
            settings_changed,
            controller_changed,
            config_file_changed,
            error_changed,
        }
    }

    pub fn reload_settings_and_paths(&mut self) {
        if file_mtime(&self.settings_path) != self.last_settings_mtime {
            let _ = self.load_settings_from_disk();
        }
        self.refresh_paths();
    }

    fn load_settings_from_disk(&mut self) -> SettingsReload {
        self.last_settings_mtime = file_mtime(&self.settings_path);
        let prev_settings = self.settings.clone();
        let prev_controller = self.settings.controller.clone();
        let had_error = self.settings_error.is_some();
        match tray::load(&self.settings_path) {
            Ok(reloaded) => {
                let changed = had_error || reloaded != prev_settings;
                let controller_changed = had_error || reloaded.controller != prev_controller;
                self.settings = reloaded;
                self.settings_error = None;
                SettingsReload {
                    changed,
                    controller_changed,
                }
            }
            Err(err) => {
                self.settings_error = Some(err);
                SettingsReload {
                    changed: false,
                    controller_changed: false,
                }
            }
        }
    }

    fn refresh_paths(&mut self) -> bool {
        self.pending_path_refresh = false;
        match discover::find_kernel(&self.portable_root, &self.settings) {
            Ok(path) => {
                self.known_kernel_path = path;
                self.kernel_path_error = None;
            }
            Err(err) => {
                self.known_kernel_path = None;
                self.kernel_path_error = Some(err);
            }
        }
        let prev_config = self.config_file.clone();
        let (home, cfg) = discover::resolve_home_and_config(
            &self.portable_root,
            &self.settings,
            self.known_kernel_path.as_deref(),
        );
        self.home_dir = home;
        self.config_file = cfg;
        let switched = self.config_file != prev_config;
        self.last_config_mtime = file_mtime(&self.config_file);
        switched
    }

    fn config_file_changed(&mut self) -> bool {
        let mtime = file_mtime(&self.config_file);
        if mtime != self.last_config_mtime {
            self.last_config_mtime = mtime;
            true
        } else {
            false
        }
    }

    pub fn refresh_autostart_cache(&mut self) {
        self.cached_autostart = platform::autostart_is_enabled();
        self.last_autostart_check = Some(Instant::now());
    }

    pub fn refresh_autostart_if_stale(&mut self) {
        if self
            .last_autostart_check
            .is_none_or(|t| t.elapsed() >= AUTOSTART_REFRESH_INTERVAL)
        {
            self.refresh_autostart_cache();
        }
    }

    #[cfg(test)]
    pub(super) fn invalidate_settings_mtime(&mut self) {
        self.last_settings_mtime = None;
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_parse_error_transitions_both_ways_even_with_defaults() {
        let root = crate::config::test_temp_dir("cfg_tracker_recover");
        let settings_path = tray::settings_path(&root);
        std::fs::write(&settings_path, "mihomo: [unclosed").unwrap();

        let mut tracker = ConfigTracker::new(
            root.clone(),
            settings_path.clone(),
            TraySettings::default(),
            Some("failed to parse tray.yml".to_string()),
        );
        assert!(tracker.current_config_error().is_some());

        // Write valid empty YAML matching defaults -> must report changed & clear error.
        std::fs::write(&settings_path, "").unwrap();
        tracker.invalidate_settings_mtime();
        let recover = tracker.poll_changes(false);
        assert!(recover.settings_changed);
        assert!(recover.controller_changed);
        assert!(recover.error_changed);
        assert_eq!(tracker.current_config_error(), None);

        // Break YAML again at runtime -> must surface `error_changed`.
        std::fs::write(&settings_path, "controller: [broken").unwrap();
        tracker.invalidate_settings_mtime();
        let broken = tracker.poll_changes(false);
        assert!(!broken.settings_changed);
        assert!(broken.error_changed);
        assert!(tracker.current_config_error().is_some());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn flushes_deferred_path_refresh_once_core_stops() {
        let root = crate::config::test_temp_dir("cfg_tracker_defer_paths");
        let settings_path = tray::settings_path(&root);
        std::fs::write(&settings_path, "").unwrap();
        let mut tracker = ConfigTracker::new(
            root.clone(),
            settings_path.clone(),
            TraySettings::default(),
            None,
        );

        std::fs::write(&settings_path, "mihomo:\n  config: ./alt/config.yaml\n").unwrap();
        tracker.invalidate_settings_mtime();
        let live_poll = tracker.poll_changes(true);
        assert!(live_poll.settings_changed);
        assert!(!live_poll.config_file_changed);
        assert_eq!(
            tracker.config_file(),
            root.join("config").join("config.yaml")
        );

        let stopped_poll = tracker.poll_changes(false);
        assert!(stopped_poll.config_file_changed);
        assert_eq!(tracker.config_file(), root.join("alt").join("config.yaml"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
