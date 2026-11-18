//! Persisted toggle state and restore tracking for [`super::Worker`].

use std::path::Path;

use crate::config::state_store::{self, PersistedState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RestoreMode {
    #[default]
    Idle,
    Ready,
    WaitingSubReload,
}

#[derive(Default)]
pub(super) struct StateSync {
    persisted: Option<PersistedState>,
    mode: RestoreMode,
}

impl StateSync {
    pub fn load_initial(&mut self, home_dir: &Path) {
        self.persisted = state_store::load(home_dir);
        self.mode = if self.persisted.is_some() {
            RestoreMode::Ready
        } else {
            RestoreMode::Idle
        };
    }

    pub fn update(&mut self, home_dir: &Path, update: impl FnOnce(&mut PersistedState)) {
        let state = self.persisted.get_or_insert_with(PersistedState::default);
        update(state);
        state_store::save(home_dir, state);
    }

    pub fn capture(
        &self,
        controller_ok: bool,
        mode: &str,
        tun: bool,
        sysproxy: bool,
    ) -> PersistedState {
        if self.mode != RestoreMode::Idle {
            return self.persisted.clone().unwrap_or_default();
        }
        if controller_ok {
            PersistedState {
                system_proxy: sysproxy,
                mode: (!mode.is_empty()).then(|| mode.to_string()),
                tun: Some(tun),
            }
        } else {
            self.persisted.clone().unwrap_or_default()
        }
    }

    pub fn sync_live_if_idle(&mut self, home_dir: &Path, sysproxy: bool, mode: &str, tun: bool) {
        if self.mode != RestoreMode::Idle {
            return;
        }
        let live = PersistedState {
            system_proxy: sysproxy,
            mode: (!mode.is_empty()).then(|| mode.to_string()),
            tun: Some(tun),
        };
        if self.persisted.as_ref() != Some(&live) {
            self.update(home_dir, |s| *s = live);
        }
    }

    pub fn arm_ready_restore(&mut self, saved: PersistedState) {
        self.persisted = Some(saved);
        self.mode = RestoreMode::Ready;
    }

    pub fn wait_sub_reload(&mut self, saved: PersistedState) {
        self.persisted = Some(saved);
        self.mode = RestoreMode::WaitingSubReload;
    }

    pub fn finish_sub_reload(&mut self, should_restore: bool) {
        if self.mode == RestoreMode::WaitingSubReload {
            self.mode = if should_restore {
                RestoreMode::Ready
            } else {
                RestoreMode::Idle
            };
        }
    }

    pub fn take_ready_restore(&mut self) -> Option<PersistedState> {
        if self.mode == RestoreMode::Ready {
            self.mode = RestoreMode::Idle;
            self.persisted.clone()
        } else {
            None
        }
    }

    pub fn clear_pending(&mut self) {
        self.mode = RestoreMode::Idle;
    }

    pub fn has_persisted_system_proxy(&self) -> bool {
        self.persisted.as_ref().is_some_and(|s| s.system_proxy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_sub_reload_preserves_saved_state_and_user_updates() {
        let dir = crate::config::test_temp_dir("state_sync_sub_reload");
        let saved = PersistedState {
            system_proxy: true,
            mode: Some("global".to_string()),
            tun: Some(true),
        };
        let mut sync = StateSync::default();
        sync.wait_sub_reload(saved.clone());

        assert_eq!(sync.capture(true, "rule", false, false), saved);
        assert!(sync.take_ready_restore().is_none());

        // Live poll must not overwrite pending state, while explicit user toggles must win.
        sync.sync_live_if_idle(&dir, false, "rule", false);
        sync.update(&dir, |s| s.mode = Some("direct".to_string()));
        assert!(sync.take_ready_restore().is_none());

        sync.finish_sub_reload(true);
        let restored = sync.take_ready_restore().expect("ready restore");
        assert_eq!(restored.mode.as_deref(), Some("direct"));
        assert!(restored.system_proxy);
        assert_eq!(restored.tun, Some(true));

        sync.wait_sub_reload(saved);
        sync.finish_sub_reload(false);
        assert!(sync.take_ready_restore().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
