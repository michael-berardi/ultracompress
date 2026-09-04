<div align="center">

# ⚡ UltraCompress

**Deterministic local compaction for [Pi](https://github.com/badlogic/pi-mono). Lossless recall. $0 per compaction.**

VCC briefs · snap frames · UC packets — one policy engine.

*by Implose Cybernetics*

</div>

---

Your agent's context window is a budget. Every coding session burns it on
7,000-line build logs, 200-record JSON dumps, and tool output nobody will
ever read twice. Then compaction torches the evidence: an LLM summarizes
your session, the summary hallucinates a little, the raw history is gone,
and the next compaction summarizes the summary.

**UltraCompress never calls an LLM to compact.** It compacts the way a
compiler would: measure every representation, keep the cheapest one that's
still faithful, and never throw away the original.

```
stock Pi:   160k billed tokens · LLM summary · history destroyed
ultracompress:  152k billed tokens · $0 compaction · 94% of facts recoverable
```

## Three engines, one policy

| Engine | What it owns | Why it wins |
|---|---|---|
| **VCC** | Conversation → structured brief | Deterministic sections (goal, files, commits, key facts, outstanding, preferences) + rolling transcript. Same input ⇒ byte-identical output. 10–300 ms. $0. |
| **Snap** | Bulky tool output → PNG frames | Text rasterized into image frames the model still reads — fixed vision-token cost instead of per-character cost, with adaptive frame shapes tuned per payload. |
| **UC** | JSON payloads → UC packets | [UltraCompact](#ultracompact-optional) lossless encoding, ~26%+ fewer tokens than minified JSON, model-readable, decode-exact. |

The policy engine routes **each block** to its cheapest faithful
representation and refuses any transform that doesn't beat plain text by a
measured margin. Compaction never degrades: sticky sections accumulate
across merges, volatile ones replace, key diagnostic facts (error codes,
warnings) survive every pass.

## Lossless by construction

Every other compactor's story ends at the summary. UltraCompress's begins
there: the raw session stays on disk and `ultracompress_recall` searches it — ranked,
paged, ~10 ms — so compacted-away history stays reachable at **94.4%
hit@5** (72 sampled facts across 9 real sessions; stock Pi: 0%, the history
is gone). The model gets a `ultracompress_recall` tool and learns to use it before
claiming it lost context.

## Built for trust

- **Deterministic** — same session + same config = byte-identical summary
- **Never-worse** — a transform ships only when measured tokens say it wins
- **Fallback-first** — any failure degrades to Pi core compaction;
  UltraCompress cannot brick a session
- **Zero API cost** — compaction is local computation; UC/snap shrink the
  *live* context every turn, before compaction even triggers

## Results

Full methodology and every run in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

| benchmark | stock Pi | pi-vcc | UltraCompress |
|---|---|---|---|
| Post-compaction context (12 real sessions) | ↓ 79.8% | ↓ 78.8% | ↓ 79.0% (at parity, $0) |
| Facts recoverable after compaction | 0% | 94.4% | **94.4%** |
| Compaction API cost | 1 LLM call each | $0 | **$0** |
| Summary determinism | none | byte-exact | **byte-exact** |
| JSON payload shrink | — | — | **−26%+ tokens** |
| Live task observed cost vs OMP | — | — | **$0.0066 vs $0.0516** |

## Install

Requires the `ultracompress` binary (Rust ≥ 1.85):

```bash
git clone https://github.com/michael-berardi/ultracompress
cd ultracompress
cargo build --release
mkdir -p ~/.local/bin
cp target/release/ultracompress ~/.local/bin/
```

Then use as a Pi extension:

```bash
pi -e /path/to/ultracompress/extension            # try it
pi install /path/to/ultracompress/extension       # or install
```

Optional — [UltraCompact](https://github.com/michael-berardi/ultracompact)
(`uc` on PATH) unlocks the UC engine. UltraCompress is fully functional
without it; when present, JSON payloads shrink automatically and
losslessly. UC stays an optional accelerator, on by default, graceful when
absent.

## Use

Automatic — UltraCompress takes over `/compact` and threshold compactions
(`overrideDefaultCompaction: false` to send them back to Pi core).

| command | what it does |
|---|---|
| `/ultracompress` | compact now · `keep:N` · `policy:auto\|vcc\|snap\|uc` · optional follow-up prompt |
| `/ultracompress-recall <query>` | search raw history (compacted turns included) · `scope:all` |
| `/ultracompress-stats` | status, policy, cache, snapshots |
| `/snaps` | pre-compaction snapshots (restorable) |

Tools the model uses on its own: `ultracompress_recall` (search history),
`ultracompress_uc` (decode a UC packet).

## Config

`~/.pi/agent/ultracompress.json` — scaffolded with safe defaults on first run:

```json
{
  "policy": "auto",
  "overrideDefaultCompaction": true,
  "smartKeepTail": true,
  "keepUserTurns": null,
  "ultracompressBin": "",
  "uc":  { "enabled": true, "bin": "uc", "minChars": 1200 },
  "snap": { "enabled": true, "minChars": 6000, "placement": "nextUser",
            "providers": ["anthropic", "google"] },
  "snapshot": { "enabled": true },
  "debug": false
}
```

Snap frames are provider-gated: they ship only where the wire format is
proven. Everywhere else you get VCC + UC — still deterministic, still $0,
still lossless.

Renaming from the pre-release project is automatic: an existing
`~/.pi/agent/rapid-compact.json` is copied to `ultracompress.json`, and an
installed `rc` binary remains a fallback. New commands, tools, settings, and
installs use the UltraCompress name.

## Architecture

```
┌────────────────────────────────────────────────────────────┐
│ Pi extension (TS) — thin adapter: hooks, commands, tools   │
│   session_before_compact ──┐                               │
│   context (per LLM call) ──┤ spawn                         │
│   before_provider_request ─┘                               │
└────────────────────────────┬───────────────────────────────┘
                             ▼ JSON in / JSON out
┌────────────────────────────────────────────────────────────┐
│ ultracompress (Rust) — the engine                           │
│   load → normalize → classify → policy → route             │
│     ├─ VCC: sections · transcript · merge · render         │
│     ├─ Snap: adaptive layout → deterministic PNG frames    │
│     ├─ UC: bridge to `uc encode --stats` (hash cache)      │
│     └─ recall: ranked search over raw session JSONL        │
└────────────────────────────────────────────────────────────┘
```

The engine is a standalone Rust crate — no Pi dependency. Point any harness
at the same JSON contract.

## Benchmarks

Reproduce everything yourself:

```bash
node scripts/bench-offline.mjs        # real sessions, zero API cost
node scripts/bench-recall.mjs         # recall quality after compaction
bash scripts/live-bench.sh all        # live: stock vs UltraCompress vs OMP
node scripts/live-bench-report.mjs
```

## Related work

- [VCC](https://github.com/lllyasviel/VCC) — the original
  transcript-preserving conversation compiler
- [pi-vcc](https://github.com/sting8k/pi-vcc) — the Pi extension that
  proved deterministic compaction; UltraCompress's VCC engine descends from it
- OMP Snap Compact — the image-frame compaction idea, reworked with
  content-adaptive shapes and honest economics
- [UltraCompact](https://github.com/michael-berardi/ultracompact) — the
  lossless JSON token-minimizer behind the UC engine

## Contributing, security, and license

Contributions are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md). Report
security issues privately as described in [SECURITY.md](SECURITY.md).
UltraCompress is released under the [MIT License](LICENSE).
