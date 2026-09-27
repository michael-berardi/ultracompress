//! The policy engine: which engine runs, and why.
//!
use crate::classify::{classify_content, ContentClass, Engine, Thresholds};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Auto,
    Vcc,
    Snap,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s.to_lowercase().as_str() {
            "auto" => Some(Policy::Auto),
            "vcc" | "vcc-only" => Some(Policy::Vcc),
            "snap" | "snapcompact" | "snap-compact" => Some(Policy::Snap),
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
    pub snap: bool,
}

pub fn engine_mix(policy: Policy, vision: bool) -> EngineMix {
    match policy {
        Policy::Auto => EngineMix { snap: vision },
        Policy::Vcc => EngineMix { snap: false },
        Policy::Snap => EngineMix { snap: vision },
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
        ContentClass::Json => Engine::None,
        ContentClass::Text => {
            if mix.snap && text.len() >= th.snap_min_chars {
                let plan = crate::snap::plan_snap(text, snap_cfg, image_tokens_per_frame, cpt);
                if plan.worthwhile {
                    return Engine::Snap;
                }
            }
            Engine::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_enables_snap_when_vision_allows() {
        let mix = engine_mix(Policy::Auto, true);
        assert!(mix.snap);
        let mix = engine_mix(Policy::Auto, false);
        assert!(!mix.snap);
    }

    #[test]
    fn pinned_policies_disable_other_engines() {
        let mix = engine_mix(Policy::Vcc, true);
        assert!(!mix.snap);
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
