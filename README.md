# UltraCompress

[![Latest release](https://img.shields.io/github/v/release/michael-berardi/ultracompress?label=release)](https://github.com/michael-berardi/ultracompress/releases/latest) [![MIT License](https://img.shields.io/github/license/michael-berardi/ultracompress)](LICENSE) ![Rust](https://img.shields.io/badge/built%20with-Rust-orange) ![Claude Code plugin](https://img.shields.io/badge/Claude%20Code-plugin-D97757)

UltraCompress compacts coding-agent conversations into deterministic briefs
without a model call. A Rust CLI builds the brief; adapters connect it to Pi,
Claude Code and Hermes Agent. Recall searches the original session on disk,
including messages no longer in the model's context.

## Install

Current CLI release: **0.7.0**. Install a prebuilt binary:

macOS and Linux:

```sh
curl -fsSL https://github.com/michael-berardi/ultracompress/releases/latest/download/install.sh | sh
```

Windows (PowerShell 5.1 or 7):

```powershell
irm https://github.com/michael-berardi/ultracompress/releases/latest/download/install.ps1 | iex
```

What the installers do: they download the release for your platform (macOS: the signed universal zip;
Linux: a static musl binary for your CPU; Windows: the x86_64 zip), verify its SHA-256 against the
release `SHA256SUMS` before installing anything, and finish by running `ultracompress --version`.
On macOS and Linux the binary lives in `~/.ultracompress` (`ULTRACOMPRESS_HOME`) with a shim in
`~/.local/bin` (`ULTRACOMPRESS_BIN_DIR`); no sudo, no shell-profile edits. On Windows it lives in
`%LOCALAPPDATA%\UltraCompress` with a `.cmd` shim in its `bin` folder, added to your user `PATH`.
Needs curl or wget, tar, and sha256sum or shasum (sh), or built-in cmdlets (PowerShell). To update, run the same line
again. To remove, delete the install folder and the shim. Pin a version with `ULTRACOMPRESS_VERSION`
(for example `0.7.0`).

### From source

Requires Rust 1.85+. From a checkout:

```sh
cargo install --locked --path crates/ultracompress-cli
```

Ensure Cargo's bin directory is on `PATH`. The CLI does not require Node.js or
an agent installation.

## First use

Inspect a Pi session, then compact it without image frames:

```sh
ultracompress stats --session session.jsonl
ultracompress compact --session session.jsonl --policy vcc --vision off
ultracompress recall --session session.jsonl --query "build failure"
```

Commands write JSON to stdout and errors to stderr. Compaction requires a safe
cut before a user turn; it can reject a session that has too little history.
`compact` and `stats` read Pi sessions. `recall` also reads Claude Code
transcripts. Adapters provide JSON input for other conversation formats.

## Agent adapters

- **Pi:** with Node.js 22.19+ and Pi 0.85.x or 1.0.4+, run `pi install ./extension`
  from a checkout. A missing or failing binary falls back to Pi compaction.
- **Claude Code:** see [plugin setup](claude-code/README.md). Function hooks
  require `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`; load the folder with
  `claude --plugin-dir ./claude-code`.
- **Hermes Agent:** see [engine setup and settings](hermes/README.md).

Do not load a standalone adapter if your host already bundles it.

## Representations and savings

| Representation | Purpose |
| --- | --- |
| VCC brief | Deterministic goal, file, decision, diagnostic, and transcript sections |
| Snap frames | Rasterized tool text for supported vision-capable provider paths |

The brief keeps the newest entries of every list (files, commits, failures)
and stays bounded over repeated compactions. Tool names are matched across
harnesses (`edit`, `Edit`, `patch`, `write_file`, `Bash`, `terminal`).
Attached images (screenshots a tool returned) are counted toward the size of
the kept tail from their pixel dimensions, so a screenshot-heavy session is
cut to the same budget as a text-heavy one.

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

Keyword results are ranked with BM25 (repeats saturate, long messages are
normalized) and weighted by how many of the query's terms a message matches.
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
  "snapshot": { "enabled": true, "maxCount": 10, "maxAgeDays": 14 },
  "summaryMaxBytes": 16384,
  "debug": false
}
```

Pre-compaction snapshots are pruned to the newest `maxCount` and to `maxAgeDays`.
`summaryMaxBytes` caps the brief of an automatic compaction (1024 to 65536);
an explicit `/ultracompress` is never capped. The brief is deterministic, so
free-text compaction instructions are not used; the adapter says so once.

Set `overrideDefaultCompaction` to `false` to retain Pi's core compaction.
The UltraCompress binary can be configured explicitly. Provider image payloads
are not rewritten.

Existing `rapid-compact.json` settings and the legacy `rc` binary name remain
migration fallbacks. New installations use the UltraCompress names.

## Development

```sh
cargo fmt --all -- --check
cargo test --locked --all-targets -j4
cargo clippy --locked --all-targets -j4 -- -D warnings
cargo build --locked --release -j4
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

## Measurements and security

[Benchmark methods and historical results](docs/BENCHMARKS.md) and the
[compaction comparison](docs/BENCHMARKS-CLAUDE.md) describe their fixtures and
limitations. Private-session measurements are not reproducible from this
checkout alone and are not product-wide performance guarantees.

See [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), and the
[MIT License](LICENSE). Keep raw sessions and credentials out of public issues.

Part of [Pareto Cybernetics](https://paretocybernetics.com/projects/ultracompress/): free, private, open-source tools for working with AI agents.
