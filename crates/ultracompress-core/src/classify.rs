//! Block classification: what kind of content is this, and which engine
//! should own it?

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentClass {
    /// Parseable JSON object/array — UC engine territory.
    Json,
    /// Plain text — VCC transcript / snap-frame territory.
    Text,
    /// Already an image or opaque payload — leave alone.
    Opaque,
    Empty,
}

/// Sniff whether a tool result is a JSON payload worth UC-encoding.
/// Must parse cleanly AND be non-trivial (constants waste a subprocess call).
/// Uses `IgnoredAny` so validation never builds a value tree — O(bytes),
/// no allocation blowup on megabyte payloads.
pub fn classify_content(text: &str) -> ContentClass {
    let t = text.trim_start();
    if t.is_empty() {
        return ContentClass::Empty;
    }
    let first = t.as_bytes()[0];
    if first == b'{' || first == b'[' {
        if serde_json::from_str::<serde::de::IgnoredAny>(t).is_ok() {
            return ContentClass::Json;
        }
        if serde_json::from_str::<serde::de::IgnoredAny>(t.trim_end()).is_ok() {
            return ContentClass::Json;
        }
    }
    ContentClass::Text
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Thresholds {
    /// Minimum chars before UC encoding is attempted (default 1200).
    pub uc_min_chars: usize,
    /// Minimum chars before a text tool result is snap-framed (default 6000).
    pub snap_min_chars: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            uc_min_chars: 1_200,
            snap_min_chars: 6_000,
        }
    }
}

/// The engine a block is routed to. `None` = keep verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Uc,
    Snap,
    Vcc,
    None,
}

/// Route one tool-result payload.
pub fn route_tool_result(
    text: &str,
    uc_available: bool,
    vision_capable: bool,
    th: &Thresholds,
) -> (Engine, ContentClass) {
    let class = classify_content(text);
    match class {
        ContentClass::Empty | ContentClass::Opaque => (Engine::None, class),
        ContentClass::Json => {
            if uc_available && text.len() >= th.uc_min_chars {
                (Engine::Uc, class)
            } else {
                (Engine::None, class)
            }
        }
        ContentClass::Text => {
            if vision_capable && text.len() >= th.snap_min_chars {
                (Engine::Snap, class)
            } else {
                (Engine::None, class)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_json() {
        assert_eq!(classify_content("{\"a\":1}"), ContentClass::Json);
        assert_eq!(classify_content("  [1,2,3]\n"), ContentClass::Json);
    }

    #[test]
    fn classifies_text_and_empty() {
        assert_eq!(classify_content("cargo test output…"), ContentClass::Text);
        assert_eq!(classify_content("{broken json"), ContentClass::Text);
        assert_eq!(classify_content(""), ContentClass::Empty);
    }

    #[test]
    fn routing_matrix() {
        let th = Thresholds::default();
        let json = serde_json::to_string(&serde_json::json!({"k": "x".repeat(2000)})).unwrap();
        let text = "a".repeat(7000);
        assert_eq!(route_tool_result(&json, true, true, &th).0, Engine::Uc);
        assert_eq!(route_tool_result(&json, false, true, &th).0, Engine::None);
        assert_eq!(route_tool_result(&text, true, true, &th).0, Engine::Snap);
        assert_eq!(route_tool_result(&text, true, false, &th).0, Engine::None);
        assert_eq!(route_tool_result("tiny", true, true, &th).0, Engine::None);
    }
}
