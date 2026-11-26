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

/// What a tool call does, whatever the harness names the tool.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolKind {
    Edit,
    Write,
    Read,
    Shell,
    Other,
}

/// Harnesses spell the same tools differently (`edit`, `Edit`, `patch`,
/// `write_file`, `Bash`, `terminal`). Matching one spelling left the files
/// and commits sections empty everywhere else.
pub(crate) fn tool_kind(name: &str) -> ToolKind {
    let n = name.trim().to_ascii_lowercase();
    match n.as_str() {
        "edit"
        | "multiedit"
        | "multi_edit"
        | "notebookedit"
        | "patch"
        | "apply_patch"
        | "str_replace"
        | "str_replace_editor"
        | "str_replace_based_edit_tool"
        | "edit_file"
        | "replace" => ToolKind::Edit,
        "write" | "write_file" | "create_file" | "create" => ToolKind::Write,
        "read" | "read_file" | "view" | "view_file" | "cat" | "readfile" | "read_file_content" => {
            ToolKind::Read
        }
        "bash" | "shell" | "terminal" | "execute" | "exec" | "run_command" | "run_terminal_cmd"
        | "execute_command" | "powershell" => ToolKind::Shell,
        _ => ToolKind::Other,
    }
}

/// Insert or refresh `path` so the list stays ordered oldest to newest touch.
fn touch(list: &mut Vec<String>, path: String) {
    if let Some(i) = list.iter().position(|p| *p == path) {
        list.remove(i);
    }
    list.push(path);
}

/// Keep the newest `max` items of an oldest-to-newest list.
pub(crate) fn keep_newest(list: &mut Vec<String>, max: usize) {
    if list.len() > max {
        list.drain(..list.len() - max);
    }
}

fn is_image_path(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".heic"]
        .iter()
        .any(|ext| l.ends_with(ext))
}

pub fn extract(messages: &[RcMessage]) -> Sections {
    extract_with(messages, true)
}

/// `first_user_is_goal` is false when an earlier brief already recorded the
/// goal: the first user message of a later window is then an ordinary turn.
pub fn extract_with(messages: &[RcMessage], first_user_is_goal: bool) -> Sections {
    let mut s = Sections::default();
    let mut files_modified: Vec<String> = Vec::new();
    let mut files_created: Vec<String> = Vec::new();
    let mut files_read: Vec<String> = Vec::new();
    let mut seen_outstanding: BTreeSet<String> = BTreeSet::new();
    let mut seen_prefs: BTreeSet<String> = BTreeSet::new();
    let mut seen_facts: BTreeSet<String> = BTreeSet::new();
    let mut goal_scope_changes: Vec<String> = Vec::new();

    let mut first_user_done = !first_user_is_goal;
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
                        match tool_kind(name) {
                            ToolKind::Edit => {
                                if let Some(p) = arg_path(arguments) {
                                    touch(&mut files_modified, p);
                                }
                            }
                            ToolKind::Write => {
                                if let Some(p) = arg_path(arguments) {
                                    touch(&mut files_created, p);
                                }
                            }
                            ToolKind::Read => {
                                // Screenshots are evidence of a moment, not
                                // files the work depends on.
                                if let Some(p) = arg_path(arguments).filter(|p| !is_image_path(p)) {
                                    touch(&mut files_read, p);
                                }
                            }
                            ToolKind::Shell => {
                                if let Some(cmd) = arguments
                                    .get("command")
                                    .or_else(|| arguments.get("cmd"))
                                    .and_then(|c| c.as_str())
                                {
                                    for c in extract_commits(cmd) {
                                        s.commits.push(c);
                                    }
                                }
                            }
                            ToolKind::Other => {}
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
                        // A successful file read that mentions "error" is source
                        // text, not a failure.
                        let source = is_source_read(tool_name) && !*is_error;
                        if !source && (*is_error || looks_failed(text)) {
                            if let Some(o) = extract_outstanding(text, *is_error) {
                                if seen_outstanding.insert(normalize_pref(&o)) {
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
    // A file that was changed is already listed; "Read" is for context only.
    files_read.retain(|p| !files_modified.contains(p) && !files_created.contains(p));
    // Every list keeps its newest entries: the latest work is what the next
    // turn needs, and an alphabetical or first-seen cut drops it.
    keep_newest(&mut files_modified, MAX_FILES);
    keep_newest(&mut files_created, MAX_FILES);
    keep_newest(&mut files_read, MAX_FILES);
    keep_newest(&mut s.outstanding, MAX_OUTSTANDING);
    for list in [&mut files_modified, &mut files_created, &mut files_read] {
        list.sort();
    }
    s.files_modified = files_modified;
    s.files_created = files_created;
    s.files_read = files_read;
    s.commits = s.commits.into_iter().rev().take(MAX_COMMITS).collect();
    s
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
    // (`/dev/null` and other device paths are not a project folder.)
    if let Some(idx) = p.find("/dev/").filter(|&i| i > 0) {
        return p[idx + 5..].to_string();
    }
    // Also collapse other absolute homes: /Users/<name>/... → rest
    for home in ["/Users/", "/home/"] {
        if let Some(rest) = p.strip_prefix(home) {
            if let Some(slash) = rest.find('/') {
                return rest[slash + 1..].to_string();
            }
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
        || lower.contains("cannot ")
        || lower.contains("err!")
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
    let recent_first = text.len() > FACT_SCAN_BYTES;
    let mut segments = fact_scan_segments(text);
    if recent_first {
        // In a long log the final failure must not be crowded out by early
        // warnings. Small outputs retain the established first-fact ordering.
        segments.reverse();
    }
    for seg in segments {
        let mut lines = seg.lines();
        while let Some(line) = if recent_first {
            lines.next_back()
        } else {
            lines.next()
        } {
            let l = line.trim();
            // Length gate *before* any allocation: a >1 MiB single line is
            // never lowercased or cloned.
            if l.len() < 10 || l.len() > FACT_LINE_MAX {
                continue;
            }
            let lower = l.to_lowercase();
            if !is_diagnostic_line(&lower, strict, named_error(l)) {
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
            // A dash joins the word into an identifier (`cf-error-footer`).
            let is_path_or_word = |c| is_word_byte(c) || matches!(c, b'.' | b'/' | b'\\' | b'-');
            (*pos == 0 || !is_path_or_word(bytes[*pos - 1]))
                && (*pos + token.len() == bytes.len()
                    || !is_path_or_word(bytes[*pos + token.len()]))
        })
        .map(|(pos, _)| pos)
        .min()
}

/// Byte offset of an exception name that states what went wrong:
/// `TypeError:`, `ValueError:`, `NullPointerException:`.
fn named_error(l: &str) -> Option<usize> {
    let mut start = 0;
    for token in l.split(|c: char| !c.is_ascii_alphanumeric()) {
        let end = start + token.len();
        let named = ["Error", "Exception"].iter().any(|suffix| {
            token.len() > suffix.len()
                && token.ends_with(suffix)
                && token.starts_with(|c: char| c.is_ascii_uppercase())
        });
        if named && l.as_bytes().get(end) == Some(&b':') {
            return Some(start);
        }
        start = end + 1;
    }
    None
}

fn is_diagnostic_line(lower: &str, strict: bool, named: Option<usize>) -> bool {
    if strict && (lower.starts_with("//") || lower.starts_with("/*") || lower.starts_with('*')) {
        return false;
    }
    // A named exception is both the severity and the code.
    let Some(sev_pos) = severity_pos(lower).or(named) else {
        return false;
    };
    // A successful source read quotes examples, not observed diagnostics.
    // Command output and failed tools may legitimately return JSON/quoted
    // diagnostics, so do not discard those just because they contain quotes.
    if strict && inside_quotes(lower, sev_pos) {
        return false;
    }
    if !has_code_token(lower) && named.is_none() {
        return false;
    }
    !strict
        || named == Some(0)
        || starts_with_severity(lower)
        || has_location_prefix(lower, sev_pos)
        || starts_with_diagnostic_code(lower)
        || has_log_prefix(lower, sev_pos)
}

fn starts_with_severity(lower: &str) -> bool {
    SEVERITY_TOKENS.iter().any(|token| {
        lower
            .strip_prefix(token)
            .is_some_and(|rest| rest.starts_with(':') || rest.starts_with(char::is_whitespace))
    }) || lower.starts_with("npm err!")
}

/// A code-prefixed log line is still a diagnostic when read from a file.
/// Keep this anchored so source expressions containing codes do not qualify.
fn starts_with_diagnostic_code(lower: &str) -> bool {
    let first = lower
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches(':');
    if has_letter_digit_code(first) {
        return true;
    }
    let Some((prefix, rest)) = first.split_once('-') else {
        return false;
    };
    let Some((digits, label)) = rest.split_once('-') else {
        return false;
    };
    (1..=5).contains(&prefix.len())
        && prefix.bytes().all(|c| c.is_ascii_alphabetic())
        && digits.len() >= 2
        && digits.bytes().all(|c| c.is_ascii_digit())
        && !label.is_empty()
        && label
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

/// Recognize common timestamp and bracketed-level prefixes in log reads.
fn has_log_prefix(lower: &str, sev_pos: usize) -> bool {
    if let Some((level, rest)) = lower.strip_prefix('[').and_then(|s| s.split_once(']')) {
        if SEVERITY_TOKENS.contains(&level)
            && (rest.starts_with(':') || rest.starts_with(char::is_whitespace))
        {
            return true;
        }
    }
    let prefix = lower[..sev_pos]
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let bytes = prefix.as_bytes();
    bytes.len() >= 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4]
            .iter()
            .chain(&bytes[5..7])
            .chain(&bytes[8..10])
            .all(u8::is_ascii_digit)
        && bytes[10..].iter().all(|c| {
            c.is_ascii_digit() || matches!(c, b't' | b'z' | b':' | b'+' | b'-' | b'.' | b' ')
        })
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

/// One token shaped like `e-8341-deploy` or `auth-123-fail`: a letter
/// prefix, a run of two or more digits and a label, joined by dashes. Counting
/// dashes and digits anywhere on the line also matched dates, CSS classes
/// and checklist prose.
fn dashed_code(lower: &str) -> bool {
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .any(|token| {
            let mut parts = token.split('-');
            let Some(first) = parts.next() else {
                return false;
            };
            let digits = parts.next().unwrap_or("");
            let label = parts.next().unwrap_or("");
            (1..=8).contains(&first.len())
                && first.bytes().all(|c| c.is_ascii_alphabetic())
                && digits.len() >= 2
                && digits.bytes().all(|c| c.is_ascii_digit())
                && !label.is_empty()
        })
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

/// An errno-style token: `ENOENT`, `EACCES`, `ECONNREFUSED`.
fn has_errno(l: &str) -> bool {
    l.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|t| t.len() >= 5 && t.starts_with('E') && t.bytes().all(|c| c.is_ascii_uppercase()))
}

/// Does this line state a failure, as tools print one? Accepted shapes: the
/// line opens with the severity (`error ...`, `Fatal error: ...`,
/// `cannot open ...`), the severity is marked (`error:`, `error[E0432]`,
/// `ERR!`, `src/a.ts(3,5): error TS2304`), or it is shouted (`FAILED`,
/// `ERROR`). A severity word inside a sentence or markup is prose.
fn is_failure_line(l: &str) -> bool {
    // Markup is page content; `157:text` is a numbered line of a searched file.
    let digits = l.bytes().take_while(u8::is_ascii_digit).count();
    if l.starts_with('<') || (digits > 0 && l.as_bytes().get(digits) == Some(&b':')) {
        return false;
    }
    const WORDS: [&str; 7] = [
        "error", "err!", "fatal", "panic", "panicked", "failed", "failure",
    ];
    let lower = l.to_ascii_lowercase();
    let lb = lower.as_bytes();
    if lower.starts_with("cannot ") || lower.starts_with("can't ") {
        return true;
    }
    for w in WORDS {
        for (pos, _) in lower.match_indices(w) {
            let end = pos + w.len();
            let before_ok = pos == 0 || !(is_word_byte(lb[pos - 1]) || lb[pos - 1] == b'-');
            let after = lb.get(end).copied();
            let after_ok = after.is_none_or(|c| !(is_word_byte(c) || c == b'-'));
            if !before_ok || !after_ok {
                continue;
            }
            // Opens the line (after list or log decoration).
            let lead = lower[..pos].trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
            if lead.is_empty() || lead == "npm " {
                return true;
            }
            // Marked as a diagnostic.
            if matches!(w, "err!" | "panicked") || matches!(after, Some(b':' | b'[' | b'!')) {
                return true;
            }
            // Follows a location: `file.rs:3:5: error`, `a.ts(3,5): error`.
            let head = lower[..pos].trim_end();
            if head.ends_with([':', ')', ']'])
                && head.rsplit(char::is_whitespace).next().is_some_and(|t| {
                    t.contains(['.', '/']) || t.bytes().any(|c| c.is_ascii_digit())
                })
            {
                return true;
            }
            // Shouted.
            let original = &l[pos..end];
            if original.bytes().all(|c| !c.is_ascii_lowercase()) {
                return true;
            }
        }
    }
    false
}

/// The line that states the failure. Words count only when they stand alone
/// (`error:`, `FAILED`), so identifiers such as `errors` or `on_error` in a
/// search result or listing are not reported as a problem. A result the tool
/// itself flagged as failed falls back to the line naming the cause.
fn extract_outstanding(text: &str, is_error: bool) -> Option<String> {
    let mut candidates: Vec<&str> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.len() < 12 {
            continue;
        }
        // Length gate before lowercasing: one huge line must not allocate.
        if l.len() > 4_000 {
            continue;
        }
        if is_error && candidates.len() < 200 {
            candidates.push(l);
        }
        if is_failure_line(l) {
            return Some(truncate_chars(l, 240));
        }
    }
    // The tool flagged a failure but printed no diagnostic: report the line
    // that says what went wrong, never an arbitrary line of its output.
    if is_error {
        const CAUSES: [&str; 14] = [
            "no such file",
            "not found",
            "denied",
            "timed out",
            "timeout",
            "unable to",
            "invalid",
            "refused",
            "aborted",
            "not permitted",
            "unknown option",
            "unrecognized",
            "missing",
            "does not exist",
        ];
        return candidates
            .into_iter()
            .find(|l| {
                let lower = l.to_ascii_lowercase();
                CAUSES.iter().any(|c| lower.contains(c)) || has_errno(l)
            })
            .map(|l| truncate_chars(l, 240));
    }
    None
}

/// Commits made by a shell command, as `commit: <subject>`.
///
/// Handles chained commands (`git add -A && git commit -m "..."`), combined
/// flags (`-am`), `--message`, and a message passed through a heredoc
/// (`-m "$(cat <<'EOF' ...`). Only the subject line is kept.
fn extract_commits(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = cmd[from..].find("git commit") {
        let start = from + found;
        from = start + "git commit".len();
        let rest = &cmd[from..];
        let entry = match commit_subject(rest) {
            Some(subject) if !subject.is_empty() => format!("commit: {subject}"),
            _ => {
                let line = cmd[start..].lines().next().unwrap_or("").trim();
                truncate_chars(line, 120)
            }
        };
        out.push(entry);
    }
    out
}

/// The subject of the commit whose arguments start at `args`.
fn commit_subject(args: &str) -> Option<String> {
    let bytes = args.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // The command ends at a separator or the end of its line.
        match bytes[i] {
            b'\n' | b';' | b'|' => return None,
            b'&' if bytes.get(i + 1) == Some(&b'&') => return None,
            b' ' | b'\t' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let end = args[i..]
            .find(|c: char| c.is_whitespace())
            .map_or(args.len(), |n| i + n);
        let word = &args[i..end];
        // `--message="two words"`: the value runs past the first space.
        let value = if word.starts_with("--message=") {
            Some(&args[i + "--message=".len()..])
        } else if word == "--message"
            || (word.starts_with('-')
                && !word.starts_with("--")
                && word.ends_with('m')
                && word[1..].bytes().all(|c| c.is_ascii_alphabetic()))
        {
            Some(args[end..].trim_start_matches([' ', '\t']))
        } else {
            None
        };
        if let Some(v) = value {
            return Some(message_subject(v));
        }
        i = end;
    }
    None
}

/// First line of a commit message given the text that follows the flag.
fn message_subject(v: &str) -> String {
    let body = match v.chars().next() {
        Some(q @ ('"' | '\'')) => {
            let inner = &v[1..];
            // A heredoc passes the message on the following lines.
            if inner.starts_with("$(cat <<") {
                inner
                    .lines()
                    .skip(1)
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
            } else {
                let close = inner
                    .char_indices()
                    .find(|&(k, c)| c == q && !inner[..k].ends_with('\\'))
                    .map_or(inner.len(), |(k, _)| k);
                &inner[..close]
            }
        }
        // Unquoted: one word, without a command separator stuck to it.
        _ => v
            .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|'))
            .next()
            .unwrap_or(""),
    };
    let subject = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    truncate_chars(subject.trim(), 120)
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

    fn call(name: &str, args: serde_json::Value) -> RcMessage {
        msg(
            Role::Assistant,
            vec![Block::ToolCall {
                id: "c".into(),
                name: name.into(),
                arguments: args,
            }],
        )
    }

    #[test]
    fn tool_names_from_other_harnesses_are_recognised() {
        let msgs = vec![
            call(
                "Edit",
                serde_json::json!({"file_path": "/home/x/proj/a.ts"}),
            ),
            call("patch", serde_json::json!({"path": "b.py"})),
            call("Write", serde_json::json!({"file_path": "c.md"})),
            call("write_file", serde_json::json!({"path": "d.md"})),
            call("Read", serde_json::json!({"file_path": "e.rs"})),
            call(
                "terminal",
                serde_json::json!({"command": "git commit -m 'ship it'"}),
            ),
            call(
                "Bash",
                serde_json::json!({"command": "git commit -m \"second\""}),
            ),
        ];
        let s = extract(&msgs);
        assert_eq!(s.files_modified, vec!["b.py", "proj/a.ts"]);
        assert_eq!(s.files_created, vec!["c.md", "d.md"]);
        assert_eq!(s.files_read, vec!["e.rs"]);
        assert_eq!(s.commits, vec!["commit: second", "commit: ship it"]);
    }

    #[test]
    fn file_lists_keep_the_newest_and_skip_screenshots() {
        let mut msgs: Vec<RcMessage> = (0..40)
            .map(|i| {
                call(
                    "read",
                    serde_json::json!({"path": format!("src/f{i:02}.rs")}),
                )
            })
            .collect();
        msgs.push(call(
            "read",
            serde_json::json!({"path": "/tmp/shots/a.PNG"}),
        ));
        msgs.push(call("edit", serde_json::json!({"path": "src/f39.rs"})));
        let s = extract(&msgs);
        assert_eq!(s.files_read.len(), MAX_FILES);
        // Oldest reads fall off; the newest stay. The edited file is listed
        // once, under Modified.
        assert!(!s.files_read.contains(&"src/f00.rs".to_string()));
        assert!(s.files_read.contains(&"src/f38.rs".to_string()));
        assert!(!s.files_read.iter().any(|p| p.ends_with(".PNG")));
        assert!(!s.files_read.contains(&"src/f39.rs".to_string()));
        assert_eq!(s.files_modified, vec!["src/f39.rs"]);
    }

    #[test]
    fn outstanding_ignores_identifiers_and_keeps_the_newest_failures() {
        let grep = "src/i18n.ts:64:  send: string; errors: Record<string, string>;";
        let s = extract(&[tool_result("bash", grep, false)]);
        assert!(s.outstanding.is_empty(), "{:?}", s.outstanding);
        // A successful source read is never a failure.
        let s = extract(&[tool_result(
            "read",
            "throw new Error(\"request failed\");",
            false,
        )]);
        assert!(s.outstanding.is_empty(), "{:?}", s.outstanding);
        for prose in [
            "<span class=\"cf-no-screenshot error\"></span>",
            "Without a declaration this is an audit only: it never blocks a deploy, and no failure inside the audit can block it",
            "Each item names the failure it prevents. Do all of them before any deploy.",
            "4. CPU: limits.cpuMs set if any request can run past 30 s. Prevents: error 1102.",
            "157:reports failure: automatic resume refuses instead of repeating",
            "- A commit the log reader cannot account for never shrinks the verified range",
        ] {
            let s = extract(&[tool_result("bash", prose, false)]);
            assert!(s.outstanding.is_empty(), "{:?}", s.outstanding);
        }
        for failure in [
            "Fatal error: A session is already selected",
            "src/a.ts(3,5): error TS2304: Cannot find name 'foo'",
            "error[E0432]: unresolved import `foo`",
            "npm ERR! code ELIFECYCLE",
            "test result: FAILED. 3 passed; 1 failed",
            "thread 'main' panicked at src/main.rs:4:5:",
            "cannot open file 'x.txt' for reading",
        ] {
            let s = extract(&[tool_result("bash", failure, false)]);
            assert_eq!(s.outstanding, vec![failure], "{failure}");
        }
        // A flagged failure with no diagnostic reports the line naming the cause.
        let s = extract(&[tool_result(
            "bash",
            "app build 0 4\nENOENT: no such file or directory, access 'x.txt'",
            true,
        )]);
        assert_eq!(
            s.outstanding,
            vec!["ENOENT: no such file or directory, access 'x.txt'"]
        );
        // Flagged, but nothing in the output names a cause: no invented line.
        let s = extract(&[tool_result("bash", "deploy 0 70050bbebc1c\napp 0", true)]);
        assert!(s.outstanding.is_empty(), "{:?}", s.outstanding);
        let msgs: Vec<RcMessage> = (0..12)
            .map(|i| tool_result("bash", &format!("error: step {i:02} failed"), false))
            .collect();
        let s = extract(&msgs);
        assert_eq!(s.outstanding.len(), MAX_OUTSTANDING);
        assert_eq!(s.outstanding[0], "error: step 04 failed");
        assert_eq!(s.outstanding[7], "error: step 11 failed");
    }

    #[test]
    fn markup_dates_and_checklists_are_not_key_facts() {
        for line in [
            "<div class=\"cf-error-footer cf-wrapper w-240 py-10\">",
            "- [ ] Post-deploy: the cron-completion/1 line is ok; error counts fall.",
            "2026-11-26 run-2 finished, 0 failed",
            "8ce7d457 Guard docs: the failure set includes 403s (T-4632)",
        ] {
            assert!(extract_key_facts(line, "bash", false).is_empty(), "{line}");
        }
        assert_eq!(
            extract_key_facts("E-8341-DEPLOY fatal rollback initiated", "bash", false).len(),
            1
        );
    }

    #[test]
    fn commit_subjects_from_every_common_form() {
        let cases = [
            (
                "git commit -am 'fix(refunds): parse amounts'",
                "commit: fix(refunds): parse amounts",
            ),
            (
                "npm test && git commit -m \"ship it\" && git push",
                "commit: ship it",
            ),
            ("git commit --message=\"long form\"", "commit: long form"),
            ("git commit --message 'spaced form'", "commit: spaced form"),
            (
                "git commit -q -m \"subject line\n\nbody text\"",
                "commit: subject line",
            ),
            (
                "git commit -m \"$(cat <<'EOF'\nHeredoc subject\n\nBody.\nEOF\n)\"",
                "commit: Heredoc subject",
            ),
            (
                "git commit --amend --no-edit && ls -m",
                "git commit --amend --no-edit && ls -m",
            ),
        ];
        for (cmd, want) in cases {
            assert_eq!(extract_commits(cmd), vec![want.to_string()], "{cmd}");
        }
        let two = extract_commits("git commit -m one; git add x && git commit -m two");
        assert_eq!(two, vec!["commit: one", "commit: two"]);
        assert!(extract_commits("git status").is_empty());
    }

    #[test]
    fn named_exceptions_are_key_facts() {
        let log = "2026-11-12T10:59:59 ERROR refund POST /refunds 500 TypeError: amount.toFixed is not a function";
        assert_eq!(extract_key_facts(log, "bash", false), vec![log.to_string()]);
        let py = "ValueError: invalid literal for int() with base 10: 'x'";
        assert_eq!(extract_key_facts(py, "bash", false), vec![py.to_string()]);
        // Source that mentions an exception is not an observed failure.
        let src = "    throw new TypeError(\"TypeError: bad amount\");";
        assert!(extract_key_facts(src, "read", false).is_empty());
        assert!(extract_key_facts("see the Error: section of the docs", "bash", false).is_empty());
    }

    #[test]
    fn a_window_after_the_goal_is_recorded_adds_no_goal() {
        let msgs = vec![msg(
            Role::User,
            vec![Block::Text {
                text: "now check coverage".into(),
            }],
        )];
        assert!(extract_with(&msgs, false).goal.is_empty());
        assert_eq!(extract_with(&msgs, true).goal, vec!["now check coverage"]);
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
        assert!(facts.iter().any(|f| f.contains("E_AUTH_FAIL")), "{facts:?}");
        assert!(
            facts.iter().any(|f| f.contains("WARN_RATE_LIMIT")),
            "{facts:?}"
        );
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
        // Failed results can legitimately carry a quoted JSON diagnostic.
        assert_eq!(
            extract_key_facts(r#"{"message":"fatal code=E_PARSE_FAIL"}"#, "read", true),
            vec![r#"{"message":"fatal code=E_PARSE_FAIL"}"#.to_string()]
        );
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
    fn successful_log_reads_keep_code_prefixed_diagnostics() {
        let line = "E-8341-DEPLOY fatal rollback initiated";
        assert_eq!(
            extract_key_facts(line, "read", false),
            vec![line.to_string()]
        );
        let auth = "AUTH-123-FAIL fatal token rejected";
        assert_eq!(
            extract_key_facts(auth, "read", false),
            vec![auth.to_string()]
        );
        assert!(extract_key_facts(
            "const CODE = \"E-8341-DEPLOY fatal rollback\";",
            "read",
            false
        )
        .is_empty());
    }

    #[test]
    fn successful_log_reads_keep_timestamp_and_level_prefixes() {
        let text = "[2026-10-13T17:00:00Z] error code=E_TIMESTAMP\n[ERROR] code=E_LEVEL\n2026-10-13 17:00:00 error code=E_CLOCK\n";
        assert_eq!(extract_key_facts(text, "read", false).len(), 3);
    }

    #[test]
    fn long_logs_prioritize_final_failure_over_early_warnings() {
        let text = format!(
            "{}{}fatal code=E_FINAL_FAILURE\n",
            "warning code=W_EARLY\n".repeat(8),
            filler(2000)
        );
        let facts = extract_key_facts(&text, "bash", false);
        assert!(facts[0].contains("E_FINAL_FAILURE"), "{facts:?}");
        assert!(facts.len() <= MAX_FACTS_PER_RESULT);
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
