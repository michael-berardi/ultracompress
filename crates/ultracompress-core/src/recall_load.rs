use crate::model::{parse_message, RcMessage};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Clone)]
pub struct RecallEntry {
    pub id: String,
    pub parent: Option<String>,
    pub message: Option<RcMessage>,
}
#[derive(Clone)]
pub struct RecallFile {
    pub session_id: String,
    pub entries: Vec<RecallEntry>,
}

pub fn load(path: &Path) -> Result<RecallFile, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse(&raw)
}
pub fn parse(raw: &str) -> Result<RecallFile, String> {
    let mut sid = String::new();
    let mut entries = Vec::new();
    let mut ids = HashSet::new();
    let lines: Vec<_> = raw.lines().collect();
    for (line_index, line) in lines.iter().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) if line_index + 1 == lines.len() && !raw.ends_with('\n') => break,
            Err(_) => return Err("invalid JSON session record".into()),
        };
        let o = match v.as_object() {
            Some(x) => x,
            None => return Err("session record must be an object".into()),
        };
        match o.get("type").and_then(Value::as_str) {
            Some("session") => {
                if !sid.is_empty() || !entries.is_empty() {
                    return Err("duplicate or misplaced session header".into());
                }
                sid = o
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or("missing session id")?
                    .into();
            }
            Some(kind) => {
                let id = o
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() || !ids.insert(id.clone()) {
                    return Err("duplicate or missing entry id".into());
                }
                let parent = match o.get("parentId") {
                    Some(Value::String(p)) if !p.is_empty() => Some(p.clone()),
                    Some(Value::Null) | None => None,
                    _ => return Err("invalid parentId".into()),
                };
                let mut m = if kind == "message" {
                    parse_message(&v, &id)
                } else {
                    None
                };
                if let Some(ref mut x) = m {
                    x.id = id.clone()
                };
                entries.push(RecallEntry {
                    id,
                    parent,
                    message: m,
                });
            }
            _ => return Err("missing or invalid record type".into()),
        }
    }
    if sid.is_empty() {
        return Err("missing session header".into());
    }
    Ok(RecallFile {
        session_id: sid,
        entries,
    })
}
/// Validate every branch in linear time without walking shared ancestors
/// repeatedly. All-session scope must not silently accept malformed trees.
pub fn validate_all(f: &RecallFile) -> Result<(), String> {
    let indices: HashMap<&str, usize> = f
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.id.as_str(), i))
        .collect();
    let mut state = vec![0u8; f.entries.len()];
    for start in 0..f.entries.len() {
        if state[start] == 2 {
            continue;
        }
        let mut chain = Vec::new();
        let mut current = start;
        loop {
            if state[current] == 2 {
                break;
            }
            if state[current] == 1 {
                return Err("cycle in parent tree".into());
            }
            state[current] = 1;
            chain.push(current);
            match &f.entries[current].parent {
                None => break,
                Some(parent) => current = *indices.get(parent.as_str()).ok_or("missing parent")?,
            }
        }
        for index in chain {
            state[index] = 2;
        }
    }
    Ok(())
}

pub fn lineage(f: &RecallFile, leaf: Option<&str>) -> Result<(Vec<usize>, Option<String>), String> {
    if leaf == Some("") {
        return Ok((Vec::new(), Some("".into())));
    }
    if f.entries.is_empty() {
        return if leaf.is_some() {
            Err("explicit leaf not found".into())
        } else {
            Ok((Vec::new(), None))
        };
    }
    let map: HashMap<&str, usize> = f
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.as_str(), i))
        .collect();
    let mut cur = match leaf {
        Some(x) => *map.get(x).ok_or("explicit leaf not found")?,
        None => f.entries.len() - 1,
    };
    let chosen = f.entries[cur].id.clone();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    loop {
        if !seen.insert(cur) {
            return Err("cycle in parent tree".into());
        }
        out.push(cur);
        match &f.entries[cur].parent {
            None => break,
            Some(p) => cur = *map.get(p.as_str()).ok_or("missing parent")?,
        }
    }
    out.reverse();
    Ok((out, Some(chosen)))
}
