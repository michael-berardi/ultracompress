//! Bridge to the UltraCompact (`uc`) engine.
//!
//! UC is an optional external binary (default `uc` on PATH). Rapid Compact is
//! fully functional without it; when present it shrinks JSON payloads
//! losslessly, and UC itself guarantees output tokens ≤ minified JSON for
//! every payload. The bridge shells out to `uc encode --stats`, parses the
//! packet + token report, and caches results by content hash.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, serde::Serialize)]
pub struct UcPacket {
    /// The UC packet text (readable mode — model-readable).
    pub packet: String,
    /// Tokens UC reports for the packet.
    pub tokens_uc: u64,
    /// Tokens UC reports for minified JSON.
    pub tokens_json: u64,
    /// Original payload size in chars.
    pub source_chars: usize,
    pub savings_pct: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UcStatus {
    pub enabled: bool,
    pub bin: String,
    pub available: bool,
    pub version: Option<String>,
    pub reason: Option<String>,
}

pub struct UcBridge {
    bin: String,
    available: Option<bool>,
    cache: HashMap<[u8; 32], Option<UcPacket>>,
    encodes: u64,
    hits: u64,
}

impl UcBridge {
    pub fn new(bin: &str) -> Self {
        UcBridge { bin: bin.to_string(), available: None, cache: HashMap::new(), encodes: 0, hits: 0 }
    }

    /// Probe the binary once. Never panics; a missing UC just disables the
    /// engine for this run (graceful degradation — Rapid Compact stays open
    /// source and standalone, UC is an accelerator).
    pub fn probe(&mut self) -> UcStatus {
        if let Some(avail) = self.available {
            return UcStatus {
                enabled: true,
                bin: self.bin.clone(),
                available: avail,
                version: None,
                reason: if avail { None } else { Some("uc binary not found or not working".into()) },
            };
        }
        let ok = Command::new(&self.bin)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let version = if ok { self.version_string() } else { None };
        self.available = Some(ok);
        UcStatus { enabled: true, bin: self.bin.clone(), available: ok, version, reason: None }
    }

    fn version_string(&self) -> Option<String> {
        Command::new(&self.bin)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn status(&self) -> UcStatus {
        let avail = self.available.unwrap_or(false);
        UcStatus {
            enabled: true,
            bin: self.bin.clone(),
            available: avail,
            version: if avail { self.version_string() } else { None },
            reason: if avail { None } else { Some("uc binary not probed/available".into()) },
        }
    }

    pub fn cache_stats(&self) -> (u64, u64) {
        (self.encodes, self.hits)
    }

    /// Encode a JSON payload. Returns None when UC is unavailable or decides
    /// the payload isn't worth encoding (never-worse guarantee: UC falls back
    /// to plain JSON itself, in which case we keep the original).
    pub fn encode_json(&mut self, text: &str) -> Option<UcPacket> {
        if self.available != Some(true) {
            return None;
        }
        let key: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        if let Some(cached) = self.cache.get(&key) {
            self.hits += 1;
            return cached.clone();
        }
        let result = self.encode_json_uncached(text);
        self.encodes += 1;
        self.cache.insert(key, result.clone());
        result
    }

    fn encode_json_uncached(&mut self, text: &str) -> Option<UcPacket> {
        use std::io::Write;
        let mut child = Command::new(&self.bin)
            .arg("encode")
            .arg("--stats")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        {
            let stdin = child.stdin.as_mut()?;
            stdin.write_all(text.as_bytes()).ok()?;
        }
        let out = child.wait_with_output().ok()?;
        if !out.status.success() || out.stdout.is_empty() {
            return None;
        }
        let packet = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        // Stats line looks like: {"codec":"j","tokens":{"uc":36,"jsonMin":36,...},...}
        let (tokens_uc, tokens_json) = parse_uc_stats(&stderr).unwrap_or((0, 0));
        if tokens_uc == 0 || tokens_json == 0 {
            return None;
        }
        let savings_pct = if tokens_json > 0 {
            (1.0 - tokens_uc as f64 / tokens_json as f64) * 100.0
        } else {
            0.0
        };
        if tokens_uc >= tokens_json {
            return None; // never ship a worse payload
        }
        Some(UcPacket { packet, tokens_uc, tokens_json, source_chars: text.len(), savings_pct })
    }
}

fn parse_uc_stats(stderr: &str) -> Option<(u64, u64)> {
    let start = stderr.find('{')?;
    let end = stderr.rfind('}')?;
    let v: serde_json::Value = serde_json::from_str(&stderr[start..=end]).ok()?;
    let t = v.get("tokens")?;
    let uc = t.get("uc")?.as_u64()?;
    let json_min = t.get("jsonMin").or_else(|| t.get("json_min"))?.as_u64()?;
    Some((uc, json_min))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uc_stats_line() {
        let line = r#"{"codec":"j","tokens":{"uc":36,"jsonMin":36,"jsonPretty":78},"savingsVsJsonMin":"0.0%","savingsVsJsonPretty":"53.8%"}"#;
        assert_eq!(parse_uc_stats(line), Some((36, 36)));
    }

    #[test]
    fn missing_binary_is_graceful() {
        let mut b = UcBridge::new("rc-definitely-not-a-binary-uc");
        let status = b.probe();
        assert!(!status.available);
        assert!(b.encode_json("{\"a\":1}").is_none());
    }
}
