<div align="center">

# UltraCompress

### Deterministic context compaction. Local, searchable recall. No LLM in the loop.

**Structured briefs instead of model-written summaries. Every condensed byte
still searchable. $0 per compaction.**

*The compaction layer for people who read their agent's bills.*

[![Latest release](https://img.shields.io/github/v/release/michael-berardi/ultracompress?label=release)](https://github.com/michael-berardi/ultracompress/releases/latest) [![MIT License](https://img.shields.io/github/license/michael-berardi/ultracompress)](LICENSE) ![Rust](https://img.shields.io/badge/built%20with-Rust-orange) ![Claude Code plugin](https://img.shields.io/badge/Claude%20Code-plugin-D97757)

[Install](#install) · [Claude Code](#claude-code) · [Hermes Agent](#hermes-agent) · [How it works](#representations-and-savings) · [Recall](#history-and-evidence) · [Measurements](#measurements) · [Security](#related-work-and-security)

</div>

---

UltraCompress is content-aware context compaction for LLM coding agents. It
builds a structured conversation brief without an LLM request, replaces bulky
tool output with image frames when supported, and keeps the
raw session on disk so a recall tool can still find anything that was
condensed. It ships as a Rust CLI plus a [Pi coding agent](https://pi.dev)
adapter, a standalone [Claude Code](#claude-code) plugin and a
[Hermes Agent](#hermes-agent) context engine, all served by the same engine.

Summary generation by another model call is a strange way to save model calls.
UltraCompress skips the middleman: compaction runs locally in roughly 10–300
ms, deterministically, at zero API cost. In Claude Code that means about 65 ms
where the built-in summarizer took 23–52 seconds
([measured](#against-claude-codes-own-compaction)). What it drops stays recoverable,
ranked recall measured at **94.4% hit@5** on its published fixture
([docs/BENCHMARKS.md](docs/BENCHMARKS.md)). Context management should not
require another context-management agent.

## What's new

- **Hermes Agent context engine** (`hermes/`, on `main`, not yet in a tagged
  release): replaces Hermes' model-written compaction summary with the local
  brief and adds raw-history recall — see [Hermes Agent](#hermes-agent).
- **0.4.0**: the optional UltraCompact encoder is removed. VCC briefs, snap
  frames and recall are unchanged.
- **0.3.0**: a standalone Claude Code plugin (`claude-code/`) and native
  Claude Code transcript recall (`--format auto|pi|claude`).

## Install

Current release: **0.4.0**. Building from source requires Rust 1.85 or newer. The Pi adapter is tested with Pi 0.85.x
and Node.js 22.19 or newer.

```sh
git clone --branch v0.4.0 https://github.com/michael-berardi/ultracompress
cd ultracompress
cargo build --locked --release
mkdir -p ~/.local/bin
cp target/release/ultracompress ~/.local/bin/
pi install ./extension
```

The release also provides a Developer ID-signed, notarized macOS arm64 CLI
archive and checksums. UltraTerm bundles the bridge with its managed Steak Pi
runtime. Do not load the standalone extension alongside Steak Pi's copy.

VCC compaction and raw-history recall work without any optional codec. The
adapter falls back to Pi's core compaction when the bridge is unavailable or fails.

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

## Hermes Agent

A Hermes context engine lives in [`hermes/`](hermes/README.md). Link it into
your profile and select it:

```sh
ln -s /path/to/ultracompress/hermes/ultracompress ~/.hermes/plugins/ultracompress
hermes config set context.engine ultracompress
```

Hermes keeps its own compaction pipeline; the engine replaces only the summary
step with the local brief, verbatim excerpts of the agent's replies and Hermes'
deterministic sections, and registers `ultracompress_recall` over a private raw
archive of everything condensed. It compacts at 30% of the window instead of
Hermes' 75% floor for models under 512k tokens. On one real 190k-token Hermes
session the summary took 0.05 s instead of 46.8 s and answered more recall
questions; missing or failing binaries fall back to Hermes' built-in summary
with one log line. Settings, measurements and uninstall:
[`hermes/README.md`](hermes/README.md).

## Representations and savings

| Representation | Purpose |
| --- | --- |
| VCC brief | Deterministic goal, file, decision, diagnostic, and transcript sections |
| Snap frames | Rasterized tool text for supported vision-capable provider paths |

Snap frames replace tool text only when line-aware economics clear the savings
margin; otherwise the original text stays in context. Fresh tool results remain
readable for the first model request that consumes them. Subsequent requests
may receive snap frames on supported vision providers; successful no-gain
decisions are memoized per session. Model switches re-evaluate vision eligibility.
Raw session history remains searchable with `ultracompress_recall`.

Older sessions can contain `uc:<hash>` original-output markers. They name
session-local cached text, not a codec; this release does not emit new markers.
Use raw-history recall to find the original after the old cache expires.

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
| `/ultracompress` | Compact now; accepts `keep:N` and `policy:auto\|vcc\|snap` |
| `/ultracompress-recall <query>` | Current lineage only; `scope:all` adds branches in the same session; JSON options support an explicit other session |
| `/ultracompress-stats` | Show settings, policy, snapshots, and bridge status |
| `/snaps` | Inspect pre-compaction snapshots |

Agent tool: `ultracompress_recall` searches raw history, including turns
omitted from the active context. The Claude Code plugin registers the same
recall tool as `mcp__ultracompress__ultracompress_recall`.

## Configuration

The adapter creates `~/.pi/agent/ultracompress.json` with safe defaults:

```json
{
  "policy": "auto",
  "overrideDefaultCompaction": true,
  "smartKeepTail": true,
  "keepUserTurns": null,
  "ultracompressBin": "",
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
The UltraCompress binary can be configured explicitly. Provider image payloads
are not rewritten.

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

Hermes adapter checks (the second needs a Hermes Agent checkout and its interpreter):

```sh
python3 -m unittest discover -s hermes/tests -v
HERMES_AGENT_DIR=~/.hermes/hermes-agent <hermes python> -m unittest discover -s hermes/tests -v
```

The Rust CLI is independent of Pi. Its JSON-in/JSON-out contract is usable by
other harnesses. Offline benchmark scripts are under `scripts/`; live benchmark
scripts make provider calls and are not required for ordinary unit testing.

## Related work and security

The VCC engine descends from [pi-vcc](https://github.com/sting8k/pi-vcc) and
[VCC](https://github.com/lllyasviel/VCC). Snap frames adapt the image-frame
compaction approach.

See [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), and the
[MIT License](LICENSE). Report security issues privately rather than including
credentials or session records in public issues.
