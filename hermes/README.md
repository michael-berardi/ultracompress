# UltraCompress for Hermes Agent

**Local compaction for [Hermes Agent](https://github.com/NousResearch/hermes-agent). A
deterministic brief instead of a model-written summary, and a recall tool that still finds
what was condensed.**

[Install](#install) · [What it does](#what-it-does) · [Settings](#settings) · [Fallback](#fallback-behaviour) · [Measured](#measured) · [Uninstall](#uninstall)

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
(`$HERMES_HOME/ultracompress/<session>.jsonl`). The `ultracompress_recall` tool searches that
archive with ranked keywords or a regular expression. If Hermes rotates the session id at a
compaction boundary, the archive follows it.

It also changes when compaction fires. Hermes raises the trigger to 75% of the window for any
model under 512k tokens, because a model-written summary is slow and expensive to repeat. A local
brief takes milliseconds, so this engine uses the threshold you set (30% by default).

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

First regular file wins: `binary` in settings, `$ULTRACOMPRESS_BIN`,
`~/.local/bin/ultracompress`, `~/.ultraterm/bin/ultracompress`,
`/opt/homebrew/bin/ultracompress`, `/usr/local/bin/ultracompress`.

## Settings

Optional `settings.json` next to `__init__.py`:

```json
{ "mode": "deterministic", "threshold": 0.30 }
```

| Key | Default | Meaning |
| --- | --- | --- |
| `mode` | `deterministic` | `builtin` makes the engine behave exactly like Hermes' own compressor |
| `threshold` | `0.30` | Compaction trigger as a fraction of the context window |
| `binary` | `""` | Explicit path to `ultracompress` |
| `archive_dir` | `""` | Raw archive location; empty means `$HERMES_HOME/ultracompress` |
| `tool_result_chars` | `0` | Characters of each tool result to include in the brief (0 = none) |
| `reply_chars` / `replies_budget_chars` | `1200` / `12000` | Size of each reply excerpt and of the whole replies section |
| `carry_users_chars` | `4000` | Earlier windows' verbatim user quotes kept in later summaries |
| `timeout_seconds` | `30` | Limit for one `ultracompress compact` run |

## Fallback behaviour

If the binary is missing, fails, times out, or returns nothing, the engine logs one line naming
the cause and Hermes' built-in summary runs for that compaction. A failure never costs the
session. Recall failures come back as readable tool results.

## Measured

One real Hermes Telegram session (GPT-6.1 Sol, 245 messages, about 190k tokens), compacted from
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
HERMES_AGENT_DIR=~/.hermes/hermes-agent <hermes python> -m unittest discover -s hermes/tests -v
```

`<hermes python>` is the interpreter Hermes runs on, so its dependencies import. The
integration tests load the adapter through Hermes' own plugin loader in a temporary
`HERMES_HOME` and use a synthetic conversation; never commit real sessions.

## Uninstall

```sh
hermes config set context.engine compressor
rm ~/.hermes/plugins/ultracompress
```

The raw archive in `$HERMES_HOME/ultracompress/` is yours to keep or delete.
