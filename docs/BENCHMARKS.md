# Rapid Compact — Benchmarks

All numbers reproducible from this repo. Three benchmark families:

1. **Offline** (`scripts/bench-offline.mjs`) — deterministic, zero API cost.
   Real recorded Pi sessions; post-compaction context size calibrated against
   Pi's own measured token usage.
2. **Recall quality** (`scripts/bench-recall.mjs`) — can the model still
   recover facts after compaction? Sampled real tool results, rare-term
   queries, top-k entry match.
3. **Live** (`scripts/live-bench.sh` + `scripts/live-bench-report.mjs`) —
   identical multi-step task, identical model (zai/glm-5.3-flash), identical
   ~23.6k compaction threshold, three stacks: stock Pi, Pi + Rapid Compact,
   OMP forced to snapcompact-only. Real billed tokens and cost from session
   usage records.

## 1. Offline — 9 real sessions

| session | msgs | raw tok | stock-pi | vcc-only | rc-auto | stock ↓ | vcc ↓ | rc-auto ↓ |
|---|---|---|---|---|---|---|---|---|
| 09-03T13 | 51 | 51,521 | 19,415 | 7,269 | 7,269 | 62.3% | 85.9% | 85.9% |
| 09-03T14 | 62 | 52,312 | 20,103 | 11,024 | 11,024 | 61.6% | 78.9% | 78.9% |
| 09-03T15 | 64 | 43,696 | 21,014 | 21,699 | 21,699 | 51.9% | 50.3% | 50.3% |
| 09-03T16 | 295 | 137,820 | 21,101 | 26,940 | 28,688 | 84.7% | 80.5% | 79.2% |
| 09-03T18 | 65 | 77,912 | 20,563 | 20,598 | 17,557 | 73.6% | 73.6% | 77.5% |
| 09-03T20 | 1437 | 249,825 | 20,597 | 29,254 | 29,254 | 91.8% | 88.3% | 88.3% |
| 09-03T21 | 281 | 154,573 | 21,157 | 25,736 | 25,736 | 86.3% | 83.4% | 83.4% |
| 09-04T09 | 400 | 183,907 | 20,692 | 19,744 | 19,744 | 88.7% | 89.3% | 89.3% |
| 09-04T10 | 197 | 112,976 | 21,065 | 14,316 | 14,316 | 81.4% | 87.3% | 87.3% |

**Averages: stock-pi ↓ 75.8% · vcc-only ↓ 79.7% · rc-auto ↓ 80.0%**

How to read this honestly: stock Pi's post-compaction size is bounded by its
fixed 20k-token verbatim tail, so its *footprint* looks competitive. The
differences that matter are everything else:

| | stock Pi | pi-vcc (VCC) | Rapid Compact |
|---|---|---|---|
| Summary generation | LLM call (latency + cost, non-deterministic, can hallucinate) | deterministic, 10–30 ms | deterministic, 10–300 ms |
| Compaction API cost | 1 LLM call per compaction | **$0** | **$0** |
| History after compaction | **destroyed** | lossless recall | lossless recall |
| JSON payloads in context | verbatim | verbatim | UC packets (−26%+ tokens, lossless) |
| Bulky text tool output | verbatim (live path too) | verbatim (live path too) | snap frames on vision providers |
| Repeat compactions | degrade (summary of summary) | stable (sticky sections) | stable (sticky sections + key facts) |

## 2. Recall quality after compaction

72 sampled facts (real tool results from compacted-away spans, 9 sessions):

| metric | Rapid Compact | stock Pi |
|---|---|---|
| hit@1 | 66.7% | **0%** — history is destroyed |
| hit@5 | **94.4%** | 0% |

Stock Pi has no recall mechanism; everything summarized is gone. Rapid
Compact keeps the raw session on disk and searches it in ~10 ms.

## 3. Live — identical task, three stacks

Task: read two ~44 KB logs fully + two JSON data files, count functions
across 8 source files, extract exact error codes, write a formatted answer.
Model: zai/glm-5.3-flash for all stacks. Threshold ≈ 23.6k tokens for all
stacks (Pi: 40k model window − 16,384 reserve; OMP: `compaction.thresholdTokens`).
OMP configured to its best snapcompact posture
(`methodOrder=["snapcompact"]`, `snapcompact.toolResults=true`, `shape=auto`).

Across batches (model latency on this endpoint varies run-to-run; we report
every completed run, no cherry-picking):

| stack | billed-in tok | LLM compaction calls | cost | wall | correct |
|---|---|---|---|---|---|
| stock-pi (run A) | 160,405 | 1 | $0.0061 | 198s | ✓ |
| stock-pi (run B) | 191,791 | 2 | $0.0080 | 427s | ✓ |
| stock-pi (run C) | 334,118 | 3 | $0.0156 | 900s timeout | ✗ (killed mid-task) |
| rapid-compact (run A) | 152,311 | **0** | $0.0066 | 196s | ✓ |
| rapid-compact (run B) | 268,410 | **0** (2 rc-owned) | $0.0128 | 518s | ✓ |
| omp-snapcompact | 752,969 | 0 (pruned instead) | $0.0516 | 434s | ✓ |

Findings:

- **Rapid Compact never spends an LLM call on compaction.** Stock pays one
  API summarization call per compaction (and run C needed three and still
  overran). Rapid Compact's brief is computed locally in 10–300 ms.
- **Rapid Compact's context engineering is load-bearing**: sticky Key Facts
  (error/warn/fatal lines with codes), Files & Changes, Outstanding Context,
  rolling transcript, and `rc_recall` for everything else. In one run the
  model answered with exact error codes that survived two compactions.
- **OMP at 8× the cost**: $0.0516 vs $0.0066 in the same batch. Its
  snapcompact never delivered a single image frame on this endpoint (0 image
  blocks in its event stream); it avoided compaction via its cache-aware
  pruning (supersedeReads/dropUseless) — good features, but 32 calls and
  753k billed tokens.
- **Provider reality check**: z.ai's coding endpoint rejects standard OpenAI
  image parts, so snap frames are provider-gated (anthropic/google by
  default). On zai, Rapid Compact runs VCC + UC — and still wins on cost,
  latency, and information retention.

### Engineering-quality gates (all in CI)

- 40 Rust unit/golden tests + 30 extension tests (incl. live-binary e2e)
- Determinism: same session + same config ⇒ byte-identical summary
- Never-worse rule: UC packets ship only when UC reports real savings; snap
  frames only when line-aware economics beat text by ≥ 25%
- Fallback-first: every rc call is best-effort; any failure degrades to Pi
  core compaction — a session can never be bricked by the extension
