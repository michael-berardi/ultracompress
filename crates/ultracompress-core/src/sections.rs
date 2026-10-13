//! Semantic section extraction (VCC engine): session goal, files & changes,
//! commits, outstanding context, user preferences. Deterministic, regex- and
//! heuristic-driven — no LLM.

use crate::model::{truncate_chars, Block, RcMessage};
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
/// Per-result cap on diagnostic facts (contract: at most 4 per result).
const MAX_FACTS_PER_RESULT: usize = 4;
/// Diagnostic scanning is bounded to a head+tail window (32 KiB total),
/// independently of the total result size.
const FACT_SCAN_BYTES: usize = 32 * 1024;
const FACT_HEAD_BYTES: usize = FACT_SCAN_BYTES / 2;
const FACT_TAIL_BYTES: usize = FACT_SCAN_BYTES - FACT_HEAD_BYTES;
/// Lines longer than this are never lowercased or copied: huge single-line
/// dumps must not allocate.
const FACT_LINE_MAX: usize = 240;

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
                    if seen_prefs.insert(normalize_pref(&p))
                        && s.preferences.len() < MAX_PREFERENCES
                    {
                        s.preferences.push(p);
                    }
                }
            }
            crate::model::Role::Assistant => {
                for b in &m.content {
                    if let Block::ToolCall {
                        name, arguments, ..
                    } = b
                    {
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
                                if let Some(cmd) = arguments.get("command").and_then(|c| c.as_str())
                                {
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
                    if let Block::ToolResult {
                        text,
                        is_error,
                        tool_name,
                        ..
                    } = b
                    {
                        if *is_error || looks_failed(text) {
                            if let Some(o) = extract_outstanding(text) {
                                if seen_outstanding.insert(normalize_pref(&o))
                                    && s.outstanding.len() < MAX_OUTSTANDING
                                {
                                    s.outstanding.push(o);
                                }
                            }
                        }
                        // Sticky diagnostic facts: warn/fatal/error lines that
                        // carry codes or statuses agents must not lose. Long
                        // outputs are scanned head+tail, and successful file
                        // reads need an anchored diagnostic shape so source
                        // strings that merely *quote* one are ignored.
                        for f in extract_key_facts(text, tool_name, *is_error) {
                            if seen_facts.insert(normalize_pref(&f))
                                && s.key_facts.len() < MAX_KEY_FACTS
                            {
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
    if triggers
        .iter()
        .any(|tr| lower.starts_with(tr) || lower.contains(&format!(". {tr}")))
        && t.len() > 8
    {
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
        let hit = [
            "always ",
            "never ",
            "prefer ",
            "make sure to ",
            "don't use ",
            "do not use ",
            "keep it ",
        ]
        .iter()
        .any(|p| lower.starts_with(p));
        if hit && l.len() >= 8 && l.len() <= 200 {
            out.push(truncate_chars(l, 200));
        }
    }
    out
}

fn normalize_pref(p: &str) -> String {
    p.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_failed(text: &str) -> bool {
    // Length check first: avoid the to_lowercase allocation on huge outputs.
    if text.len() >= 20_000 {
        return false;
    }
    let lower = text.to_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("panic")
        || lower.contains("fatal")
}

/// Lines that state a diagnostic condition with a code or status token —
/// exactly the facts that must survive every compaction.
///
/// Outputs bigger than the scan window are searched head+tail on char
/// boundaries instead of being dropped, so a truncated compile/test log still
/// yields `error TS2304` / `npm ERR! code ELIFECYCLE` style facts. Oversized
/// single lines are skipped rather than copied (no giant allocations).
/// `tool_name`/`is_error` are used prudently: a *successful* file read only
/// counts anchored diagnostics, because source code and fixtures routinely
/// embed diagnostic-looking strings.
fn extract_key_facts(text: &str, tool_name: &str, is_error: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if text.is_empty() {
        return out;
    }
    let strict = is_source_read(tool_name) && !is_error;
    let mut seen_keys: Vec<String> = Vec::new();
    for seg in fact_scan_segments(text) {
        for line in seg.lines() {
            let l = line.trim();
            // Length gate *before* any allocation: a >1 MiB single line is
            // never lowercased or cloned.
            if l.len() < 10 || l.len() > FACT_LINE_MAX {
                continue;
            }
            let lower = l.to_lowercase();
            if !is_diagnostic_line(&lower, strict) {
                continue;
            }
            // Exact dedup inside one result: a repeated code must not spend the
            // per-result fact budget.
            let key = normalize_pref(l);
            if seen_keys.iter().any(|k| k == &key) {
                continue;
            }
            seen_keys.push(key);
            out.push(truncate_chars(l, FACT_LINE_MAX));
            if out.len() >= MAX_FACTS_PER_RESULT {
                break;
            }
        }
        if out.len() >= MAX_FACTS_PER_RESULT {
            break;
        }
    }
    out
}

/// File-reading tools emit file *contents*, not command output; their success
/// is the strongest signal that diagnostic-looking text is just source.
fn is_source_read(tool_name: &str) -> bool {
    let t = tool_name.trim().to_ascii_lowercase();
    matches!(
        t.as_str(),
        "read" | "view" | "cat" | "read_file" | "readfile" | "read_file_content"
    )
}

/// Bounded head+tail window over a possibly huge output. Slices always land on
/// char boundaries and on line starts/ends, so a line is never half-scanned.
fn fact_scan_segments(text: &str) -> Vec<&str> {
    let n = text.len();
    if n <= FACT_SCAN_BYTES {
        return vec![text];
    }
    let head_end = floor_char_boundary(text, FACT_HEAD_BYTES);
    let head_raw = &text[..head_end];
    // Keep only whole lines in the head window.
    let head = match head_raw.rfind('\n') {
        Some(i) => &head_raw[..i],
        None => "",
    };
    let tail_start = ceil_char_boundary(text, n - FACT_TAIL_BYTES);
    let tail = if tail_start <= head_end || tail_start == 0 {
        ""
    } else if text.as_bytes()[tail_start - 1] == b'\n' {
        &text[tail_start..]
    } else {
        // The tail window began mid-line: skip to the next line start.
        match text[tail_start..].find('\n') {
            Some(i) => &text[tail_start + i + 1..],
            None => "",
        }
    };
    let mut out = Vec::with_capacity(2);
    if !head.is_empty() {
        out.push(head);
    }
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Severity/violation markers that make a line a diagnostic rather than prose.
/// `err!` is npm's shape (`npm ERR! code ELIFECYCLE`), which contains no
/// literal "error".
const SEVERITY_TOKENS: [&str; 8] = [
    "fatal", "error", "err!", "warn", "warning", "panic", "failed", "failure",
];

fn severity_pos(lower: &str) -> Option<usize> {
    SEVERITY_TOKENS
        .iter()
        .flat_map(|token| lower.match_indices(token))
        .filter(|(pos, token)| {
            let bytes = lower.as_bytes();
            let is_path_or_word = |c| is_word_byte(c) || matches!(c, b'.' | b'/' | b'\\');
            (*pos == 0 || !is_path_or_word(bytes[*pos - 1]))
                && (*pos + token.len() == bytes.len()
                    || !is_path_or_word(bytes[*pos + token.len()]))
        })
        .map(|(pos, _)| pos)
        .min()
}

fn is_diagnostic_line(lower: &str, strict: bool) -> bool {
    if lower.starts_with("//") || lower.starts_with("/*") || lower.starts_with('*') {
        return false;
    }
    let Some(sev_pos) = severity_pos(lower) else {
        return false;
    };
    // A diagnostic quoted inside a string literal (source, fixture, JSON
    // example) is not an observed diagnostic.
    if inside_quotes(lower, sev_pos) {
        return false;
    }
    if !has_code_token(lower) {
        return false;
    }
    !strict || starts_with_severity(lower) || has_location_prefix(lower, sev_pos)
}

fn starts_with_severity(lower: &str) -> bool {
    SEVERITY_TOKENS.iter().any(|token| {
        lower
            .strip_prefix(token)
            .is_some_and(|rest| rest.starts_with(':') || rest.starts_with(char::is_whitespace))
    }) || lower.starts_with("npm err!")
}

/// `src/a.ts(3,5): error TS2304:` / `src/a.ts:3:5 - error TS2304:` shapes.
fn has_location_prefix(lower: &str, sev_pos: usize) -> bool {
    let head = lower[..sev_pos].trim_end();
    head.ends_with(')') || head.ends_with(':') || head.ends_with('-')
}

/// A code or status token: `code=E_AUTH_FAIL`, `code ELIFECYCLE`, `code:`,
/// `TS2304`, `E0432`, or the dashed `E-8341-DEPLOY` form. Literal codes are
/// preserved verbatim (only the lowercased copy is inspected).
fn has_code_token(lower: &str) -> bool {
    lower.contains("code=")
        || lower.contains("code ")
        || lower.contains("code:")
        || dashed_code(lower)
        || has_letter_digit_code(lower)
}

fn dashed_code(lower: &str) -> bool {
    lower.matches('-').count() >= 2 && lower.chars().any(|c| c.is_ascii_digit())
}

/// Letter-prefixed digit runs with word boundaries: `ts2304`, `e0432`,
/// `err1201`. Byte-wise, so no allocation and no UTF-8 slicing risk.
fn has_letter_digit_code(lower: &str) -> bool {
    let b = lower.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let alpha_len = i - start;
        let digits_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let digits = i - digits_start;
        if matches!(&b[start..start + alpha_len], b"ts" | b"e" | b"err") && digits >= 3 {
            let prev_word = start > 0 && is_word_byte(b[start - 1]);
            let next_word = i < b.len() && is_word_byte(b[i]);
            if !prev_word && !next_word {
                return true;
            }
        }
    }
    false
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Cheap approximation of "this byte offset sits inside a quoted literal":
/// odd counts of `"`, `'` or a backtick before the offset. Byte indices keep
/// this safe for multibyte text (`to_lowercase` can change byte lengths).
fn inside_quotes(lower: &str, pos: usize) -> bool {
    let b = lower.as_bytes();
    let end = pos.min(b.len());
    let (mut dq, mut sq, mut bt) = (0u32, 0u32, 0u32);
    for &c in &b[..end] {
        match c {
            b'"' => dq += 1,
            b'\'' => sq += 1,
            b'`' => bt += 1,
            _ => {}
        }
    }
    dq % 2 == 1 || sq % 2 == 1 || bt % 2 == 1
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
        RcMessage {
            id: format!("m{}", role as u8),
            role,
            content,
            timestamp: None,
        }
    }

    #[test]
    fn extracts_goal_from_first_user_message() {
        let msgs = vec![
            msg(
                Role::User,
                vec![Block::Text {
                    text: "Fix the auth bug in login flow. Users cannot log in.".into(),
                }],
            ),
            msg(
                Role::User,
                vec![Block::Text {
                    text: "Actually, also refresh session tokens after reset.".into(),
                }],
            ),
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
            vec![Block::Text {
                text: "Always run tests before committing.\nRandom chat line.".into(),
            }],
        )];
        let s = extract(&msgs);
        assert_eq!(
            s.preferences,
            vec!["Always run tests before committing.".to_string()]
        );
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
        assert_eq!(
            s.commits,
            vec!["commit: fix(auth): refresh token".to_string()]
        );
    }

    fn tool_result(tool_name: &str, text: &str, is_error: bool) -> RcMessage {
        msg(
            Role::ToolResult,
            vec![Block::ToolResult {
                tool_call_id: "x".into(),
                tool_name: tool_name.into(),
                text: text.into(),
                is_error,
            }],
        )
    }

    fn filler(n: usize) -> String {
        (0..n)
            .map(|i| format!("building module {i} ok\n"))
            .collect()
    }

    #[test]
    fn key_facts_capture_ts_npm_and_code_diagnostics() {
        let text = "error TS2304: Cannot find name 'foo'\n\
                    npm ERR! code ELIFECYCLE\n\
                    fatal: code=E_AUTH_FAIL\n\
                    E-8341-DEPLOY fatal rollback initiated\n\
                    all green\n";
        assert_eq!(
            extract_key_facts(text, "bash", false),
            vec![
                "error TS2304: Cannot find name 'foo'".to_string(),
                "npm ERR! code ELIFECYCLE".to_string(),
                "fatal: code=E_AUTH_FAIL".to_string(),
                "E-8341-DEPLOY fatal rollback initiated".to_string(),
            ]
        );
    }

    #[test]
    fn key_facts_scan_long_output_head_and_tail() {
        let mut text = String::new();
        text.push_str("src/app.ts(1,1): error TS2304: Cannot find name 'foo'\n");
        text.push_str(&filler(1_200));
        text.push_str("error TS9999: middle-only marker outside scan window\n");
        text.push_str(&filler(1_200));
        text.push_str("npm ERR! code ELIFECYCLE\n");
        assert!(text.len() > FACT_SCAN_BYTES);
        let facts = extract_key_facts(&text, "bash", false);
        assert!(facts.iter().any(|f| f.contains("TS2304")), "{facts:?}");
        assert!(facts.iter().any(|f| f.contains("ELIFECYCLE")), "{facts:?}");
        assert!(
            !facts.iter().any(|f| f.contains("TS9999")),
            "middle-only diagnostic leaked out of the bounded window: {facts:?}"
        );
        assert!(facts.len() <= MAX_FACTS_PER_RESULT);
    }

    #[test]
    fn key_facts_unicode_windows_land_on_char_boundaries() {
        let mut text = String::new();
        text.push_str("fatal: code=E_AUTH_FAIL token rejected\n");
        // 2-byte chars put the raw 16 KiB cut mid-character on both windows.
        text.push_str(&"é".repeat(8_000));
        text.push('\n');
        text.push_str(&"β".repeat(8_000));
        text.push('\n');
        text.push_str("warn dépôt: code=WARN_RATE_LIMIT seuil atteint\n");
        text.push_str(&"𝄞".repeat(4_000));
        assert!(text.len() > FACT_SCAN_BYTES);
        let facts = extract_key_facts(&text, "bash", false);
        assert_eq!(facts.len(), 2, "{facts:?}");
        assert!(facts[0].contains("E_AUTH_FAIL"), "{facts:?}");
        assert!(facts[1].contains("WARN_RATE_LIMIT"), "{facts:?}");
    }

    #[test]
    fn key_facts_handle_megabyte_single_line_without_giant_allocations() {
        let text = "x".repeat(1_100_000);
        assert!(extract_key_facts(&text, "bash", true).is_empty());
        // The tail window still finds a diagnostic that follows the huge blob.
        let text = format!("{text}\nerror TS1005: ';' expected\n");
        assert_eq!(
            extract_key_facts(&text, "bash", false),
            vec!["error TS1005: ';' expected".to_string()]
        );
    }

    #[test]
    fn key_facts_ignore_quoted_diagnostics_in_successful_source_reads() {
        let text = "let pattern = \"error TS2304: Cannot find name 'foo'\";\n\
                    const NPM_FAIL = 'npm ERR! code ELIFECYCLE';\n\
                    // error TS2304: legacy fixture only\n\
                    *    error TS2304: doc comment string\n";
        assert!(extract_key_facts(text, "read", false).is_empty());
        // Failed tools do not turn quoted source or comments into facts.
        assert!(extract_key_facts(text, "read", true).is_empty());
        // Anchored (unquoted) diagnostics still count, even from a real read.
        assert_eq!(
            extract_key_facts("error TS2304: Cannot find name 'foo'\n", "read", false),
            vec!["error TS2304: Cannot find name 'foo'".to_string()]
        );
    }

    #[test]
    fn key_facts_keep_distinct_codes_and_cap_per_result() {
        let text = "error TS2304: missing foo\n\
                    error TS2305: missing bar\n\
                    error TS2306: missing baz\n\
                    error TS2307: missing qux\n\
                    error TS2308: missing quux\n";
        let facts = extract_key_facts(text, "bash", false);
        assert_eq!(facts.len(), MAX_FACTS_PER_RESULT);
        assert_eq!(facts[3], "error TS2307: missing qux".to_string());
    }

    #[test]
    fn extract_dedups_identical_facts_across_results() {
        let line = "error TS2304: Cannot find name 'foo'";
        let msgs = vec![
            tool_result("bash", line, false),
            tool_result("bash", line, false),
        ];
        let s = extract(&msgs);
        assert_eq!(s.key_facts, vec![line.to_string()]);
    }

    #[test]
    fn extract_keeps_distinct_literal_codes_and_long_output_facts() {
        let msgs = vec![
            tool_result(
                "bash",
                "fatal: code=E_AUTH_FAIL\nE-8341-DEPLOY fatal rollback initiated\n",
                true,
            ),
            tool_result(
                "bash",
                &format!("error TS2304: Cannot find name 'foo'\n{}", filler(2_000)),
                false,
            ),
        ];
        let s = extract(&msgs);
        assert_eq!(
            s.key_facts,
            vec![
                "fatal: code=E_AUTH_FAIL".to_string(),
                "E-8341-DEPLOY fatal rollback initiated".to_string(),
                "error TS2304: Cannot find name 'foo'".to_string(),
            ]
        );
    }

    #[test]
    fn source_names_and_severity_substrings_are_not_diagnostics() {
        assert!(
            extract_key_facts("terror code=E_FAKE\nerror module foo123\n", "bash", false)
                .is_empty()
        );
        assert!(extract_key_facts("error(\"code=E_FAKE\");\n", "read", false).is_empty());
        assert_eq!(
            extract_key_facts(
                "src/error.ts:3:5 - error TS2304: unknown name\n",
                "read",
                false
            )
            .len(),
            1
        );
    }

    #[test]
    fn extraction_caps_ten_distinct_facts_without_merging_codes() {
        let messages: Vec<_> = (1000..1014)
            .map(|code| tool_result("bash", &format!("error TS{code}: unknown name"), false))
            .collect();
        let facts = extract(&messages).key_facts;
        assert_eq!(facts.len(), MAX_KEY_FACTS);
        assert!(facts[0].contains("TS1000"));
        assert!(facts[9].contains("TS1009"));
    }

    #[test]
    fn clipped_single_lines_are_not_facts() {
        let text = format!("fatal code=E_FAKE{}continued payload", " ".repeat(40_000));
        assert!(extract_key_facts(&text, "bash", false).is_empty());
    }

    #[test]
    fn short_output_behavior_unchanged() {
        // Non-diagnostic prose and code-free severity lines are still ignored.
        let facts = extract_key_facts(
            "server started on port 3000\nerror: connection refused\nwarn: retrying\n",
            "bash",
            false,
        );
        assert!(facts.is_empty(), "{facts:?}");
    }
}
