//! Persisted toggle state and unified restore tracking for [`super::Worker`].

use std::path::Path;

use crate::config::state_store::{self, PersistedState};
use crate::state::Snapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingRestore {
    Ready(PersistedState),
    Deferred {
        generation: u64,
        state: PersistedState,
    },
}

impl PendingRestore {
    fn state(&self) -> &PersistedState {
        match self {
            Self::Ready(state) | Self::Deferred { state, .. } => state,
        }
    }
}

#[derive(Default)]
pub(super) struct StateSync {
    persisted: Option<PersistedState>,
    pending: Option<PendingRestore>,
}

impl StateSync {
    pub fn load_initial(&mut self, home_dir: &Path) {
        self.persisted = state_store::load(home_dir);
        self.pending = self.persisted.clone().map(PendingRestore::Ready);
    }

    pub fn update(&mut self, home_dir: &Path, update: impl FnOnce(&mut PersistedState)) {
        let state = self.persisted.get_or_insert_with(PersistedState::default);
        update(state);
        state_store::save(home_dir, state);
    }

    pub fn capture(&self, snap: &Snapshot, sysproxy: bool) -> PersistedState {
        if let Some(pending) = &self.pending {
            return pending.state().clone();
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
        if self.pending.is_some() {
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

    pub fn set_ready_restore(&mut self, saved: PersistedState) {
        self.pending = Some(PendingRestore::Ready(saved));
    }

    pub fn defer_restore(&mut self, generation: u64, saved: PersistedState) {
        self.pending = Some(PendingRestore::Deferred {
            generation,
            state: saved,
        });
    }

    pub fn resolve_deferred(&mut self, generation: u64, should_restore: bool) {
        if matches!(
            self.pending,
            Some(PendingRestore::Deferred { generation: g, .. }) if g == generation
        ) {
            self.pending = match self.pending.take() {
                Some(PendingRestore::Deferred { state, .. }) if should_restore => {
                    Some(PendingRestore::Ready(state))
                }
                _ => None,
            };
        }
    }

    pub fn take_ready_restore(&mut self) -> Option<PersistedState> {
        if matches!(self.pending, Some(PendingRestore::Ready(_))) {
            match self.pending.take() {
                Some(PendingRestore::Ready(state)) => Some(state),
                _ => None,
            }
        } else {
            None
        }
    }

    pub fn clear_pending(&mut self) {
        self.pending = None;
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
        matches!(self.pending, Some(PendingRestore::Ready(_)))
    }

    #[cfg(test)]
    pub(super) fn has_deferred_restore(&self) -> bool {
        matches!(self.pending, Some(PendingRestore::Deferred { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_prefers_deferred_restore_over_reset_snapshot() {
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

        // Stale generation must not affect the current deferred state.
        sync.resolve_deferred(0, false);
        assert!(sync.has_deferred_restore());

        sync.resolve_deferred(1, false);
        assert!(!sync.has_ready_restore());
        assert!(!sync.has_deferred_restore());
    }
}
