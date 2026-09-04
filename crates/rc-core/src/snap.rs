//! Snap-frame rendering: turn bulky text into compact PNG image frames.
//!
//! This is Rapid Compact's open-source take on OMP's "snapcompact": archive
//! text at fixed vision-token cost by rasterizing it into deterministic PNG
//! frames the model can still read. Text edges (head/tail) stay as text; the
//! expensive middle becomes pixels. Rendering is fully deterministic — same
//! text + same config ⇒ byte-identical PNG.

use crate::model::truncate_chars;
use png::{BitDepth, ColorType, Encoder};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapConfig {
    /// Terminal columns per frame (chars).
    pub cols: usize,
    /// Terminal rows per frame (chars).
    pub rows: usize,
    /// Chars of original text kept as the text head.
    pub head_chars: usize,
    /// Chars kept as the text tail.
    pub tail_chars: usize,
    /// Font cell width in pixels (8x8 bitmap font, integer scale).
    pub scale: u32,
}

impl Default for SnapConfig {
    fn default() -> Self {
        // cols == 0 means adaptive: shape is auto-tuned to the content's line
        // length distribution (OMP-style "auto" shape, but per-block instead
        // of per-model). Fixed width via cfg overrides.
        SnapConfig { cols: 0, rows: 120, head_chars: 600, tail_chars: 400, scale: 1 }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Frame {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub chars: usize,
    pub png_base64: String,
    #[serde(skip_serializing)]
    pub png_bytes: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SnapResult {
    pub frames: Vec<Frame>,
    /// Text head kept verbatim (includes leading marker).
    pub head: String,
    /// Text tail kept verbatim.
    pub tail: String,
    pub source_chars: usize,
    pub archived_chars: usize,
}

const FG: u8 = 230; // near-white
const BG: u8 = 16; // near-black

/// Content-adaptive frame width: p90 line length, rounded to an 8px boundary
/// with margin, clamped to a sane band. Line-oriented output (logs, listings)
/// gets narrow, efficient frames; wide output (minified JSON, table rows)
/// gets wide frames. Returns cols in characters.
pub fn adaptive_cols(text: &str) -> usize {
    let mut lens: Vec<usize> = text.lines().map(|l| l.chars().count()).collect();
    if lens.is_empty() {
        return 80;
    }
    lens.sort_unstable();
    let p90 = lens[lens.len() * 9 / 10];
    let cols = p90.next_multiple_of(8) + 8; // margin for wrapping
    cols.clamp(40, 200)
}

/// Line-aware frame estimate: how many frames would `text` occupy at the
/// given shape? Terminal content is line-bound — a frame holds `rows - 1`
/// source lines (after hard-wrap at cols), regardless of char capacity.
pub fn estimate_frames(text: &str, cols: usize, rows: usize) -> usize {
    let grid_rows = rows.saturating_sub(1).max(1);
    let wrapped: usize = text
        .lines()
        .map(|l| l.chars().count().div_ceil(cols).max(1))
        .sum();
    wrapped.div_ceil(grid_rows).max(1)
}

/// Full snap economics for one payload: shape, frame count, and whether the
/// swap beats text by the required margin (≥ 25% block savings).
#[derive(Debug, Clone)]
pub struct SnapPlan {
    pub cols: usize,
    pub rows: usize,
    pub frames_needed: usize,
    pub snap_tokens: u64,
    pub text_tokens: u64,
    pub worthwhile: bool,
}

pub fn plan_snap(
    text: &str,
    cfg: &SnapConfig,
    image_tokens_per_frame: Option<u64>,
    cpt: f64,
) -> SnapPlan {
    let cols = if cfg.cols == 0 { adaptive_cols(text) } else { cfg.cols };
    let rows = cfg.rows;
    let frames_needed = estimate_frames(text, cols, rows);
    let w = (cols as u32) * 8 + 8;
    let h = (rows as u32) * 8 + 8;
    let per_frame = crate::estimate::frame_tokens(w, h, image_tokens_per_frame);
    let snap_tokens = frames_needed as u64 * per_frame;
    let text_tokens = crate::estimate::tokens_from_chars(text.len(), cpt);
    let worthwhile = snap_tokens * 4 <= text_tokens * 3; // ≥25% savings required
    SnapPlan { cols, rows, frames_needed, snap_tokens, text_tokens, worthwhile }
}

/// Lay text into grids of (cols × rows) cells, splitting on line boundaries.
pub fn layout_lines(text: &str, cols: usize, rows: usize) -> Vec<Vec<String>> {
    let mut grids: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::with_capacity(rows);
    for raw_line in text.lines() {
        // Hard-wrap long lines at the column limit.
        let mut line = raw_line;
        loop {
            let (head, rest) = split_at_cols(line, cols);
            current.push(head.to_string());
            if current.len() == rows {
                grids.push(std::mem::replace(&mut current, Vec::with_capacity(rows)));
            }
            if rest.is_empty() {
                break;
            }
            line = rest;
        }
    }
    if !current.is_empty() {
        grids.push(current);
    }
    if grids.is_empty() {
        grids.push(vec![String::new()]);
    }
    grids
}

fn split_at_cols(s: &str, cols: usize) -> (&str, &str) {
    if s.len() <= cols {
        return (s, "");
    }
    let mut end = cols;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], &s[end..])
}

/// Render text into frames. `label` names the payload in the frame header.
/// When cfg.cols == 0 the width is content-adaptive (p90 line length).
pub fn render_frames(text: &str, label: &str, cfg: &SnapConfig) -> SnapResult {
    let source_chars = text.len();
    let cols = if cfg.cols == 0 { adaptive_cols(text) } else { cfg.cols };
    let body_start = cfg.head_chars.min(source_chars);
    let body_end = source_chars.saturating_sub(cfg.tail_chars).max(body_start);
    let archived = &text[clamp_boundary(text, body_start)..clamp_boundary(text, body_end)];

    let total_grid_rows = cfg.rows.saturating_sub(1); // header row per frame
    let grids = layout_lines(archived, cols, total_grid_rows);
    let total = grids.len();

    let mut frames = Vec::with_capacity(grids.len());
    for (i, grid) in grids.iter().enumerate() {
        let header = format!("── rapid-compact frame {}/{} · {} · archived text, verbatim ──", i + 1, total, label);
        let mut buf = String::with_capacity(cfg.rows * (cfg.cols + 1));
        buf.push_str(&header);
        buf.push('\n');
        for row in grid {
            buf.push_str(row);
            buf.push('\n');
        }
        let (w, h, png) = render_grid_png(&buf, cols, cfg.rows, cfg.scale);
        let id = format!("f{}", i + 1);
        frames.push(Frame {
            id: id.clone(),
            width: w,
            height: h,
            chars: buf.len(),
            png_base64: base64_encode(&png),
            png_bytes: png.len(),
        });
    }

    let omitted = source_chars.saturating_sub(body_start).saturating_sub(cfg.tail_chars);
    let head = if body_start > 0 {
        format!("{}\n", truncate_chars(text, body_start).trim_end())
    } else {
        String::new()
    };
    let tail = if cfg.tail_chars > 0 && omitted > 0 {
        let start = clamp_boundary(text, source_chars - cfg.tail_chars.min(source_chars));
        format!("\n{}", text[start..].trim_start())
    } else {
        String::new()
    };

    SnapResult { frames, head, tail, source_chars, archived_chars: omitted }
}

fn clamp_boundary(s: &str, i: usize) -> usize {
    let mut e = i.min(s.len());
    while e > 0 && !s.is_char_boundary(e) {
        e -= 1;
    }
    e
}

/// Render a char grid to a grayscale PNG. Deterministic: fixed palette,
/// fixed encoder settings.
pub fn render_grid_png(text: &str, cols: usize, rows: usize, scale: u32) -> (u32, u32, Vec<u8>) {
    let scale = scale.max(1);
    let cw = 8 * scale;
    let pad = 4 * scale;
    let width = (cols as u32 * cw) + pad * 2;
    let height = (rows as u32 * cw) + pad * 2;
    let mut buf = vec![BG; (width * height) as usize];

    let mut cy = pad;
    for line in text.lines().take(rows) {
        let mut cx = pad;
        for ch in line.chars().take(cols) {
            if ch != ' ' {
                let glyph = glyph_bits(ch);
                for (gy, bits) in glyph.iter().enumerate() {
                    for gx in 0..8 {
                        if bits & (1 << gx) != 0 {
                            for sy in 0..scale {
                                for sx in 0..scale {
                                    let px = cx + (gx as u32) * scale + sx;
                                    let py = cy + (gy as u32) * scale + sy;
                                    if px < width && py < height {
                                        buf[(py * width + px) as usize] = FG;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            cx += cw;
        }
        cy += cw;
    }

    let mut png = Vec::new();
    {
        let mut enc = Encoder::new(&mut png, width, height);
        enc.set_color(ColorType::Grayscale);
        enc.set_depth(BitDepth::Eight);
        enc.set_compression(png::Compression::Balanced);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&buf).expect("png data");
    }
    (width, height, png)
}

/// Glyph bits from the embedded 8x8 font (public-domain font data via the
/// font8x8 crate). Printable ASCII only; anything else renders as '·'.
fn glyph_bits(ch: char) -> [u8; 8] {
    use font8x8::UnicodeFonts;
    const FALLBACK: [u8; 8] = [0x00, 0x3C, 0x7E, 0x7E, 0x7E, 0x7E, 0x3C, 0x00]; // solid dot
    if ch.is_ascii_graphic() || ch == ' ' {
        font8x8::BASIC_FONTS.get(ch).unwrap_or(FALLBACK)
    } else {
        font8x8::BASIC_FONTS.get('·').or_else(|| font8x8::BASIC_FONTS.get('?')).unwrap_or(FALLBACK)
    }
}

/// Minimal standard base64 (RFC 4648) — avoids a heavy dependency.
pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_png_output() {
        let cfg = SnapConfig::default();
        let a = render_frames("line one\nline two\nline three", "test", &cfg);
        let b = render_frames("line one\nline two\nline three", "test", &cfg);
        assert_eq!(a.frames.len(), b.frames.len());
        assert_eq!(a.frames[0].png_base64, b.frames[0].png_base64);
        assert!(a.frames[0].png_bytes > 100);
        // Valid PNG magic.
        let raw = a.frames[0].png_base64.clone();
        let dec = base64_decode(&raw);
        assert_eq!(&dec[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn splits_long_text_into_multiple_frames() {
        let cfg = SnapConfig { cols: 40, rows: 5, head_chars: 0, tail_chars: 0, scale: 1 };
        let text = (0..40).map(|i| format!("row-{i:02} {}", "x".repeat(30))).collect::<Vec<_>>().join("\n");
        let r = render_frames(&text, "bench", &cfg);
        assert!(r.frames.len() >= 2, "expected multiple frames, got {}", r.frames.len());
        assert!(r.frames.iter().all(|f| f.width > 0 && f.height > 0));
    }

    #[test]
    fn keeps_head_and_tail() {
        let cfg = SnapConfig { cols: 80, rows: 10, head_chars: 20, tail_chars: 10, scale: 1 };
        let text = "H".repeat(20) + &"M".repeat(500) + &"T".repeat(10);
        let r = render_frames(&text, "t", &cfg);
        assert!(r.head.starts_with("HHHH"));
        assert!(r.tail.trim_start().starts_with("TTTT"));
        assert_eq!(r.archived_chars, 500);
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        let mut table = [255u8; 256];
        for (i, c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
            table[*c as usize] = i as u8;
        }
        let bytes: Vec<u8> = s.bytes().filter(|b| table[*b as usize] != 255).collect();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let n = chunk.iter().fold(0u32, |acc, c| (acc << 6) | table[*c as usize] as u32);
            out.push((n >> 16) as u8);
            if chunk.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(n as u8);
            }
        }
        out
    }
}
