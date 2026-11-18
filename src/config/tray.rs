//! Portable `tray.yml` loading and relative-path resolution.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::platform;
use crate::text::TRAY_FILE_NAME;

pub(crate) const CONTROLLER_TIMEOUT_MIN_MS: u32 = 200;
pub(crate) const CONTROLLER_TIMEOUT_MAX_MS: u32 = 30_000;
const MAX_PAGE_SIZE: usize = 1000;
const DEFAULT_SAMPLE: &str = include_str!("../../assets/tray.sample.yml");

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TraySettings {
    pub mihomo: MihomoSettings,
    pub controller: ControllerSettings,
    pub proxy: ProxySettings,
    pub groups: GroupsSettings,
    pub ui: UiSettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MihomoSettings {
    pub path: String,
    pub home: String,
    pub config: String,
    pub args: Vec<String>,
    pub auto_start: bool,
}

impl Default for MihomoSettings {
    fn default() -> Self {
        Self {
            path: String::new(),
            home: String::new(),
            config: String::new(),
            args: Vec::new(),
            auto_start: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ControllerSettings {
    pub address: String,
    pub secret: String,
    pub timeout_ms: u32,
}

impl Default for ControllerSettings {
    fn default() -> Self {
        Self {
            address: String::new(),
            secret: String::new(),
            timeout_ms: 2000,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub bypass: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GroupsSettings {
    pub order: Vec<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub page_size: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub external_ui: String,
    pub web_url: String,
    pub poll_interval_ms: u32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            external_ui: String::new(),
            web_url: String::new(),
            poll_interval_ms: 3000,
        }
    }
}

/// Always anchors `tray.yml` directly inside `portable_root`.
pub fn settings_path(portable_root: &Path) -> PathBuf {
    portable_root.join(TRAY_FILE_NAME)
}

/// Writes the commented sample configuration when `tray.yml` is missing or empty.
pub fn ensure_sample_file(path: &Path) {
    crate::config::ensure_template_file(path, DEFAULT_SAMPLE);
}

pub fn load(path: &Path) -> Result<TraySettings, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            parse(&content).map_err(|e| format!("failed to parse {}: {e}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(TraySettings::default()),
        Err(err) => Err(format!("failed to read {}: {err}", path.display())),
    }
}

fn parse(content: &str) -> Result<TraySettings, serde_yaml_ng::Error> {
    let clean = crate::config::strip_utf8_bom(content);
    let mut settings: TraySettings = if clean.trim().is_empty() {
        TraySettings::default()
    } else {
        serde_yaml_ng::from_str(clean)?
    };
    settings.controller.timeout_ms = settings
        .controller
        .timeout_ms
        .clamp(CONTROLLER_TIMEOUT_MIN_MS, CONTROLLER_TIMEOUT_MAX_MS);
    settings.ui.poll_interval_ms = settings.ui.poll_interval_ms.clamp(500, 60_000);
    settings.groups.page_size = settings.groups.page_size.min(MAX_PAGE_SIZE);
    Ok(settings)
}

/// Resolves a user-supplied path against `portable_root` when relative, expanding OS env vars first.
pub fn resolve_portable_path(portable_root: &Path, raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let expanded = platform::expand_env(trimmed);
    let candidate = PathBuf::from(&expanded);
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        portable_root.join(candidate)
    };
    Some(normalize_path(&joined))
}

/// Lexically cleans `.` and `..` components without requiring the path to already exist on disk.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                None | Some(Component::ParentDir | Component::Prefix(_)) => {
                    out.push(component.as_os_str());
                }
                Some(Component::RootDir) => {}
                Some(_) => {
                    out.pop();
                }
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sample_with_defaults() {
        let parsed = parse(DEFAULT_SAMPLE).expect("sample should parse");
        assert!(parsed.mihomo.auto_start);
        assert_eq!(parsed.controller.timeout_ms, 2000);
        assert_eq!(parsed.ui.poll_interval_ms, 3000);
        assert_eq!(parsed.groups.page_size, 0);
    }

    #[test]
    fn clamps_limits_and_handles_bom() {
        let raw = "\u{feff}controller:\n  timeout_ms: 50\ngroups:\n  page_size: 999999\nui:\n  poll_interval_ms: 100\n";
        let parsed = parse(raw).expect("parse with BOM");
        assert_eq!(parsed.controller.timeout_ms, 200);
        assert_eq!(parsed.groups.page_size, 1000);
        assert_eq!(parsed.ui.poll_interval_ms, 500);
    }

    #[test]
    fn resolves_relative_paths_against_portable_root() {
        let root = PathBuf::from("/opt/mihomo-portable");
        assert_eq!(
            resolve_portable_path(&root, "./core/../core/mihomo"),
            Some(PathBuf::from("/opt/mihomo-portable/core/mihomo"))
        );
        assert_eq!(
            resolve_portable_path(&root, "/abs/path/config.yaml"),
            Some(PathBuf::from("/abs/path/config.yaml"))
        );
        assert_eq!(resolve_portable_path(&root, "   "), None);
        assert_eq!(
            normalize_path(Path::new("../../core/mihomo")),
            PathBuf::from("../../core/mihomo")
        );
        assert_eq!(
            normalize_path(Path::new("a/../../b")),
            PathBuf::from("../b")
        );
        assert_eq!(
            normalize_path(Path::new("/../../a/b")),
            PathBuf::from("/a/b")
        );
    }

    #[test]
    fn ensure_sample_file_preserves_non_utf8_content() {
        let dir = crate::config::test_temp_dir("cfg_non_utf8");
        let path = dir.join("tray.yml");
        let raw_gbk = b"# \xd6\xd0\xce\xc4\nmihomo:\n  auto_start: false\n";
        std::fs::write(&path, raw_gbk).unwrap();

        ensure_sample_file(&path);
        assert_eq!(std::fs::read(&path).unwrap(), raw_gbk);

        let empty = dir.join("empty.yml");
        std::fs::write(&empty, "\u{feff}  \n").unwrap();
        ensure_sample_file(&empty);
        assert!(std::fs::read_to_string(&empty).unwrap().contains("mihomo:"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
