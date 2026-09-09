//! Bridge to the UltraCompact (`uc`) engine.
//!
//! UC is an optional external binary (default `uc` on PATH). UltraCompress is
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
    /// Original tool text measured with UC's o200k tokenizer, before enveloping.
    pub tokens_source: u64,
    /// Original payload size in chars.
    pub source_chars: usize,
    pub savings_pct: f64,
    /// True when the raw text was not JSON and was wrapped as `{"t": text}`
    /// before encoding; decoding yields exact JSON whose `"t"` value is the
    /// original text.
    pub envelope: bool,
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
        UcBridge {
            bin: bin.to_string(),
            available: None,
            cache: HashMap::new(),
            encodes: 0,
            hits: 0,
        }
    }

    /// Probe the binary once. Never panics; a missing UC just disables the
    /// engine for this run (graceful degradation — UltraCompress stays open
    /// source and standalone, UC is an accelerator).
    pub fn probe(&mut self) -> UcStatus {
        if let Some(avail) = self.available {
            return UcStatus {
                enabled: true,
                bin: self.bin.clone(),
                available: avail,
                version: None,
                reason: if avail {
                    None
                } else {
                    Some("uc binary not found or not working".into())
                },
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
        UcStatus {
            enabled: true,
            bin: self.bin.clone(),
            available: ok,
            version,
            reason: None,
        }
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
            reason: if avail {
                None
            } else {
                Some("uc binary not probed/available".into())
            },
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
        match result {
            Ok(packet) => {
                self.cache.insert(key, packet.clone());
                packet
            }
            Err(()) => None, // A transient failure is not a no-gain decision.
        }
    }

    /// True only after a successful, deterministic no-gain decision.
    pub fn no_gain(&self, text: &str) -> bool {
        let key: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        matches!(self.cache.get(&key), Some(None))
    }

    /// Run `uc encode --stats` on one wire payload. Returns the packet text
    /// plus the reported (uc, jsonMin) token counts. Tagged with a telemetry
    /// source so bridge traffic is attributable when a sink is enabled.
    fn run_encode(&self, wire: &str) -> Result<(String, u64, u64), ()> {
        use std::io::Write;
        let mut child = Command::new(&self.bin)
            .arg("encode")
            .arg("--stats")
            .env("UC_TELEMETRY_SOURCE", "ultracompress")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| ())?;
        {
            let stdin = child.stdin.as_mut().ok_or(())?;
            stdin.write_all(wire.as_bytes()).map_err(|_| ())?;
        }
        let out = child.wait_with_output().map_err(|_| ())?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(());
        }
        let packet = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        // Stats line looks like: {"codec":"j","tokens":{"uc":36,"jsonMin":36,...},...}
        let (tokens_uc, tokens_json) = parse_uc_stats(&stderr).ok_or(())?;
        if tokens_uc == 0 || tokens_json == 0 {
            return Err(());
        }
        Ok((packet, tokens_uc, tokens_json))
    }

    pub fn count_tokens(&self, text: &str) -> Option<u64> {
        use std::io::Write;
        let mut child = Command::new(&self.bin)
            .arg("count")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        child.stdin.as_mut()?.write_all(text.as_bytes()).ok()?;
        let out = child.wait_with_output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()?.trim().parse().ok()
    }

    fn encode_json_uncached(&mut self, text: &str) -> Result<Option<UcPacket>, ()> {
        // Older extensions copy dense packets through the model. Do not enable
        // plain-text envelopes until a reference-aware caller explicitly opts in.
        let allow_text = std::env::var("UC_TEXT_ENVELOPES").as_deref() == Ok("1");
        self.try_encode_input(text, allow_text)
    }

    #[cfg(test)]
    fn encode_input(&self, text: &str, allow_text: bool) -> Option<UcPacket> {
        self.try_encode_input(text, allow_text).ok().flatten()
    }

    fn try_encode_input(&self, text: &str, allow_text: bool) -> Result<Option<UcPacket>, ()> {
        let envelope = serde_json::from_str::<serde_json::Value>(text).is_err();
        if envelope && !allow_text {
            return Ok(None);
        }
        // Valid JSON never takes the envelope fallback, even on a codec-j tie.
        let wire = if envelope {
            serde_json::json!({ "t": text }).to_string()
        } else {
            text.to_string()
        };
        let (packet, tokens_uc, tokens_json) = self.run_encode(&wire)?;
        // A codec-j tie cannot win. Avoid launching another tokenizer process
        // merely to count an already-rejected candidate.
        if tokens_uc >= tokens_json {
            return Ok(None);
        }
        let tokens_source = self.count_tokens(text).ok_or(())?;
        if tokens_uc >= tokens_source {
            return Ok(None);
        }
        Ok(Some(UcPacket {
            packet,
            tokens_uc,
            tokens_json,
            tokens_source,
            source_chars: text.len(),
            savings_pct: (1.0 - tokens_uc as f64 / tokens_source as f64) * 100.0,
            envelope,
        }))
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

    #[test]
    fn envelope_fallback_compresses_plain_text_and_roundtrips() {
        let ok = Command::new("uc")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return; // uc unavailable — never-worse coverage lives in the engine tests
        }
        let mut b = UcBridge::new("uc");
        assert!(b.probe().available);
        let text = format!(
            "build step {}\ncompiling module {}\nwrote artifact {}\n",
            "alpha/".repeat(24),
            "beta/".repeat(24),
            "gamma/".repeat(24)
        )
        .repeat(8);
        assert!(
            b.encode_input(&text, false).is_none(),
            "legacy callers retain stock text"
        );
        let packet = b
            .encode_input(&text, true)
            .expect("opted-in envelope attempt should win");
        assert!(packet.envelope, "plain text must take the envelope path");
        assert!(packet.tokens_uc < packet.tokens_json);
        // Decode must return exact JSON whose "t" value is the original text.
        let mut child = Command::new("uc")
            .arg("decode")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(packet.packet.as_bytes())
                .unwrap();
        }
        let decoded = child.wait_with_output().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&decoded.stdout).unwrap();
        assert_eq!(value["t"].as_str().unwrap(), text);
    }

    #[test]
    fn json_payload_keeps_direct_path() {
        let ok = Command::new("uc")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return;
        }
        let mut b = UcBridge::new("uc");
        assert!(b.probe().available);
        let json = serde_json::json!({
            "records": (0..30).map(|i| serde_json::json!({
                "id": format!("req_{:04}", i),
                "channel": "telegram",
                "region": "south_carolina",
                "status": "delivered",
            })).collect::<Vec<_>>()
        })
        .to_string();
        let packet = b.encode_json(&json).expect("repetitive JSON should win");
        assert!(!packet.envelope, "valid JSON must not be wrapped");
    }
}
