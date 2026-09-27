//! Block classification: what kind of content is this, and which engine
//! should own it?

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentClass {
    /// Parseable JSON object/array.
    Json,
    Text,
    /// Already an image or opaque payload — leave alone.
    Opaque,
    Empty,
}

/// Sniff whether a tool result is a JSON payload.
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
    /// Minimum chars before a text tool result is snap-framed (default 6000).
    pub snap_min_chars: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            snap_min_chars: 6_000,
        }
    }
}

/// The engine a block is routed to. `None` = keep verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Snap,
    Vcc,
    None,
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
}
