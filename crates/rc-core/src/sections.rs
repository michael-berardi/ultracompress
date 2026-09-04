//! Semantic section extraction (VCC engine): session goal, files & changes,
//! commits, outstanding context, user preferences. Deterministic, regex- and
//! heuristic-driven — no LLM.

use crate::model::{Block, RcMessage, truncate_chars};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Sections {
    pub goal: Vec<String>,
    pub files_modified: Vec<String>,
    pub files_created: Vec<String>,
    pub files_read: Vec<String>,
    pub commits: Vec<String>,
    pub outstanding: Vec<String>,
    pub preferences: Vec<String>,
    /// Sticky diagnostic facts (error/warn/fatal lines with codes) carried
    /// across merges — the details agents need and LLM summaries paraphrase
    /// into lossiness.
    pub key_facts: Vec<String>,
}

const MAX_FILES: usize = 24;
const MAX_COMMITS: usize = 8;
const MAX_OUTSTANDING: usize = 8;
const MAX_PREFERENCES: usize = 8;
const MAX_KEY_FACTS: usize = 10;

pub fn extract(messages: &[RcMessage]) -> Sections {
    let mut s = Sections::default();
    let mut files_modified: BTreeSet<String> = BTreeSet::new();
    let mut files_created: BTreeSet<String> = BTreeSet::new();
    let mut files_read: BTreeSet<String> = BTreeSet::new();
    let mut seen_outstanding: BTreeSet<String> = BTreeSet::new();
    let mut seen_prefs: BTreeSet<String> = BTreeSet::new();
    let mut seen_facts: BTreeSet<String> = BTreeSet::new();
    let mut goal_scope_changes: Vec<String> = Vec::new();

    let mut first_user_done = false;
    for m in messages {
        match m.role {
            crate::model::Role::User => {
                let text = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        Block::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    continue;
                }
                if !first_user_done {
                    first_user_done = true;
                    // The opening prompt is the mission: keep enough of it
                    // that multi-step instructions survive compaction.
                    let goal = truncate_chars(text.trim(), 1_400);
                    if !goal.is_empty() {
                        s.goal.push(goal);
                    }
                } else if let Some(sc) = detect_scope_change(&text) {
                    goal_scope_changes.push(sc);
                }
                for p in extract_preferences(&text) {
                    if seen_prefs.insert(normalize_pref(&p)) && s.preferences.len() < MAX_PREFERENCES {
                        s.preferences.push(p);
                    }
                }
            }
            crate::model::Role::Assistant => {
                for b in &m.content {
                    if let Block::ToolCall { name, arguments, .. } = b {
                        match name.as_str() {
                            "edit" | "write" => {
                                let path = arg_path(arguments);
                                if let Some(p) = path {
                                    if name == "write" {
                                        files_created.insert(p);
                                    } else {
                                        files_modified.insert(p);
                                    }
                                }
                            }
                            "read" => {
                                if let Some(p) = arg_path(arguments) {
                                    files_read.insert(p);
                                }
                            }
                            "bash" => {
                                if let Some(cmd) = arguments.get("command").and_then(|c| c.as_str()) {
                                    for c in extract_commits(cmd) {
                                        s.commits.push(c);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            crate::model::Role::ToolResult => {
                for b in &m.content {
                    if let Block::ToolResult { text, is_error, .. } = b {
                        if *is_error || looks_failed(text) {
                            if let Some(o) = extract_outstanding(text) {
                                if seen_outstanding.insert(normalize_pref(&o)) && s.outstanding.len() < MAX_OUTSTANDING {
                                    s.outstanding.push(o);
                                }
                            }
                        }
                        // Sticky diagnostic facts: warn/fatal/error lines that
                        // carry codes or statuses agents must not lose.
                        for f in extract_key_facts(text) {
                            if seen_facts.insert(normalize_pref(&f)) && s.key_facts.len() < MAX_KEY_FACTS {
                                s.key_facts.push(f);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for sc in goal_scope_changes.into_iter().take(3) {
        s.goal.push(format!("[Scope change] {sc}"));
    }
    s.files_modified = cap(files_modified.into_iter(), MAX_FILES);
    s.files_created = cap(files_created.into_iter(), MAX_FILES);
    s.files_read = cap(files_read.into_iter(), MAX_FILES);
    s.commits = s.commits.into_iter().rev().take(MAX_COMMITS).collect();
    s
}

fn cap<I: Iterator<Item = String>>(it: I, max: usize) -> Vec<String> {
    it.take(max).collect()
}

fn arg_path(args: &serde_json::Value) -> Option<String> {
    args.get("path")
        .or_else(|| args.get("file_path"))
        .or_else(|| args.get("filePath"))
        .and_then(|p| p.as_str())
        .map(trim_common_prefix)
}

fn trim_common_prefix(p: &str) -> String {
    // Collapse noisy machine prefixes while keeping the project segment for
    // disambiguation: /Users/<name>/dev/<repo>/src/x.ts → repo/src/x.ts
    if let Some(idx) = p.find("/dev/") {
        return p[idx + 5..].to_string();
    }
    // Also collapse other absolute homes: /Users/<name>/... → rest
    if let Some(rest) = p.strip_prefix("/Users/") {
        if let Some(slash) = rest.find('/') {
            return rest[slash + 1..].to_string();
        }
    }
    p.to_string()
}

fn detect_scope_change(text: &str) -> Option<String> {
    let t = text.trim();
    let lower = t.to_lowercase();
    let triggers = [
        "actually, ",
        "wait,",
        "instead of",
        "scratch that",
        "change of plan",
        "also ",
        "additionally,",
        "one more thing",
        "new requirement",
    ];
    if triggers.iter().any(|tr| lower.starts_with(tr) || lower.contains(&format!(". {tr}"))) && t.len() > 8 {
        Some(truncate_chars(t, 220))
    } else {
        None
    }
}

fn extract_preferences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        let lower = l.to_lowercase();
        let hit = ["always ", "never ", "prefer ", "make sure to ", "don't use ", "do not use ", "keep it "]
            .iter()
            .any(|p| lower.starts_with(p));
        if hit && l.len() >= 8 && l.len() <= 200 {
            out.push(truncate_chars(l, 200));
        }
    }
    out
}

fn normalize_pref(p: &str) -> String {
    p.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn looks_failed(text: &str) -> bool {
    // Length check first: avoid the to_lowercase allocation on huge outputs.
    if text.len() >= 20_000 {
        return false;
    }
    let lower = text.to_lowercase();
    lower.contains("error") || lower.contains("failed") || lower.contains("panic") || lower.contains("fatal")
}

/// Lines that state a diagnostic condition with a code or status token —
/// exactly the facts that must survive every compaction.
fn extract_key_facts(text: &str) -> Vec<String> {
    if text.len() > 20_000 {
        return vec![];
    }
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.len() < 10 || l.len() > 240 {
            continue;
        }
        let lower = l.to_lowercase();
        let severity =
            lower.contains("fatal") || lower.contains("error") || lower.contains("warn") || lower.contains("panic");
        if !severity {
            continue;
        }
        let has_code = lower.contains("code=")
            || lower.contains("code ")
            || lower.contains("code:")
            || lower.matches('-').count() >= 2 && lower.chars().any(|c| c.is_ascii_digit());
        if has_code {
            out.push(truncate_chars(l, 240));
            if out.len() >= 4 {
                break;
            }
        }
    }
    out
}

fn extract_outstanding(text: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim();
        let lower = l.to_lowercase();
        if (lower.contains("error") || lower.contains("failed") || lower.contains("cannot"))
            && l.len() >= 12
        {
            return Some(truncate_chars(l, 240));
        }
    }
    None
}

fn extract_commits(cmd: &str) -> Vec<String> {
    // Detect `git commit` invocations anywhere in the command line (chained
    // commands are common: `git add -A && git commit -m "..."`).
    let mut out = Vec::new();
    let lower = cmd.to_lowercase();
    if !lower.contains("git commit") {
        return out;
    }
    for line in cmd.lines() {
        let l = line.trim();
        let ll = l.to_lowercase();
        if !ll.contains("git commit") {
            continue;
        }
        let msg = l
            .split("-m")
            .nth(1)
            .map(|m| m.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
            .unwrap_or_default();
        let entry = if msg.is_empty() {
            crate::model::truncate_chars(l, 120)
        } else {
            format!("commit: {msg}")
        };
        out.push(entry);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Role;

    fn msg(role: Role, content: Vec<Block>) -> RcMessage {
        RcMessage { id: format!("m{}", role as u8), role, content, timestamp: None }
    }

    #[test]
    fn extracts_goal_from_first_user_message() {
        let msgs = vec![
            msg(Role::User, vec![Block::Text { text: "Fix the auth bug in login flow. Users cannot log in.".into() }]),
            msg(Role::User, vec![Block::Text { text: "Actually, also refresh session tokens after reset.".into() }]),
        ];
        let s = extract(&msgs);
        assert!(s.goal[0].starts_with("Fix the auth bug"));
        assert!(s.goal.iter().any(|g| g.contains("[Scope change]")));
    }

    #[test]
    fn extracts_files_from_tool_calls() {
        let msgs = vec![msg(
            Role::Assistant,
            vec![Block::ToolCall {
                id: "1".into(),
                name: "edit".into(),
                arguments: serde_json::json!({"path": "/Users/x/dev/app/src/auth.ts", "oldText": "a", "newText": "b"}),
            }],
        )];
        let s = extract(&msgs);
        assert_eq!(s.files_modified, vec!["app/src/auth.ts".to_string()]);
    }

    #[test]
    fn extracts_preferences() {
        let msgs = vec![msg(
            Role::User,
            vec![Block::Text { text: "Always run tests before committing.\nRandom chat line.".into() }],
        )];
        let s = extract(&msgs);
        assert_eq!(s.preferences, vec!["Always run tests before committing.".to_string()]);
    }

    #[test]
    fn extracts_outstanding_errors() {
        let msgs = vec![msg(
            Role::ToolResult,
            vec![Block::ToolResult {
                tool_call_id: "x".into(),
                tool_name: "bash".into(),
                text: "npm ERR! code ELIFECYCLE\nerror TS2304: Cannot find name 'foo'".into(),
                is_error: false,
            }],
        )];
        let s = extract(&msgs);
        assert!(!s.outstanding.is_empty());
    }

    #[test]
    fn extracts_git_commits() {
        let msgs = vec![msg(
            Role::Assistant,
            vec![Block::ToolCall {
                id: "2".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "git add -A && git commit -m \"fix(auth): refresh token\""}),
            }],
        )];
        let s = extract(&msgs);
        assert_eq!(s.commits, vec!["commit: fix(auth): refresh token".to_string()]);
    }
}
