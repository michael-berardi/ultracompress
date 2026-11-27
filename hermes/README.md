# UltraCompress for Hermes Agent

**Local compaction for [Hermes Agent](https://github.com/NousResearch/hermes-agent). A
deterministic brief instead of a model-written summary, and a recall tool that still finds
what was condensed.**

[Install](#install) · [What it does](#what-it-does) · [Settings](#settings) · [Recall tool](#recall-tool) · [Fallback](#fallback-behaviour) · [Measured](#measured) · [Uninstall](#uninstall)

---

## What it does

This is a Hermes context engine. Hermes keeps its own compaction pipeline: when to compact,
which recent turns stay, where the handoff goes, and how the session database is rewritten.
The engine replaces one step, the summary. Instead of sending the condensed turns to a model,
it runs the `ultracompress` binary locally and inserts:

- the UltraCompress brief of what the user and the agent said,
- verbatim excerpts of the agent's replies, newest first,
- Hermes' own deterministic sections: anchor index, the user's messages verbatim, recovery footer.

Earlier windows carry forward through later compactions. Tool output stays out of the brief by
default, since an agent's decisions live in the conversation, and goes to a private raw archive
(`$HERMES_HOME/ultracompress/<session>.jsonl`). Two kinds of tool call do survive into the brief,
as names and paths only: a call that writes or edits a file (`write_file`, `patch`, `edit`,
`write`, `str_replace_editor`) and a shell call whose command contains `git commit`. That is what
lets the brief say which files changed and what was committed. Everything else — read-only calls,
and every tool result — stays out. The `ultracompress_recall` tool searches the archive with
ranked keywords or a regular expression, 10 hits to a page, and reports the total hit and page
count so the agent can ask for the next page. If Hermes rotates the session id at a compaction
boundary, the archive follows it.

It also changes when compaction fires. Hermes raises the trigger to 75% of the window for any
model under 512k tokens, because a model-written summary is slow and expensive to repeat. A local
brief takes milliseconds, so this engine uses the threshold you set (30% by default, or Hermes'
`compression.threshold` when the plugin setting is unset).

## Install

You need Hermes Agent (tested with 0.21.5) and the `ultracompress` binary (see the
[main README](../README.md#install)).

```sh
git clone https://github.com/michael-berardi/ultracompress
ln -s "$PWD/ultracompress/hermes/ultracompress" ~/.hermes/plugins/ultracompress
hermes config set context.engine ultracompress
hermes tools enable context_engine --platform cli        # the recall tool; repeat per platform
```

Use `$HERMES_HOME/plugins/ultracompress` for a non-default profile. Restart the gateway (or start
a new CLI session) to load it. When the engine is active, the agent log shows
`Using context engine: ultracompress` and each compaction logs one line such as
`UltraCompress · 183 turns (~60000 tokens) → summary ~5000 tokens · 14 ms`.

### Where the binary is looked up

First executable regular file wins: `binary` in settings, `$ULTRACOMPRESS_BIN`,
`~/.local/bin/ultracompress`, `~/.ultraterm/bin/ultracompress`,
`/opt/homebrew/bin/ultracompress`, `/usr/local/bin/ultracompress`. A path without the executable
bit is skipped rather than run.

## Settings

Optional `settings.json` next to `__init__.py`:

```json
{ "mode": "deterministic", "threshold": 0.30 }
```

| Key | Default | Meaning |
| --- | --- | --- |
| `mode` | `deterministic` | `builtin` makes the engine behave exactly like Hermes' own compressor |
| `threshold` | unset | Compaction trigger as a fraction of the context window. Unset means Hermes' `compression.threshold`, and 0.30 when that is unset too |
| `binary` | `""` | Explicit path to `ultracompress` |
| `archive_dir` | `""` | Raw archive location; empty means `$HERMES_HOME/ultracompress` |
| `archive_max_bytes` | `67108864` | Keep the newest whole entries in the raw archive within this byte budget |
| `tool_result_chars` | `0` | Characters of each tool result to include in the brief (0 = none) |
| `reply_chars` / `replies_budget_chars` | `1200` / `12000` | Size of each reply excerpt and of the whole replies section |
| `carry_users_chars` | `4000` | Earlier windows' verbatim user quotes kept in later summaries |
| `timeout_seconds` | `30` | Limit for one `ultracompress compact` run, and for one `ultracompress_recall` call |

## Recall tool

`ultracompress_recall(query, page?, regex?, role?)` searches the raw archive. It asks the CLI for
10 hits a page and adds a `result` line giving the total hits and the page count, so a caller that
wants more than the first page passes `page`. A bad `page`, a missing query, a timeout or a CLI
that cannot run all come back as `{"error": ...}` JSON rather than an exception.

## Fallback behaviour

The raw window is archived before the binary is called, so a missing, failing or timed-out binary
costs the brief and never the history recall depends on. If the binary is missing, fails, times
out, or returns nothing, the engine logs one line naming the cause and Hermes' built-in summary
runs for that compaction. A failure never costs the session. Recall failures come back as readable
tool results.

A stretch made only of tool calls (nothing said by the user or the agent) is not a failure: the
engine keeps the previous brief and adds one line naming the tools that ran, with no model call.
If the binary succeeds but the window has no text of its own to summarise, the brief says so in one
local line instead of falling back to a model summary.

## Measured

One real Hermes Telegram session (an operator-chosen model, 245 messages, about 190k tokens), compacted from
the same copy by Hermes' built-in summarizer and by this engine, then quizzed on five facts that
only existed in the condensed part:

| | Hermes built-in | UltraCompress |
| --- | --- | --- |
| Summary time | 46.8 s (aux model call) | **0.05 s** (no model call) |
| Facts answered from the summary | 3.5 of 5 | **4 of 5** |
| Facts recovered with `ultracompress_recall` | — | **5 of 5** |

In live use the same session dropped from about 200k to 40k tokens of context in 14 ms, and
the next reply took 16 s instead of 107 s. One session and five questions is a small sample.

## Development

```sh
python3 -m unittest discover -s hermes/tests -v                 # standalone + CLI contract
ULTRACOMPRESS_BIN=/path/to/uc python3 -m unittest discover -s hermes/tests -v   # pinned binary
HERMES_AGENT_DIR=~/.hermes/hermes-agent <hermes python> -m unittest discover -s hermes/tests -v
```

`$ULTRACOMPRESS_BIN` points the CLI-contract tests at a specific binary, which is how they run
against a frozen build while the installed one is being rebuilt; without it they use the installed
binary and skip when there is none.

`<hermes python>` is the interpreter Hermes runs on, so its dependencies import. The
integration tests load the adapter through Hermes' own plugin loader in a temporary
`HERMES_HOME` and use a synthetic conversation; never commit real sessions.

Recent Hermes builds treat a fresh `HERMES_HOME` as an unfinished install. On import they
download their default tools into it (several minutes, over 1 GB) and rewrite the checkout's
`.hermes/bin` launchers to use that temporary Python. The tests put the launchers back on
teardown, but a run killed before teardown leaves `hermes` broken. Copy `.hermes/bin` before
running them, and give the run time to finish.

## Privacy

This adapter runs the local binary; it sends no data itself and has no telemetry.
The raw recall archive is stored at `$HERMES_HOME/ultracompress/<session>.jsonl`,
in a `0700` directory as a `0600` file, bounded to `archive_max_bytes` (64 MiB by
default). Delete it with `rm -rf "$HERMES_HOME/ultracompress"`. Your model
provider still receives the prompts Hermes sends it. The binary runs without a
terminal here, so its update check never runs. See the
[privacy statement](../PRIVACY.md) for the full local-file inventory.

## Uninstall

```sh
hermes config set context.engine compressor
rm ~/.hermes/plugins/ultracompress
```

The raw archive in `$HERMES_HOME/ultracompress/` is yours to keep or delete.
