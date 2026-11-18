//! Persisted toggle state and unified restore tracking for [`super::Worker`].

use std::path::Path;

use crate::config::state_store::{self, PersistedState};
use crate::state::Snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreFlag {
    Ready,
    Deferred { generation: u64 },
}

#[derive(Default)]
pub(super) struct StateSync {
    persisted: Option<PersistedState>,
    restore: Option<RestoreFlag>,
}

impl StateSync {
    pub fn load_initial(&mut self, home_dir: &Path) {
        self.persisted = state_store::load(home_dir);
        self.restore = self.persisted.as_ref().map(|_| RestoreFlag::Ready);
    }

    pub fn update(&mut self, home_dir: &Path, update: impl FnOnce(&mut PersistedState)) {
        let state = self.persisted.get_or_insert_with(PersistedState::default);
        update(state);
        state_store::save(home_dir, state);
    }

    pub fn capture(&self, snap: &Snapshot, sysproxy: bool) -> PersistedState {
        if self.restore.is_some() {
            return self.persisted.clone().unwrap_or_default();
        }
        if snap.controller_ok {
            PersistedState {
                system_proxy: sysproxy,
                mode: (!snap.mode.is_empty()).then(|| snap.mode.clone()),
                tun: Some(snap.tun),
            }
        } else {
            self.persisted.clone().unwrap_or_default()
        }
    }

    pub fn sync_live_if_idle(&mut self, home_dir: &Path, sysproxy: bool, mode: &str, tun: bool) {
        if self.restore.is_some() {
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
        self.restore = Some(RestoreFlag::Ready);
    }

    pub fn defer_restore(&mut self, generation: u64, saved: PersistedState) {
        self.persisted = Some(saved);
        self.restore = Some(RestoreFlag::Deferred { generation });
    }

    pub fn resolve_deferred(&mut self, generation: u64, should_restore: bool) {
        if self.restore == Some(RestoreFlag::Deferred { generation }) {
            self.restore = should_restore.then_some(RestoreFlag::Ready);
        }
    }

    pub fn take_ready_restore(&mut self) -> Option<PersistedState> {
        if self.restore == Some(RestoreFlag::Ready) {
            self.restore = None;
            self.persisted.clone()
        } else {
            None
        }
    }

    pub fn clear_pending(&mut self) {
        self.restore = None;
    }

    pub fn has_persisted_system_proxy(&self) -> bool {
        self.persisted.as_ref().is_some_and(|s| s.system_proxy)
    }

    #[cfg(test)]
    pub(super) fn persisted(&self) -> Option<&PersistedState> {
        self.persisted.as_ref()
    }

    #[cfg(test)]
    pub(super) fn set_persisted_for_test(&mut self, state: PersistedState) {
        self.persisted = Some(state);
    }

    #[cfg(test)]
    pub(super) fn has_ready_restore(&self) -> bool {
        self.restore == Some(RestoreFlag::Ready)
    }

    #[cfg(test)]
    pub(super) fn has_deferred_restore(&self) -> bool {
        matches!(self.restore, Some(RestoreFlag::Deferred { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_prefers_deferred_restore_and_preserves_user_updates() {
        let dir = crate::config::test_temp_dir("state_sync_defer");
        let saved = PersistedState {
            system_proxy: true,
            mode: Some("global".to_string()),
            tun: Some(true),
        };
        let mut sync = StateSync::default();
        sync.defer_restore(1, saved.clone());

        let reset_snap = Snapshot {
            controller_ok: true,
            mode: "rule".to_string(),
            tun: false,
            ..Default::default()
        };
        assert_eq!(sync.capture(&reset_snap, false), saved);
        assert!(sync.take_ready_restore().is_none());

        // Live poll must not overwrite deferred state, while explicit user toggles must win.
        sync.sync_live_if_idle(&dir, false, "rule", false);
        sync.update(&dir, |s| s.mode = Some("direct".to_string()));

        // Stale generation must not affect the current deferred state.
        sync.resolve_deferred(0, false);
        assert!(sync.has_deferred_restore());

        sync.resolve_deferred(1, true);
        let restored = sync.take_ready_restore().expect("ready restore");
        assert_eq!(restored.mode.as_deref(), Some("direct"));
        assert!(restored.system_proxy);
        assert_eq!(restored.tun, Some(true));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
