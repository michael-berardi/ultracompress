# UltraCompress

Deterministic local compaction and raw-session recall for [Pi](https://pi.dev).

UltraCompress builds structured conversation briefs without making an LLM
request. It can also replace suitable tool output with image frames or optional
UltraCompact encodings. Original session records remain available for recall.

## Install

Release: **0.2.2** (Pi adapter 0.2.2, CLI 0.2.1) — avoids immediate retrieval and
repeated no-gain work; retains streaming snapshots and stdin. Building from source requires Rust
1.85 or newer. The Pi adapter is tested with Pi 0.85.x and Node.js 22.19
or newer.

```sh
git clone --branch v0.2.2 https://github.com/michael-berardi/ultracompress
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

The 0.1.2 adapter avoids asking the model to transcribe dense packets. It keeps
a bounded, session-local original-text cache and puts a `uc:<hash>` retrieval
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
archive scan or automatic widening occurs. Pi itself also retains session
history; compaction does not imply that on-disk records were destroyed.

- Default `scope:lineage`: the current path, including pre-compaction history.
- `scope:all`: all branches of that same session, **not all sessions**.
- `sessionFile`: explicitly select another session JSONL. For another file,
  lineage starts at its last recorded entry; the current session's tip is not
  reused. Missing files, broken lineage, and invalid selectors return errors.
- `role`, `toolName`, `afterEntry`, `beforeEntry`: narrow before ranking. Entry
  ranges are exclusive and refer to the selected scope's entry order.
- `perPage` (default 5, maximum 20), `snippetBytes` (default 1000), and
  `maxOutputBytes` (default 12000) bound output. These are UTF-8 **byte** budgets,
  not token guarantees. The output budget covers complete result JSON, excluding
  the host's tool-transport wrapper. Excerpts may shrink to fit; metadata that
  cannot fit returns an error rather than silently dropping page members.
  Pages beyond the available results return an error, not repeated final-page hits.

Results include session identity, effective scope/tip, search count and entry
IDs. Prefer a narrow query and small page before explicitly widening.

```text
/ultracompress-recall collision contract role:toolResult toolName:bash perPage:3
/ultracompress-recall {"query":"release decision","sessionFile":"/path/other session.jsonl","scope":"all"}
```

The CLI requires `--session FILE`; use `--leaf ID` for an explicit tip,
`--scope lineage|all`, `--role`, `--tool-name`, `--after-entry`, `--before-entry`,
`--per-page`, `--snippet-bytes`, and `--max-output-bytes`. Regex is explicit via
`--regex` / tool `regex:true`; the command also accepts `/pattern/`.

The recall benchmark reports **94.4% hit@5** across 72 sampled facts from nine
sessions. It measures UltraCompress's retrieval accuracy; other systems' recall
accuracy is outside that fixture's scope. Latency, task-cost, and JSON-size
measurements are documented in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md); they are fixture-specific, not guaranteed
product performance.

## Commands

| Command | Purpose |
| --- | --- |
| `/ultracompress` | Compact now; accepts `keep:N` and `policy:auto\|vcc\|snap\|uc` |
| `/ultracompress-recall <query>` | Current lineage only; `scope:all` adds branches in the same session; JSON options support an explicit other session |
| `/ultracompress-stats` | Show settings, policy, snapshots, and bridge status |
| `/snaps` | Inspect pre-compaction snapshots |

Agent tools: `ultracompress_recall` searches history;
`ultracompress_uc` retrieves a reference or decodes a complete legacy packet.
Never reconstruct, abbreviate, or repeatedly retry a damaged packet.

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
