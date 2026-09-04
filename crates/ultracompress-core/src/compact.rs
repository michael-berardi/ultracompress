//! Compaction orchestrator: cut resolution, engine routing, summary build.
//!
//! This is the Rust heart of UltraCompress. The extension feeds it the live
//! branch entries (raw Pi session-entry shapes), calibration data, and the
//! previous summary; it returns a ready-to-save compaction result.

use crate::classify::Thresholds;
use crate::estimate::{calibrate, tokens_from_chars};
use crate::format::{merge, render, PreviousSummary};
use crate::model::{parse_message, Block, RcMessage};
use crate::policy::{engine_mix, Policy, VisionMode};
use crate::sections::{extract, Sections};
use crate::snap::SnapConfig;
use crate::transcript::{build as build_transcript, TranscriptConfig};
use crate::ucbridge::UcBridge;
use crate::ucbridge::UcPacket;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactInput {
    /// Raw Pi branch entries (session-entry JSON), newest last. Message and
    /// compaction entries are honored; other entry types are ignored.
    pub entries: Vec<serde_json::Value>,
    #[serde(default)]
    pub tokens_before: Option<u64>,
    #[serde(default)]
    pub previous_summary: Option<String>,
    #[serde(default = "default_policy")]
    pub policy: Policy,
    #[serde(default = "default_keep")]
    pub keep_user_turns: Option<usize>,
    #[serde(default = "default_true")]
    pub smart_keep_tail: bool,
    #[serde(default)]
    pub vision: VisionMode,
    /// True when the model registry reports image input support.
    #[serde(default)]
    pub model_vision: Option<bool>,
    #[serde(default)]
    pub uc_bin: Option<String>,
    #[serde(default)]
    pub uc_enabled: bool,
    #[serde(default)]
    pub thresholds: Option<Thresholds>,
    #[serde(default)]
    pub snap: Option<SnapConfig>,
    #[serde(default)]
    pub transcript: Option<TranscriptConfig>,
    #[serde(default)]
    pub image_tokens_per_frame: Option<u64>,
    /// Dry run: no frames are rendered, only the plan is computed.
    #[serde(default)]
    pub dry_run: bool,
}

fn default_policy() -> Policy {
    Policy::Auto
}
fn default_keep() -> Option<usize> {
    Some(1)
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockDecision {
    pub message_index: usize,
    pub tool: String,
    pub chars: usize,
    pub engine: crate::classify::Engine,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uc_savings_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompactStats {
    pub tokens_before_est: u64,
    pub tokens_after_est: u64,
    pub savings_pct: f64,
    pub summarized_messages: usize,
    pub kept_messages: usize,
    pub keep_user_turns_resolved: usize,
    pub smart_keep_adjusted: bool,
    pub uc_blocks: usize,
    pub uc_tokens_saved: u64,
    pub snap_blocks: usize,
    pub snap_frames: usize,
    pub snap_chars_archived: u64,
    pub chars_per_token: f64,
    pub calibrated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompactResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub decisions: Vec<BlockDecision>,
    pub details: serde_json::Value,
    pub stats: CompactStats,
    pub uc_status: crate::ucbridge::UcStatus,
    pub policy: Policy,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Cut {
    pub summarize_end: usize, // exclusive index into live messages
    pub first_kept_index: usize,
    pub compact_all: bool,
}

/// Live-message collection over branch entries, mirroring Pi semantics:
/// starts after the last compaction's kept boundary, with orphan recovery.
pub fn collect_live(entries: &[serde_json::Value]) -> (Vec<RcMessage>, Vec<String>) {
    let parsed: Vec<(String, String, Option<RcMessage>)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let obj = v.as_object()?;
            let ty = obj.get("type").and_then(|t| t.as_str())?;
            let id = obj
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let m = if ty == "message" {
                parse_message(v, &format!("m{i}"))
            } else {
                None
            };
            Some((id, ty.to_string(), m))
        })
        .collect();

    let last_compaction_idx = parsed.iter().rposition(|(_, ty, _)| ty == "compaction");
    let last_kept_id = last_compaction_idx
        .and_then(|i| entries[i].get("firstKeptEntryId"))
        .and_then(|k| k.as_str())
        .unwrap_or("")
        .to_string();

    let has_prior = last_compaction_idx.is_some();
    let has_valid_kept =
        !last_kept_id.is_empty() && parsed.iter().any(|(id, _, _)| *id == last_kept_id);
    let orphan = has_prior && !has_valid_kept;

    let mut live: Vec<RcMessage> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    if orphan {
        let start = last_compaction_idx.unwrap() + 1;
        for (id, _, m) in &parsed[start.min(parsed.len())..] {
            if let Some(m) = m {
                live.push(m.clone());
                ids.push(id.clone());
            }
        }
    } else {
        let mut found = last_kept_id.is_empty();
        for (id, ty, m) in &parsed {
            if !found && id == &last_kept_id {
                found = true;
            }
            if !found || ty == "compaction" {
                continue;
            }
            if let Some(m) = m {
                live.push(m.clone());
                ids.push(id.clone());
            }
        }
    }
    (live, ids)
}

/// Resolve the cut over live messages: keep the last N user turns. A single
/// user turn (agentic loop) is a split-turn: cut inside it at a token budget,
/// snapping to the nearest non-toolResult boundary — mirroring Pi semantics.
pub fn resolve_cut(live: &[RcMessage], keep_user_turns: usize, cpt: f64) -> Option<Cut> {
    if live.is_empty() {
        return None;
    }
    let user_indices: Vec<usize> = live
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == crate::model::Role::User)
        .map(|(i, _)| i)
        .collect();

    if keep_user_turns == 0 {
        return Some(Cut {
            summarize_end: live.len(),
            first_kept_index: usize::MAX,
            compact_all: true,
        });
    }
    if user_indices.len() < 2 {
        // Split-turn: cut inside the single turn on a token budget.
        if live.len() < 4 {
            return None; // too small to be worth compacting
        }
        let idx = find_budget_cut_index(live, MAX_SMART_TAIL_TOKENS, cpt)?;
        if idx == 0 {
            return None;
        }
        return Some(Cut {
            summarize_end: idx,
            first_kept_index: idx,
            compact_all: false,
        });
    }
    let target_count = user_indices.len().checked_sub(keep_user_turns)?;
    if target_count == 0 {
        return Some(Cut {
            summarize_end: live.len(),
            first_kept_index: usize::MAX,
            compact_all: true,
        });
    }
    let cut_idx = user_indices[target_count];
    if cut_idx == 0 {
        return Some(Cut {
            summarize_end: live.len(),
            first_kept_index: usize::MAX,
            compact_all: true,
        });
    }
    Some(Cut {
        summarize_end: cut_idx,
        first_kept_index: cut_idx,
        compact_all: false,
    })
}

/// Smart keep: boost keep:N when the tail is tiny so we retain more verbatim
/// context without blowing the budget. Mirrors pi-vcc's behavior.
pub fn resolve_smart_keep(
    live: &[RcMessage],
    requested: Option<usize>,
    smart: bool,
    cpt: f64,
    min_tokens: u64,
    max_tokens: u64,
) -> (usize, bool) {
    let base = requested.unwrap_or(1);
    if requested.is_some() || !smart {
        return (base, false);
    }
    let tail_chars = |k: usize| -> Option<usize> {
        let cut = resolve_cut(live, k, cpt)?;
        if cut.compact_all {
            return None;
        }
        Some(
            live[cut.first_kept_index..]
                .iter()
                .map(|m| m.total_chars())
                .sum(),
        )
    };
    let base_chars = match tail_chars(base) {
        Some(c) => c,
        None => return (base, false),
    };
    if tokens_from_chars(base_chars, cpt) > min_tokens {
        return (base, false);
    }
    let total_user = live
        .iter()
        .filter(|m| m.role == crate::model::Role::User)
        .count();
    let mut selected = base;
    for k in (base + 1)..=total_user.max(base) {
        match tail_chars(k) {
            Some(c) if tokens_from_chars(c, cpt) <= max_tokens => selected = k,
            _ => break,
        }
    }
    (selected, selected != base)
}

/// Token-budget tail rescue (port of pi-vcc's oversized-tail cut): when the
/// user-turn-anchored tail is absent or oversized, cut at the nearest valid
/// non-toolResult boundary inside a token budget instead. Explicit keep:N is
/// always respected and skips this path.
pub const MAX_SMART_TAIL_TOKENS: u64 = 25_000;
pub const OVERSIZED_TAIL_FACTOR: u64 = 2;

fn find_budget_cut_index(live: &[RcMessage], max_tokens: u64, cpt: f64) -> Option<usize> {
    let boundary = |from: usize| -> Option<usize> {
        (from.max(1)..live.len()).find(|&j| live[j].role != crate::model::Role::ToolResult)
    };
    let mut acc = 0u64;
    for i in (0..live.len()).rev() {
        acc += tokens_from_chars(live[i].total_chars(), cpt);
        if acc >= max_tokens {
            // Snap forward off any toolResult to the next valid boundary.
            return boundary(i);
        }
    }
    // Whole window is under the budget — but compaction was requested, so
    // keep only a minimal recent tail instead of refusing.
    boundary(1)
}

/// Apply the tail budget to a resolved cut. Returns the (possibly re-cut)
/// cut plus whether a budget cut happened and why.
pub fn apply_tail_budget(
    live: &[RcMessage],
    cut: Cut,
    explicit: bool,
    cpt: f64,
    max_tokens: u64,
) -> (Cut, Option<&'static str>) {
    if explicit {
        return (cut, None);
    }
    if cut.compact_all {
        // keep:0 explicit compacts everything absolutely; default-path
        // compact-all (no anchor) gets a budget rescue.
        if let Some(idx) = find_budget_cut_index(live, max_tokens, cpt) {
            return (
                Cut {
                    summarize_end: idx,
                    first_kept_index: idx,
                    compact_all: false,
                },
                Some("no_anchor"),
            );
        }
        return (cut, None);
    }
    let tail_tokens: u64 = live[cut.first_kept_index..]
        .iter()
        .map(|m| tokens_from_chars(m.total_chars(), cpt))
        .sum();
    if tail_tokens <= max_tokens * OVERSIZED_TAIL_FACTOR {
        return (cut, None);
    }
    let idx = match find_budget_cut_index(live, max_tokens, cpt) {
        Some(i) => i,
        None => return (cut, None),
    };
    if idx <= cut.first_kept_index {
        return (cut, None);
    }
    (
        Cut {
            summarize_end: idx,
            first_kept_index: idx,
            compact_all: false,
        },
        Some("oversized_tail"),
    )
}

/// Critical-context heuristic: the most recent UC packet in the summarized
/// span inlines into the summary; older ones become archive notes.
const INLINE_UC_MAX_TOKENS: u64 = 1200;

pub fn run(input: &CompactInput) -> Result<CompactResult, String> {
    let (live, ids) = collect_live(&input.entries);

    // Calibration over everything (summarized span will be close enough;
    // tokens_before refers to the whole context including the tail).
    let total_chars: usize = live.iter().map(|m| m.total_chars()).sum();
    let prev_chars = input
        .previous_summary
        .as_deref()
        .map(|s| s.len())
        .unwrap_or(0);
    let est = calibrate(total_chars + prev_chars, input.tokens_before);
    let cpt = est.chars_per_token;

    let (keep, smart_adjusted) = resolve_smart_keep(
        &live,
        input.keep_user_turns,
        input.smart_keep_tail,
        cpt,
        5_000,
        MAX_SMART_TAIL_TOKENS,
    );
    let explicit = input.keep_user_turns.is_some();
    let cut0 = resolve_cut(&live, keep, cpt).ok_or("nothing to compact: no safe cut point")?;
    let (cut, budget_cut) = apply_tail_budget(&live, cut0, explicit, cpt, MAX_SMART_TAIL_TOKENS);
    if cut.summarize_end == 0 {
        return Err("nothing to compact: empty summarized span".into());
    }

    let first_kept_entry_id = if cut.compact_all {
        String::new()
    } else {
        ids.get(cut.first_kept_index).cloned().unwrap_or_default()
    };

    // Engine availability.
    let mut uc = UcBridge::new(input.uc_bin.as_deref().unwrap_or("uc"));
    let uc_status = if input.uc_enabled {
        uc.probe()
    } else {
        crate::ucbridge::UcStatus {
            enabled: false,
            bin: input.uc_bin.clone().unwrap_or_else(|| "uc".into()),
            available: false,
            version: None,
            reason: Some("uc disabled by config".into()),
        }
    };
    let vision = input.vision.resolves(input.model_vision);
    let mix = engine_mix(
        input.policy,
        uc_status.available && input.uc_enabled,
        vision,
    );
    let th = input.thresholds.unwrap_or_default();

    let tr_cfg = input.transcript.clone().unwrap_or_default();

    let summarized = &live[..cut.summarize_end];

    // Route tool results: UC-only in summaries (snap belongs to the live path).
    let mut decisions: Vec<BlockDecision> = Vec::new();
    let mut uc_inline: Vec<(usize, UcPacket)> = Vec::new();
    let mut uc_notes: Vec<String> = Vec::new();

    for (i, m) in summarized.iter().enumerate() {
        for b in &m.content {
            if let Block::ToolResult {
                tool_name, text, ..
            } = b
            {
                if text.len() < th.uc_min_chars {
                    continue;
                }
                let engine = if mix.uc
                    && crate::classify::classify_content(text)
                        == crate::classify::ContentClass::Json
                {
                    crate::classify::Engine::Uc
                } else {
                    crate::classify::Engine::None
                };
                let mut decision = BlockDecision {
                    message_index: i,
                    tool: if tool_name.is_empty() {
                        "tool".into()
                    } else {
                        tool_name.clone()
                    },
                    chars: text.len(),
                    engine,
                    uc_savings_pct: None,
                    frames: None,
                };
                if engine == crate::classify::Engine::Uc {
                    if let Some(packet) = uc.encode_json(text) {
                        decision.uc_savings_pct = Some(packet.savings_pct);
                        uc_inline.push((i, packet));
                    } else {
                        decision.engine = crate::classify::Engine::None;
                    }
                }
                decisions.push(decision);
            }
        }
    }

    // UC inline selection: the most recent packet inlines as critical context
    // when small enough; older ones become archive notes (recall/decode to
    // recover). This is summary-appropriate: UC packets are model-readable text.
    let mut critical: Vec<String> = Vec::new();
    let mut uc_blocks = 0usize;
    let mut uc_tokens_saved: u64 = 0;
    if !uc_inline.is_empty() {
        let (_, last) = uc_inline.last().unwrap().clone();
        if last.tokens_uc <= INLINE_UC_MAX_TOKENS {
            critical.push(format!(
                "[UC packet — most recent JSON payload ({} → {} tokens, -{:.0}%); decode via ultracompress_uc decode]",
                human_chars(last.source_chars),
                last.tokens_uc,
                last.savings_pct
            ));
            critical.push(last.packet.clone());
            uc_blocks += 1;
            uc_tokens_saved += last.tokens_json.saturating_sub(last.tokens_uc);
            uc_inline.pop();
        }
        for (_, p) in &uc_inline {
            uc_notes.push(format!(
                "[ultracompress-archive uc: JSON payload, {}→{} tokens (-{:.0}%), decode via ultracompress_uc]",
                p.tokens_json, p.tokens_uc, p.savings_pct
            ));
            uc_blocks += 1;
            uc_tokens_saved += p.tokens_json.saturating_sub(p.tokens_uc);
        }
    }

    // Sections + transcript + merge + render.
    let mut new_sections = extract(summarized);
    let prev = input
        .previous_summary
        .as_deref()
        .and_then(PreviousSummary::parse);
    let merged: Sections = merge(prev.as_ref(), &new_sections);
    let mut archived: Vec<String> = Vec::new();
    if let Some(p) = &prev {
        archived.extend(p.archived_notes.iter().cloned());
    }
    archived.extend(uc_notes);
    new_sections = merged;

    let transcript = build_transcript(summarized, &tr_cfg);
    let summary = render(&new_sections, &transcript, &archived, &critical);

    // Token accounting. Snap frames are a live-path concern; compaction
    // savings come from the summary alone. The kept tail's savings via live
    // transforms are reported separately by `ultracompress transform`.
    let summarized_tokens = tokens_from_chars(total_chars_of(summarized), cpt);
    let summary_tokens = tokens_from_chars(summary.len(), cpt);
    let kept_tokens = if cut.compact_all {
        0
    } else {
        tokens_from_chars(total_chars_of(&live[cut.first_kept_index..]), cpt)
    };
    let tokens_before_est = summarized_tokens + kept_tokens;
    let tokens_after_est = summary_tokens + kept_tokens;
    let savings_pct = if tokens_before_est > 0 {
        (1.0 - tokens_after_est as f64 / tokens_before_est as f64) * 100.0
    } else {
        0.0
    };

    let (uc_encodes, uc_hits) = uc.cache_stats();
    let details = serde_json::json!({
        "compactor": "ultracompress",
        "version": env!("CARGO_PKG_VERSION"),
        "policy": input.policy,
        "engines": { "uc": mix.uc && uc_status.available, "snap": mix.snap },
        "sections": section_names(&new_sections, &archived, &critical, &transcript),
        "sourceMessageCount": summarized.len(),
        "previousSummaryUsed": prev.is_some(),
        "transcriptLines": transcript.lines.len(),
        "transcriptOmitted": transcript.omitted_lines,
        "ucCache": { "encodes": uc_encodes, "hits": uc_hits },
        "budgetCut": budget_cut,
        "explicitKeep": explicit,
    });

    Ok(CompactResult {
        summary,
        first_kept_entry_id,
        decisions,
        details,
        stats: CompactStats {
            tokens_before_est,
            tokens_after_est,
            savings_pct,
            summarized_messages: summarized.len(),
            kept_messages: live.len().saturating_sub(cut.summarize_end),
            keep_user_turns_resolved: keep,
            smart_keep_adjusted: smart_adjusted,
            uc_blocks,
            uc_tokens_saved,
            snap_blocks: 0,
            snap_frames: 0,
            snap_chars_archived: 0,
            chars_per_token: cpt,
            calibrated: est.calibrated,
        },
        uc_status,
        policy: input.policy,
        dry_run: input.dry_run,
    })
}

fn total_chars_of(msgs: &[RcMessage]) -> usize {
    msgs.iter().map(|m| m.total_chars()).sum()
}

fn section_names(
    s: &Sections,
    archived: &[String],
    critical: &[String],
    t: &crate::transcript::Transcript,
) -> Vec<String> {
    let mut v = vec![];
    if !s.goal.is_empty() {
        v.push("Session Goal");
    }
    if !(s.files_modified.is_empty() && s.files_created.is_empty() && s.files_read.is_empty()) {
        v.push("Files And Changes");
    }
    if !s.commits.is_empty() {
        v.push("Commits");
    }
    if !s.key_facts.is_empty() {
        v.push("Key Facts");
    }
    if !critical.is_empty() {
        v.push("Critical Context");
    }
    if !s.outstanding.is_empty() {
        v.push("Outstanding Context");
    }
    if !s.preferences.is_empty() {
        v.push("User Preferences");
    }
    if !archived.is_empty() {
        v.push("Archived");
    }
    if !t.lines.is_empty() {
        v.push("Transcript");
    }
    v.into_iter().map(String::from).collect()
}

fn human_chars(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, parent: &str, role: &str, text: &str) -> serde_json::Value {
        json!({
            "type": "message",
            "id": id,
            "parentId": parent,
            "message": { "role": role, "content": [{ "type": "text", "text": text }] }
        })
    }

    fn tool_entry(
        id: &str,
        parent: &str,
        tool: &str,
        call_id: &str,
        text: &str,
    ) -> serde_json::Value {
        json!({
            "type": "message",
            "id": id,
            "parentId": parent,
            "message": { "role": "toolResult", "content": [{ "type": "toolResult", "toolCallId": call_id, "toolName": tool, "content": text }] }
        })
    }

    fn assistant_call(id: &str, parent: &str, call_id: &str) -> serde_json::Value {
        json!({
            "type": "message",
            "id": id,
            "parentId": parent,
            "message": { "role": "assistant", "content": [{ "type": "toolCall", "id": call_id, "name": "bash", "arguments": { "command": "cargo test" } }] }
        })
    }

    fn big_json() -> String {
        serde_json::to_string(&json!({
            "records": (0..80).map(|i| json!({"id": i, "name": format!("item-{i}"), "tags": ["a","b","c"], "score": i * 7})).collect::<Vec<_>>()
        }))
        .unwrap()
    }

    fn big_text() -> String {
        (0..400)
            .map(|i| format!("build line {i}: compiling crate number {i} with warnings galore"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample_entries() -> Vec<serde_json::Value> {
        vec![
            entry(
                "e1",
                null_parent(),
                "user",
                "Fix the build and run all tests. Always run clippy after fixes.",
            ),
            assistant_call("e2", "e1", "c1"),
            tool_entry("e3", "e2", "bash", "c1", &big_json()),
            assistant_call("e4", "e3", "c2"),
            tool_entry("e5", "e4", "bash", "c2", &big_text()),
            entry(
                "e6",
                "e5",
                "assistant",
                "Fixed two failing modules; rerunning.",
            ),
            entry("e7", "e6", "user", "now check coverage"),
            entry("e8", "e7", "assistant", "Coverage is at 84%."),
        ]
    }

    fn null_parent() -> &'static str {
        "p0"
    }

    #[test]
    fn compacts_with_auto_policy() {
        let input = CompactInput {
            entries: sample_entries(),
            tokens_before: Some(30_000),
            previous_summary: None,
            policy: Policy::Auto,
            keep_user_turns: Some(1),
            smart_keep_tail: false,
            vision: VisionMode::On,
            model_vision: None,
            uc_bin: Some("rc-definitely-not-a-binary".into()),
            uc_enabled: true,
            thresholds: None,
            snap: Some(SnapConfig {
                cols: 100,
                rows: 30,
                head_chars: 200,
                tail_chars: 100,
                scale: 1,
            }),
            transcript: None,
            image_tokens_per_frame: None,
            dry_run: false,
        };
        let r = run(&input).unwrap();
        assert_eq!(r.first_kept_entry_id, "e7");
        assert!(r.summary.contains("[Session Goal]"));
        assert!(r.summary.contains("Fix the build"));
        assert!(r.summary.contains("Always run clippy"));
        // Snap never participates in compaction — frames live in the
        // transform (live-context) path.
        assert_eq!(r.stats.snap_blocks, 0);
        assert!(!r.dry_run);
    }

    #[test]
    fn compact_all_when_keep_zero() {
        let input = CompactInput {
            entries: sample_entries(),
            tokens_before: None,
            previous_summary: None,
            policy: Policy::Vcc,
            keep_user_turns: Some(0),
            smart_keep_tail: false,
            vision: VisionMode::Off,
            model_vision: None,
            uc_bin: None,
            uc_enabled: false,
            thresholds: None,
            snap: None,
            transcript: None,
            image_tokens_per_frame: None,
            dry_run: true,
        };
        let r = run(&input).unwrap();
        assert_eq!(r.first_kept_entry_id, "");
        assert!(r.stats.kept_messages == 0);
    }

    #[test]
    fn honors_prior_compaction_boundary() {
        let mut entries = vec![
            entry("e0", null_parent(), "user", "old stuff"),
            json!({ "type": "compaction", "id": "cp1", "parentId": "e0", "summary": "old", "firstKeptEntryId": "e1" }),
        ];
        entries.extend(sample_entries().into_iter().skip(1));
        let (live, ids) = collect_live(&entries);
        assert_eq!(live.len(), 7);
        assert_eq!(ids[0], "e2");
    }

    #[test]
    fn smart_keep_boosts_small_tails() {
        let live: Vec<RcMessage> = (0..6)
            .map(|i| RcMessage {
                id: format!("u{i}"),
                role: crate::model::Role::User,
                content: vec![Block::Text {
                    text: "tiny".into(),
                }],
                timestamp: None,
            })
            .collect();
        let (keep, adjusted) = resolve_smart_keep(&live, None, true, 4.0, 5_000, 25_000);
        assert!(adjusted);
        // keep == total turns would mean compact-all (nothing kept), so the
        // boost caps at total - 1.
        assert_eq!(keep, 5);
    }

    #[test]
    fn vcc_policy_disables_engines() {
        let input = CompactInput {
            entries: sample_entries(),
            tokens_before: None,
            previous_summary: None,
            policy: Policy::Vcc,
            keep_user_turns: Some(1),
            smart_keep_tail: false,
            vision: VisionMode::On,
            model_vision: None,
            uc_bin: None,
            uc_enabled: true,
            thresholds: None,
            snap: None,
            transcript: None,
            image_tokens_per_frame: None,
            dry_run: true,
        };
        let r = run(&input).unwrap();
        assert_eq!(r.stats.snap_blocks, 0);
        assert_eq!(r.stats.uc_blocks, 0);
        assert!(r.summary.contains("[Transcript]"));
    }

    #[test]
    fn merge_uses_previous_summary() {
        let prev = "[Session Goal]\n- Fix the build and run all tests\n\n[Files And Changes]\n- Modified: src/old.ts\n";
        let input = CompactInput {
            entries: sample_entries(),
            tokens_before: None,
            previous_summary: Some(prev.into()),
            policy: Policy::Vcc,
            keep_user_turns: Some(1),
            smart_keep_tail: false,
            vision: VisionMode::Off,
            model_vision: None,
            uc_bin: None,
            uc_enabled: false,
            thresholds: None,
            snap: None,
            transcript: None,
            image_tokens_per_frame: None,
            dry_run: true,
        };
        let r = run(&input).unwrap();
        assert!(
            r.summary.contains("src/old.ts"),
            "previous files should persist across merge"
        );
        assert!(r.details["previousSummaryUsed"].as_bool().unwrap());
    }

    #[test]
    fn single_user_turn_splits_at_budget() {
        // One user prompt, then a long agentic loop (the bench shape): pi
        // calls this a split turn — we must cut inside it, not refuse.
        let mut entries = vec![entry("b0", null_parent(), "user", "do the whole task now")];
        let mut parent = "b0".to_string();
        for i in 0..40 {
            let id = format!("b{}", i + 1);
            if i % 2 == 0 {
                entries.push(assistant_call(&id, &parent, &format!("c{i}")));
            } else {
                entries.push(tool_entry(
                    &id,
                    &parent,
                    "bash",
                    &format!("c{}", i - 1),
                    &big_text(),
                ));
            }
            parent = id;
        }
        let input = CompactInput {
            entries,
            tokens_before: Some(120_000),
            previous_summary: None,
            policy: Policy::Auto,
            keep_user_turns: Some(1),
            smart_keep_tail: false,
            vision: VisionMode::Off,
            model_vision: None,
            uc_bin: None,
            uc_enabled: false,
            thresholds: None,
            snap: None,
            transcript: None,
            image_tokens_per_frame: None,
            dry_run: true,
        };
        let r = run(&input).unwrap();
        assert!(
            r.stats.summarized_messages > 0,
            "split turn must summarize a prefix"
        );
        assert!(r.stats.kept_messages > 0, "split turn must keep a tail");
        assert_ne!(r.first_kept_entry_id, "");
    }

    #[test]
    fn dry_run_matches_full_run() {
        let make = |dry: bool| CompactInput {
            entries: sample_entries(),
            tokens_before: None,
            previous_summary: None,
            policy: Policy::Auto,
            keep_user_turns: Some(1),
            smart_keep_tail: false,
            vision: VisionMode::On,
            model_vision: None,
            uc_bin: None,
            uc_enabled: false,
            thresholds: None,
            snap: None,
            transcript: None,
            image_tokens_per_frame: None,
            dry_run: dry,
        };
        let dry = run(&make(true)).unwrap();
        let full = run(&make(false)).unwrap();
        assert!(dry.dry_run && !full.dry_run);
        assert_eq!(dry.summary, full.summary, "compaction is deterministic");
        assert_eq!(dry.stats.savings_pct, full.stats.savings_pct);
    }

    #[test]
    fn truncates_huge_goal_safely() {
        let huge = "x".repeat(10_000);
        let entries = vec![
            entry("a1", null_parent(), "user", &huge),
            entry("a2", "a1", "assistant", "ok"),
            entry("a3", "a2", "user", "next"),
            entry("a4", "a3", "assistant", "done"),
        ];
        let input = CompactInput {
            entries,
            tokens_before: None,
            previous_summary: None,
            policy: Policy::Vcc,
            keep_user_turns: Some(1),
            smart_keep_tail: false,
            vision: VisionMode::Off,
            model_vision: None,
            uc_bin: None,
            uc_enabled: false,
            thresholds: None,
            snap: None,
            transcript: None,
            image_tokens_per_frame: None,
            dry_run: true,
        };
        let r = run(&input).unwrap();
        assert!(r.summary.len() < huge.len());
    }
}
