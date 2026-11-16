//! Lightweight extraction of mihomo `config.yaml` controller and external-ui fields.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MihomoConfigInfo {
    pub external_controller: Option<String>,
    pub secret: Option<String>,
    pub external_ui: Option<String>,
    pub other_controller_family: bool,
    pub tun_enabled: bool,
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
    secret: Option<String>,
    #[serde(rename = "external-ui")]
    external_ui: Option<String>,
    #[serde(default)]
    tun: PartialTunYaml,
}

#[derive(Debug, Default, Deserialize)]
struct PartialTunYaml {
    #[serde(default)]
    enable: bool,
}

pub fn read_file(path: &Path) -> std::io::Result<MihomoConfigInfo> {
    let text = std::fs::read_to_string(path)?;
    Ok(parse_str(&text))
}

pub fn parse_str(text: &str) -> MihomoConfigInfo {
    let clean = text.strip_prefix('\u{feff}').unwrap_or(text);
    let parsed: PartialMihomoYaml = serde_yaml_ng::from_str(clean).unwrap_or_default();
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

    MihomoConfigInfo {
        external_controller: non_empty(parsed.external_controller),
        secret: non_empty(parsed.secret),
        external_ui: non_empty(parsed.external_ui),
        other_controller_family: other_family,
        tun_enabled: parsed.tun.enable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_controller_secret_ui_and_tun() {
        let yaml = r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
secret: "my#secret"
external-ui: ./ui
tun:
  enable: true
"#;
        let info = parse_str(yaml);
        assert_eq!(info.external_controller.as_deref(), Some("127.0.0.1:9090"));
        assert_eq!(info.secret.as_deref(), Some("my#secret"));
        assert_eq!(info.external_ui.as_deref(), Some("./ui"));
        assert!(info.tun_enabled);
        assert!(!info.other_controller_family);
    }

    #[test]
    fn detects_non_http_controller_families() {
        let yaml = "external-controller-unix: /tmp/mihomo.sock\n";
        let info = parse_str(yaml);
        assert!(info.other_controller_family);
        assert!(info.external_controller.is_none());
    }
}
