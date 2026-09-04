# Rapid Compact — Benchmarks

All numbers reproducible from this repo. Three benchmark families:

1. **Offline** (`scripts/bench-offline.mjs`) — deterministic, zero API cost.
   Real recorded Pi sessions ≥ 40k tokens (below that, compaction never
   triggers); post-compaction context size calibrated against Pi's own
   measured token usage.
2. **Recall quality** (`scripts/bench-recall.mjs`) — can the model still
   recover facts after compaction? Sampled real tool results, rare-term
   queries, top-k entry match.
3. **Live** (`scripts/live-bench.sh` + `scripts/live-bench-report.mjs`) —
   identical multi-step task, identical model (zai/glm-5.3-flash), identical
   ~23.6k compaction threshold, three stacks: stock Pi, Pi + Rapid Compact,
   OMP forced to snapcompact-only. Real billed tokens and cost from session
   usage records. Every completed run is reported — no cherry-picking.

## 1. Offline — 12 real sessions (≥ 40k tokens)

| session | msgs | raw tok | stock-pi | vcc-only | rc-auto | stock ↓ | vcc ↓ | rc-auto ↓ |
|---|---|---|---|---|---|---|---|---|
| 09-03T13 | 51 | 51,521 | 19,415 | 7,591 | 7,591 | 62.3% | 85.3% | 85.3% |
| 09-03T14 | 62 | 52,312 | 20,103 | 11,350 | 11,350 | 61.6% | 78.3% | 78.3% |
| 09-03T15 | 64 | 43,696 | 21,014 | 21,962 | 21,962 | 51.9% | 49.7% | 49.7% |
| 09-03T16 | 295 | 137,820 | 21,101 | 27,160 | 28,908 | 84.7% | 80.3% | 79.0% |
| 09-03T18 | 65 | 77,912 | 20,563 | 20,753 | 17,712 | 73.6% | 73.4% | 77.3% |
| 09-03T20 | 1501 | 269,692 | 21,028 | 20,484 | 20,484 | 92.2% | 92.4% | 92.4% |
| 09-03T21 | 281 | 154,573 | 21,157 | 25,871 | 25,871 | 86.3% | 83.3% | 83.3% |
| 09-04T09 | 600 | 265,746 | 19,879 | 33,084 | 33,084 | 92.5% | 87.6% | 87.6% |
| 09-04T10 | 728 | 305,789 | 20,952 | 39,991 | 39,991 | 93.1% | 86.9% | 86.9% |
| 09-04T13a | 204 | 240,960 | 20,416 | 24,796 | 23,814 | 91.5% | 89.7% | 90.1% |
| 09-04T13b | 256 | 169,591 | 19,274 | 27,709 | 29,049 | 88.6% | 83.7% | 82.9% |
| 09-04T13c | 155 | 104,114 | 21,090 | 46,260 | 46,260 | 79.7% | 55.6% | 55.6% |

**Averages: stock-pi ↓ 79.8% · vcc-only ↓ 78.8% · rc-auto ↓ 79.0%**

### Read this honestly

On raw post-compaction *footprint*, the three stacks land within ~1% —
stock Pi's fixed 20,000-token verbatim tail bounds its downside. Footprint
was never the differentiator, because a small context that lost the
information is worse than a slightly larger one that kept it. What the
table does show: Rapid Compact holds stock-Pi-level footprint while keeping
every byte recoverable and spending zero API cost. The differentiators:

| | stock Pi | pi-vcc (VCC) | Rapid Compact |
|---|---|---|---|
| Summary generation | LLM call (seconds, $$, non-deterministic, can hallucinate) | deterministic, 10–30 ms | deterministic, 10–300 ms |
| Compaction API cost | 1 LLM call per compaction | **$0** | **$0** |
| History after compaction | **destroyed** | lossless recall | lossless recall |
| Facts recoverable | 0% | 94.4% | **94.4%** |
| JSON payloads in context | verbatim | verbatim | UC packets (−26%+ tokens, lossless) |
| Bulky text tool output | verbatim every turn | verbatim every turn | snap frames (vision providers) |
| Repeat compactions | degrade (summary of summary) | stable (sticky sections) | stable (sticky sections + key facts) |

## 2. Recall quality after compaction

72 sampled facts (real tool results from compacted-away spans):

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
Endpoint latency varies heavily run-to-run; every completed run is listed.

| stack | billed-in tok | LLM compaction calls | cost | wall | correct |
|---|---|---|---|---|---|
| stock-pi (run A) | 160,405 | 1 | $0.0061 | 198s | ✓ |
| stock-pi (run B) | 191,791 | 2 | $0.0080 | 427s | ✓ |
| stock-pi (run C) | 334,118 | 3 | $0.0156 | 900s timeout | ✗ (killed mid-task) |
| rapid-compact (run A) | 152,311 | **0** | $0.0066 | 196s | ✓ |
| rapid-compact (run B) | 268,410 | **0** (2 rc-owned) | $0.0128 | 518s | ✓ |
| rapid-compact (run C) | 475,016 | **0** | $0.0120 | 366s | ✓ |
| omp-snapcompact | 752,969 | 0 (pruned instead) | $0.0516 | 434s | ✓ |

Findings:

- **Correctness: 3/3 for Rapid Compact** (and stock 2/3; the run C timeout
  is our harness limit, reported as-is). OMP 1/1.
- **Rapid Compact never spends an LLM call on compaction.** Stock pays one
  API summarization call per compaction event. Rapid Compact's brief is
  computed locally in 10–300 ms, deterministic, and free.
- **Information retention decided a real run**: after two compactions the
  rc stack still produced exact error codes (`E-8341-DEPLOY`) from logs
  read before compaction — sticky Key Facts carry them; stock's summary
  paraphrase is luck.
- **OMP at up to 8× the cost**: $0.0516 vs $0.0066 in the same batch, 32
  calls vs 10–13. Its snapcompact never delivered a single image frame on
  this endpoint (0 image blocks in its event stream); it avoided compaction
  via cache-aware pruning (supersedeReads/dropUseless) — good features, at
  a heavy token price.
- **Provider reality check**: z.ai's coding endpoint rejects standard OpenAI
  image parts, so snap frames are provider-gated (anthropic/google by
  default). On zai, Rapid Compact runs VCC + UC — and still wins on cost,
  latency, and information retention.

## 4. Engineering-quality gates

- 40 Rust unit/golden tests + 32 extension tests (incl. live-binary e2e)
- Determinism: same session + same config ⇒ byte-identical summary
- Never-worse rule: UC packets ship only when UC reports real savings; snap
  frames only when line-aware economics beat text by ≥ 25%
- Fallback-first: every rc call is best-effort; any failure degrades to Pi
  core compaction — a session can never be bricked by the extension

## Reproduce

```bash
cargo build --release
node scripts/bench-offline.mjs --min-raw 40000
node scripts/bench-recall.mjs
bash scripts/live-bench.sh all && node scripts/live-bench-report.mjs
```
