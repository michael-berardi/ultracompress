//! Claude Code transcript reader (`~/.claude/projects/**/*.jsonl`).
//!
//! Claude Code sessions are append-only JSONL trees keyed by `uuid` /
//! `parentUuid` instead of Pi's `id` / `parentId`. Records carry a `message`
//! with `role` and `content` (string or block array), optional `isSidechain`
//! / `isMeta` flags, and auxiliary record types (attachments, file snapshots,
//! telemetry) that never hold conversation text. A compaction inserts a
//! `system` / `compact_boundary` record whose `parentUuid` resets to null
//! while `logicalParentUuid` preserves the logical position in the
//! conversation; the surviving summary is re-emitted after the boundary as a
//! user record flagged `isCompactSummary`.
//!
//! Mapping into the recall entry model:
//! - id = `uuid`, parent = `parentUuid`, else `logicalParentUuid`;
//! - `tool_use` blocks become tool calls, `tool_result` blocks become tool
//!   results with the tool name resolved from the earlier `tool_use`;
//! - sidechain records are dropped (they never anchor main-chain lineage);
//!   meta records stay as unsearchable chain nodes so lineage stays intact;
//! - records written before a compaction stay searchable — recall reaches
//!   history the live context lost;
//! - malformed lines are skipped and counted, never fatal.

use crate::model::{parse_content, Block, RcMessage, Role};
use crate::recall_load::{RecallEntry, RecallFile};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::Path;

/// Does this record shape identify a Claude Code transcript? Pi sessions open
/// with a `type:"session"` header and key entries on `id`; Claude records key
/// on `uuid` and auxiliary records (last-prompt, mode, queue-operation, …)
/// never carry the Pi entry `id`.
pub(crate) fn is_claude_record(v: &Value) -> bool {
    let Some(o) = v.as_object() else {
        return false;
    };
    match o.get("type").and_then(Value::as_str) {
        Some("session") => false,
        Some("user" | "assistant" | "system" | "summary") => true,
        _ => o.contains_key("uuid") || !o.contains_key("id"),
    }
}

pub(crate) fn load_path(path: &Path, fallback_session: &str) -> Result<RecallFile, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    parse(std::io::BufReader::new(f), fallback_session)
}

/// Stream-parse a Claude Code transcript. Lines are read incrementally so
/// peak memory stays bounded by one line plus the retained entry model.
pub(crate) fn parse(rdr: impl BufRead, fallback_session: &str) -> Result<RecallFile, String> {
    let mut session_id = String::new();
    let mut entries: Vec<RecallEntry> = Vec::new();
    let mut logical_edges: Vec<usize> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut warnings = 0usize;

    for line in rdr.lines() {
        let line = line.map_err(|e| e.to_string())?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                warnings += 1;
                continue;
            }
        };
        let Some(o) = v.as_object() else {
            warnings += 1;
            continue;
        };
        let Some(kind) = o.get("type").and_then(Value::as_str) else {
            warnings += 1;
            continue;
        };
        if session_id.is_empty() {
            if let Some(s) = o
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                session_id = s.to_string();
            }
        }
        // Compaction index records ("summary" + leafUuid) and auxiliary
        // records without a uuid are not chain nodes.
        if kind == "summary" || o.get("uuid").is_none() {
            continue;
        }
        let Some(id) = o
            .get("uuid")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            warnings += 1;
            continue;
        };
        // Subagent sidechains form separate trees that never anchor main
        // lineage; dropping them keeps the default leaf the newest main
        // record. Duplicate uuids keep the first occurrence.
        if o.get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || !ids.insert(id.to_string())
        {
            continue;
        }
        // Meta records hold command wrappers and injected context: keep them
        // as chain nodes, never as searchable prose.
        let message = if matches!(kind, "user" | "assistant" | "system")
            && !o.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        {
            map_message(o, kind, id, &mut tool_names)
        } else {
            None
        };
        let (parent, logical) = resolve_parent(o);
        if logical {
            logical_edges.push(entries.len());
        }
        entries.push(RecallEntry {
            id: id.to_string(),
            parent,
            message,
        });
    }

    if session_id.is_empty() {
        session_id = fallback_session.to_string();
    }
    // A parent may have been skipped (sidechain, malformed line): treat it as
    // a root instead of orphaning the rest of the transcript.
    let known: HashSet<String> = entries.iter().map(|e| e.id.clone()).collect();
    for e in &mut entries {
        if e.parent
            .as_ref()
            .is_some_and(|p| !known.contains(p.as_str()))
        {
            e.parent = None;
        }
    }
    cut_reanchored_logical_edges(&mut entries, logical_edges);
    if entries.is_empty() {
        return Err("no usable records in Claude Code transcript".into());
    }
    Ok(RecallFile {
        session_id,
        entries,
        warnings,
    })
}

/// Parent for the entry graph: `parentUuid` when present, else the
/// compaction re-anchor `logicalParentUuid`, so lineage spans boundaries.
fn resolve_parent(o: &Map<String, Value>) -> (Option<String>, bool) {
    for (key, logical) in [("parentUuid", false), ("logicalParentUuid", true)] {
        if let Some(p) = o.get(key).and_then(Value::as_str).filter(|p| !p.is_empty()) {
            return (Some(p.to_string()), logical);
        }
    }
    (None, false)
}

fn map_message(
    o: &Map<String, Value>,
    kind: &str,
    id: &str,
    tool_names: &mut HashMap<String, String>,
) -> Option<RcMessage> {
    let timestamp = o.get("timestamp").and_then(|t| match t {
        Value::String(s) => parse_iso8601(s),
        Value::Number(n) => n.as_u64(),
        _ => None,
    });
    if kind == "system" {
        // The boundary itself is structure, not conversation.
        if o.get("subtype").and_then(Value::as_str) == Some("compact_boundary") {
            return None;
        }
        let text = o.get("content").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            return None;
        }
        return Some(RcMessage {
            id: id.to_string(),
            role: Role::System,
            content: vec![Block::Text {
                text: text.to_string(),
            }],
            timestamp,
        });
    }
    let inner = o.get("message")?.as_object()?;
    let role = Role::parse(inner.get("role").and_then(Value::as_str)?);
    if matches!(role, Role::Other | Role::Custom) {
        return None;
    }
    let mut content = parse_content(inner.get("content").unwrap_or(&Value::Null));
    resolve_tool_names(&mut content, tool_names);
    // Claude carries tool results inside user records; when a user record is
    // nothing but tool results, treat it as a toolResult entry so role and
    // tool-name filters behave like raw Pi sessions.
    let role = if role == Role::User
        && !content.is_empty()
        && content
            .iter()
            .all(|b| matches!(b, Block::ToolResult { .. }))
    {
        Role::ToolResult
    } else {
        role
    };
    Some(RcMessage {
        id: id.to_string(),
        role,
        content,
        timestamp,
    })
}

fn resolve_tool_names(content: &mut [Block], tool_names: &mut HashMap<String, String>) {
    for b in content.iter_mut() {
        match b {
            Block::ToolCall { id, name, .. } if !id.is_empty() => {
                tool_names.insert(id.clone(), name.clone());
            }
            Block::ToolResult {
                tool_call_id,
                tool_name,
                ..
            } if tool_name.is_empty() && !tool_call_id.is_empty() => {
                if let Some(n) = tool_names.get(tool_call_id.as_str()) {
                    *tool_name = n.clone();
                }
            }
            _ => {}
        }
    }
}

/// `logicalParentUuid` re-anchors a compact boundary after the preserved
/// segment. When Claude Code re-emits that segment after the boundary, the
/// edge points into the boundary's own descendants; cut such edges so lineage
/// stops at the boundary instead of failing with a cycle.
fn cut_reanchored_logical_edges(entries: &mut [RecallEntry], logical_edges: Vec<usize>) {
    if logical_edges.is_empty() {
        return;
    }
    let index: HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.clone(), i))
        .collect();
    for start in logical_edges {
        let Some(mut cur) = entries[start]
            .parent
            .as_ref()
            .and_then(|p| index.get(p.as_str()).copied())
        else {
            continue;
        };
        for _ in 0..=entries.len() {
            if cur == start {
                entries[start].parent = None;
                break;
            }
            match entries[cur]
                .parent
                .as_ref()
                .and_then(|p| index.get(p.as_str()).copied())
            {
                Some(next) => cur = next,
                None => break,
            }
        }
    }
}

/// Epoch seconds from an ISO 8601 timestamp ("2026-09-23T06:23:34.440Z").
/// Offsets are ignored (recorded as UTC); unparseable values map to None.
fn parse_iso8601(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || (b[10] != b'T' && b[10] != b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from a civil date (proleptic Gregorian, Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + sec).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn iso8601_timestamps_map_to_epoch_seconds() {
        assert_eq!(parse_iso8601("2026-01-02T03:04:05.000Z"), Some(1767323045));
        assert_eq!(parse_iso8601("2026-09-23T06:23:34Z"), Some(1790144614));
        assert_eq!(parse_iso8601("2026-09-23 06:23:34"), Some(1790144614));
        assert_eq!(parse_iso8601("not-a-timestamp"), None);
        assert_eq!(parse_iso8601("2026-13-02T03:04:05Z"), None);
    }

    #[test]
    fn record_shapes_distinguish_claude_from_pi() {
        assert!(is_claude_record(&json!({"type":"user","uuid":"u1"})));
        assert!(is_claude_record(&json!({"type":"summary","summary":"s"})));
        assert!(is_claude_record(
            &json!({"type":"file-history-snapshot","uuid":"f1","message":{}})
        ));
        // Real transcripts open with auxiliary records: uuid-less, no Pi id.
        assert!(is_claude_record(
            &json!({"type":"last-prompt","leafUuid":"x","sessionId":"s"})
        ));
        assert!(is_claude_record(&json!({"type":"mode","mode":"normal"})));
        assert!(!is_claude_record(
            &json!({"type":"session","version":3,"id":"s"})
        ));
        assert!(!is_claude_record(
            &json!({"type":"message","id":"e1","parentId":null})
        ));
        assert!(!is_claude_record(&json!({"id":"e1"})));
    }

    #[test]
    fn parse_maps_sidechains_meta_and_boundaries() {
        let raw = concat!(
            "{\"type\":\"summary\",\"summary\":\"title\",\"leafUuid\":\"a1\"}\n",
            "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"isSidechain\":false,\"isMeta\":false,\"sessionId\":\"s\",\"timestamp\":\"2026-01-02T03:04:05.000Z\",\"message\":{\"role\":\"user\",\"content\":\"hello world\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":\"u1\",\"isSidechain\":true,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"sidechain only\"}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"m1\",\"parentUuid\":\"a1\",\"isMeta\":true,\"message\":{\"role\":\"user\",\"content\":\"<command-name>/x</command-name>\"}}\n",
            "{\"type\":\"system\",\"uuid\":\"b1\",\"parentUuid\":null,\"logicalParentUuid\":\"m1\",\"subtype\":\"compact_boundary\",\"content\":\"Conversation compacted\"}\n",
            "{\"type\":\"user\",\"uuid\":\"u2\",\"parentUuid\":\"b1\",\"isCompactSummary\":true,\"message\":{\"role\":\"user\",\"content\":\"the summary\"}}\n",
            "this line is not json\n",
        );
        let f = parse(raw.as_bytes(), "stem").unwrap();
        assert_eq!(f.session_id, "s");
        assert_eq!(f.warnings, 1);
        let ids: Vec<&str> = f.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["u1", "m1", "b1", "u2"]);
        assert!(f.entries[0].message.as_ref().unwrap().timestamp == Some(1767323045));
        // sidechain dropped, meta kept as a chain node without message
        assert!(f.entries[1].message.is_none());
        // boundary: unsearchable chain node re-anchored through logicalParentUuid
        assert!(f.entries[2].message.is_none());
        assert_eq!(f.entries[2].parent.as_deref(), Some("m1"));
        assert_eq!(
            f.entries[3].message.as_ref().unwrap().role.to_string(),
            "user"
        );
    }
}
