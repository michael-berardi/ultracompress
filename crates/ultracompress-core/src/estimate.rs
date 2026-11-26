//! Token estimation and chars-per-token calibration.
//!
//! UltraCompress is tokenizer-free by default: it estimates with a calibrated
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
                return TokenEstimate {
                    chars_per_token: ratio,
                    calibrated: true,
                };
            }
        }
    }
    TokenEstimate {
        chars_per_token: DEFAULT_CHARS_PER_TOKEN,
        calibrated: false,
    }
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

/// Flat estimate for an image whose pixel size cannot be read.
pub const IMAGE_TOKENS_UNKNOWN: u64 = 1_200;
/// Providers downscale large images, so cost stops growing past this.
pub const IMAGE_TOKENS_MAX: u64 = 1_600;

/// Estimate the tokens an attached image costs from its base64 payload.
///
/// Images carry no characters, so a character count alone treats a
/// screenshot-heavy context as nearly empty. Only the header is decoded (PNG,
/// GIF, WebP and JPEG sizes sit in the first bytes), never the whole payload.
pub fn image_tokens_from_base64(data: &str) -> u64 {
    if data.is_empty() {
        return IMAGE_TOKENS_UNKNOWN;
    }
    // PNG, GIF and WebP state their size in the first 32 bytes. Only a JPEG
    // needs more: its size marker can follow embedded metadata (96 KiB covers it).
    let mut head = base64_decode_prefix(data, 48);
    if head.starts_with(&[0xff, 0xd8]) {
        head = base64_decode_prefix(data, 96 * 1024);
    }
    match image_dimensions(&head) {
        Some((w, h)) if w > 0 && h > 0 => ((w as u64 * h as u64) / 750).clamp(70, IMAGE_TOKENS_MAX),
        _ => IMAGE_TOKENS_UNKNOWN,
    }
}

/// Decode at most `max_bytes` from the start of a base64 string. Stops at the
/// first byte outside the alphabet (padding, a data-URL remnant, whitespace).
fn base64_decode_prefix(data: &str, max_bytes: usize) -> Vec<u8> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(max_bytes.min(data.len() / 4 * 3 + 3));
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in data.as_bytes() {
        if out.len() >= max_bytes {
            break;
        }
        let Some(v) = val(c) else { break };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// Pixel size from an image header: PNG, GIF, WebP or JPEG.
fn image_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le24 = |i: usize| Some(le16(i)? | ((*b.get(i + 2)? as u32) << 16));
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let be32 = |i: usize| {
            Some(u32::from_be_bytes([
                *b.get(i)?,
                *b.get(i + 1)?,
                *b.get(i + 2)?,
                *b.get(i + 3)?,
            ]))
        };
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let v = u32::from_le_bytes([*b.get(21)?, *b.get(22)?, *b.get(23)?, *b.get(24)?]);
                Some(((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1))
            }
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if marker == 0xff {
                i += 1;
                continue;
            }
            // Start-of-frame markers carry the size; C4, C8 and CC do not.
            if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            if (0xd0..=0xd9).contains(&marker) || marker == 0x01 {
                i += 2;
                continue;
            }
            i += 2 + be16(i + 2)? as usize;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_tokens_read_the_png_header_only() {
        // 1x1 PNG: floor applies.
        let tiny = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk";
        assert_eq!(image_tokens_from_base64(tiny), 70);
        // Header for a 1500x1000 PNG; the body is never needed.
        let mut head = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        head.extend_from_slice(&1500u32.to_be_bytes());
        head.extend_from_slice(&1000u32.to_be_bytes());
        let b64 = crate::snap::base64_encode(&head);
        assert_eq!(image_tokens_from_base64(&b64), IMAGE_TOKENS_MAX);
        let mut small = head[..16].to_vec();
        small.extend_from_slice(&600u32.to_be_bytes());
        small.extend_from_slice(&500u32.to_be_bytes());
        assert_eq!(
            image_tokens_from_base64(&crate::snap::base64_encode(&small)),
            400
        );
    }

    #[test]
    fn image_tokens_read_jpeg_gif_and_fall_back() {
        // JPEG: SOI, APP0 (16 bytes), SOF0 with height 300, width 750.
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        jpeg.extend_from_slice(&[0u8; 14]);
        jpeg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08, 0x01, 0x2c, 0x02, 0xee]);
        assert_eq!(
            image_tokens_from_base64(&crate::snap::base64_encode(&jpeg)),
            300
        );
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0xee, 0x02, 0x2c, 0x01]);
        assert_eq!(
            image_tokens_from_base64(&crate::snap::base64_encode(&gif)),
            300
        );
        assert_eq!(image_tokens_from_base64(""), IMAGE_TOKENS_UNKNOWN);
        assert_eq!(
            image_tokens_from_base64("not an image"),
            IMAGE_TOKENS_UNKNOWN
        );
    }

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
