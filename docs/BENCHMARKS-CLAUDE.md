# Benchmark: UltraCompress vs Claude Code stock compaction

`scripts/bench-claude-compaction.py` compares UltraCompress's local compaction
against Claude Code's built-in (`stock`) compaction using **real historical
compactions** from Claude Code transcripts on this machine — offline, with no
model calls and no new Claude sessions.

## Method

1. Scan transcripts (`~/.claude/projects/*/*.jsonl`, or `--transcripts DIR`,
   also `DIR/*.jsonl`) for compact records: `type=system`,
   `subtype=compact_boundary`, reading `compactMetadata`
   `{trigger, preTokens, postTokens, durationMs}` plus the Claude Code version
   and the session's model (nearest assistant record's `message.model`).
2. For each boundary, reconstruct the summarized conversation: walk
   `parentUuid` back from the boundary's `logicalParentUuid`, keeping
   non-sidechain records and stopping at a previous compact boundary. Raw
   records are normalized to SessionMessage rows and mapped to UltraCompress
   entries exactly like `toEntries()` in `claude-code/hooks/ultracompress.mjs`
   (text blocks; `tool_use` → `toolCall`; `tool_result` → `toolResult` with
   tool name; ids `m<i>` / `m<i>:r<j>`).
3. Run `ultracompress compact --policy auto --vision auto` with the plugin's
   exact stdin defaults (`COMPACT_DEFAULTS`), timing one wall-clock
   `subprocess.run` with `time.perf_counter`. If the binary reports
   `no safe cut point` — or the plugin's `chooseCut()` finds no clean user
   turn at/after `first_kept_entry_id` — it retries once with
   `keepUserTurns: 0`, as the plugin does, and flags the row `retried`.
4. One row per boundary plus an aggregate; results go to `--out` JSON.

Bound work: `--limit` samples (default 60), transcripts over
`--max-file-bytes` (default 256 MiB) skipped, per-sample UltraCompress timeout
`--timeout` (default 60 s). Sample ids are `sha256(path + uuid)[:12]` — no
message text, paths, commands or tool output ever enters the output.

## Fields (per-sample rows)

| field | meaning |
| --- | --- |
| `id` | anonymous `sha256(path + uuid)[:12]` |
| `preTokens` / `postTokens` | Claude Code's own token counts around its compaction |
| `stock_ms` | Claude stock compaction wall time from `compactMetadata.durationMs` |
| `trigger` | compact trigger (`manual` / `auto`) |
| `model`, `version` | session model id; Claude Code version at the boundary |
| `uc_ms` | UltraCompress subprocess wall time (ms) |
| `uc_tokens_before_est` / `uc_tokens_after_est` | UltraCompress **estimates** (chars/token) |
| `uc_kept` | messages kept by UltraCompress (`kept_messages`) |
| `retried` | keepUserTurns=0 retry was needed |
| `uc_savings_pct`, `uc_chars_per_token` | binary-reported savings and estimator calibration |
| `chained` | lineage stopped at a previous compact boundary |
| `lineage_records` | records in the reconstructed lineage |

## Results

> Placeholder — filled from `bench-claude-compaction.json` by the maintainer;
> regenerate with the command below. Do not quote numbers here without a
> dated rerun.

```
$ ./scripts/bench-claude-compaction.py --out /tmp/bench-claude-compaction.json
```

## Caveats

- **Estimate vs measured tokens.** `uc_tokens_before_est` /
  `uc_tokens_after_est` are the binary's estimates (chars per token, reported
  as `uc_chars_per_token`); `preTokens` / `postTokens` are Claude Code's own
  counts. Any comparison across the two families is approximate. `*_est`
  fields are the estimated ones.
- **What `stock_ms` includes.** Claude's duration covers its whole compaction:
  the summarization model call plus Claude's own post-processing. `uc_ms`
  covers only the local binary. The ratio is therefore a latency ratio of two
  different pipelines, and stock durations also vary with model load.
- **Tokens avoided ≠ dollars.** `summary_input_tokens_avoided` is the sum of
  `preTokens`: stock compaction sends the whole context to the model once,
  while UltraCompress sends nothing (local, deterministic). It is a
  prompt-token figure, not a cost claim.
- **Chained compactions.** When the lineage stops at an earlier compact
  boundary (`chained=true`), the reconstructed input covers only history since
  that boundary while Claude's `preTokens` covers its full context; those rows
  understate UltraCompress's input relative to what stock summarized.
- **Coverage.** Only transcripts with `compact_boundary` records are
  benchmarked; older formats (bare `isCompactSummary` user records) are not.
  Some reconstructions are skipped (empty lineage, partial `parentUuid`
  chains, UltraCompress failures/timeouts) and reported under `skipped`.
- **Machine.** Apple Silicon Mac (M-series, local SSD), single run, Python
  `subprocess` spawn included in `uc_ms`; no model or network calls were made.
- **Privacy.** Transcripts were read programmatically only; results contain
  numbers, counts, model ids, version strings and hashed sample ids. No
  content was printed, copied into the repo, or published.
