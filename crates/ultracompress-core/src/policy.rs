//! The policy engine: which engine runs, and why.
//!
//! UltraCompress's core premise is that different content has different
//! cheapest lossy-to-the-model-but-lossless-to-disk representation:
//!
//! - JSON payloads  → UC packets when measured savings clear the margin
//! - Huge text      → snap frames when vision allows; otherwise opted-in UC
//!   envelopes, with plain text wrapped as {"t": …} for exact retrieval
//! - Conversation   → VCC sections + brief transcript (deterministic text)
//! - Recent turns   → kept verbatim (lossless tail)
//!
//! `auto` picks per block. Fixed policies pin the mix for A/B comparison.

use crate::classify::{classify_content, ContentClass, Engine, Thresholds};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Auto,
    Vcc,
    Snap,
    Uc,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s.to_lowercase().as_str() {
            "auto" => Some(Policy::Auto),
            "vcc" | "vcc-only" => Some(Policy::Vcc),
            "snap" | "snapcompact" | "snap-compact" => Some(Policy::Snap),
            "uc" | "ultracompact" => Some(Policy::Uc),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VisionMode {
    #[default]
    Auto,
    On,
    Off,
}

impl VisionMode {
    pub fn resolves(self, model_reports_vision: Option<bool>) -> bool {
        match self {
            VisionMode::On => true,
            VisionMode::Off => false,
            VisionMode::Auto => model_reports_vision.unwrap_or(false),
        }
    }
}

/// Effective engine switches derived from policy + environment.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct EngineMix {
    pub uc: bool,
    pub snap: bool,
}

pub fn engine_mix(policy: Policy, uc_available: bool, vision: bool) -> EngineMix {
    match policy {
        Policy::Auto => EngineMix {
            uc: uc_available,
            snap: vision,
        },
        Policy::Vcc => EngineMix {
            uc: false,
            snap: false,
        },
        Policy::Snap => EngineMix {
            uc: false,
            snap: vision,
        },
        Policy::Uc => EngineMix {
            uc: uc_available,
            snap: false,
        },
    }
}

/// Routing decision for one tool-result block, resolved through the mix.
/// Snap routing runs the full line-aware economics (adaptive shape, frame
/// count, ≥25% margin) before committing — marginal blocks stay text.
pub fn resolve_block(
    text: &str,
    mix: &EngineMix,
    th: &Thresholds,
    snap_cfg: &crate::snap::SnapConfig,
    image_tokens_per_frame: Option<u64>,
    cpt: f64,
) -> Engine {
    let class = classify_content(text);
    match class {
        ContentClass::Empty | ContentClass::Opaque => Engine::None,
        ContentClass::Json => {
            if mix.uc && text.len() >= th.uc_min_chars {
                Engine::Uc
            } else {
                Engine::None
            }
        }
        ContentClass::Text => {
            // Snap keeps priority when it is enabled and worthwhile (fixed,
            // model-readable cost). Otherwise oversized text falls to UC:
            // since 0.1.1 the bridge envelopes non-JSON text as {"t": …},
            // and the bridge's never-worse check makes a failed attempt
            // harmless (the block just stays verbatim).
            if mix.snap && text.len() >= th.snap_min_chars {
                let plan = crate::snap::plan_snap(text, snap_cfg, image_tokens_per_frame, cpt);
                if plan.worthwhile {
                    return Engine::Snap;
                }
            }
            if mix.uc && text.len() >= th.uc_min_chars {
                return Engine::Uc;
            }
            Engine::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_enables_both_when_environment_allows() {
        let mix = engine_mix(Policy::Auto, true, true);
        assert!(mix.uc && mix.snap);
        let mix = engine_mix(Policy::Auto, false, false);
        assert!(!mix.uc && !mix.snap);
    }

    #[test]
    fn pinned_policies_disable_other_engines() {
        let mix = engine_mix(Policy::Vcc, true, true);
        assert!(!mix.uc && !mix.snap);
        let mix = engine_mix(Policy::Uc, true, true);
        assert!(mix.uc && !mix.snap);
    }

    #[test]
    fn vision_auto_requires_model_support() {
        assert!(!VisionMode::Auto.resolves(None));
        assert!(VisionMode::Auto.resolves(Some(true)));
        assert!(!VisionMode::Auto.resolves(Some(false)));
        assert!(VisionMode::On.resolves(Some(false)));
        assert!(!VisionMode::Off.resolves(Some(true)));
    }
}
