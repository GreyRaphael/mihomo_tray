//! Pure Windows helper logic extracted so host unit tests can verify it cross-platform.

use std::path::PathBuf;

/// Extracts the target executable path from a Scoop `.shim` file (`path = "..."`).
pub(super) fn parse_shim_path(content: &str) -> Option<PathBuf> {
    content
        .strip_prefix('\u{feff}')
        .unwrap_or(content)
        .lines()
        .find_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                return None;
            }
            let (k, v) = trimmed.split_once('=')?;
            if !k.trim().eq_ignore_ascii_case("path") {
                return None;
            }
            let val = v.trim_start();
            let raw_path = if let Some(quoted) = val.strip_prefix('"') {
                quoted.split('"').next()?
            } else {
                val.split('#').next()?.trim()
            };
            (!raw_path.is_empty()).then(|| PathBuf::from(raw_path))
        })
}

/// Computes an updated Windows `ProxyOverride` value if needed.
/// - When `extra_bypass` is empty and an entry already exists, returns `None` to preserve user settings.
/// - When `extra_bypass` is non-empty (or no entry exists yet), merges deduplicated entries and ensures `<local>` is included.
pub(super) fn merge_proxy_override(
    existing: Option<&str>,
    extra_bypass: &[String],
) -> Option<String> {
    let extras: Vec<&str> = extra_bypass
        .iter()
        .flat_map(|item| item.split(';'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if extras.is_empty() {
        return existing.is_none().then(|| "<local>".to_string());
    }

    let mut entries: Vec<String> = Vec::new();
    let mut push_unique = |entry: &str| {
        if entry.eq_ignore_ascii_case("<local>") {
            return;
        }
        if !entries.iter().any(|e| e.eq_ignore_ascii_case(entry)) {
            entries.push(entry.to_string());
        }
    };

    if let Some(raw) = existing {
        for part in raw.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            push_unique(part);
        }
    }
    for part in extras {
        push_unique(part);
    }
    entries.push("<local>".to_string());

    let merged = entries.join(";");
    if existing == Some(merged.as_str()) {
        None
    } else {
        Some(merged)
    }
}

/// Quotes arguments following Microsoft `CommandLineToArgvW` rules so paths with spaces,
/// backslashes, and UNC prefixes survive `ShellExecuteExW` verbatim.
pub(super) fn quote_win32_args(args: &[String]) -> String {
    let mut out = String::new();
    for (idx, arg) in args.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
            out.push_str(arg);
            continue;
        }
        out.push('"');
        let mut backslashes = 0usize;
        for ch in arg.chars() {
            if ch == '\\' {
                backslashes += 1;
            } else if ch == '"' {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            } else {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(ch);
            }
        }
        for _ in 0..(backslashes * 2) {
            out.push('\\');
        }
        out.push('"');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_win32_arguments_without_corrupting_backslashes() {
        let args = vec![
            "--elevate-start".to_string(),
            "1234".to_string(),
            "-d".to_string(),
            r"C:\My Portable Tools\mihomo tray\config".to_string(),
            r"\\server\share name\config.yaml".to_string(),
            r"C:\Trailing Space\".to_string(),
            String::new(),
        ];
        let joined = quote_win32_args(&args);
        assert_eq!(
            joined,
            r#"--elevate-start 1234 -d "C:\My Portable Tools\mihomo tray\config" "\\server\share name\config.yaml" "C:\Trailing Space\\" """#
        );
    }

    #[test]
    fn merges_proxy_override_only_when_seeding_or_bypass_configured() {
        assert_eq!(merge_proxy_override(None, &[]), Some("<local>".to_string()));
        assert_eq!(
            merge_proxy_override(Some("localhost;127.*;<local>"), &[]),
            None
        );
        assert_eq!(
            merge_proxy_override(
                Some("localhost;127.*;<local>"),
                &[" 10.* ".to_string(), "*.corp;127.*".to_string()]
            ),
            Some("localhost;127.*;10.*;*.corp;<local>".to_string())
        );
        assert_eq!(
            merge_proxy_override(
                Some("localhost;127.*;10.*;*.corp;<local>"),
                &["10.*".to_string(), "*.corp".to_string()]
            ),
            None
        );
    }

    #[test]
    fn parses_scoop_shim_paths_preserving_hash_inside_quotes() {
        let content = "\u{feff}# scoop shim\npath = \"C:\\Users\\me#1\\scoop\\mihomo.exe\" # note\nargs = -d .\n";
        assert_eq!(
            parse_shim_path(content),
            Some(PathBuf::from(r"C:\Users\me#1\scoop\mihomo.exe"))
        );
        assert_eq!(
            parse_shim_path("path = C:\\scoop\\mihomo.exe # comment\n"),
            Some(PathBuf::from(r"C:\scoop\mihomo.exe"))
        );
    }
}
