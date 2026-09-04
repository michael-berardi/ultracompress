# Changelog

## 0.1.0 — initial release

- VCC engine: deterministic section briefs (goal, files & changes, commits,
  key facts, outstanding context, preferences) + rolling transcript, bounded
  sticky merge across successive compactions
- Snap engine: content-adaptive PNG frames (p90 line-length shapes), line-aware
  worthwhileness economics (≥25% block savings required), deterministic bytes
- UC engine: bridge to the optional UltraCompact binary — lossless JSON packet
  encoding, hash-cached, never-worse guarantee, graceful when absent
- Policy engine: per-block routing (auto/vcc/snap/uc), vision provider gating,
  smart keep-tail, split-turn budget cuts, oversized-tail rescue
- Lossless recall: ranked OR/regex search over raw session JSONL (~10 ms)
- Pi extension: compaction hook, live context transforms, /rc · /rc-recall ·
  /rc-stats · /snaps commands, rc_recall + rc_uc tools, pre-compaction
  snapshots, z.ai payload adapter, fallback-first failure posture
- Benchmarks: offline (9 real sessions), recall quality (72 facts), live
  three-stack comparison vs stock Pi and OMP snapcompact
