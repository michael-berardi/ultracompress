<!-- Suggested GitHub repo topics: context-compaction, claude-code, claude-code-plugin, pi-coding-agent, llm, ai-agents, coding-agent, token-optimization, rust, cli, developer-tools -->

<div align="center">

# UltraCompress

### Deterministic context compaction. Local, searchable recall. No LLM in the loop.

**Structured briefs instead of model-written summaries. Every condensed byte
still searchable. $0 per compaction.**

*The compaction layer for people who read their agent's bills.*

[Install](#install) · [Claude Code](#claude-code) · [How it works](#representations-and-savings) · [Recall](#history-and-evidence) · [Measurements](#measurements) · [Security](#related-work-and-security)

</div>

---

UltraCompress is content-aware context compaction for LLM coding agents. It
builds a structured conversation brief without an LLM request, replaces bulky
tool output with image frames or optional lossless encodings, and keeps the
raw session on disk so a recall tool can still find anything that was
condensed. It ships as a Rust CLI plus a [Pi coding agent](https://pi.dev)
adapter and a standalone [Claude Code](#claude-code) plugin — the two places
coding agents currently compact, both served by the same engine.

Summary generation by another model call is a strange way to save model calls.
UltraCompress skips the middleman: compaction runs locally in roughly 10–300
ms, deterministically, at zero API cost. In Claude Code that means about 65 ms
where the built-in summarizer took 23–52 seconds
([measured](#against-claude-codes-own-compaction)). What it drops stays recoverable,
ranked recall measured at **94.4% hit@5** on its published fixture
([docs/BENCHMARKS.md](docs/BENCHMARKS.md)). Context management should not
require another context-management agent.

## New in 0.3.0

- **A standalone Claude Code plugin** (`claude-code/`): compaction and recall
  for Claude Code without UltraTerm — see [Claude Code](#claude-code) and the
  [measured comparison](#against-claude-codes-own-compaction).
- **Native Claude Code transcript recall**: `ultracompress recall` reads
  `~/.claude/projects/**/*.jsonl` directly (`--format auto|pi|claude`), keyed
  by `uuid`/`parentUuid`, crossing compaction boundaries through
  `logicalParentUuid`. No converters. Pi sessions behave exactly as before.

## Install

Release: **0.3.0** (this branch; published with the v0.3.0 tag). Building from
source requires Rust 1.85 or newer. The Pi adapter is tested with Pi 0.85.x
and Node.js 22.19 or newer.

```sh
git clone --branch v0.3.0 https://github.com/michael-berardi/ultracompress
cd ultracompress
cargo build --locked --release
mkdir -p ~/.local/bin
cp target/release/ultracompress ~/.local/bin/
pi install ./extension
```

The release also provides a Developer ID-signed, notarized macOS arm64 CLI
archive and checksums. UltraTerm bundles the bridge with its managed Steak Pi
runtime. Do not load the standalone extension alongside Steak Pi's copy.

[UltraCompact](https://github.com/michael-berardi/ultracompact) is an optional,
separately distributed dependency: an available `uc` executable enables UC
encoding. Without it, VCC compaction and raw-history recall still work. The
adapter falls back to Pi's core compaction when the bridge is unavailable or
fails.

## Claude Code

A standalone, drop-in Claude Code plugin lives in
[`claude-code/`](claude-code/README.md). One session:

```sh
claude --plugin-dir /path/to/ultracompress/claude-code
```

or symlink the folder into `~/.claude/skills/ultracompress` to auto-load. It
replaces Claude Code's compaction summarizer with the same deterministic local
engine — no model call for the summary — and registers an
`ultracompress_recall` tool over the raw transcript. If the binary is missing
or fails, one log line explains why and Claude's stock compaction runs;
nothing bricks. Claude recall defaults to the whole current session (every
branch, pre-compaction history included) because lineage alone stops at
compaction boundaries in real transcripts.

Full install, the binary lookup order, and uninstall:
[`claude-code/README.md`](claude-code/README.md).

**UltraTerm users already have this built in. Do not load both.**

## Representations and savings

| Representation | Purpose |
| --- | --- |
| VCC brief | Deterministic goal, file, decision, diagnostic, and transcript sections |
| Snap frames | Rasterized tool text for supported vision-capable provider paths |
| UC | Optional lossless encoding of JSON and opted-in plain-text envelopes |

The engine measures candidate representations and leaves text unchanged when
an available transform does not clear its margin. Readable-only UC often
selects ordinary minified JSON for a single long string; zero additional savings
in that case is expected. There is no guaranteed percentage improvement for an
arbitrary input.

The adapter avoids asking the model to transcribe dense packets. It keeps a
bounded, session-local original-text cache and puts a `uc:<hash>` retrieval
reference in context instead. `ultracompress_uc` returns the exact original.
Decoded and recalled results remain readable rather than being recompressed.
All fresh tool results remain readable for the first model request that
consumes them: archiving requested output and immediately retrieving it adds
cost. Older results can still be archived on subsequent requests. This avoids
that immediate round trip, not a guarantee of lower total session cost.
Successful no-gain decisions are memoized separately (600 entries, session-local);
failed encodes remain retryable. Duplicate candidates are evaluated once per
request. Model switches re-evaluate vision eligibility. Non-beneficial compaction
is cancelled without invoking a paid core summary.
The original-text cache is limited to 256 entries / 32 MiB; unavailable references direct the
agent to raw-session recall or the original source. Oversized entries stay as
text. Complete legacy `@UC1` packets remain supported.

**A retrieval reference is not a summary.** Reading its content adds those
tokens back, plus the retrieval call. Engine encoding statistics are not
provider-billed end-to-end savings. The bridge reports token counts rather than
labeling character counts as tokens, and includes packet/stub overhead when
comparing encodings.

Local compaction makes no LLM call; that stage has no model API charge. Model
requests before and after compaction, image inputs, and retrieval still have
their normal provider costs.

## History and evidence

UltraCompress's recall command searches **one explicitly selected session
JSONL**, including records omitted from the active context. The Pi adapter
passes the current session file and actual branch tip, so tree navigation and
resume do not accidentally select the last-written sibling branch. No session
archive scan or automatic widening occurs. Claude Code transcripts are read
the same way: one file, whole.

- Default `scope:lineage` (Pi): the current path, including pre-compaction history.
- `scope:all`: all branches of that same session, **not all sessions**. The
  Claude Code plugin defaults to `all`, because lineage alone stops at the
  newest compaction boundary in a real transcript.
- `sessionFile`: explicitly select another session JSONL. For another file,
  lineage starts at its last recorded entry; the current session's tip is not
  reused. Missing files, broken lineage, and invalid selectors return errors.
- `role`, `toolName`, `afterEntry`, `beforeEntry`: narrow before ranking. Entry
  ranges are exclusive and refer to the selected scope's entry order.
- `perPage` (default 5, maximum 20), `snippetBytes` (default 1000; the host
  bridges pass 4000), and `maxOutputBytes` (default 12000) bound output. These
  are UTF-8 **byte** budgets, not token
  guarantees. The output budget covers complete result JSON, excluding the
  host's tool-transport wrapper. Excerpts may shrink to fit; metadata that
  cannot fit returns an error rather than silently dropping page members.
  Pages beyond the available results return an error, not repeated final-page hits.

Results include session identity, effective scope/tip, search count and entry
IDs. Prefer a narrow query and small page before explicitly widening.

```text
/ultracompress-recall collision contract role:toolResult toolName:bash perPage:3
/ultracompress-recall {"query":"release decision","sessionFile":"/path/other session.jsonl","scope":"all"}
```

The CLI requires `--session FILE` (Pi session or Claude Code transcript;
`--format auto|pi|claude`, default auto), with `--leaf ID` for an explicit tip,
`--scope lineage|all`, `--role`, `--tool-name`, `--after-entry`,
`--before-entry`, `--per-page`, `--snippet-bytes`, and `--max-output-bytes`.
Regex is explicit via `--regex` / tool `regex:true`; the command also accepts
`/pattern/`.

## Measurements

The recall fixture reports **94.4% hit@5** across 72 sampled facts from nine
sessions. It measures UltraCompress's retrieval accuracy; other systems' recall
accuracy is outside that fixture's scope. Offline compaction footprints,
latency, task-cost, and JSON-size measurements are documented in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md); they are fixture-specific, not
guaranteed product performance.

### Against Claude Code's own compaction

Two real Claude Code sessions (Opus 5.5, 273k and 370k tokens), each compacted
from an identical copy by Claude's built-in summarizer and by UltraCompress.
Figures are Claude Code's own counters.

| | Claude built-in | UltraCompress |
| --- | --- | --- |
| Compaction time | 23.0 s and 51.9 s | **65 ms and 66 ms** |
| Tokens sent to a model to write the summary | 273k and 370k | **0** |
| Context after | 3.2k and 11.4k | 11.3k and 7.9k |
| Edited files recalled, memory only | 8 of 12 | 8 of 12 |
| Edited files recovered with recall | — | **12 of 12** |

Roughly 350–800× faster, no model call, and nothing actually lost. Two
sessions is a small sample; the method, harness and caveats are in
[docs/BENCHMARKS-CLAUDE.md](docs/BENCHMARKS-CLAUDE.md).

## Commands (Pi adapter)

| Command | Purpose |
| --- | --- |
| `/ultracompress` | Compact now; accepts `keep:N` and `policy:auto\|vcc\|snap\|uc` |
| `/ultracompress-recall <query>` | Current lineage only; `scope:all` adds branches in the same session; JSON options support an explicit other session |
| `/ultracompress-stats` | Show settings, policy, snapshots, and bridge status |
| `/snaps` | Inspect pre-compaction snapshots |

Agent tools: `ultracompress_recall` searches history;
`ultracompress_uc` retrieves a reference or decodes a complete legacy packet.
Never reconstruct, abbreviate, or repeatedly retry a damaged packet. The Claude
Code plugin registers the same recall tool as `mcp__ultracompress__ultracompress_recall`.

## Configuration

The adapter creates `~/.pi/agent/ultracompress.json` with safe defaults:

```json
{
  "policy": "auto",
  "overrideDefaultCompaction": true,
  "smartKeepTail": true,
  "keepUserTurns": null,
  "ultracompressBin": "",
  "uc": { "enabled": true, "bin": "uc", "minChars": 1200 },
  "snap": {
    "enabled": true,
    "minChars": 6000,
    "placement": "nextUser",
    "providers": ["anthropic", "google"]
  },
  "snapshot": { "enabled": true },
  "debug": false
}
```

Set `overrideDefaultCompaction` to `false` to retain Pi's core compaction.
Explicit bridge and UC executable overrides are respected. The adapter opts
into plain-text envelopes with `UC_TEXT_ENVELOPES=1`; older callers keep their
JSON-only behavior. Provider image payloads are not rewritten.

UC aggregate accounting is local. An explicit `UC_TELEMETRY` or
`UC_TELEMETRY_PATH` setting is preserved, including telemetry opt-out. No new
on-disk original-payload cache is introduced by reference retrieval.

Existing `rapid-compact.json` settings and the legacy `rc` binary name remain
migration fallbacks. New installations use the UltraCompress names.

## Development

```sh
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
cd extension
npm ci
npm run typecheck
npm test
```

Claude Code plugin checks (function hooks are early access, hence the switch):

```sh
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude plugin validate claude-code
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude plugin test claude-code
```

The Rust CLI is independent of Pi. Its JSON-in/JSON-out contract is usable by
other harnesses. Offline benchmark scripts are under `scripts/`; live benchmark
scripts make provider calls and are not required for ordinary unit testing.

## Related work and security

The VCC engine descends from [pi-vcc](https://github.com/sting8k/pi-vcc) and
[VCC](https://github.com/lllyasviel/VCC). Snap frames adapt the image-frame
compaction approach; optional UC encoding uses UltraCompact.

See [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), and the
[MIT License](LICENSE). Report security issues privately rather than including
credentials or session records in public issues.
