# Changelog

## Extension 0.2.1 — streaming snapshots and stdin on oversized sessions

- Pre-compaction snapshots are written by streaming JSON to disk entry by
  entry (`writeSnapEntries`). Serializing the whole session with one
  `JSON.stringify` threw `RangeError: Invalid string length` once the
  payload crossed V8's ~512 MiB max string length, silently disabling the
  safety net on very large sessions. A single entry beyond the limit is
  replaced by a placeholder so the snap file stays valid JSON; partial
  files are removed on failure.
- Compaction stdin is streamed to the `uc` process leaf by leaf
  (`streamJsonTo`) with the same root cause fixed; a failing serialization
  still fails with the `stdin write failed:` prefix instead of crashing,
  and an early child exit can no longer surface EPIPE as an unhandled
  stdin error. Child stdout/stderr accumulation is capped so parse-side
  string limits cannot be hit either.
- The streamed stdin writer follows the JSON.stringify serialization
  algorithm: toJSON invoked exactly once with the property key, property
  values read lazily so hooks can mutate the parent, callable objects
  honor their toJSON before omission rules, boxed Number/String/Boolean
  (and subclasses) unbox via the prototype chain, Symbol.toStringTag
  cannot spoof wrapper detection, bigints throw TypeError like native.
- Regression coverage: payloads and snapshots beyond V8's max string
  length round-trip byte-identically to `JSON.stringify` output, plus a
  seeded differential fuzz against native JSON.stringify and adversarial
  toJSON/mutation/wrapper/Symbol.toPrimitive cases.
- `uc` binary unchanged at 0.2.0.

## 0.2.0 — explicit session scope and bounded recall

- Recall uses the actual current branch tip; all-branch search stays inside one
  selected session, and another session requires an explicit file path.
- Role/tool and exclusive entry-range filters narrow search before ranking.
- Bounded pages, UTF-8 excerpts and complete result JSON budgets; invalid
  selectors fail closed instead of implicitly widening.
- Fresh explicit reads are not archived before the model can first consume
  them, avoiding an immediate archive/retrieve round trip. Older reads remain
  eligible. No universal whole-session token saving is claimed.
- CLI/tool/command propagation and synthetic branch, isolation, Unicode,
  pagination and budget regression coverage.

## 0.1.2 — reference-aware retrieval and measured token accounting

- Plain-text envelopes now require `UC_TEXT_ENVELOPES=1` from a
  reference-aware caller. Older extensions retain their previous JSON-only
  behavior; non-winning JSON is never retried as an envelope.
- Measure original text and the complete emitted packet plus stub with
  o200k before accepting a live transform. Labels no longer describe byte
  counts as tokens. These are tokenizer measurements, not provider billing
  or end-to-end savings after retrieval.
- Dense packets are not embedded in compaction summaries for models to
  transcribe. Archive notes direct readers to the original raw history via
  recall and explicitly say when no packet is present.
- The standalone Pi adapter and Steak Pi 0.3.5 retrieve archived live output
  by a bounded session-local reference and exempt decode/recall tool results
  from recompression. Provider image payloads are no longer rewritten.
- The standalone adapter is version-aligned at 0.1.2 and tested against Pi
  0.85.1. README savings and raw-history claims distinguish measured fixture
  results from general guarantees.

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
- Pi extension: compaction hook, live context transforms, `/ultracompress` ·
  `/ultracompress-recall` · `/ultracompress-stats` · `/snaps` commands,
  `ultracompress_recall` + `ultracompress_uc` tools, pre-compaction
  snapshots, z.ai payload adapter, fallback-first failure posture
- Benchmarks: offline (9 real sessions), recall quality (72 facts), live
  three-stack comparison vs stock Pi and OMP snapcompact
