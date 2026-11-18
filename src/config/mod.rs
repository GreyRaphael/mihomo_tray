use std::io::ErrorKind;
use std::path::Path;

use serde::Serialize;

pub mod mihomo;
pub mod state_store;
pub mod tray;

pub(crate) fn write_json_pretty<T: Serialize>(path: &Path, value: &T) {
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("Failed to create directory {}: {err}", parent.display());
        return;
    }
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            if let Err(err) = std::fs::write(path, json) {
                tracing::warn!("Failed to write {}: {err}", path.display());
            }
        }
        Err(err) => {
            tracing::warn!("Failed to serialize {}: {err}", path.display());
        }
    }
}

/// Writes `template` only when `path` is missing or an empty/whitespace-only UTF-8 file.
/// Non-UTF-8 files and transient read failures are preserved untouched.
pub(crate) fn ensure_template_file(path: &Path, template: &str) {
    let should_write = match std::fs::read(path) {
        Ok(bytes) => {
            bytes.is_empty()
                || std::str::from_utf8(&bytes)
                    .is_ok_and(|s| s.trim_start_matches('\u{feff}').trim().is_empty())
        }
        Err(err) if err.kind() == ErrorKind::NotFound => true,
        Err(err) => {
            tracing::warn!(
                "Skipping template initialization for {} because reading failed: {err}",
                path.display()
            );
            false
        }
    };
    if !should_write {
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("Failed to create directory {}: {err}", parent.display());
        return;
    }
    if let Err(err) = std::fs::write(path, template) {
        tracing::warn!("Failed to initialize {}: {err}", path.display());
    }
}
