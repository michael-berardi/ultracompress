//! Token estimation and chars-per-token calibration.
//!
//! Rapid Compact is tokenizer-free by default: it estimates with a calibrated
//! chars/token ratio. When the extension supplies `tokensBefore` (Pi's real
//! measured context size), the ratio is calibrated against the actual
//! tokenizer for this session — the same trick pi-vcc uses, generalized.

/// Default heuristic ratio: ~3.8 chars per token for mixed code/prose tool
/// output (BPE tokenizers average 3.5–4.5 on coding sessions).
pub const DEFAULT_CHARS_PER_TOKEN: f64 = 3.8;

#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenEstimate {
    pub chars_per_token: f64,
    pub calibrated: bool,
}

/// Calibrate chars/token from a session's real measured token count.
///
/// `total_chars` is the character count of the content the measured
/// `tokens_before` refers to. Falls back to the heuristic when either side is
/// unusable. Clamped to a sane band so a weird payload can't skew decisions.
pub fn calibrate(total_chars: usize, tokens_before: Option<u64>) -> TokenEstimate {
    if let Some(t) = tokens_before {
        if t > 200 && total_chars > 1000 {
            let ratio = total_chars as f64 / t as f64;
            if (2.0..=8.0).contains(&ratio) {
                return TokenEstimate { chars_per_token: ratio, calibrated: true };
            }
        }
    }
    TokenEstimate { chars_per_token: DEFAULT_CHARS_PER_TOKEN, calibrated: false }
}

pub fn tokens_from_chars(chars: usize, cpt: f64) -> u64 {
    ((chars as f64) / cpt.max(0.5)) as u64
}

/// Estimate tokens for a PNG frame. Providers charge vision images roughly by
/// area; the widely-used anchor is Claude's (w*h)/750 tokens. OpenAI-style
/// tile pricing lands in the same order of magnitude for our frame shapes.
/// Overridable via config (`imageTokensPerFrame`).
pub fn frame_tokens(width: u32, height: u32, per_frame_override: Option<u64>) -> u64 {
    if let Some(n) = per_frame_override {
        return n;
    }
    ((width as u64 * height as u64) / 750).max(70)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_from_real_tokens() {
        let e = calibrate(38_000, Some(10_000));
        assert!(e.calibrated);
        assert!((e.chars_per_token - 3.8).abs() < 0.01);
    }

    #[test]
    fn calibration_outlier_falls_back() {
        let e = calibrate(1_000, Some(10_000)); // 0.1 chars/token — nonsense
        assert!(!e.calibrated);
        assert!((e.chars_per_token - DEFAULT_CHARS_PER_TOKEN).abs() < f64::EPSILON);
    }

    #[test]
    fn frame_tokens_floor() {
        assert_eq!(frame_tokens(1280, 80, None), 136);
        assert_eq!(frame_tokens(8, 8, None), 70);
        assert_eq!(frame_tokens(0, 0, Some(500)), 500);
    }
}
