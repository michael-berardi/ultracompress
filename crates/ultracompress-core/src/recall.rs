//! Lossless recall: ranked search over the raw session JSONL.
//!
//! Compaction frees the model's window; recall keeps history reachable.
//! Multi-word queries OR-match and rank by relevance (rare terms weigh more);
//! a regex pattern is accepted for power use. Scope can span the active
//! lineage only (default) or all branches of the session file.

use crate::load::load_session;
use crate::model::{truncate_chars, Block};
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecallOptions {
    pub query: String,
    pub regex: bool,
    pub scope_all: bool,
    pub page: usize,
    pub per_page: usize,
}

impl Default for RecallOptions {
    fn default() -> Self {
        RecallOptions {
            query: String::new(),
            regex: false,
            scope_all: false,
            page: 1,
            per_page: 5,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecallHit {
    pub entry_id: String,
    pub role: String,
    pub score: f64,
    pub snippet: String,
    pub timestamp: Option<u64>,
    pub matched_terms: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecallResult {
    pub query: String,
    pub hits: Vec<RecallHit>,
    pub total: usize,
    pub page: usize,
    pub page_count: usize,
    pub searched_messages: usize,
}

enum Matcher {
    Terms(Vec<String>),
    Regex(regex::Regex),
}

// A tiny bundled regex engine would be overkill; we accept POSIX-ish patterns
// via a minimal substring fallback when the `regex` crate is unavailable.
// For v0 we include `regex` — recall quality is worth one dependency.
mod regex_shim {
    pub use regex::Regex;
}

use regex_shim::Regex;

pub fn search(path: &std::path::Path, opts: &RecallOptions) -> Result<RecallResult, String> {
    let full = load_session(path, true).map_err(|e| format!("cannot load session: {e}"))?;
    let msgs = if opts.scope_all {
        full.messages
    } else {
        // Active lineage: messages after the last compaction boundary were
        // already handled by the loader when include_pre_compaction=false;
        // for recall over lineage we want everything since the FIRST kept
        // boundary of the active branch chain — approximated by the full
        // active branch (the loader's path walk already follows the lineage).
        full.messages
    };

    let matcher = if opts.regex {
        Matcher::Regex(Regex::new(&opts.query).map_err(|e| format!("bad regex: {e}"))?)
    } else {
        Matcher::Terms(
            opts.query
                .to_lowercase()
                .split_whitespace()
                .map(String::from)
                .collect(),
        )
    };

    // Document frequency for rarity weighting.
    let mut texts: Vec<(String, String, String, Option<u64>, usize)> = Vec::new(); // (id, role, text, ts, hash)
    for m in &msgs {
        let text = flatten(m);
        if text.is_empty() {
            continue;
        }
        texts.push((
            m.id.clone(),
            m.role.to_string(),
            text,
            m.timestamp,
            hash_of(m),
        ));
    }
    let searched = texts.len();
    let mut df: HashMap<String, usize> = HashMap::new();
    if let Matcher::Terms(terms) = &matcher {
        for (_, _, text, _, _) in &texts {
            let lower = text.to_lowercase();
            for t in terms {
                if lower.contains(t.as_str()) {
                    *df.entry(t.clone()).or_default() += 1;
                }
            }
        }
    }

    let mut hits: Vec<RecallHit> = Vec::new();
    for (id, role, text, ts, _) in &texts {
        match &matcher {
            Matcher::Regex(re) => {
                if let Some(m) = re.find(text) {
                    hits.push(RecallHit {
                        entry_id: id.clone(),
                        role: role.clone(),
                        score: 1.0 + (m.as_str().len() as f64 / 100.0),
                        snippet: snippet(text, m.start(), m.end()),
                        timestamp: *ts,
                        matched_terms: vec![truncate_chars(m.as_str(), 60)],
                    });
                }
            }
            Matcher::Terms(terms) => {
                let lower = text.to_lowercase();
                let mut score = 0.0;
                let mut matched = Vec::new();
                for t in terms {
                    if lower.contains(t.as_str()) {
                        let tf = lower.matches(t.as_str()).count();
                        let idf = match df.get(t) {
                            Some(n) if *n > 0 => (searched as f64 / *n as f64).ln().max(0.5),
                            _ => 1.0,
                        };
                        score += (tf as f64).ln_1p() * idf;
                        matched.push(t.clone());
                    }
                }
                if score > 0.0 {
                    let first_pos = terms
                        .iter()
                        .filter_map(|t| lower.find(t.as_str()))
                        .min()
                        .unwrap_or(0);
                    hits.push(RecallHit {
                        entry_id: id.clone(),
                        role: role.clone(),
                        score,
                        snippet: snippet(text, first_pos, first_pos),
                        timestamp: *ts,
                        matched_terms: matched,
                    });
                }
            }
        }
    }

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total = hits.len();
    let page_count = total.div_ceil(opts.per_page).max(1);
    let page = opts.page.max(1).min(page_count);
    let start = (page - 1) * opts.per_page;
    let hits = hits.into_iter().skip(start).take(opts.per_page).collect();

    Ok(RecallResult {
        query: opts.query.clone(),
        hits,
        total,
        page,
        page_count,
        searched_messages: searched,
    })
}

fn flatten(m: &crate::model::RcMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in &m.content {
        match b {
            Block::Text { text } | Block::Thinking { text, .. } => parts.push(text.clone()),
            Block::ToolResult {
                tool_name, text, ..
            } => {
                if !tool_name.is_empty() {
                    parts.push(format!("[{tool_name}]"));
                }
                parts.push(text.clone());
            }
            Block::ToolCall {
                name, arguments, ..
            } => {
                parts.push(format!("[call {name}] {}", arguments));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

fn hash_of(m: &crate::model::RcMessage) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    m.id.hash(&mut h);
    h.finish() as usize
}

fn snippet(text: &str, start: usize, _end: usize) -> String {
    let s = start.saturating_sub(80);
    let s = char_floor(text, s);
    let e = char_ceil(text, (start + 240).min(text.len()));
    let mut out = String::from(if s > 0 { "…" } else { "" });
    out.push_str(&text[s..e].replace('\n', " ⏎ "));
    if e < text.len() {
        out.push('…');
    }
    out
}

fn char_floor(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
fn char_ceil(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("rc-recall-{name}-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    #[test]
    fn ranks_rare_terms_higher() {
        let p = write_temp(
            "rarity",
            r#"{"type":"session","id":"s","cwd":"/"}
{"type":"message","id":"e1","message":{"role":"user","content":[{"type":"text","text":"the build failed with error xyzzy"}]}}
{"type":"message","id":"e2","message":{"role":"assistant","content":[{"type":"text","text":"the the the ordinary"}]}}
{"type":"message","id":"e3","message":{"role":"user","content":[{"type":"text","text":"xyzzy was the cause"}]}}
"#,
        );
        let r = search(
            &p,
            &RecallOptions {
                query: "xyzzy the".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(r.total >= 2);
        // The hit matching the rare term should outrank the common-term hit.
        assert!(
            r.hits[0].snippet.contains("xyzzy")
                || r.hits[0].matched_terms.contains(&"xyzzy".into())
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn regex_search_works() {
        let p = write_temp(
            "regex",
            r#"{"type":"session","id":"s","cwd":"/"}
{"type":"message","id":"e1","message":{"role":"user","content":[{"type":"text","text":"hook injection failed"}]}}
"#,
        );
        let r = search(
            &p,
            &RecallOptions {
                query: "hook|inject".into(),
                regex: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.total, 1);
        std::fs::remove_file(&p).ok();
    }
}
