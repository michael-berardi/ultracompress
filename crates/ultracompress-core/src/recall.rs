//! Lossless, branch-aware search over raw session JSONL.
use crate::model::{truncate_chars, Block};
use crate::recall_load;
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

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
}

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
    let f = recall_load::load(path)?;
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
    let mut df = HashMap::new();
    if let Matcher::Terms(ts) = &matcher {
        for (_, _, t, _) in &rows {
            let l = t.to_lowercase();
            for q in ts {
                if l.contains(q) {
                    *df.entry(q.clone()).or_insert(0) += 1
                }
            }
        }
    }
    let mut hits = Vec::new();
    for (id, role, t, stamp) in rows {
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
                let l = t.to_lowercase();
                let mut score = 0.;
                let mut mt = Vec::new();
                for q in ts {
                    if l.contains(q) {
                        let n = l.matches(q).count();
                        let d = df.get(q).copied().unwrap_or(1);
                        score += (n as f64).ln_1p() * (searched as f64 / d as f64).ln().max(0.5);
                        mt.push(q.clone())
                    }
                }
                if score > 0.0 {
                    let p =
                        original_offset(&t, ts.iter().filter_map(|q| l.find(q)).min().unwrap_or(0));
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
    };
    shrink(&mut r, o.max_output_bytes)?;
    Ok(r)
}
