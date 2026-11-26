//! Lossless, branch-aware search over raw session JSONL.
use crate::model::{truncate_chars, Block};
use crate::recall_load;
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

/// Transcript container format for recall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallFormat {
    /// Detect Pi session headers vs Claude Code record shapes.
    #[default]
    Auto,
    /// Classic Pi session JSONL (`type:"session"` header, `id`/`parentId`).
    Pi,
    /// Claude Code transcript (`uuid`/`parentUuid` records).
    Claude,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallOptions {
    pub query: String,
    pub regex: bool,
    pub scope_all: bool,
    pub page: usize,
    pub per_page: usize,
    pub leaf_id: Option<String>,
    pub role: Option<String>,
    pub tool_name: Option<String>,
    pub after_entry: Option<String>,
    pub before_entry: Option<String>,
    pub snippet_bytes: usize,
    pub max_output_bytes: usize,
    #[serde(default)]
    pub format: RecallFormat,
}
impl Default for RecallOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            regex: false,
            scope_all: false,
            page: 1,
            per_page: 5,
            leaf_id: None,
            role: None,
            tool_name: None,
            after_entry: None,
            before_entry: None,
            snippet_bytes: 1000,
            max_output_bytes: 12000,
            format: RecallFormat::Auto,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct RecallHit {
    pub entry_id: String,
    pub role: String,
    pub score: f64,
    pub snippet: String,
    pub timestamp: Option<u64>,
    pub matched_terms: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RecallResult {
    pub query: String,
    pub hits: Vec<RecallHit>,
    pub total: usize,
    pub page: usize,
    pub page_count: usize,
    pub searched_messages: usize,
    pub session_id: String,
    pub scope: String,
    pub leaf_id: Option<String>,
    /// Malformed transcript lines skipped while parsing.
    pub warnings: usize,
}

/// BM25 term saturation and length normalization (standard values).
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

enum Matcher {
    Terms(Vec<String>),
    Regex(Regex),
}
fn flat(m: &crate::model::RcMessage) -> String {
    m.content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } | Block::Thinking { text, .. } => Some(text.clone()),
            Block::ToolResult {
                tool_name, text, ..
            } => Some(if tool_name.is_empty() {
                text.clone()
            } else {
                format!("[{tool_name}] {text}")
            }),
            Block::ToolCall {
                name, arguments, ..
            } => Some(format!("[call {name}] {arguments}")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
/// Lowercase for matching. ASCII text (most tool output) folds in place
/// with no per-character Unicode table lookups.
fn fold(t: &str) -> std::borrow::Cow<'_, str> {
    if t.is_ascii() {
        if t.bytes().any(|b| b.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(t.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(t)
        }
    } else {
        std::borrow::Cow::Owned(t.to_lowercase())
    }
}
fn has_tool(m: &crate::model::RcMessage, n: &str) -> bool {
    if !matches!(m.role, crate::model::Role::ToolResult) {
        return false;
    }
    m.content.iter().any(|b| match b {
        Block::ToolResult { tool_name, .. } => tool_name == n,
        _ => false,
    })
}
fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    if max < 3 {
        return String::new();
    }
    let mut end = max - 3;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
fn snippet(text: &str, pos: usize, max: usize) -> String {
    let mut start = pos.min(text.len()).saturating_sub(max / 4);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let prefix = if start > 0 { "…" } else { "" };
    format!(
        "{prefix}{}",
        truncate_bytes(&text[start..], max - prefix.len())
    )
}
// Lowercasing can expand Unicode (e.g. İ -> i + combining dot). Convert the
// match offset back to original bytes before slicing the original evidence.
fn original_offset(text: &str, folded_pos: usize) -> usize {
    let mut offset = 0;
    for (pos, ch) in text.char_indices() {
        let next = offset + ch.to_lowercase().map(char::len_utf8).sum::<usize>();
        if folded_pos < next {
            return pos;
        }
        offset = next;
    }
    text.len()
}
fn shrink(r: &mut RecallResult, budget: usize) -> Result<(), String> {
    loop {
        let n = serde_json::to_vec(r).map_err(|e| e.to_string())?.len();
        if n <= budget {
            return Ok(());
        }
        let mut changed = false;
        for h in &mut r.hits {
            if !h.snippet.is_empty() {
                h.snippet = truncate_bytes(&h.snippet, h.snippet.len() * 3 / 4);
                changed = true
            }
        }
        if !changed {
            return Err("max_output_bytes too small for result metadata".into());
        }
    }
}

pub fn search(path: &Path, o: &RecallOptions) -> Result<RecallResult, String> {
    if o.query.trim().is_empty() || o.query.chars().count() > 512 {
        return Err("query must be 1..512 characters".into());
    }
    if !(1..=20).contains(&o.per_page) || !(1..=1_000_000).contains(&o.page) {
        return Err("page and per_page must be within valid bounds".into());
    }
    if !(128..=4000).contains(&o.snippet_bytes) || !(1024..=32000).contains(&o.max_output_bytes) {
        return Err("snippet_bytes or max_output_bytes out of bounds".into());
    }
    if o.role
        .as_ref()
        .is_some_and(|r| !["user", "assistant", "toolResult"].contains(&r.as_str()))
    {
        return Err("invalid role".into());
    }
    for value in [&o.tool_name, &o.after_entry, &o.before_entry]
        .into_iter()
        .flatten()
    {
        if value.is_empty() || value.chars().count() > 512 {
            return Err("invalid recall filter".into());
        }
    }
    if o.scope_all && o.leaf_id.is_some() {
        return Err("explicit leaf is incompatible with scope_all".into());
    }
    let f = recall_load::load_with_format(path, o.format)?;
    let (indices, leaf) = if o.scope_all {
        recall_load::validate_all(&f)?;
        ((0..f.entries.len()).collect(), None)
    } else {
        recall_load::lineage(&f, o.leaf_id.as_deref())?
    };
    let pos: HashMap<String, usize> = indices
        .iter()
        .enumerate()
        .map(|(p, i)| (f.entries[*i].id.clone(), p))
        .collect();
    let lo = o
        .after_entry
        .as_ref()
        .map(|x| {
            pos.get(x)
                .copied()
                .ok_or_else(|| "after_entry not found in selected scope".to_string())
        })
        .transpose()?;
    let hi = o
        .before_entry
        .as_ref()
        .map(|x| {
            pos.get(x)
                .copied()
                .ok_or_else(|| "before_entry not found in selected scope".to_string())
        })
        .transpose()?;
    if let (Some(a), Some(b)) = (lo, hi) {
        if a >= b {
            return Err("invalid entry range".into());
        }
    }
    let matcher = if o.regex {
        Matcher::Regex(Regex::new(&o.query).map_err(|e| format!("bad regex: {e}"))?)
    } else {
        Matcher::Terms(
            o.query
                .to_lowercase()
                .split_whitespace()
                .map(str::to_string)
                .collect(),
        )
    };
    let mut rows = Vec::new();
    for (p, i) in indices.iter().enumerate() {
        if lo.is_some_and(|x| p <= x) || hi.is_some_and(|x| p >= x) {
            continue;
        }
        let e = &f.entries[*i];
        let m = match &e.message {
            Some(x) => x,
            None => continue,
        };
        if o.role.as_ref().is_some_and(|x| x != &m.role.to_string())
            || o.tool_name.as_ref().is_some_and(|x| !has_tool(m, x))
        {
            continue;
        }
        let t = flat(m);
        if t.is_empty() {
            continue;
        }
        rows.push((e.id.clone(), m.role.to_string(), t, m.timestamp));
    }
    let searched = rows.len();
    // One lowercase pass per message: term counts and the first match are
    // taken together, then document frequency follows from the counts.
    let mut term_hits: Vec<Option<(Vec<usize>, Vec<usize>)>> = Vec::new();
    let mut df: Vec<usize> = Vec::new();
    if let Matcher::Terms(ts) = &matcher {
        df = vec![0; ts.len()];
        term_hits.reserve(rows.len());
        for (_, _, t, _) in &rows {
            let l = fold(t);
            let mut any = false;
            let mut firsts = vec![usize::MAX; ts.len()];
            let counts: Vec<usize> = ts
                .iter()
                .enumerate()
                .map(|(qi, q)| {
                    let mut n = 0;
                    for (pos, _) in l.match_indices(q.as_str()) {
                        if n == 0 {
                            firsts[qi] = pos;
                        }
                        n += 1;
                    }
                    if n > 0 {
                        df[qi] += 1;
                        any = true;
                    }
                    n
                })
                .collect();
            if any && !(l.len() == t.len() && t.is_ascii()) {
                // ASCII text folds byte for byte; otherwise map offsets back.
                for pos in firsts.iter_mut().filter(|p| **p != usize::MAX) {
                    *pos = original_offset(t, *pos);
                }
            }
            term_hits.push(any.then_some((counts, firsts)));
        }
    }
    let avg_len =
        (rows.iter().map(|r| r.2.len()).sum::<usize>() as f64 / rows.len().max(1) as f64).max(1.0);
    let mut hits = Vec::new();
    for (row, (id, role, t, stamp)) in rows.into_iter().enumerate() {
        match &matcher {
            Matcher::Regex(re) => {
                if let Some(x) = re.find(&t) {
                    hits.push(RecallHit {
                        entry_id: id,
                        role,
                        score: 1.0,
                        snippet: snippet(&t, x.start(), o.snippet_bytes),
                        timestamp: stamp,
                        matched_terms: vec![truncate_chars(x.as_str(), 60)],
                    })
                }
            }
            Matcher::Terms(ts) => {
                let Some((counts, firsts)) = term_hits[row].take() else {
                    continue;
                };
                // Show the excerpt around the rarest matched term (fewest
                // messages, then fewest repeats here): in a long log the
                // common words match at the top, the specific one (an id, a
                // code) is the line the caller is looking for.
                let p = (0..ts.len())
                    .filter(|&qi| counts[qi] > 0)
                    .min_by_key(|&qi| (df[qi], counts[qi], firsts[qi]))
                    .map_or(0, |qi| firsts[qi]);
                let mut score = 0.;
                let mut mt = Vec::new();
                for (qi, q) in ts.iter().enumerate() {
                    let n = counts[qi];
                    if n > 0 {
                        let d = df[qi].max(1);
                        // BM25: repeats saturate and long messages are
                        // normalized, so one huge log that mentions a term
                        // many times does not outrank the message about it.
                        let idf = (searched as f64 / d as f64).ln().max(0.5);
                        let nf = n as f64;
                        let norm = 1.0 - BM25_B + BM25_B * t.len() as f64 / avg_len;
                        score += idf * nf * (BM25_K1 + 1.0) / (nf + BM25_K1 * norm);
                        mt.push(q.clone())
                    }
                }
                // A message matching every query term beats one that repeats
                // a single term.
                let coverage = mt.len() as f64 / ts.len().max(1) as f64;
                score *= coverage * coverage;
                if score > 0.0 {
                    hits.push(RecallHit {
                        entry_id: id,
                        role,
                        score,
                        snippet: snippet(&t, p, o.snippet_bytes),
                        timestamp: stamp,
                        matched_terms: mt,
                    })
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
    let pc = total.div_ceil(o.per_page).max(1);
    if o.page > pc {
        return Err("page exceeds available result pages".into());
    }
    let page = o.page;
    let hits = hits
        .into_iter()
        .skip((page - 1) * o.per_page)
        .take(o.per_page)
        .collect();
    let mut r = RecallResult {
        query: o.query.clone(),
        hits,
        total,
        page,
        page_count: pc,
        searched_messages: searched,
        session_id: f.session_id,
        scope: if o.scope_all {
            "all".into()
        } else {
            "lineage".into()
        },
        leaf_id: leaf,
        warnings: f.warnings,
    };
    shrink(&mut r, o.max_output_bytes)?;
    Ok(r)
}
