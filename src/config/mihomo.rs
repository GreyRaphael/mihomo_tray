//! Lightweight extraction and subscription management for mihomo `config.yaml`.

use std::path::Path;
use std::str::FromStr;

use serde::Deserialize;
use yaml_edit::{Document, Mapping, MappingBuilder, Sequence, SequenceBuilder};

const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../../assets/config.sample.yaml");
pub(crate) const SUBSCRIPTION_PROVIDER_NAME: &str = "subscription";
const SUBSCRIPTION_INTERVAL_SECS: u64 = 43_200; // 12 hours
pub(crate) const SUBSCRIPTION_LOCAL_PATH: &str = "./providers/subscription.yaml";
pub(crate) const HEALTH_CHECK_URL: &str = "https://www.gstatic.com/generate_204";
const HEALTH_CHECK_INTERVAL_SECS: i64 = 300;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileEntry {
    pub name: String,
    pub url: String,
    pub path: String,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProfilesOverview {
    pub profiles: Vec<ProfileEntry>,
    pub active: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteProfileOutcome {
    pub deleted_path: Option<String>,
    pub next_active: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MihomoConfigInfo {
    pub external_controller: Option<String>,
    pub secret: Option<String>,
    pub external_ui: Option<String>,
    pub mixed_port: u16,
    pub other_controller_family: bool,
}

#[derive(Debug, Default, Deserialize)]
struct PartialMihomoYaml {
    #[serde(rename = "external-controller")]
    external_controller: Option<String>,
    #[serde(rename = "external-controller-tls")]
    external_controller_tls: Option<String>,
    #[serde(rename = "external-controller-unix")]
    external_controller_unix: Option<String>,
    #[serde(rename = "external-controller-pipe")]
    external_controller_pipe: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_scalar_string")]
    secret: Option<String>,
    #[serde(rename = "external-ui")]
    external_ui: Option<String>,
    #[serde(rename = "mixed-port")]
    mixed_port: Option<u16>,
    port: Option<u16>,
}

fn deserialize_opt_scalar_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ScalarOrString {
        Str(String),
        Int(i64),
        Uint(u64),
        Float(f64),
        Bool(bool),
    }

    Option::<ScalarOrString>::deserialize(deserializer).map(|opt| {
        opt.map(|v| match v {
            ScalarOrString::Str(s) => s,
            ScalarOrString::Int(i) => i.to_string(),
            ScalarOrString::Uint(u) => u.to_string(),
            ScalarOrString::Float(f) => f.to_string(),
            ScalarOrString::Bool(b) => b.to_string(),
        })
    })
}

pub fn read_file(path: &Path) -> Result<MihomoConfigInfo, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    try_parse_str(&text).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

fn try_parse_str(text: &str) -> Result<MihomoConfigInfo, String> {
    let clean = crate::config::strip_utf8_bom(text);
    let parsed: PartialMihomoYaml = if clean.trim().is_empty() {
        PartialMihomoYaml::default()
    } else {
        serde_yaml_ng::from_str(clean).map_err(|e| e.to_string())?
    };
    Ok(extract_config_info(parsed))
}

fn extract_config_info(parsed: PartialMihomoYaml) -> MihomoConfigInfo {
    let non_empty = |opt: Option<String>| -> Option<String> {
        opt.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    let other_family = parsed
        .external_controller_tls
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
        || parsed
            .external_controller_unix
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        || parsed
            .external_controller_pipe
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());

    let proxy_port = parsed
        .mixed_port
        .filter(|&p| p > 0)
        .or_else(|| parsed.port.filter(|&p| p > 0))
        .unwrap_or(0);

    MihomoConfigInfo {
        external_controller: non_empty(parsed.external_controller),
        secret: non_empty(parsed.secret),
        external_ui: non_empty(parsed.external_ui),
        mixed_port: proxy_port,
        other_controller_family: other_family,
    }
}

/// Writes the default `config/config.yaml` only if missing or empty.
pub fn ensure_default_config(path: &Path) {
    crate::config::ensure_template_file(path, DEFAULT_CONFIG_TEMPLATE);
}

/// Reads the currently configured legacy subscription URL (`proxy-providers.subscription.url`) if present.
pub fn read_subscription_url(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let clean = crate::config::strip_utf8_bom(&text);
    let doc = Document::from_str(clean).ok()?;
    doc.get_mapping("proxy-providers")?
        .get_mapping(SUBSCRIPTION_PROVIDER_NAME)?
        .get("url")?
        .as_scalar()
        .map(|s| s.as_string())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Reads all HTTP subscription profiles and the active profile directly from `config.yaml`.
#[allow(dead_code)]
pub fn read_profiles(path: &Path) -> ProfilesOverview {
    let Ok(text) = std::fs::read_to_string(path) else {
        return ProfilesOverview::default();
    };
    let clean = crate::config::strip_utf8_bom(&text);
    let Ok(doc) = Document::from_str(clean) else {
        return ProfilesOverview::default();
    };
    let Some(root_map) = doc.as_mapping() else {
        return ProfilesOverview::default();
    };
    extract_profiles(&root_map)
}

/// Legacy single-subscription entrypoint (replaced in Phase 3 by `upsert_profile`).
pub fn apply_subscription_url(path: &Path, raw_url: &str) -> Result<(), String> {
    upsert_profile(path, SUBSCRIPTION_PROVIDER_NAME, raw_url).map(|_| ())
}

/// Inserts or updates a named HTTP subscription profile under `proxy-providers.<name>`,
/// activates it exclusively across managed `proxy-groups`, and returns `(name, local_path)`.
pub fn upsert_profile(
    path: &Path,
    raw_name: &str,
    raw_url: &str,
) -> Result<(String, String), String> {
    let name = validate_profile_name(raw_name)?;
    let url = validate_subscription_url(raw_url)?;
    let local_path = profile_local_path(&name);

    edit_config_yaml(path, true, |root_map| {
        let mut managed = collect_http_profile_names(root_map);
        if !managed.iter().any(|n| n == &name) {
            managed.push(name.clone());
        }

        upsert_mapping(root_map, "proxy-providers", |providers_map| {
            upsert_mapping(providers_map, &name, |sub_map| {
                sub_map.set("type", "http");
                sub_map.set("url", url.as_str());
                sub_map.set("path", local_path.as_str());
                sub_map.set("interval", SUBSCRIPTION_INTERVAL_SECS as i64);
                upsert_mapping(sub_map, "health-check", |hc_map| {
                    hc_map.set("enable", true);
                    hc_map.set("url", HEALTH_CHECK_URL);
                    hc_map.set("interval", HEALTH_CHECK_INTERVAL_SECS);
                    hc_map.set("lazy", true);
                });
            });
        });

        sync_active_profile(root_map, &managed, Some(&name));
        Ok(())
    })?;

    Ok((name, local_path))
}

/// Activates an existing profile exclusively in `config.yaml` and returns its local cache path.
#[allow(dead_code)]
pub fn switch_profile(path: &Path, name: &str) -> Result<String, String> {
    edit_config_yaml(path, false, |root_map| {
        let overview = extract_profiles(root_map);
        let entry = overview
            .profiles
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("profile '{name}' not found in {}", path.display()))?;
        let local_path = entry.path.clone();
        let managed: Vec<String> = overview.profiles.into_iter().map(|p| p.name).collect();

        sync_active_profile(root_map, &managed, Some(name));
        Ok(local_path)
    })
}

/// Removes a profile from `config.yaml`, rewires groups to the next remaining profile if it was
/// active (or clears managed `use:` entries if it was the last), and returns cleanup metadata.
#[allow(dead_code)]
pub fn delete_profile(path: &Path, name: &str) -> Result<DeleteProfileOutcome, String> {
    edit_config_yaml(path, false, |root_map| {
        let overview = extract_profiles(root_map);
        let entry = overview
            .profiles
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("profile '{name}' not found in {}", path.display()))?;
        let deleted_path = Some(entry.path.clone());
        let managed: Vec<String> = overview.profiles.iter().map(|p| p.name.clone()).collect();

        let remaining: Vec<String> = overview
            .profiles
            .iter()
            .filter(|p| p.name != name)
            .map(|p| p.name.clone())
            .collect();
        let next_active = if overview.active.as_deref() == Some(name) {
            remaining.first().cloned()
        } else {
            overview.active.filter(|a| remaining.iter().any(|r| r == a))
        };

        if let Some(providers_map) = root_map.get_mapping("proxy-providers") {
            providers_map.remove(name);
            if providers_map.is_empty() {
                root_map.remove("proxy-providers");
            }
        }

        sync_active_profile(root_map, &managed, next_active.as_deref());
        Ok(DeleteProfileOutcome {
            deleted_path,
            next_active,
        })
    })
}

pub fn validate_profile_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with('.')
        || name.ends_with(' ')
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err(crate::text::ERR_INVALID_PROFILE_NAME.to_string());
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        return Err(crate::text::ERR_INVALID_PROFILE_NAME.to_string());
    }
    Ok(name.to_string())
}

fn validate_subscription_url(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    let valid_http =
        url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host());
    if !valid_http {
        return Err(crate::text::ERR_INVALID_SUBSCRIPTION_URL.to_string());
    }
    Ok(url.to_string())
}

fn profile_local_path(name: &str) -> String {
    format!("./providers/{name}.yaml")
}

fn edit_config_yaml<T>(
    path: &Path,
    seed_if_missing: bool,
    update: impl FnOnce(&Mapping) -> Result<T, String>,
) -> Result<T, String> {
    let source_text = match std::fs::read_to_string(path) {
        Ok(text) if !crate::config::strip_utf8_bom(&text).trim().is_empty() => text,
        Ok(_) if seed_if_missing => DEFAULT_CONFIG_TEMPLATE.to_string(),
        Ok(text) => text,
        Err(err) if seed_if_missing && err.kind() == std::io::ErrorKind::NotFound => {
            DEFAULT_CONFIG_TEMPLATE.to_string()
        }
        Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
    };
    let (bom, clean) = match source_text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", source_text.as_str()),
    };
    let doc = Document::from_str(clean)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    let root_map = doc
        .as_mapping()
        .ok_or_else(|| format!("{} root must be a YAML mapping", path.display()))?;

    let result = update(&root_map)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let out_yaml = format!("{bom}{doc}");
    std::fs::write(path, out_yaml)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(result)
}

fn extract_profiles(root_map: &Mapping) -> ProfilesOverview {
    let mut profiles = Vec::new();
    if let Some(providers_map) = root_map.get_mapping("proxy-providers") {
        for (key, val) in providers_map.iter() {
            let Some(name) = key.as_scalar().map(|s| s.as_string()) else {
                continue;
            };
            let Some(entry) = val.as_mapping() else {
                continue;
            };
            let is_http = scalar_str(entry, "type")
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("http"));
            let Some(url) = scalar_str(entry, "url")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if !is_http {
                continue;
            }
            let path = scalar_str(entry, "path")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| profile_local_path(&name));
            profiles.push(ProfileEntry { name, url, path });
        }
    }

    let active = find_active_profile(root_map, &profiles);
    ProfilesOverview { profiles, active }
}

fn collect_http_profile_names(root_map: &Mapping) -> Vec<String> {
    extract_profiles(root_map)
        .profiles
        .into_iter()
        .map(|p| p.name)
        .collect()
}

fn find_active_profile(root_map: &Mapping, profiles: &[ProfileEntry]) -> Option<String> {
    if profiles.is_empty() {
        return None;
    }
    let groups = root_map.get_sequence("proxy-groups")?;
    for group_val in groups.values() {
        let Some(group_map) = group_val.as_mapping() else {
            continue;
        };
        let Some(use_seq) = group_map.get_sequence("use") else {
            continue;
        };
        for item in use_seq.values() {
            if let Some(used) = item.as_scalar().map(|s| s.as_string())
                && profiles.iter().any(|p| p.name == used)
            {
                return Some(used);
            }
        }
    }
    None
}

fn upsert_mapping(parent: &Mapping, key: &str, update: impl FnOnce(&Mapping)) {
    if let Some(existing) = parent.get_mapping(key) {
        update(&existing);
        return;
    }
    let doc = Document::new_mapping();
    if let Some(map) = doc.as_mapping() {
        update(&map);
    }
    parent.set(key, &doc);
}

fn configure_default_proxy_group(builder: MappingBuilder, provider_name: &str) -> MappingBuilder {
    builder
        .pair("name", "Proxy")
        .pair("type", "select")
        .sequence("use", |u| u.item(provider_name))
}

fn sync_active_profile(root_map: &Mapping, managed_names: &[String], active: Option<&str>) {
    if let Some(providers_map) = root_map.get_mapping("proxy-providers") {
        for pname in managed_names {
            if let Some(sub_map) = providers_map.get_mapping(pname.as_str())
                && let Some(hc_map) = sub_map.get_mapping("health-check")
            {
                hc_map.set("enable", active == Some(pname.as_str()));
                hc_map.set("lazy", true);
            }
        }
    }

    let Some(groups) = root_map.get_sequence("proxy-groups") else {
        if let Some(active_name) = active {
            let fallback_groups = SequenceBuilder::new()
                .mapping(|b| configure_default_proxy_group(b, active_name))
                .build_document();
            root_map.set("proxy-groups", &fallback_groups);
        }
        return;
    };

    let mut wired_any = false;
    for group_val in groups.values() {
        let Some(group_map) = group_val.as_mapping() else {
            continue;
        };
        let name = scalar_str(group_map, "name").unwrap_or_default();
        let kind = scalar_str(group_map, "type").unwrap_or_default();

        let is_standard_group = matches!(
            name.as_str(),
            "Proxy" | "🚀Proxy" | "Auto" | "⚡Auto" | "Fallback" | "🚑Fallback" | "🤖AI"
        );
        let is_auto_select = matches!(name.as_str(), "Auto" | "⚡Auto" | "Fallback" | "🚑Fallback")
            && matches!(kind.as_str(), "url-test" | "fallback");
        let has_include_all =
            scalar_str(group_map, "include-all").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let uses_managed = group_map.get_sequence("use").is_some_and(|seq| {
            seq.values().any(|v| {
                v.as_scalar()
                    .is_some_and(|s| managed_names.iter().any(|m| m == &s.as_string()))
            })
        });

        if is_standard_group || has_include_all || uses_managed {
            if has_include_all {
                group_map.remove("include-all");
                group_map.set("include-all-proxies", true);
            }
            replace_managed_in_use(group_map, managed_names, active);
            if is_auto_select {
                if active.is_some() {
                    remove_single_direct_placeholder(group_map);
                } else if group_map.get_sequence("use").is_none()
                    && group_map.get_sequence("proxies").is_none()
                {
                    let direct = SequenceBuilder::new().item("DIRECT").build_document();
                    group_map.set("proxies", &direct);
                }
            }
            wired_any = true;
        }
    }

    if let Some(active_name) = active
        && !wired_any
    {
        // Fallback for custom configs: wire into first `select` group if present, else add `Proxy`.
        let mut attached_to_select = false;
        for group_val in groups.values() {
            if let Some(group_map) = group_val.as_mapping()
                && scalar_str(group_map, "type").as_deref() == Some("select")
            {
                replace_managed_in_use(group_map, managed_names, Some(active_name));
                attached_to_select = true;
                break;
            }
        }
        if !attached_to_select {
            let proxy_group =
                configure_default_proxy_group(MappingBuilder::new(), active_name).build_document();
            groups.insert(0, &proxy_group);
        }
    }
}

fn scalar_str(map: &Mapping, key: &str) -> Option<String> {
    map.get(key)?.as_scalar().map(|s| s.as_string())
}

fn replace_managed_in_use(group_map: &Mapping, managed_names: &[String], active: Option<&str>) {
    let mut preserved_custom = Vec::new();
    if let Some(seq) = group_map.get_sequence("use") {
        for v in seq.values() {
            if let Some(item) = v.as_scalar().map(|s| s.as_string())
                && !managed_names.iter().any(|m| m == &item)
                && active != Some(item.as_str())
                && !preserved_custom.iter().any(|p| p == &item)
            {
                preserved_custom.push(item);
            }
        }
    }

    let mut builder = SequenceBuilder::new();
    let mut count = 0usize;
    if let Some(active_name) = active {
        builder = builder.item(active_name);
        count += 1;
    }
    for custom in preserved_custom {
        builder = builder.item(custom);
        count += 1;
    }

    if count == 0 {
        group_map.remove("use");
    } else {
        let seq_doc = builder.build_document();
        group_map.set("use", &seq_doc);
    }
}

fn remove_single_direct_placeholder(group_map: &Mapping) {
    let should_remove = group_map
        .get_sequence("proxies")
        .is_some_and(|seq: Sequence| {
            seq.len() == 1
                && seq
                    .get(0)
                    .and_then(|v| v.as_scalar().map(|s| s.as_string()))
                    .as_deref()
                    == Some("DIRECT")
        });
    if should_remove {
        group_map.remove("proxies");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml_ng::Value;

    #[test]
    fn extracts_controller_secret_and_ui() {
        let yaml = r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
secret: "my#secret"
external-ui: ./ui
"#;
        let info = try_parse_str(yaml).unwrap();
        assert_eq!(info.external_controller.as_deref(), Some("127.0.0.1:9090"));
        assert_eq!(info.secret.as_deref(), Some("my#secret"));
        assert_eq!(info.external_ui.as_deref(), Some("./ui"));
        assert_eq!(info.mixed_port, 7890);
        assert!(!info.other_controller_family);

        let fallback_port = try_parse_str("port: 7891\n").unwrap();
        assert_eq!(fallback_port.mixed_port, 7891);

        let numeric_secret = try_parse_str("secret: 123456\n").unwrap();
        assert_eq!(numeric_secret.secret.as_deref(), Some("123456"));
    }

    #[test]
    fn detects_non_http_controller_families() {
        let yaml = "external-controller-unix: /tmp/mihomo.sock\n";
        let info = try_parse_str(yaml).unwrap();
        assert!(info.other_controller_family);
        assert!(info.external_controller.is_none());
    }

    #[test]
    fn writes_subscription_with_12h_interval_and_wires_groups() {
        let dir = crate::config::test_temp_dir("sub_test");
        let cfg = dir.join("config.yaml");

        apply_subscription_url(&cfg, "https://example.com/sub?token=abc").unwrap();
        assert_eq!(
            read_subscription_url(&cfg).as_deref(),
            Some("https://example.com/sub?token=abc")
        );

        let text = std::fs::read_to_string(&cfg).unwrap();
        let root: Value = serde_yaml_ng::from_str(&text).unwrap();
        let sub = &root["proxy-providers"]["subscription"];
        assert_eq!(sub["interval"].as_u64(), Some(43_200));
        assert_eq!(sub["path"].as_str(), Some("./providers/subscription.yaml"));

        let groups = root["proxy-groups"].as_sequence().unwrap();
        for name in ["🚀Proxy", "⚡Auto", "🚑Fallback"] {
            let group = groups
                .iter()
                .find(|g| g["name"].as_str() == Some(name))
                .unwrap();
            assert_eq!(group["use"][0].as_str(), Some("subscription"));
            if name != "🚀Proxy" {
                assert!(group.get("proxies").is_none());
            }
        }
        let proxy_group = groups
            .iter()
            .find(|g| g["name"].as_str() == Some("🚀Proxy"))
            .unwrap();
        let proxy_entries: Vec<&str> = proxy_group["proxies"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            proxy_entries,
            vec!["⚡Auto", "🚑Fallback", "Local_HTTP_1080", "DIRECT"]
        );
        assert_eq!(root["rule-providers"]["cn"]["format"].as_str(), Some("mrs"));
        assert_eq!(
            root["rule-providers"]["binance"]["format"].as_str(),
            Some("mrs")
        );

        assert!(apply_subscription_url(&cfg, "ftp://invalid").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preserves_non_utf8_config_on_default_init_and_subscription_update() {
        let dir = crate::config::test_temp_dir("non_utf8");
        let cfg = dir.join("config.yaml");
        let raw_gbk = b"# \xd6\xd0\xce\xc4\nmixed-port: 7890\n";
        std::fs::write(&cfg, raw_gbk).unwrap();

        ensure_default_config(&cfg);
        assert_eq!(std::fs::read(&cfg).unwrap(), raw_gbk);

        let err = apply_subscription_url(&cfg, "https://example.com/sub").unwrap_err();
        assert!(err.contains("failed to read"));
        assert_eq!(std::fs::read(&cfg).unwrap(), raw_gbk);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn try_parse_str_rejects_invalid_yaml_and_accepts_empty() {
        assert_eq!(try_parse_str("   \n").unwrap(), MihomoConfigInfo::default());
        assert!(try_parse_str("external-controller: [unclosed").is_err());
    }

    #[test]
    fn preserves_comments_and_formatting_on_subscription_update() {
        let dir = crate::config::test_temp_dir("yaml_comments");
        let cfg = dir.join("config.yaml");
        let initial = "\
# Custom header comment
mixed-port: 7890 # keep inline comment
external-controller: 127.0.0.1:9090

# Group section comment
proxy-groups:
  - name: MySelect
    type: select
    proxies:
      - DIRECT
";
        std::fs::write(&cfg, initial).unwrap();

        apply_subscription_url(&cfg, "https://example.com/sub?a=1&b=2").unwrap();
        apply_subscription_url(&cfg, "https://example.com/sub2").unwrap();

        let updated = std::fs::read_to_string(&cfg).unwrap();
        assert!(updated.contains("# Custom header comment"));
        assert!(updated.contains("# keep inline comment"));
        assert!(updated.contains("# Group section comment"));
        assert_eq!(
            read_subscription_url(&cfg).as_deref(),
            Some("https://example.com/sub2")
        );
        assert!(apply_subscription_url(&cfg, "https://").is_err());

        // Custom keys inside `proxy-providers.subscription` must survive URL updates.
        let with_custom = "\
proxy-providers:
  subscription:
    type: http
    url: https://example.com/old
    filter: \"HK|SG\"
    override:
      udp: true
";
        std::fs::write(&cfg, with_custom).unwrap();
        apply_subscription_url(&cfg, "https://example.com/new").unwrap();
        let after_custom = std::fs::read_to_string(&cfg).unwrap();
        assert!(after_custom.contains("filter: \"HK|SG\""));
        assert!(after_custom.contains("udp: true"));
        assert_eq!(
            read_subscription_url(&cfg).as_deref(),
            Some("https://example.com/new")
        );

        let parsed: Value = serde_yaml_ng::from_str(&updated).unwrap();
        let use_list = parsed["proxy-groups"][0]["use"].as_sequence().unwrap();
        assert_eq!(use_list.len(), 1);
        assert_eq!(use_list[0].as_str(), Some("subscription"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manages_multiple_profiles_exclusively_in_config_yaml() {
        let dir = crate::config::test_temp_dir("multi_profile");
        let cfg = dir.join("config.yaml");

        // 1. Add first profile (Chinese name) and second profile (with space).
        let (n1, p1) = upsert_profile(&cfg, " 主力机场 ", "https://example.com/a").unwrap();
        assert_eq!(n1, "主力机场");
        assert_eq!(p1, "./providers/主力机场.yaml");

        let (n2, p2) = upsert_profile(&cfg, "Backup US", "https://example.com/b").unwrap();
        assert_eq!(n2, "Backup US");
        assert_eq!(p2, "./providers/Backup US.yaml");

        let ov = read_profiles(&cfg);
        assert_eq!(ov.active.as_deref(), Some("Backup US"));
        assert_eq!(
            ov.profiles
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["主力机场", "Backup US"]
        );

        // Verify exclusive `use:` across all standard groups including 🤖AI, and health-check toggle.
        let root: Value = serde_yaml_ng::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(
            root["proxy-providers"]["主力机场"]["health-check"]["enable"].as_bool(),
            Some(false)
        );
        assert_eq!(
            root["proxy-providers"]["Backup US"]["health-check"]["enable"].as_bool(),
            Some(true)
        );
        for gname in ["🚀Proxy", "⚡Auto", "🚑Fallback", "🤖AI"] {
            let group = root["proxy-groups"]
                .as_sequence()
                .unwrap()
                .iter()
                .find(|g| g["name"].as_str() == Some(gname))
                .unwrap();
            let uses: Vec<&str> = group["use"]
                .as_sequence()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(uses, vec!["Backup US"]);
            if gname == "🤖AI" {
                assert!(group.get("include-all").is_none());
                assert_eq!(group["include-all-proxies"].as_bool(), Some(true));
            }
        }

        // 2. Same-name upsert updates URL in place without duplicating entries.
        upsert_profile(&cfg, "主力机场", "https://example.com/a-v2").unwrap();
        let ov = read_profiles(&cfg);
        assert_eq!(ov.profiles.len(), 2);
        assert_eq!(ov.active.as_deref(), Some("主力机场"));
        assert_eq!(ov.profiles[0].url, "https://example.com/a-v2");

        // 3. Switch back to Backup US.
        let switched_path = switch_profile(&cfg, "Backup US").unwrap();
        assert_eq!(switched_path, "./providers/Backup US.yaml");
        assert_eq!(read_profiles(&cfg).active.as_deref(), Some("Backup US"));

        // 4. Delete active profile -> auto-switches to remaining profile.
        let del_active = delete_profile(&cfg, "Backup US").unwrap();
        assert_eq!(
            del_active.deleted_path.as_deref(),
            Some("./providers/Backup US.yaml")
        );
        assert_eq!(del_active.next_active.as_deref(), Some("主力机场"));
        assert_eq!(read_profiles(&cfg).active.as_deref(), Some("主力机场"));

        // 5. Delete last profile -> removes proxy-providers and restores DIRECT placeholders.
        let del_last = delete_profile(&cfg, "主力机场").unwrap();
        assert_eq!(del_last.next_active, None);
        assert!(read_profiles(&cfg).profiles.is_empty());
        assert_eq!(read_profiles(&cfg).active, None);

        let after_empty: Value =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(after_empty.get("proxy-providers").is_none());
        for gname in ["⚡Auto", "🚑Fallback"] {
            let group = after_empty["proxy-groups"]
                .as_sequence()
                .unwrap()
                .iter()
                .find(|g| g["name"].as_str() == Some(gname))
                .unwrap();
            assert!(group.get("use").is_none());
            assert_eq!(group["proxies"][0].as_str(), Some("DIRECT"));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_legacy_include_all_and_rejects_invalid_profile_names() {
        let dir = crate::config::test_temp_dir("profile_validation");
        let cfg = dir.join("config.yaml");
        let legacy = "\
proxy-groups:
  - name: 🤖AI
    type: select
    include-all: true
    exclude-filter: \"HK|CN\"
";
        std::fs::write(&cfg, legacy).unwrap();
        upsert_profile(&cfg, "Main", "https://example.com/main").unwrap();
        let parsed: Value =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        let ai = &parsed["proxy-groups"][0];
        assert!(ai.get("include-all").is_none());
        assert_eq!(ai["include-all-proxies"].as_bool(), Some(true));
        assert_eq!(ai["use"][0].as_str(), Some("Main"));

        for bad in [
            "", "   ", ".", "..", "a/b", r"a\b", "a:b", "end.", "CON", "nul.txt",
        ] {
            assert!(
                validate_profile_name(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
        assert_eq!(
            validate_profile_name("  My-Sub_1.0  ").unwrap(),
            "My-Sub_1.0"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
