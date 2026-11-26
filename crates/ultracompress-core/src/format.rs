//! Summary formatting and bounded merge across successive compactions.
//!
//! Sticky sections (goal, files, commits, preferences) dedup and accumulate
//! across merges; volatile sections (outstanding context) replace; the
//! transcript rolls. This keeps repeated compactions from degrading — the
//! failure mode of LLM-summary compaction chains.

use crate::sections::Sections;
use crate::transcript::Transcript;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PreviousSummary {
    pub goal: Vec<String>,
    pub files_modified: Vec<String>,
    pub files_created: Vec<String>,
    pub files_read: Vec<String>,
    pub commits: Vec<String>,
    pub preferences: Vec<String>,
    pub key_facts: Vec<String>,
    pub archived_notes: Vec<String>,
}

/// A parsed previous summary lets the merge be structural instead of
/// string-splicing. Extensions round-trip this via compaction `details`.
impl PreviousSummary {
    /// Best-effort parse of a rendered summary (the bracketed format below).
    pub fn parse(text: &str) -> Option<PreviousSummary> {
        let mut ps = PreviousSummary {
            goal: vec![],
            files_modified: vec![],
            files_created: vec![],
            files_read: vec![],
            commits: vec![],
            preferences: vec![],
            key_facts: vec![],
            archived_notes: vec![],
        };
        let mut current = "";
        for line in text.lines() {
            let l = line.trim_end();
            if l.starts_with('[') && l.ends_with(']') && !l.starts_with("[ultracompress-archive") {
                current = &l[1..l.len() - 1];
                continue;
            }
            if l.is_empty() || l.starts_with("---") {
                continue;
            }
            let item = l.strip_prefix("- ").unwrap_or(l);
            match current {
                "Session Goal" => ps.goal.push(item.to_string()),
                "Files And Changes" => {
                    if let Some(p) = item.strip_prefix("Modified: ") {
                        ps.files_modified.push(p.to_string());
                    } else if let Some(p) = item.strip_prefix("Created: ") {
                        ps.files_created.push(p.to_string());
                    } else if let Some(p) = item.strip_prefix("Read: ") {
                        ps.files_read.push(p.to_string());
                    }
                }
                "Commits" => ps.commits.push(item.to_string()),
                "User Preferences" => ps.preferences.push(item.to_string()),
                "Key Facts" => ps.key_facts.push(item.to_string()),
                "Archived" => ps.archived_notes.push(item.to_string()),
                _ => {}
            }
        }
        if ps.goal.is_empty() && ps.files_modified.is_empty() && ps.commits.is_empty() {
            None
        } else {
            Some(ps)
        }
    }
}

/// Caps for sticky sections after merging a previous brief. Per-window
/// extraction is capped separately; without these a long session's brief
/// grew by a full window of entries at every compaction.
const MAX_MERGED_GOAL: usize = 6;
const MAX_MERGED_FILES: usize = 48;
const MAX_MERGED_READS: usize = 24;
const MAX_MERGED_COMMITS: usize = 16;
const MAX_MERGED_PREFERENCES: usize = 12;
const MAX_MERGED_KEY_FACTS: usize = 20;

fn dedup_push(out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>, item: String) {
    let key = item.to_lowercase();
    if seen.insert(key) {
        out.push(item);
    }
}

/// Merge new extraction over a previous summary. Returns the merged sections
/// to render.
pub fn merge(prev: Option<&PreviousSummary>, new: &Sections) -> Sections {
    let mut out = Sections::default();
    let mut seen = std::collections::HashSet::new();

    match prev {
        None => new.clone(),
        Some(p) => {
            for g in p.goal.iter().chain(new.goal.iter()) {
                dedup_push(&mut out.goal, &mut seen, g.clone());
            }
            let mut seen_m = std::collections::HashSet::new();
            for f in p.files_modified.iter().chain(new.files_modified.iter()) {
                if seen_m.insert(f.clone()) {
                    out.files_modified.push(f.clone());
                }
            }
            let mut seen_c = std::collections::HashSet::new();
            for f in p.files_created.iter().chain(new.files_created.iter()) {
                if seen_c.insert(f.clone()) {
                    out.files_created.push(f.clone());
                }
            }
            let mut seen_r = std::collections::HashSet::new();
            for f in p.files_read.iter().chain(new.files_read.iter()) {
                if seen_r.insert(f.clone()) {
                    out.files_read.push(f.clone());
                }
            }
            let mut seen_g = std::collections::HashSet::new();
            for c in p.commits.iter().chain(new.commits.iter()) {
                if seen_g.insert(c.clone()) {
                    out.commits.push(c.clone());
                }
            }
            // Volatile: outstanding context comes only from the new pass.
            out.outstanding = new.outstanding.clone();
            let mut seen_p = std::collections::HashSet::new();
            for p2 in p.preferences.iter().chain(new.preferences.iter()) {
                if seen_p.insert(p2.to_lowercase()) {
                    out.preferences.push(p2.clone());
                }
            }
            // Sticky: key diagnostic facts accumulate — losing a fact that
            // was already earned is the cardinal sin of compaction.
            let mut seen_f = std::collections::HashSet::new();
            for f in p.key_facts.iter().chain(new.key_facts.iter()) {
                if seen_f.insert(f.to_lowercase()) {
                    out.key_facts.push(f.clone());
                }
            }
            let _ = seen;
            // Repeated compactions must not grow the brief without bound:
            // each sticky list keeps its newest entries. The goal keeps the
            // original mission plus the latest scope changes.
            use crate::sections::keep_newest;
            if out.goal.len() > MAX_MERGED_GOAL {
                let tail = out.goal.split_off(out.goal.len() - (MAX_MERGED_GOAL - 1));
                out.goal.truncate(1);
                out.goal.extend(tail);
            }
            out.files_read
                .retain(|f| !out.files_modified.contains(f) && !out.files_created.contains(f));
            keep_newest(&mut out.files_modified, MAX_MERGED_FILES);
            keep_newest(&mut out.files_created, MAX_MERGED_FILES);
            keep_newest(&mut out.files_read, MAX_MERGED_READS);
            keep_newest(&mut out.commits, MAX_MERGED_COMMITS);
            keep_newest(&mut out.preferences, MAX_MERGED_PREFERENCES);
            keep_newest(&mut out.key_facts, MAX_MERGED_KEY_FACTS);
            out
        }
    }
}

/// Render the final summary text.
pub fn render(
    s: &Sections,
    transcript: &Transcript,
    archived_notes: &[String],
    critical_context: &[String],
) -> String {
    let mut out = String::with_capacity(2048);
    let mut section = |title: &str, body: &dyn Fn(&mut String)| {
        out.push('[');
        out.push_str(title);
        out.push_str("]\n");
        body(&mut out);
        out.push('\n');
    };

    if !s.goal.is_empty() {
        section("Session Goal", &|o| {
            for g in &s.goal {
                o.push_str("- ");
                o.push_str(g);
                o.push('\n');
            }
        });
    }
    if !(s.files_modified.is_empty() && s.files_created.is_empty() && s.files_read.is_empty()) {
        section("Files And Changes", &|o| {
            for f in &s.files_modified {
                o.push_str(&format!("- Modified: {f}\n"));
            }
            for f in &s.files_created {
                o.push_str(&format!("- Created: {f}\n"));
            }
            for f in &s.files_read {
                o.push_str(&format!("- Read: {f}\n"));
            }
        });
    }
    if !s.commits.is_empty() {
        section("Commits", &|o| {
            for c in &s.commits {
                o.push_str(&format!("- {c}\n"));
            }
        });
    }
    if !s.key_facts.is_empty() {
        section("Key Facts", &|o| {
            for f in &s.key_facts {
                o.push_str("- ");
                o.push_str(f);
                o.push('\n');
            }
        });
    }
    if !critical_context.is_empty() {
        section("Critical Context", &|o| {
            for c in critical_context {
                o.push_str(c);
                o.push('\n');
            }
        });
    }
    if !s.outstanding.is_empty() {
        section("Outstanding Context", &|o| {
            for x in &s.outstanding {
                o.push_str("- ");
                o.push_str(x);
                o.push('\n');
            }
        });
    }
    if !s.preferences.is_empty() {
        section("User Preferences", &|o| {
            for p in &s.preferences {
                o.push_str("- ");
                o.push_str(p);
                o.push('\n');
            }
        });
    }
    if !archived_notes.is_empty() {
        section("Archived", &|o| {
            for a in archived_notes {
                o.push_str("- ");
                o.push_str(a);
                o.push('\n');
            }
            o.push_str("- Recover verbatim detail with ultracompress_recall (raw session history is preserved).\n");
        });
    }
    if !transcript.lines.is_empty() {
        section("Transcript", &|o| {
            if transcript.omitted_lines > 0 {
                o.push_str(&format!(
                    "- ({} earlier lines omitted; full history recoverable via ultracompress_recall)\n",
                    transcript.omitted_lines
                ));
            }
            for l in &transcript.lines {
                o.push_str(&l.line);
                o.push('\n');
            }
        });
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_dedups_sticky_and_replaces_volatile() {
        let prev = PreviousSummary {
            goal: vec!["Fix auth".into()],
            files_modified: vec!["a.ts".into()],
            files_created: vec![],
            files_read: vec![],
            commits: vec!["c1".into()],
            preferences: vec!["Always test".into()],
            key_facts: vec![],
            archived_notes: vec![],
        };
        let new = Sections {
            goal: vec!["Fix auth".into(), "Add feature".into()],
            files_modified: vec!["a.ts".into(), "b.ts".into()],
            files_created: vec![],
            files_read: vec![],
            commits: vec!["c2".into()],
            outstanding: vec!["new error".into()],
            preferences: vec![],
            key_facts: vec![],
        };
        let m = merge(Some(&prev), &new);
        assert_eq!(
            m.goal,
            vec!["Fix auth".to_string(), "Add feature".to_string()]
        );
        assert_eq!(
            m.files_modified,
            vec!["a.ts".to_string(), "b.ts".to_string()]
        );
        assert_eq!(m.commits, vec!["c1".to_string(), "c2".to_string()]);
        assert_eq!(m.outstanding, vec!["new error".to_string()]);
    }

    #[test]
    fn repeated_merges_stay_bounded_and_keep_the_newest() {
        let mut summary = String::new();
        for round in 0..30 {
            let new = Sections {
                goal: vec![if round == 0 {
                    "Original mission".to_string()
                } else {
                    format!("[Scope change] also do {round}")
                }],
                files_modified: (0..10).map(|i| format!("src/m{round}_{i}.rs")).collect(),
                files_created: vec![],
                files_read: (0..10).map(|i| format!("src/r{round}_{i}.rs")).collect(),
                commits: vec![format!("commit: round {round}")],
                outstanding: vec![],
                preferences: vec![format!("Always check {round}")],
                key_facts: vec![format!("error TS{round}000: fact")],
            };
            let prev = PreviousSummary::parse(&summary);
            let merged = merge(prev.as_ref(), &new);
            summary = render(&merged, &Transcript::default(), &[], &[]);
        }
        let p = PreviousSummary::parse(&summary).unwrap();
        assert_eq!(p.goal.len(), MAX_MERGED_GOAL);
        assert_eq!(p.goal[0], "Original mission");
        assert_eq!(p.goal.last().unwrap(), "[Scope change] also do 29");
        assert_eq!(p.files_modified.len(), MAX_MERGED_FILES);
        assert!(p.files_modified.contains(&"src/m29_9.rs".to_string()));
        assert!(!p.files_modified.contains(&"src/m0_0.rs".to_string()));
        assert_eq!(p.files_read.len(), MAX_MERGED_READS);
        assert_eq!(p.commits.len(), MAX_MERGED_COMMITS);
        assert_eq!(p.preferences.len(), MAX_MERGED_PREFERENCES);
        assert_eq!(p.key_facts.len(), MAX_MERGED_KEY_FACTS);
        assert!(summary.len() < 6_000, "{}", summary.len());
    }

    #[test]
    fn render_produces_parseable_output() {
        let s = Sections {
            goal: vec!["Ship the thing".into()],
            ..Default::default()
        };
        let t = Transcript::default();
        let text = render(
            &s,
            &t,
            &["[ultracompress-archive frame f1: bash output]".to_string()],
            &[],
        );
        assert!(text.contains("[Session Goal]"));
        let parsed = PreviousSummary::parse(&text);
        assert!(parsed.is_some());
        assert_eq!(parsed.unwrap().goal, vec!["Ship the thing".to_string()]);
    }

    #[test]
    fn roundtrip_full_summary() {
        let s = Sections {
            goal: vec!["Build x".into()],
            files_modified: vec!["src/a.ts".into()],
            files_created: vec!["src/b.ts".into()],
            files_read: vec!["src/c.ts".into()],
            commits: vec!["commit: init".into()],
            outstanding: vec!["err".into()],
            preferences: vec!["Always lint".into()],
            key_facts: vec!["E-8341-DEPLOY fatal rollback".into()],
        };
        let text = render(&s, &Transcript::default(), &[], &["ctx".into()]);
        let p = PreviousSummary::parse(&text).unwrap();
        assert_eq!(p.files_modified, vec!["src/a.ts".to_string()]);
        assert_eq!(p.files_created, vec!["src/b.ts".to_string()]);
        assert_eq!(p.files_read, vec!["src/c.ts".to_string()]);
        assert_eq!(p.commits, vec!["commit: init".to_string()]);
        assert_eq!(
            p.key_facts,
            vec!["E-8341-DEPLOY fatal rollback".to_string()]
        );
        assert_eq!(p.preferences, vec!["Always lint".to_string()]);
    }
}
