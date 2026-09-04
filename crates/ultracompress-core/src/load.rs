//! Readers for Pi session JSONL files and stdin message arrays.

use crate::model::{parse_message, RcMessage};
use serde_json::Value;
use std::io::Read;
use std::path::Path;

/// A parsed session: header info + live messages of the active branch.
#[derive(Debug, Clone)]
pub struct LoadedSession {
    pub session_id: String,
    pub cwd: String,
    pub messages: Vec<RcMessage>,
    pub entry_count: usize,
}

/// Load the active branch of a Pi session JSONL (version 2/3 format).
///
/// Pi sessions are append-only trees: each entry has a `parentId`. The active
/// branch is found by walking from the last entry back to the root, then
/// replaying entries along that path in order. Compaction entries mark summary
/// boundaries; `include_pre_compaction` controls whether messages before the
/// last compaction boundary are included (they exist on disk and stay
/// searchable — UltraCompress uses this for lossless recall).
pub fn load_session(path: &Path, include_pre_compaction: bool) -> std::io::Result<LoadedSession> {
    let raw = std::fs::read_to_string(path)?;
    parse_session_str(&raw, include_pre_compaction)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "unparseable session"))
}

pub fn parse_session_str(raw: &str, include_pre_compaction: bool) -> Option<LoadedSession> {
    let mut session_id = String::new();
    let mut cwd = String::new();
    let mut entries: Vec<(String, Option<String>, String, Value)> = Vec::new(); // (id, parent, type, value)

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let obj = v.as_object()?;
        match obj.get("type").and_then(|t| t.as_str()) {
            Some("session") => {
                session_id = obj
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                cwd = obj
                    .get("cwd")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            Some(ty) => {
                let id = obj
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let parent = obj
                    .get("parentId")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string());
                entries.push((id, parent, ty.to_string(), v));
            }
            None => continue,
        }
    }

    if entries.is_empty() {
        return Some(LoadedSession {
            session_id,
            cwd,
            messages: vec![],
            entry_count: 0,
        });
    }

    // Walk from the last entry back to the root to find the active branch.
    // Entries whose parentId is missing or unresolvable (hand-made fixtures,
    // truncated files) chain to the immediately preceding entry, preserving
    // linear order — well-formed sessions keep exact tree semantics.
    let index_by_id: std::collections::HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, (id, _, _, _))| (id.as_str(), i))
        .collect();

    let mut path_idx: Vec<usize> = Vec::new();
    let mut cur = entries.len() - 1;
    loop {
        path_idx.push(cur);
        let parent = entries[cur].1.as_deref();
        let next = match parent.and_then(|p| index_by_id.get(p)) {
            Some(&p) => p,
            None => {
                // Tolerant chaining: parent unknown → previous entry, unless
                // this entry explicitly declared a parent that is simply absent
                // AND this is the first entry we touched (root fallback).
                if cur > 0 && path_idx.len() < entries.len() {
                    cur - 1
                } else {
                    break;
                }
            }
        };
        cur = next;
        if path_idx.len() > entries.len() + 1 {
            break; // cycle guard
        }
    }
    path_idx.reverse();

    // Collect messages on the branch, honoring the last compaction boundary
    // unless the caller wants full history (recall / stats).
    let mut messages = Vec::new();
    let mut last_compaction_pos: Option<usize> = None;
    for (pos, &i) in path_idx.iter().enumerate() {
        if entries[i].2 == "compaction" {
            last_compaction_pos = Some(pos);
        }
    }

    let start = if include_pre_compaction {
        0
    } else {
        match last_compaction_pos {
            Some(pos) => {
                // Skip past the compaction entry, then honor firstKeptEntryId.
                let comp_i = path_idx[pos];
                let kept_id = entries[comp_i]
                    .3
                    .get("firstKeptEntryId")
                    .and_then(|k| k.as_str())
                    .unwrap_or("");
                match if kept_id.is_empty() {
                    None
                } else {
                    index_by_id.get(kept_id).copied()
                } {
                    Some(ki) => path_idx.iter().position(|&x| x == ki).unwrap_or(pos + 1),
                    None => pos + 1,
                }
            }
            None => 0,
        }
    };

    let mut synth = 0usize;
    for &i in &path_idx[start.min(path_idx.len())..] {
        let (_, _, ty, val) = &entries[i];
        if ty == "message" {
            synth += 1;
            if let Some(m) = parse_message(val, &format!("s{synth}")) {
                messages.push(m);
            }
        }
    }

    Some(LoadedSession {
        session_id,
        cwd,
        messages,
        entry_count: entries.len(),
    })
}

/// Read all bytes from stdin.
pub fn read_stdin() -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin().lock().read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
{"type":"session","version":3,"id":"sess1","cwd":"/tmp/proj"}
{"type":"model_change","id":"mc1","parentId":null,"timestamp":1}
{"type":"message","id":"e1","parentId":"mc1","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"message","id":"e2","parentId":"e1","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}
{"type":"compaction","id":"c1","parentId":"e2","summary":"old summary","firstKeptEntryId":"e3","tokensBefore":100}
{"type":"message","id":"e3","parentId":"c1","message":{"role":"user","content":[{"type":"text","text":"second"}]}}
{"type":"message","id":"e4","parentId":"e3","message":{"role":"assistant","content":[{"type":"text","text":"answer"}]}}
"#;

    #[test]
    fn active_branch_honors_compaction_boundary() {
        let s = parse_session_str(SAMPLE, false).unwrap();
        assert_eq!(s.session_id, "sess1");
        assert_eq!(
            s.messages.len(),
            2,
            "only post-compaction messages are live"
        );
        assert_eq!(s.messages[0].text_preview(20), "second");
    }

    #[test]
    fn full_history_includes_all_messages() {
        let s = parse_session_str(SAMPLE, true).unwrap();
        assert_eq!(s.messages.len(), 4);
        assert_eq!(s.messages[0].text_preview(20), "hello");
    }
}
