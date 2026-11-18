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

pub(crate) fn strip_utf8_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Writes `template` only when `path` is missing or an empty/whitespace-only UTF-8 file.
/// Non-UTF-8 files and transient read failures are preserved untouched.
pub(crate) fn ensure_template_file(path: &Path, template: &str) {
    let should_write = match std::fs::read(path) {
        Ok(bytes) => {
            bytes.is_empty()
                || std::str::from_utf8(&bytes).is_ok_and(|s| strip_utf8_bom(s).trim().is_empty())
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

#[cfg(test)]
pub(crate) fn test_temp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mihomo_tray_{tag}_{nanos}_{seq}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
