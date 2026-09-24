# UltraCompress vs Claude Code's built-in compaction

**Bottom line:** on two real Claude Code sessions (Opus 5.5, 273k and 370k
tokens), UltraCompress compacted in about 65 ms instead of 23–52 seconds, sent
nothing to a model instead of the whole context, and — with its recall tool —
recovered every file the session had edited (12 of 12). Without tools, both
kept the same amount (8 of 12).

## Results

Measured 2026-09-23 on an Apple Silicon Mac, Claude Code 2.1.281, model
`claude-opus-5-5`, UltraCompress 0.3.0. Token and duration figures are Claude
Code's own `compactMetadata` for each compaction.

| Session | Arm | Context before → after | Compaction time | Summary tokens sent to a model |
| --- | --- | --- | --- | --- |
| A (12 edits, 9 files) | Claude built-in | 369,765 → 11,402 | 51.9 s | 369,765 |
| A | UltraCompress | 370,292 → 7,863 | 66 ms | 0 |
| B (3 files) | Claude built-in | 273,369 → 3,226 | 23.0 s | 273,369 |
| B | UltraCompress | 273,896 → 11,261 | 65 ms | 0 |

Recall of the files each session created or edited, asked right after
compaction:

| Session | Claude built-in, memory only | UltraCompress, memory only | UltraCompress + `ultracompress_recall` |
| --- | --- | --- | --- |
| A | 7 of 9 | 7 of 9 | **9 of 9** |
| B | 1 of 3 | 1 of 3 | **3 of 3** |

What this says:

- **Speed.** About 350–800× faster: tens of milliseconds, locally, against a
  model call measured in tens of seconds. Claude recorded 119 s for one earlier
  automatic compaction of a 976k-token session on the same machine.
- **Cost.** Claude's built-in compaction sends the whole context to the model
  once per compaction (here 270k–370k input tokens). On a subscription that
  counts against your usage limits; on the API it is billed. UltraCompress
  sends nothing.
- **Size after.** Mixed: smaller in one session, larger in the other. The
  brief keeps structured sections and recent turns rather than prose.
- **What survives.** Equal from memory alone. The difference is that nothing is
  gone: UltraCompress keeps the raw session searchable, and the model found
  every edited file when it looked.

## Method

`scripts/claude-ab/` holds the harness. Per session and arm it:

1. Copies one real, never-compacted transcript under a fresh session id
   (`prep.py`), so each arm starts from identical history.
2. Resumes the copy in a detached tmux pane (`claude --resume <id> --model
   claude-opus-5-5`) and runs `/compact`. The built-in arm starts Claude with
   `--settings '{"disableAllHooks":true}'`, which keeps the UltraCompress plugin
   from loading; the UltraCompress arm runs the plugin's `session.compact` hook.
3. Reads Claude's `compact_boundary` record (`preTokens`, `postTokens`,
   `durationMs`).
4. Asks one question — list every file you created or edited with Edit or
   Write — either from memory with no tools, or with only the recall tool
   allowed, and scores the answer against the transcript's own Edit/Write
   calls by full path (`score.py`). Only counts are printed; no transcript
   content leaves the machine.

Run: `AB_DIR=/private/results TMUX_BIN=tmux scripts/claude-ab/arm.sh <id-prefix>
uc|stock`, then delete the transcript copies it made.

## Caveats

- Two sessions, one run each, one machine. Treat the ratios as indicative, not
  as a distribution.
- Before-counts differ slightly between arms (about 500 tokens) because the
  UltraCompress arm loads the plugin's tool and hook context.
- The memory question checks one kind of detail (file paths). It does not
  measure reasoning quality after compaction.
- Built-in compaction time includes Claude's model call; UltraCompress time is
  the local hook, as Claude recorded it.
