//! Configuration, portable path resolution, and autostart state tracking for [`super::Worker`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::config::tray::{self, TraySettings};
use crate::core::discover;
use crate::platform;

const AUTOSTART_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SettingsReload {
    pub changed: bool,
    pub controller_changed: bool,
}

pub(super) struct ConfigTracker {
    pub portable_root: PathBuf,
    pub settings_path: PathBuf,
    pub settings: TraySettings,
    pub last_settings_mtime: Option<SystemTime>,
    pub last_config_mtime: Option<SystemTime>,
    pub settings_error: Option<String>,
    pub kernel_path_error: Option<String>,
    pub known_kernel_path: Option<PathBuf>,
    pub home_dir: PathBuf,
    pub config_file: PathBuf,
    pub cached_autostart: bool,
    last_autostart_check: Option<Instant>,
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
        }
    }

    pub fn current_config_error(&self) -> Option<String> {
        self.settings_error
            .clone()
            .or_else(|| self.kernel_path_error.clone())
    }

    pub fn reload_settings_if_changed(&mut self) -> SettingsReload {
        if file_mtime(&self.settings_path) == self.last_settings_mtime {
            return SettingsReload {
                changed: false,
                controller_changed: false,
            };
        }
        self.load_settings_from_disk()
    }

    pub fn load_settings_from_disk(&mut self) -> SettingsReload {
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

    pub fn refresh_paths(&mut self) {
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
        let (home, cfg) = discover::resolve_home_and_config(
            &self.portable_root,
            &self.settings,
            self.known_kernel_path.as_deref(),
        );
        self.home_dir = home;
        self.config_file = cfg;
        self.last_config_mtime = file_mtime(&self.config_file);
    }

    pub fn config_file_changed(&mut self) -> bool {
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
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_parse_error_recovery_even_when_settings_match_defaults() {
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

        // Write valid empty YAML matching defaults -> must still report changed & clear error.
        std::fs::write(&settings_path, "").unwrap();
        tracker.last_settings_mtime = None;
        let reload = tracker.reload_settings_if_changed();
        assert!(reload.changed);
        assert!(reload.controller_changed);
        assert_eq!(tracker.current_config_error(), None);

        let _ = std::fs::remove_dir_all(&root);
    }
}
