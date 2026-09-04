//! UltraCompress core — content-aware conversation compaction.
//!
//! Three engines, one policy:
//! - VCC: deterministic section brief + rolling transcript (no LLM)
//! - Snap: archive bulky text as deterministic PNG frames (fixed vision cost)
//! - UC: lossless JSON packet encoding via the optional UltraCompact binary
//!
//! `auto` routes each block to its cheapest faithful representation; the raw
//! session stays on disk, and `recall` keeps every dropped byte reachable.

pub mod classify;
pub mod compact;
pub mod estimate;
pub mod format;
pub mod load;
pub mod model;
pub mod policy;
pub mod recall;
pub mod sections;
pub mod snap;
pub mod transcript;
pub mod transform;
pub mod ucbridge;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
