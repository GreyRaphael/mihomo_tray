//! Portable runtime state persistence (`.tray-state.json`) across app restarts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::text::STATE_FILE_NAME;

/// User-facing runtime toggles remembered across `mihomo-tray` restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PersistedState {
    pub system_proxy: bool,
    pub mode: Option<String>,
    pub tun: Option<bool>,
}

fn state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(STATE_FILE_NAME)
}

pub fn load(home_dir: &Path) -> Option<PersistedState> {
    let text = std::fs::read_to_string(state_path(home_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(home_dir: &Path, state: &PersistedState) {
    crate::config::write_json_pretty(&state_path(home_dir), state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_persisted_state_and_handles_missing_file() {
        let dir = crate::config::test_temp_dir("state_roundtrip");
        assert_eq!(load(&dir), None);

        let st = PersistedState {
            system_proxy: true,
            mode: Some("global".to_string()),
            tun: Some(false),
        };
        save(&dir, &st);
        assert_eq!(load(&dir), Some(st));

        std::fs::write(state_path(&dir), "{not json").unwrap();
        assert_eq!(load(&dir), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
