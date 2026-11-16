//! Mihomo external-controller REST client using `ureq 3.5` (plain local HTTP).

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
use ureq::Agent;

use crate::state::{Group, Provider};

const PROBE_TIMEOUT_MS: u64 = 800;
const DELAY_QUERY_TIMEOUT_MS: u32 = 3000;
const DELAY_HTTP_TIMEOUT_MS: u64 = 10_000;
const PROVIDER_HTTP_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub host: String,
    pub port: u16,
    pub secret: String,
    pub timeout_ms: u32,
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
    pub latency: Vec<(String, Option<u32>)>,
}

#[derive(Debug, Default)]
pub struct ProvidersData {
    pub providers: Vec<Provider>,
    pub latency: Vec<(String, Option<u32>)>,
}

impl Client {
    pub fn new(address: &str, secret: &str, timeout_ms: u32) -> Result<Self, ClientError> {
        let trimmed = address.trim();
        if trimmed.is_empty() {
            return Err(ClientError::InvalidAddress);
        }
        if trimmed.to_ascii_lowercase().starts_with("https://") {
            return Err(ClientError::TlsUnsupported);
        }
        let without_scheme = trimmed
            .strip_prefix("http://")
            .or_else(|| trimmed.strip_prefix("HTTP://"))
            .unwrap_or(trimmed);
        let authority = without_scheme.split('/').next().unwrap_or("").trim();
        let (host, port) = parse_host_port(authority).ok_or(ClientError::InvalidAddress)?;

        Ok(Self {
            host,
            port,
            secret: secret.to_string(),
            timeout_ms: timeout_ms.clamp(200, 30_000),
        })
    }

    pub fn base_url(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("http://[{}]:{}", self.host, self.port)
        } else {
            format!("http://{}:{}", self.host, self.port)
        }
    }

    pub fn format_web_ui_url(&self, template: &str) -> String {
        let tpl = template.trim();
        let effective = if tpl.is_empty() {
            "http://{host}:{port}/ui/?hostname={host}&port={port}&secret={secret}"
        } else {
            tpl
        };
        let encoded_secret = percent_encode(&self.secret);
        let host_for_url = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        effective
            .replace("{host}", &host_for_url)
            .replace("{port}", &self.port.to_string())
            .replace("{secret}", &encoded_secret)
    }

    fn agent(&self, timeout_ms: u64) -> Agent {
        Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(timeout_ms)))
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn auth_header(&self) -> Option<String> {
        (!self.secret.is_empty()).then(|| format!("Bearer {}", self.secret))
    }

    fn get_json(&self, path: &str, timeout_ms: u64) -> Result<Value, String> {
        let url = format!("{}{path}", self.base_url());
        let agent = self.agent(timeout_ms);
        let mut req = agent.get(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", &auth);
        }
        let mut resp = req.call().map_err(|e| format!("GET {path} failed: {e}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read GET {path} body failed: {e}"))?;
        if !(200..300).contains(&status) {
            return Err(format!("GET {path} returned HTTP {status}: {body}"));
        }
        serde_json::from_str(&body).map_err(|e| format!("invalid JSON from GET {path}: {e}"))
    }

    fn patch_json(&self, path: &str, payload: &Value) -> Result<(), String> {
        let url = format!("{}{path}", self.base_url());
        let agent = self.agent(self.timeout_ms as u64);
        let mut req = agent.patch(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", &auth);
        }
        let mut resp = req
            .send_json(payload)
            .map_err(|e| format!("PATCH {path} failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("PATCH {path} returned HTTP {status}: {body}"));
        }
        Ok(())
    }

    fn put_json(&self, path: &str, payload: &Value, timeout_ms: u64) -> Result<(), String> {
        let url = format!("{}{path}", self.base_url());
        let agent = self.agent(timeout_ms);
        let mut req = agent.put(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", &auth);
        }
        let mut resp = req
            .send_json(payload)
            .map_err(|e| format!("PUT {path} failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("PUT {path} returned HTTP {status}: {body}"));
        }
        Ok(())
    }

    fn put_empty(&self, path: &str, timeout_ms: u64) -> Result<(), String> {
        let url = format!("{}{path}", self.base_url());
        let agent = self.agent(timeout_ms);
        let mut req = agent.put(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", &auth);
        }
        let mut resp = req
            .send_empty()
            .map_err(|e| format!("PUT {path} failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("PUT {path} returned HTTP {status}: {body}"));
        }
        Ok(())
    }

    fn post_empty(&self, path: &str, timeout_ms: u64) -> Result<(), String> {
        let url = format!("{}{path}", self.base_url());
        let agent = self.agent(timeout_ms);
        let mut req = agent.post(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", &auth);
        }
        let mut resp = req
            .send_empty()
            .map_err(|e| format!("POST {path} failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("POST {path} returned HTTP {status}: {body}"));
        }
        Ok(())
    }

    fn delete(&self, path: &str) -> Result<(), String> {
        let url = format!("{}{path}", self.base_url());
        let agent = self.agent(self.timeout_ms as u64);
        let mut req = agent.delete(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", &auth);
        }
        let mut resp = req
            .call()
            .map_err(|e| format!("DELETE {path} failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("DELETE {path} returned HTTP {status}: {body}"));
        }
        Ok(())
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
        self.put_json("/configs?force=true", &json!({}), self.timeout_ms as u64)
    }

    pub fn restart(&self) -> Result<(), String> {
        self.post_empty("/restart", self.timeout_ms as u64)
    }

    pub fn close_all_connections(&self) -> Result<(), String> {
        self.delete("/connections")
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
            percent_encode("https://www.gstatic.com/generate_204")
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
        let mut latency = Vec::new();

        for (name, entry) in map {
            latency.push((name.clone(), last_history_delay(entry)));
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
        let mut latency_map: HashMap<String, Option<u32>> = HashMap::new();

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
                        latency_map.insert(node_name.to_string(), last_history_delay(node));
                    }
                }
            }
        }
        providers.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });

        Ok(ProvidersData {
            providers,
            latency: latency_map.into_iter().collect(),
        })
    }
}

fn last_history_delay(entry: &Value) -> Option<u32> {
    let history = entry.get("history").and_then(Value::as_array)?;
    let last = history.last()?;
    last.get("delay").and_then(Value::as_u64).map(|d| d as u32)
}

fn parse_host_port(authority: &str) -> Option<(String, u16)> {
    if authority.is_empty() {
        return None;
    }
    let (raw_host, port_str) = if let Some(rest) = authority.strip_prefix('[') {
        let (h, p) = rest.split_once("]:")?;
        (h, p)
    } else if let Some(p) = authority.strip_prefix(':') {
        ("127.0.0.1", p)
    } else {
        authority.rsplit_once(':')?
    };

    let port: u16 = port_str.parse().ok()?;
    if port == 0 {
        return None;
    }
    let host = match raw_host.trim() {
        "" | "0.0.0.0" | "::" => "127.0.0.1".to_string(),
        other => other.to_string(),
    };
    Some((host, port))
}

pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Parses an RFC 3339 timestamp (`YYYY-MM-DDTHH:MM:SS...`) into Unix seconds.
/// Returns `None` for Go's zero time (`0001-01-01...`).
fn parse_rfc3339_unix(raw: &str) -> Option<i64> {
    if raw.starts_with("0001-") {
        return None;
    }
    let date_part = raw.get(..10)?;
    let time_part = raw.get(11..19)?;
    if raw.as_bytes().get(10).copied() != Some(b'T') {
        return None;
    }
    let year: i64 = date_part.get(0..4)?.parse().ok()?;
    let month: u32 = date_part.get(5..7)?.parse().ok()?;
    let day: u32 = date_part.get(8..10)?.parse().ok()?;
    let hour: i64 = time_part.get(0..2)?.parse().ok()?;
    let min: i64 = time_part.get(3..5)?.parse().ok()?;
    let sec: i64 = time_part.get(6..8)?.parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let mut unix = days * 86_400 + hour * 3600 + min * 60 + sec;

    // Handle optional timezone offset (`Z` or `+HH:MM` / `-HH:MM`) after fractional seconds.
    let rest = &raw[19..];
    let tz_start = rest.find(['Z', 'z', '+', '-'])?;
    let tz = &rest[tz_start..];
    if !tz.eq_ignore_ascii_case("Z") {
        let sign = if tz.starts_with('+') { 1i64 } else { -1i64 };
        let clean_tz = &tz[1..];
        let (th, tm) = if let Some((h, m)) = clean_tz.split_once(':') {
            (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?)
        } else if clean_tz.len() >= 4 {
            (
                clean_tz[..2].parse::<i64>().ok()?,
                clean_tz[2..4].parse::<i64>().ok()?,
            )
        } else {
            return None;
        };
        unix -= sign * (th * 3600 + tm * 60);
    }

    Some(unix)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let d = i64::from(d);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
    }

    #[test]
    fn parses_rfc3339_timestamps() {
        assert_eq!(parse_rfc3339_unix("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_unix("2025-01-01T08:00:00+08:00"),
            Some(1_735_689_600)
        );
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
}
