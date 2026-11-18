//! Mihomo external-controller REST client using `ureq 3.5` (plain local HTTP).

use std::collections::BTreeMap;
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ureq::{Agent, RequestBuilder};
use url::Url;

use crate::config::mihomo::HEALTH_CHECK_URL;
use crate::config::tray::{CONTROLLER_TIMEOUT_MAX_MS, CONTROLLER_TIMEOUT_MIN_MS};
use crate::state::{Group, Provider};

/// RFC 3986 unreserved characters (`A-Za-z0-9-_.~`) left unencoded.
const UNRESERVED_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

const PROBE_TIMEOUT_MS: u64 = 800;
const DELAY_QUERY_TIMEOUT_MS: u32 = 3000;
const DELAY_HTTP_TIMEOUT_MS: u64 = 10_000;
const PROVIDER_HTTP_TIMEOUT_MS: u64 = 30_000;
// Reloading config runs synchronous provider initialization on mihomo's side.
const RELOAD_HTTP_TIMEOUT_MS: u64 = 45_000;

#[derive(Debug, Clone)]
pub struct Client {
    host: String,
    pub(crate) port: u16,
    pub(crate) secret: String,
    timeout_ms: u32,
    agent: Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientError {
    InvalidAddress,
    TlsUnsupported,
}

impl ClientError {
    pub fn message(self, raw: &str) -> String {
        match self {
            Self::InvalidAddress => {
                format!(
                    "Invalid controller address '{raw}': expected host:port or http://host:port"
                )
            }
            Self::TlsUnsupported => {
                format!(
                    "Controller address '{raw}' uses https://, which is not supported for local plain HTTP controllers"
                )
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct CoreConfigs {
    pub mode: String,
    pub mixed_port: u16,
    pub tun_enable: bool,
}

#[derive(Debug, Default)]
pub struct ProxiesData {
    pub groups: Vec<Group>,
    pub latency: BTreeMap<String, Option<u32>>,
}

#[derive(Debug, Default)]
pub struct ProvidersData {
    pub providers: Vec<Provider>,
    pub latency: BTreeMap<String, Option<u32>>,
}

#[derive(Clone, Copy)]
enum RequestOp<'a> {
    Get,
    Delete,
    PatchJson(&'a Value),
    PutJson(&'a Value),
    PutEmpty,
}

impl Client {
    pub fn new(address: &str, secret: &str, timeout_ms: u32) -> Result<Self, ClientError> {
        let trimmed = address.trim();
        if trimmed.is_empty() {
            return Err(ClientError::InvalidAddress);
        }

        let candidate = if trimmed.contains("://") {
            trimmed.to_string()
        } else if let Some(port_part) = trimmed.strip_prefix(':') {
            format!("http://127.0.0.1:{port_part}")
        } else {
            format!("http://{trimmed}")
        };

        let parsed = Url::parse(&candidate).map_err(|_| ClientError::InvalidAddress)?;
        if parsed.scheme().eq_ignore_ascii_case("https") {
            return Err(ClientError::TlsUnsupported);
        }
        if !parsed.scheme().eq_ignore_ascii_case("http") {
            return Err(ClientError::InvalidAddress);
        }

        let raw_host = parsed.host_str().ok_or(ClientError::InvalidAddress)?;
        // `Url::port()` omits the scheme's default `:80`, so inspect the raw authority if explicit `:80` was passed.
        let authority = candidate
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
            .unwrap_or("");
        let port = parsed
            .port()
            .or_else(|| {
                authority
                    .rsplit_once(':')
                    .is_some_and(|(_, p)| p == "80")
                    .then_some(80)
            })
            .ok_or(ClientError::InvalidAddress)?;
        if port == 0 {
            return Err(ClientError::InvalidAddress);
        }

        let host = match raw_host {
            "0.0.0.0" | "[::]" => "127.0.0.1".to_string(),
            other => other.to_string(),
        };
        let agent: Agent = Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build()
            .into();

        Ok(Self {
            host,
            port,
            secret: secret.to_string(),
            timeout_ms: timeout_ms.clamp(CONTROLLER_TIMEOUT_MIN_MS, CONTROLLER_TIMEOUT_MAX_MS),
            agent,
        })
    }

    fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn format_web_ui_url(&self, template: &str) -> String {
        let tpl = template.trim();
        let effective = if tpl.is_empty() {
            "http://{host}:{port}/ui/?hostname={host}&port={port}&secret={secret}"
        } else {
            tpl
        };
        let encoded_secret = percent_encode(&self.secret);
        effective
            .replace("{host}", &self.host)
            .replace("{port}", &self.port.to_string())
            .replace("{secret}", &encoded_secret)
    }

    fn auth_header(&self) -> Option<String> {
        (!self.secret.is_empty()).then(|| format!("Bearer {}", self.secret))
    }

    fn execute_request(
        &self,
        op: RequestOp<'_>,
        path: &str,
        timeout_ms: u64,
    ) -> Result<String, String> {
        let url = format!("{}{path}", self.base_url());
        let auth = self.auth_header();
        let method_name = match op {
            RequestOp::Get => "GET",
            RequestOp::Delete => "DELETE",
            RequestOp::PatchJson(_) => "PATCH",
            RequestOp::PutJson(_) | RequestOp::PutEmpty => "PUT",
        };

        let resp_res = match op {
            RequestOp::Get => {
                configure_request(self.agent.get(&url), auth.as_deref(), timeout_ms).call()
            }
            RequestOp::Delete => {
                configure_request(self.agent.delete(&url), auth.as_deref(), timeout_ms).call()
            }
            RequestOp::PatchJson(payload) => {
                configure_request(self.agent.patch(&url), auth.as_deref(), timeout_ms)
                    .send_json(payload)
            }
            RequestOp::PutJson(payload) => {
                configure_request(self.agent.put(&url), auth.as_deref(), timeout_ms)
                    .send_json(payload)
            }
            RequestOp::PutEmpty => {
                configure_request(self.agent.put(&url), auth.as_deref(), timeout_ms).send_empty()
            }
        };

        let mut resp = resp_res.map_err(|e| format!("{method_name} {path} failed: {e}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read {method_name} {path} body failed: {e}"))?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "{method_name} {path} returned HTTP {status}: {body}"
            ));
        }
        Ok(body)
    }

    fn get_json(&self, path: &str, timeout_ms: u64) -> Result<Value, String> {
        let body = self.execute_request(RequestOp::Get, path, timeout_ms)?;
        serde_json::from_str(&body).map_err(|e| format!("invalid JSON from GET {path}: {e}"))
    }

    fn patch_json(&self, path: &str, payload: &Value) -> Result<(), String> {
        self.execute_request(RequestOp::PatchJson(payload), path, self.timeout_ms as u64)
            .map(|_| ())
    }

    fn put_json(&self, path: &str, payload: &Value, timeout_ms: u64) -> Result<(), String> {
        self.execute_request(RequestOp::PutJson(payload), path, timeout_ms)
            .map(|_| ())
    }

    fn put_empty(&self, path: &str, timeout_ms: u64) -> Result<(), String> {
        self.execute_request(RequestOp::PutEmpty, path, timeout_ms)
            .map(|_| ())
    }

    fn delete(&self, path: &str) -> Result<(), String> {
        self.execute_request(RequestOp::Delete, path, self.timeout_ms as u64)
            .map(|_| ())
    }

    pub fn probe(&self) -> Result<(), String> {
        self.get_json("/version", PROBE_TIMEOUT_MS).map(|_| ())
    }

    pub fn get_version(&self) -> Result<String, String> {
        let v = self.get_json("/version", self.timeout_ms as u64)?;
        Ok(v.get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    pub fn get_configs(&self) -> Result<CoreConfigs, String> {
        let v = self.get_json("/configs", self.timeout_ms as u64)?;
        let mode = v
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("rule")
            .to_string();
        let mixed_port = v
            .get("mixed-port")
            .and_then(Value::as_u64)
            .filter(|&p| p > 0 && p <= u16::MAX as u64)
            .or_else(|| {
                v.get("port")
                    .and_then(Value::as_u64)
                    .filter(|&p| p > 0 && p <= u16::MAX as u64)
            })
            .unwrap_or(0) as u16;
        let tun_enable = v
            .get("tun")
            .and_then(|t| t.get("enable"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(CoreConfigs {
            mode,
            mixed_port,
            tun_enable,
        })
    }

    pub fn set_mode(&self, mode: &str) -> Result<(), String> {
        self.patch_json("/configs", &json!({ "mode": mode }))
    }

    pub fn set_tun(&self, enable: bool) -> Result<(), String> {
        self.patch_json("/configs", &json!({ "tun": { "enable": enable } }))
    }

    pub fn reload_configs(&self) -> Result<(), String> {
        self.put_json("/configs?force=true", &json!({}), RELOAD_HTTP_TIMEOUT_MS)
    }

    pub fn select_proxy(&self, group: &str, member: &str) -> Result<(), String> {
        let path = format!("/proxies/{}", percent_encode(group));
        self.put_json(&path, &json!({ "name": member }), self.timeout_ms as u64)
    }

    pub fn unfix_group(&self, group: &str) -> Result<(), String> {
        let path = format!("/proxies/{}", percent_encode(group));
        self.delete(&path)
    }

    pub fn test_group_delay(&self, group: &str) -> Result<(), String> {
        let path = format!(
            "/group/{}/delay?timeout={DELAY_QUERY_TIMEOUT_MS}&url={}",
            percent_encode(group),
            percent_encode(HEALTH_CHECK_URL)
        );
        self.get_json(&path, DELAY_HTTP_TIMEOUT_MS).map(|_| ())
    }

    pub fn refresh_provider(&self, name: &str) -> Result<(), String> {
        let path = format!("/providers/proxies/{}", percent_encode(name));
        self.put_empty(&path, PROVIDER_HTTP_TIMEOUT_MS)
    }

    pub fn get_proxies(&self) -> Result<ProxiesData, String> {
        let root = self.get_json("/proxies", self.timeout_ms as u64)?;
        let Some(map) = root.get("proxies").and_then(Value::as_object) else {
            return Ok(ProxiesData::default());
        };
        let mut groups = Vec::new();
        let mut latency = BTreeMap::new();

        for (name, entry) in map {
            latency.insert(name.clone(), last_history_delay(entry));
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let Some(all) = entry.get("all").and_then(Value::as_array) else {
                continue;
            };
            let hidden = entry
                .get("hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if hidden {
                continue;
            }
            let members: Vec<String> = all
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect();
            let switchable = matches!(kind.as_str(), "Selector" | "URLTest" | "Fallback");
            let now = entry
                .get("now")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let fixed = entry
                .get("fixed")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            groups.push(Group {
                name: name.clone(),
                kind,
                switchable,
                now,
                fixed,
                members,
            });
        }

        Ok(ProxiesData { groups, latency })
    }

    pub fn get_providers(&self) -> Result<ProvidersData, String> {
        let root = self.get_json("/providers/proxies", self.timeout_ms as u64)?;
        let Some(map) = root.get("providers").and_then(Value::as_object) else {
            return Ok(ProvidersData::default());
        };
        let mut providers = Vec::new();
        let mut latency = BTreeMap::new();

        for (name, entry) in map {
            let vehicle = entry
                .get("vehicleType")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let updated_at = entry
                .get("updatedAt")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_unix);
            providers.push(Provider {
                name: name.clone(),
                vehicle,
                updated_at,
            });
            if let Some(proxies) = entry.get("proxies").and_then(Value::as_array) {
                for node in proxies {
                    if let Some(node_name) = node.get("name").and_then(Value::as_str) {
                        latency.insert(node_name.to_string(), last_history_delay(node));
                    }
                }
            }
        }
        providers.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });

        Ok(ProvidersData { providers, latency })
    }
}

fn configure_request<B>(
    builder: RequestBuilder<B>,
    auth: Option<&str>,
    timeout_ms: u64,
) -> RequestBuilder<B> {
    let mut req = builder
        .config()
        .timeout_global(Some(Duration::from_millis(timeout_ms)))
        .build();
    if let Some(auth) = auth {
        req = req.header("Authorization", auth);
    }
    req
}

fn last_history_delay(entry: &Value) -> Option<u32> {
    let history = entry.get("history").and_then(Value::as_array)?;
    let last = history.last()?;
    last.get("delay").and_then(Value::as_u64).map(|d| d as u32)
}

fn percent_encode(input: &str) -> String {
    utf8_percent_encode(input, UNRESERVED_ENCODE_SET).to_string()
}

/// Parses an RFC 3339 timestamp (`YYYY-MM-DDTHH:MM:SS...`) into Unix seconds.
/// Returns `None` for Go's zero time (`0001-01-01...`).
fn parse_rfc3339_unix(raw: &str) -> Option<i64> {
    if raw.starts_with("0001-") {
        return None;
    }
    OffsetDateTime::parse(raw, &Rfc3339)
        .ok()
        .map(|dt| dt.unix_timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn normalizes_controller_addresses_and_rejects_https() {
        let c1 = Client::new(":9090", "abc", 1000).unwrap();
        assert_eq!(c1.host, "127.0.0.1");
        assert_eq!(c1.port, 9090);

        let c2 = Client::new("http://0.0.0.0:9091", "s e", 1000).unwrap();
        assert_eq!(c2.host, "127.0.0.1");
        assert_eq!(
            c2.format_web_ui_url(""),
            "http://127.0.0.1:9091/ui/?hostname=127.0.0.1&port=9091&secret=s%20e"
        );

        assert_eq!(
            Client::new("https://127.0.0.1:9090", "", 1000).unwrap_err(),
            ClientError::TlsUnsupported
        );
        assert_eq!(
            Client::new("127.0.0.1", "", 1000).unwrap_err(),
            ClientError::InvalidAddress
        );
        assert_eq!(
            Client::new("127.0.0.1:0", "", 1000).unwrap_err(),
            ClientError::InvalidAddress
        );

        let c3 = Client::new("http://127.0.0.1:80/api", "", 1000).unwrap();
        assert_eq!(c3.port, 80);

        let c4 = Client::new("[::1]:9090", "", 1000).unwrap();
        assert_eq!(c4.host, "[::1]");
        assert_eq!(c4.port, 9090);
        assert_eq!(
            c4.format_web_ui_url(""),
            "http://[::1]:9090/ui/?hostname=[::1]&port=9090&secret="
        );
    }

    #[test]
    fn parses_rfc3339_timestamps() {
        assert_eq!(parse_rfc3339_unix("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_unix("2025-01-01T08:00:00+08:00"),
            Some(1_735_689_600)
        );
        assert_eq!(parse_rfc3339_unix("2025-02-29T00:00:00Z"), None);
        assert!(parse_rfc3339_unix("2024-02-29T00:00:00Z").is_some());
        assert_eq!(parse_rfc3339_unix("2025-01-01T08:00:00+99:00"), None);
    }

    #[test]
    fn sends_http_requests_with_ureq() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("GET /configs ") {
                    r#"{"mode":"rule","mixed-port":7890,"tun":{"enable":true}}"#
                } else {
                    r#"{"version":"v1.19.0"}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });

        let client = Client::new(&format!("127.0.0.1:{port}"), "tok", 2000).unwrap();
        let ver = client.get_version().unwrap();
        assert_eq!(ver, "v1.19.0");
        let cfg = client.get_configs().unwrap();
        assert_eq!(cfg.mode, "rule");
        assert_eq!(cfg.mixed_port, 7890);
        assert!(cfg.tun_enable);
        handle.join().unwrap();
    }

    #[test]
    fn reload_configs_allows_slow_provider_bootstrap_beyond_poll_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).unwrap();
            std::thread::sleep(Duration::from_millis(350));
            let resp = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
            stream.write_all(resp.as_bytes()).unwrap();
        });

        // Short poll timeout (200ms) must not abort `PUT /configs?force=true`.
        let client = Client::new(&format!("127.0.0.1:{port}"), "", 200).unwrap();
        client.reload_configs().unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn parses_providers_into_sorted_latency_map_and_surfaces_http_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).unwrap();
                let resp = if idx == 0 {
                    let body = r#"{"providers":{"subscription":{"vehicleType":"HTTP","updatedAt":"2025-01-01T08:00:00+08:00","proxies":[{"name":"Z-Node","history":[{"delay":88}]},{"name":"A-Node","history":[{"delay":24}]}]}}}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 400 Bad Request\r\nContent-Length: 7\r\nConnection: close\r\n\r\nbad mod".to_string()
                };
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });

        let client = Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap();
        let data = client.get_providers().unwrap();
        let keys: Vec<&str> = data.latency.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["A-Node", "Z-Node"]);
        assert_eq!(data.latency.get("A-Node"), Some(&Some(24)));

        let err = client.set_mode("invalid").unwrap_err();
        assert!(err.contains("PATCH /configs returned HTTP 400: bad mod"));
        handle.join().unwrap();
    }
}
