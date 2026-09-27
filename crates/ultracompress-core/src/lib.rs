//! UltraCompress core — content-aware conversation compaction.
//!
//! VCC briefs and snap frames keep context manageable; the raw session stays
//! on disk, and `recall` keeps every dropped byte reachable.
pub mod classify;
pub mod compact;
pub mod estimate;
pub mod format;
pub mod load;
pub mod model;
pub mod policy;
pub mod recall;
pub(crate) mod recall_claude;
pub(crate) mod recall_load;
pub mod sections;
pub mod snap;
pub mod transcript;
pub mod transform;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
