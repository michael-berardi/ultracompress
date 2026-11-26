# UltraCompress for Claude Code

**Local compaction for Claude Code. A deterministic summary instead of a model
call, and a recall tool that still finds what was condensed.**

*Standalone and drop-in: point Claude Code at this folder and keep working.*

[Install](#install) · [What it does](#what-it-does) · [Fallback behaviour](#fallback-behaviour) · [Uninstall](#uninstall)

---

## What it does

This plugin replaces Claude Code's compaction summarizer with the
[UltraCompress](https://github.com/michael-berardi/ultracompress) binary. When
the session compacts — manually or automatically — the binary builds a
structured summary of the earlier conversation locally. No model call for the
summary, no compaction line on your provider bill, same output every time for
the same transcript.

Everything condensed stays on disk. A `ultracompress_recall` tool searches the
raw session transcript — every branch of the current session, including history
written before earlier compactions — with ranked keyword or regex queries,
role/tool filters, and bounded excerpts. It reads one explicitly selected
transcript; it never scans the session archive or other sessions.

Each compaction shows a short notice as a toast and in the log (for example
`UltraCompress · 182k → 24k tokens (−87%) · kept 3 messages`), so you always
know who summarized what.

## Install

You need the `ultracompress` binary (Rust 1.85+ to build):

```sh
git clone --branch v0.8.1 https://github.com/michael-berardi/ultracompress
cd ultracompress
cargo build --locked --release -j4
mkdir -p ~/.local/bin && cp target/release/ultracompress ~/.local/bin/
```

Then load the plugin. For one session:

```sh
claude --plugin-dir /path/to/ultracompress/claude-code
```

For every session, copy or symlink the folder where Claude Code auto-loads
plugins from (`claude plugin init` scaffolds that same layout, and the plugin
loads next session as `ultracompress@skills-dir`):

```sh
mkdir -p ~/.claude/skills
ln -s /path/to/ultracompress/claude-code ~/.claude/skills/ultracompress
```

Claude Code's function hooks are early access, so the switch must be on.
Either export it before starting:

```sh
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude
```

or set it in `~/.claude/settings.json` so it applies everywhere:

```json
{
  "env": {
    "CLAUDE_CODE_ENABLE_FUNCTION_HOOKS": "1"
  }
}
```

### Where the binary is looked up

The plugin checks, in order, and uses the first regular file it finds:

1. `$ULTRACOMPRESS_BIN` (explicit override)
2. `~/.local/bin/ultracompress`
3. `~/.ultraterm/bin/ultracompress`
4. `/opt/homebrew/bin/ultracompress`
5. `/usr/local/bin/ultracompress`

Set `ULTRACOMPRESS_BIN` if your binary lives elsewhere. The engine's file stat
exposes no permission bits, so a file that exists but is not executable
surfaces as a readable failure in the log, then stock compaction takes over.

## Fallback behaviour

Every UltraCompress call is best-effort, and one compaction shares a single
30 s budget: a retry gets what is left of it, never a fresh 30 s.

When the binary's own cut lands on a tool result, the plugin re-summarizes
exactly the messages it does not keep (one extra call inside that same budget),
so the summary never claims to cover messages it left behind. When a
conversation has no clean cut at all, it retries once asking for a summary of
everything. When the binary has already summarized every message (an empty
`first_kept_entry_id`), that is accepted as it stands, with no retry.

If the binary is missing, fails, runs out of that budget, or yields no usable
summary, the plugin steps aside with a visible log line
(`UltraCompress · stock compaction: <reason>`) plus a debug detail line, and
Claude Code's stock compaction runs instead. A failed compaction never costs
you the session; it costs you one deterministic summary you can retry next
time. Recall failures come back as readable tool results naming the reason,
never as hangs or throws.

## Uninstall

Remove the symlink or copy:

```sh
rm ~/.claude/skills/ultracompress
```

If you ran it with `--plugin-dir`, it was never installed — the next session
without the flag is already clean. Remove `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS`
from `~/.claude/settings.json` only if nothing else uses function hooks.

## Privacy

This plugin runs the local binary on this session's transcript; it sends no data
itself, has no telemetry and writes no files of its own. The binary's update
check never runs inside Claude Code. Claude Code's own network calls, transcript
and logs are controlled by Claude Code. Details: [PRIVACY.md](../PRIVACY.md).

## UltraTerm users

UltraTerm ships this plugin already, wired to its managed binary. If UltraTerm
is installed, you have this built in — **do not load both**. Two copies of the
same hooks compete for the same events, and neither wins gracefully.

## Notes

- Recall defaults to the whole current session (every branch, including
  pre-compaction history); `scope: "lineage"` narrows to the current branch
  since the last compaction. Lineage stops at compaction boundaries in real
  transcripts, which is why it is not the default here.
- Results are bounded: 5 hits per page (20 max), 4,000 UTF-8 bytes per
  excerpt, and 12,000 bytes for the complete result — `maxOutputBytes` raises
  that, up to the tool's 32,000-byte maximum. These are byte budgets, not token
  guarantees.
- An explicit `sessionFile` must be a regular `.jsonl` transcript file.
  Anything else fails with a readable tool result instead of being read.
- No benchmark claims are made here. See
  [`docs/BENCHMARKS.md`](../docs/BENCHMARKS.md) for what has actually been
  measured, on which fixtures.

[MIT](../LICENSE) · part of [UltraCompress](https://github.com/michael-berardi/ultracompress)
