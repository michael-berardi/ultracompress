# Changelog

## 0.7.2 (2026-11-26) — Update install hardening

All adapters are aligned at 0.7.2, above the published `v0.7.1` tag, so the
release check no longer treats an already-superseded release as available.

- Fixed B5a: download, archive and unpacked staging under the shared cache are
  removed after both a successful and a failed install. Checksum verification,
  the single rollback copy and the bounded install log remain unchanged.
- Fixed B5b: a Cargo-managed install is directed to `cargo install ... --version
  <new version>` rather than the version currently running.
- Fixed B5c: the opt-in auto-install worker runs through `nice -n 10` where
  supported, with a direct spawn fallback.
- Fixed B5d: an exclusive in-flight claim in the shared cache prevents quick
  successive commands from starting more than one anonymous check.

## 0.6.8 (2026-11-26) — Shared update cache

CLI, core, Pi, Claude Code and Hermes adapters are all at 0.6.8.

- Fixed finding (5): the CLI, Pi and Hermes adapters now share one per-machine
  cache file at `<platform cache root>/ultracompress/update.json`. All three
  read and write the same snake_case schema with integer Unix seconds and the
  same 86,400-second interval, so a check by one adapter suppresses the others
  for 24 hours. Old Pi caches with camelCase millisecond timestamps are
  migrated on read. The Windows and macOS cache roots now match too.
- Capped the background `install.log` at 256 KB; the newest lines are kept and
  the file stays `0600` (contract §4a). Checksum verification, the single
  rollback copy, snapshot pruning and Hermes archive bounds are unchanged.
- Added cross-adapter regression tests using one shared cache fixture, cache
  path and legacy-migration tests in Rust, Vitest and Python.

## 0.6.7 (2026-11-26) — Update contract v1

CLI, core, Pi, Claude Code and Hermes adapters are all at 0.6.7.

- Added anonymous update checks: one GitHub latest-release GET per 24 hours per
  machine, four-second timeout, fixed user agent, no identifiers or usage data.
  Checks are on by default in interactive use and off in CI, non-TTY and
  headless/agent sessions; `ULTRACOMPRESS_NO_UPDATE_CHECK=1` and config switches
  opt out. Failures are cached with their cause and shown by `update --check`.
- Added `ultracompress update --check|--skip|--auto on|off|status|--rollback`,
  a once-per-version notice, checksum-verified atomic installation with one
  rollback copy, package-manager detection, and opt-in background installation.
- Pi shows the notice at session start and adds `/ultracompress-update`; Hermes
  reports it in engine status. Neither writes it into JSON output.
- Added `docs/updates.md` with the `<!-- update-contract: v1 -->` marker, README
  Privacy/Updates sections, and offline tests for versions, cache, gates,
  notices, checksum refusal and rollback. Snapshot pruning and Hermes archive
  bounds are unchanged.

## 0.6.6 (2026-11-26) — Privacy hardening and documentation

CLI/core 0.6.6; Pi, Claude Code and Hermes adapters 0.6.0.

- Pi snapshots and settings now use `0700` directories and `0600` files. Snapshot
  retention keeps the newest 20 files within 64 MiB, with the newest always kept.
- Hermes raw recall archives use a `0700` directory and `0600` file and keep the
  newest whole entries within `archive_max_bytes` (64 MiB by default).
- Pi's optional debug dump moved from a shared `/tmp` file to one overwritten
  `0600` file in the private state directory (`~/.local/state/ultracompress`).
- Added a README Privacy section, adapter privacy notes, and regression tests that
  reject telemetry packages/endpoints and check checksum-verified installers,
  private storage and bounded retention. No telemetry or network call was added.

## 0.6.5 (2026-11-15) — CLI option validation

- Reject invalid `--vision` values with exit status 2 instead of silently choosing
  automatic image eligibility. Valid modes remain case-insensitive; compaction
  output for valid options is unchanged.
- Align the Hermes manifest with its runtime version (0.5.0) and correct adapter repository links.
- Shorten setup instructions, separate CLI installation from adapter installation,
  and correct release-signing requirements.


## 0.6.4 (2026-11-12) — Windows Path fix

- The Windows installer read the user Path with its `%VAR%` entries already expanded and wrote it back as a plain string,
  so a stock entry such as `%USERPROFILE%\AppData\Local\Microsoft\WindowsApps` was permanently hard-coded and the value
  lost its expandable type. It now edits the registry value without expanding it, keeps its existing type, and treats the
  bin folder as present when an entry matches it literally or after expansion. This is fixed.
- The Windows installer was tested under PowerShell 7 on Linux, not yet on a Windows machine.
## 0.6.3 (2026-11-12) — README links

- README: real owner in install, badge and clone links; neutrality test allows the repo's own URL.

## 0.6.2 (2026-11-11) — Installer fixes

- The Windows installer wrote its `.cmd` shim with an absolute path in ASCII. With `ULTRACOMPRESS_BIN_DIR` outside the
  install folder the shim pointed at nothing and the installer failed after installing the program; a non-ASCII profile
  name turned into `?`. The shim now uses a relative path when it sits in the install folder and otherwise the absolute
  path in the console code page, and the installer refuses a path that code page cannot hold instead of writing `?`.
- Both installers now write a marker line in the shim and never overwrite an `ultracompress` they did not write; they say
  so in one line and leave it alone. A shim from 0.6.1 still counts as theirs.
- `install.ps1` runs inside a function and leaves no preferences or variables in your session, hides the slow download
  progress bar, resolves relative and `/`-style folder overrides, and runs native commands so that a stderr line cannot
  abort the install on Windows PowerShell 5.1.
- `scripts/build-release-cross.sh` reads `ssh://git@github.com/...` remotes and requires a plain `owner/repo` name.
- The Windows installer was tested under PowerShell 7 on Linux, not yet on a Windows machine.

## 0.6.1 (2026-11-11) — Installers that run

- The `install.sh` and `install.ps1` attached to 0.6.0 refused to run with "this is a source template". The release
  build writes the repository address into the installers, and it also rewrote the check that detects an unstamped
  copy, so the check always fired. The check now looks for the shape of the unreplaced placeholder, the build fails
  if stamping touches it, and the installer test runs a release copy without an address override.

## 0.6.0 (2026-11-11) — One-line installers, Linux and Windows binaries

- One-line installers for macOS, Linux and Windows: `scripts/install.sh` and `scripts/install.ps1`,
  attached to each release. They verify the download's SHA-256 before installing, put `ultracompress`
  on your PATH without sudo or profile edits, and re-running upgrades in place.
- Linux (x86_64 and arm64, statically linked musl) and Windows (x86_64) binaries are now published as
  release downloads, built by `scripts/build-release-cross.sh` in a native arm64 Rust container
  (cross-compilation, no emulation). The signed macOS flow in `scripts/release.sh` is unchanged.
- `scripts/test-install.sh` exercises the installers against a local release base on macOS, in an
  Ubuntu container and under PowerShell 7 on Linux, including upgrade and corrupted-archive refusal.

## 0.5.1 (2026-11-10) — Universal macOS download

CLI/core 0.5.1. Pi adapter 0.5.0, Claude Code plugin 0.5.0 and Hermes adapter 0.1.0 are unchanged.

- The macOS CLI is one universal (arm64 + x86_64) binary in `UltraCompress-macos-universal.zip`,
  Developer ID-signed and notarized. It replaces the arm64-only zip.
- The `.pkg` installer is no longer published. It carried the same CLI and only copied it to
  `/usr/local/bin`; copy the binary from the zip into your `PATH` instead.
- `scripts/release.sh` builds both architectures from the same locked source and no longer needs
  the installer signing identity or package identifier.
- The README has a Download section at the top with the direct macOS link.

## 0.5.0 (2026-11-01) — Provider-neutral shipped files

CLI/core 0.5.0, Pi adapter 0.5.0, Claude Code plugin 0.5.0, Hermes adapter 0.5.0.

UltraCompress ships no built-in model, provider preference, fallback chain, paid-route
grant or personal name. Every shipped file now carries neutral placeholders or requires
the operator to name the model:

- Documentation and install commands use `your-github-username` instead of the
  maintainer's GitHub handle; the plugin author is the neutral `Alex`.
- Benchmark scripts no longer default to a built-in model: `AB_MODEL` and `BENCH_MODEL`
  must be set, and the scripts fail with a message naming the variable when they are
  not. Benchmark documentation and report text use neutral wording.
- `tests/test_neutrality.py` scans shipped source and documentation for model ids,
  provider preferences and personal names. It fails on 0.4.2's tree and passes now.
- No product behaviour changed: compaction, snap frames, recall, settings and the
  Claude Code hook are unchanged.

## 0.4.2 (2026-10-13) — Development dependency security updates

CLI/core 0.4.2, Pi adapter 0.4.0, unchanged Claude Code plugin 0.4.0 and
Hermes adapter 0.1.0.

- Updated the Pi adapter development host to Pi 1.0.4, which includes patched
  `undici` 8.10.2 and `brace-expansion` 5.0.12; also updated `source-map-js`
  to 1.2.2. The adapter now declares tested Pi 1.0.4+ support while retaining
  its existing Pi 0.85.x compatibility range.
- Diagnostic retention, snap frames, recall and adapter behavior are unchanged
  from 0.4.1. The adapter's root development install passes `npm audit` with
  zero reported vulnerabilities; this does not update an independently
  installed Pi host.

## 0.4.1 (2026-10-13) — Bounded diagnostic retention

CLI/core 0.4.1, unchanged Claude Code plugin 0.4.0 and Pi adapter 0.3.0,
Hermes adapter 0.1.0.

- Diagnostic extraction no longer skips every tool output over 20,000 bytes.
  A bounded head/tail scan retains literal error codes from long logs, including
  TypeScript and npm diagnostics. Recent long-log diagnostics take priority;
  code-prefixed and timestamped log reads remain recognized. Fact counts and
  line lengths remain capped; omitted detail remains searchable through
  raw-history recall.
- Added matched baseline/candidate diagnostic fixtures and a hash-verified,
  frozen-input corpus comparison. Benchmarks bound individual subprocesses
  and report excluded oversized inputs and failed operations.
- Inspired by context-mode's explicit diagnostic events. No new dependencies,
  database, proxy, or inference that an earlier error was resolved.

- New Hermes Agent context engine under `hermes/` (adapter 0.1.0, tested with
  Hermes Agent 0.21.5). It subclasses Hermes' `ContextCompressor` and replaces
  only the summary step with `ultracompress compact`: a deterministic brief of
  the conversation, verbatim excerpts of the agent's replies and Hermes' own
  deterministic sections, carried forward across compactions. Compaction fires
  at the configured threshold (30% by default) instead of Hermes' 75% floor for
  windows under 512k. Condensed turns go to a private Pi-format archive per
  session, searched by the `ultracompress_recall` tool. A missing or failing
  binary falls back to Hermes' built-in summary with one log line; `mode:
  builtin` restores stock behaviour. On one real 190k-token session: 0.05 s
  instead of 46.8 s, 4 of 5 facts from the summary versus 3.5, 5 of 5 with
  recall.

## 0.4.0 (2026-09-27) — UltraCompact removed

CLI/core 0.4.0, Claude Code plugin 0.4.0, Pi adapter 0.3.0.

- Removed the optional UltraCompact integration.

- Claude Code plugin: compaction no longer drops messages when the kept tail
  has to move forward to a clean user message. Previously everything between
  the binary's cut and that message, typically a tool result plus a
  requirement the user typed with it, appeared in neither the summary nor the
  kept tail. The hook now summarizes exactly the messages it does not keep.
- `ultracompress stats` lists roles in a fixed order; the per-role breakdown
  previously varied between identical runs.

## 0.3.0 (2026-09-23) — Claude Code plugin and transcript recall

Includes **CLI/core 0.3.0**, the **Claude Code plugin 0.3.0** and the unchanged
**Pi adapter 0.2.2**. Measured against Claude Code's built-in compaction on two
real sessions: 65 ms instead of 23–52 s, no summary model call, and 12 of 12
edited files recovered with recall ([docs/BENCHMARKS-CLAUDE.md](docs/BENCHMARKS-CLAUDE.md)).

- New standalone Claude Code plugin under `claude-code/`: a drop-in function
  hooks plugin (`session.compact` + recall tool `mcp__ultracompress__ultracompress_recall`)
  that replaces Claude Code's compaction summarizer with the local binary — no
  model call for the summary — and falls back to stock compaction with one log
  line whenever the binary is missing, fails, times out (30 s), or yields no
  clean cut. Binary lookup: `$ULTRACOMPRESS_BIN`, `~/.local/bin/ultracompress`,
  `~/.ultraterm/bin/ultracompress`, `/opt/homebrew/bin/ultracompress`,
  `/usr/local/bin/ultracompress` (first regular file wins). Tested with
  `claude plugin validate` / `claude plugin test`; install and uninstall are
  documented in `claude-code/README.md`. UltraTerm users already have this
  plugin built in and should not load both copies.

- `ultracompress recall` reads Claude Code transcripts (`~/.claude/projects/**/*.jsonl`)
  natively — no converters. Records are keyed by `uuid` and parented by
  `parentUuid`; `tool_use` blocks map to tool calls and `tool_result` blocks
  to tool results with the tool name resolved from the originating call;
  sidechain records are skipped and meta records stay as unsearchable chain
  nodes. Records written before compactions remain searchable; lineage
  follows the parent chain from the newest non-sidechain leaf and crosses
  compact boundaries through `logicalParentUuid` (a boundary re-anchor that
  points back into the preserved post-boundary chain is cut, not failed).
- New `--format auto|pi|claude` flag, default `auto`: detection keys on the
  Pi `type:"session"` header vs Claude record shapes. Explicit `pi` keeps the
  previous parser and its strict errors byte for byte; `--leaf` accepts a
  Claude uuid.
- Claude parsing streams line by line and is fault-tolerant: malformed lines
  are skipped and reported as a counted warning (result `warnings` field plus
  a stderr note) instead of aborting; duplicate uuids keep the first record.
- ISO 8601 record timestamps map to epoch seconds on hits. No new
  dependencies, wire-format breaks, or behavior changes for Pi sessions.
## Extension 0.2.2 / CLI 0.2.1 — avoid repeated work and immediate retrieval

- All fresh tool results, not only reads, remain readable until their first
  consuming model request. Historical results remain eligible for transforms.
- Deduplicate identical candidates within a request. Memoize the CLI's explicit
  successful no-gain decisions in a bounded session cache; transient failures
  and legacy responses without this metadata remain retryable.
- Codec-j ties skip the redundant source-token counting subprocess. Failed
  encodes are no longer memoized as successful no-gain decisions.
- Re-evaluate vision eligibility when models change, preventing stale cached
  image transforms from being sent to text-only models.
- Honor adapter JSON settings at the CLI boundary (codec binary/opt-out,
  thresholds, keep-tail policy, model vision and previous summary). Previously
  most were silently ignored. Explicit CLI options retain precedence; regression
  tests exercise the real process boundary and summary carry-forward.
- Include the previous summary in pre-compaction token estimates. Cancel
  non-beneficial compaction in the Pi adapter without invoking a paid core
  fallback. Estimates remain estimates, not provider-billed savings.
- No new dependencies, model calls, persistent payload stores, or wire-format
  changes. The optional transform response field `no_gain` is backward compatible.

## Extension 0.2.1 — streaming snapshots and stdin on oversized sessions

- Pre-compaction snapshots are written by streaming JSON to disk entry by
  entry (`writeSnapEntries`). Serializing the whole session with one
  `JSON.stringify` threw `RangeError: Invalid string length` once the
  payload crossed V8's ~512 MiB max string length, silently disabling the
  safety net on very large sessions. A single entry beyond the limit is
  replaced by a placeholder so the snap file stays valid JSON; partial
  files are removed on failure.
- Compaction stdin is streamed to the UltraCompress process leaf by leaf
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

## 0.1.0 — initial release

- VCC engine: deterministic section briefs (goal, files & changes, commits,
  key facts, outstanding context, preferences) + rolling transcript, bounded
  sticky merge across successive compactions
- Snap engine: content-adaptive PNG frames (p90 line-length shapes), line-aware
  worthwhileness economics (≥25% block savings required), deterministic bytes
- Policy engine: vision provider gating, smart keep-tail, split-turn budget cuts,
  oversized-tail rescue
- Lossless recall: ranked OR/regex search over raw session JSONL (~10 ms)
- Pi extension: compaction hook, live context transforms, `/ultracompress` ·
  `/ultracompress-recall` · `/ultracompress-stats` · `/snaps` commands,
  `ultracompress_recall` tool, pre-compaction
  snapshots, z.ai payload adapter, fallback-first failure posture
- Benchmarks: offline (9 real sessions), recall quality (72 facts), live
  three-stack comparison vs stock Pi and OMP snapcompact
