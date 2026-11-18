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
    let clean = text.strip_prefix('\u{feff}').unwrap_or(text);
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

/// Reads the currently configured subscription URL (`proxy-providers.subscription.url`) if present.
pub fn read_subscription_url(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let clean = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let doc = Document::from_str(clean).ok()?;
    doc.get_mapping("proxy-providers")?
        .get_mapping(SUBSCRIPTION_PROVIDER_NAME)?
        .get("url")?
        .as_scalar()
        .map(|s| s.as_string())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Writes/updates `proxy-providers.subscription` (12h update interval) and attaches it to proxy groups
/// while preserving existing YAML comments and formatting via `yaml-edit`.
pub fn apply_subscription_url(path: &Path, raw_url: &str) -> Result<(), String> {
    let url = raw_url.trim();
    let valid_http =
        url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host());
    if !valid_http {
        return Err(crate::text::ERR_INVALID_SUBSCRIPTION_URL.to_string());
    }

    let source_text = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim_start_matches('\u{feff}').trim().is_empty() => text,
        Ok(_) => DEFAULT_CONFIG_TEMPLATE.to_string(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
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

    // 1. Insert or update `proxy-providers.subscription` while preserving user keys.
    if let Some(providers_map) = root_map.get_mapping("proxy-providers") {
        if let Some(sub_map) = providers_map.get_mapping(SUBSCRIPTION_PROVIDER_NAME) {
            sub_map.set("type", "http");
            sub_map.set("url", url);
            sub_map.set("path", SUBSCRIPTION_LOCAL_PATH);
            sub_map.set("interval", SUBSCRIPTION_INTERVAL_SECS as i64);
            if let Some(hc_map) = sub_map.get_mapping("health-check") {
                hc_map.set("enable", true);
                hc_map.set("url", HEALTH_CHECK_URL);
                hc_map.set("interval", HEALTH_CHECK_INTERVAL_SECS);
            } else {
                sub_map.set("health-check", build_health_check_doc());
            }
        } else {
            providers_map.set(SUBSCRIPTION_PROVIDER_NAME, build_subscription_doc(url));
        }
    } else {
        let providers_doc = MappingBuilder::new()
            .pair(SUBSCRIPTION_PROVIDER_NAME, build_subscription_doc(url))
            .build_document();
        root_map.set("proxy-providers", &providers_doc);
    }

    // 2. Wire `use: [subscription]` into proxy groups
    wire_subscription_into_groups(&root_map);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let out_yaml = format!("{bom}{doc}");
    std::fs::write(path, out_yaml)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;

    Ok(())
}

fn build_health_check_doc() -> Document {
    MappingBuilder::new()
        .pair("enable", true)
        .pair("url", HEALTH_CHECK_URL)
        .pair("interval", HEALTH_CHECK_INTERVAL_SECS)
        .build_document()
}

fn build_subscription_doc(url: &str) -> Document {
    MappingBuilder::new()
        .pair("type", "http")
        .pair("url", url)
        .pair("path", SUBSCRIPTION_LOCAL_PATH)
        .pair("interval", SUBSCRIPTION_INTERVAL_SECS as i64)
        .pair("health-check", build_health_check_doc())
        .build_document()
}

fn wire_subscription_into_groups(root_map: &Mapping) {
    let Some(groups) = root_map.get_sequence("proxy-groups") else {
        let fallback_groups = SequenceBuilder::new()
            .mapping(|g| {
                g.pair("name", "Proxy")
                    .pair("type", "select")
                    .sequence("use", |u| u.item(SUBSCRIPTION_PROVIDER_NAME))
            })
            .build_document();
        root_map.set("proxy-groups", &fallback_groups);
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
            "Proxy" | "🚀Proxy" | "Auto" | "⚡Auto" | "Fallback" | "🚑Fallback"
        );
        let strip_direct = matches!(name.as_str(), "Auto" | "⚡Auto" | "Fallback" | "🚑Fallback")
            && matches!(kind.as_str(), "url-test" | "fallback");
        let already_uses_providers = group_map.get_sequence("use").is_some();

        if is_standard_group || already_uses_providers {
            ensure_use_contains(group_map, SUBSCRIPTION_PROVIDER_NAME);
            // Strip placeholder `DIRECT` from auto-selection groups once backed by a subscription.
            if strip_direct {
                remove_single_direct_placeholder(group_map);
            }
            wired_any = true;
        }
    }

    if !wired_any {
        // Fallback for custom configs: wire into first `select` group if present, else add `Proxy`.
        let mut attached_to_select = false;
        for group_val in groups.values() {
            if let Some(group_map) = group_val.as_mapping()
                && scalar_str(group_map, "type").as_deref() == Some("select")
            {
                ensure_use_contains(group_map, SUBSCRIPTION_PROVIDER_NAME);
                attached_to_select = true;
                break;
            }
        }
        if !attached_to_select {
            let proxy_group = MappingBuilder::new()
                .pair("name", "Proxy")
                .pair("type", "select")
                .sequence("use", |u| u.item(SUBSCRIPTION_PROVIDER_NAME))
                .build_document();
            groups.insert(0, &proxy_group);
        }
    }
}

fn scalar_str(map: &Mapping, key: &str) -> Option<String> {
    map.get(key)?.as_scalar().map(|s| s.as_string())
}

fn ensure_use_contains(group_map: &Mapping, provider_name: &str) {
    if let Some(seq) = group_map.get_sequence("use") {
        let already = seq.values().any(|v| {
            v.as_scalar()
                .is_some_and(|s| s.as_string() == provider_name)
        });
        if !already {
            seq.push(provider_name);
        }
    } else {
        let seq_doc = SequenceBuilder::new().item(provider_name).build_document();
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
}
