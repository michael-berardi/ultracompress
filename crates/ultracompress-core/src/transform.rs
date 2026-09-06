//! Live context transforms: per-block routing of oversized tool results to
//! UC packets or snap frames. This is the path that saves real tokens every
//! LLM call — the compaction summary is text-only, so frames only pay off
//! when they replace text that would otherwise sit in the live window.

use crate::classify::Thresholds;
use crate::model::{parse_messages, Block, RcMessage};
use crate::policy::{engine_mix, resolve_block, Policy, VisionMode};
use crate::snap::{plan_snap, render_frames, SnapConfig, SnapResult};
use crate::ucbridge::UcBridge;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformInput {
    /// Message array (raw Pi entries or converted messages).
    pub messages: Vec<serde_json::Value>,
    #[serde(default = "default_policy")]
    pub policy: Policy,
    #[serde(default)]
    pub vision: VisionMode,
    #[serde(default)]
    pub model_vision: Option<bool>,
    #[serde(default)]
    pub uc_bin: Option<String>,
    #[serde(default = "default_true")]
    pub uc_enabled: bool,
    #[serde(default)]
    pub thresholds: Option<Thresholds>,
    #[serde(default)]
    pub snap: Option<SnapConfig>,
    #[serde(default)]
    pub image_tokens_per_frame: Option<u64>,
    /// Calibrated chars/token from the session; heuristic when absent.
    #[serde(default)]
    pub chars_per_token: Option<f64>,
}

fn default_policy() -> Policy {
    Policy::Auto
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TransformOp {
    /// Replace one text block with a UC packet + stub.
    Uc {
        message_index: usize,
        block_index: usize,
        stub: String,
        packet: String,
        tokens_before: u64,
        tokens_after: u64,
    },
    /// Replace one text block with text edges + PNG frames.
    Snap {
        message_index: usize,
        block_index: usize,
        head: String,
        tail: String,
        frames: Vec<FrameOut>,
        tokens_before: u64,
        tokens_after: u64,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameOut {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub png_base64: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransformStats {
    pub blocks_scanned: usize,
    pub uc_ops: usize,
    pub snap_ops: usize,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub savings_pct: f64,
    pub uc_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransformResult {
    pub ops: Vec<TransformOp>,
    pub stats: TransformStats,
    pub uc_status: crate::ucbridge::UcStatus,
}

const DEFAULT_CPT: f64 = 3.8;

pub fn run(input: &TransformInput) -> Result<TransformResult, String> {
    let msgs: Vec<RcMessage> = parse_messages(&serde_json::Value::Array(input.messages.clone()));
    let cpt = input.chars_per_token.unwrap_or(DEFAULT_CPT);

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
    let snap_cfg = input.snap.clone().unwrap_or_default();

    let mut ops = Vec::new();
    let mut blocks_scanned = 0usize;
    let mut tokens_before = 0u64;
    let mut tokens_after = 0u64;

    for (mi, m) in msgs.iter().enumerate() {
        for (bi, b) in m.content.iter().enumerate() {
            let text = match b {
                Block::ToolResult { text, .. } => text,
                Block::Text { text } if m.role == crate::model::Role::ToolResult => text,
                _ => continue,
            };
            if text.len() < th.uc_min_chars {
                continue;
            }
            blocks_scanned += 1;
            let engine = resolve_block(
                text,
                &mix,
                &th,
                &snap_cfg,
                input.image_tokens_per_frame,
                cpt,
            );
            match engine {
                crate::classify::Engine::Uc => {
                    if let Some(packet) = uc.encode_json(text) {
                        let before = packet.tokens_source;
                        let stub = if packet.envelope {
                            format!(
                                "[UC packet: text payload in JSON envelope (key \"t\"), {} → {} o200k tokens (packet only), -{:.0}%; decode via ultracompress_uc decode, then use the \"t\" value]",
                                packet.tokens_source, packet.tokens_uc, packet.savings_pct
                            )
                        } else {
                            format!(
                                "[UC packet: JSON payload, {} → {} o200k tokens (packet only), -{:.0}%; decode via ultracompress_uc decode]",
                                packet.tokens_source, packet.tokens_uc, packet.savings_pct
                            )
                        };
                        let Some(after) = uc.count_tokens(&format!("{stub}\n\n{}", packet.packet))
                        else {
                            continue;
                        };
                        if after >= before {
                            continue;
                        }
                        tokens_before += before;
                        tokens_after += after;
                        ops.push(TransformOp::Uc {
                            message_index: mi,
                            block_index: bi,
                            stub,
                            packet: packet.packet.clone(),
                            tokens_before: before,
                            tokens_after: after,
                        });
                    }
                }
                crate::classify::Engine::Snap => {
                    let plan = plan_snap(text, &snap_cfg, input.image_tokens_per_frame, cpt);
                    if !plan.worthwhile {
                        continue;
                    }
                    let SnapResult {
                        frames, head, tail, ..
                    } = render_frames(text, "tool output", &snap_cfg);
                    let per_frame = crate::estimate::frame_tokens(
                        (plan.cols as u32) * 8 + 8,
                        (plan.rows as u32) * 8 + 8,
                        input.image_tokens_per_frame,
                    );
                    let before = crate::estimate::tokens_from_chars(text.len(), cpt);
                    let after = (frames.len() as u64) * per_frame
                        + crate::estimate::tokens_from_chars(head.len() + tail.len(), cpt);
                    tokens_before += before;
                    tokens_after += after;
                    ops.push(TransformOp::Snap {
                        message_index: mi,
                        block_index: bi,
                        head,
                        tail,
                        frames: frames
                            .into_iter()
                            .map(|f| FrameOut {
                                id: f.id,
                                width: f.width,
                                height: f.height,
                                png_base64: f.png_base64,
                            })
                            .collect(),
                        tokens_before: before,
                        tokens_after: after,
                    });
                }
                _ => {}
            }
        }
    }

    let savings_pct = if tokens_before > 0 {
        (1.0 - tokens_after as f64 / tokens_before as f64) * 100.0
    } else {
        0.0
    };
    let uc_ops = ops
        .iter()
        .filter(|o| matches!(o, TransformOp::Uc { .. }))
        .count();
    let snap_ops = ops
        .iter()
        .filter(|o| matches!(o, TransformOp::Snap { .. }))
        .count();

    Ok(TransformResult {
        ops,
        stats: TransformStats {
            blocks_scanned,
            uc_ops,
            snap_ops,
            tokens_before,
            tokens_after,
            savings_pct,
            uc_available: uc_status.available,
        },
        uc_status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn big_text() -> String {
        (0..400)
            .map(|i| format!("build line {i}: compiling crate number {i} with warnings"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn transforms_oversized_text_into_frames() {
        let input = TransformInput {
            messages: vec![json!({
                "type": "message", "id": "e1",
                "message": { "role": "toolResult", "toolName": "bash", "toolCallId": "c1",
                    "content": [{ "type": "text", "text": big_text() }] }
            })],
            policy: Policy::Auto,
            vision: VisionMode::On,
            model_vision: None,
            uc_bin: None,
            uc_enabled: false,
            thresholds: None,
            snap: None,
            image_tokens_per_frame: None,
            chars_per_token: None,
        };
        let r = run(&input).unwrap();
        assert_eq!(r.stats.snap_ops, 1);
        assert!(
            r.stats.savings_pct > 25.0,
            "savings: {}",
            r.stats.savings_pct
        );
        match &r.ops[0] {
            TransformOp::Snap { frames, head, .. } => {
                assert!(!frames.is_empty());
                assert!(frames[0].png_base64.len() > 100);
                assert!(!head.is_empty());
            }
            other => panic!("expected snap op, got {other:?}"),
        }
    }

    #[test]
    fn no_transforms_without_vision() {
        let input = TransformInput {
            messages: vec![json!({
                "type": "message", "id": "e1",
                "message": { "role": "toolResult", "toolName": "bash", "toolCallId": "c1",
                    "content": [{ "type": "text", "text": big_text() }] }
            })],
            policy: Policy::Auto,
            vision: VisionMode::Off,
            model_vision: None,
            uc_bin: None,
            uc_enabled: false,
            thresholds: None,
            snap: None,
            image_tokens_per_frame: None,
            chars_per_token: None,
        };
        let r = run(&input).unwrap();
        assert_eq!(r.stats.snap_ops, 0);
        assert_eq!(r.ops.len(), 0);
    }
}
