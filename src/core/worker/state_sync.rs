//! Persisted toggle state and deferred restore tracking for [`super::Worker`].

use std::path::Path;

use crate::config::state_store::{self, PersistedState};
use crate::platform;
use crate::state::Snapshot;

#[derive(Default)]
pub(super) struct StateSync {
    pub persisted: Option<PersistedState>,
    pub pending_restore: Option<PersistedState>,
    pub pending_reload_restore: Option<PersistedState>,
}

impl StateSync {
    pub fn load_initial(&mut self, home_dir: &Path) {
        self.persisted = state_store::load(home_dir);
        self.pending_restore = self.persisted.clone();
    }

    pub fn update(&mut self, home_dir: &Path, update: impl FnOnce(&mut PersistedState)) {
        let state = self.persisted.get_or_insert_with(PersistedState::default);
        update(state);
        state_store::save(home_dir, state);
    }

    pub fn capture_live_or_persisted(&self, snap: &Snapshot) -> PersistedState {
        if let Some(pending) = self
            .pending_restore
            .as_ref()
            .or(self.pending_reload_restore.as_ref())
        {
            return pending.clone();
        }
        if snap.controller_ok {
            PersistedState {
                system_proxy: platform::proxy_is_enabled(),
                mode: (!snap.mode.is_empty()).then(|| snap.mode.clone()),
                tun: Some(snap.tun),
            }
        } else {
            self.persisted.clone().unwrap_or_default()
        }
    }

    pub fn sync_live_if_idle(&mut self, home_dir: &Path, sysproxy: bool, mode: &str, tun: bool) {
        if self.pending_restore.is_some() || self.pending_reload_restore.is_some() {
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

    pub fn set_pending_restore(&mut self, saved: PersistedState) {
        self.pending_restore = Some(saved);
    }

    pub fn arm_reload_restore(&mut self, saved: PersistedState) {
        self.pending_reload_restore = Some(saved);
    }

    pub fn promote_reload_restore(&mut self, should_restore: bool) {
        let restore = self.pending_reload_restore.take();
        if should_restore && restore.is_some() {
            self.pending_restore = restore;
        }
    }

    pub fn take_pending_restore(&mut self) -> Option<PersistedState> {
        self.pending_restore.take()
    }

    pub fn clear_pending_restore(&mut self) {
        self.pending_restore = None;
    }

    pub fn has_persisted_system_proxy(&self) -> bool {
        self.persisted.as_ref().is_some_and(|s| s.system_proxy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_prefers_pending_reload_restore_over_reset_snapshot() {
        let saved = PersistedState {
            system_proxy: true,
            mode: Some("global".to_string()),
            tun: Some(true),
        };
        let mut sync = StateSync::default();
        sync.arm_reload_restore(saved.clone());

        let reset_snap = Snapshot {
            controller_ok: true,
            mode: "rule".to_string(),
            tun: false,
            ..Default::default()
        };
        assert_eq!(sync.capture_live_or_persisted(&reset_snap), saved);

        sync.promote_reload_restore(false);
        assert!(sync.pending_restore.is_none());
        assert!(sync.pending_reload_restore.is_none());
    }
}
