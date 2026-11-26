# Privacy

UltraCompress runs on your machine. It has no account and sends no telemetry,
analytics, crash reports, unique install IDs or machine fingerprints; no
dependency with its own telemetry is part of it. Compaction, recall and snap
frames make no network call: the CLI reads the session file or stdin you give
it, writes JSON to stdout and errors to stderr. The only request it makes is
the update check below.

## The update check

The CLI makes one kind of request: a check for a newer release.

- **What:** one anonymous HTTPS GET to the public GitHub record
  `api.github.com/repos/<owner>/<repo>/releases/latest`, where owner/repo come
  from the package metadata. The fixed User-Agent is `ultracompress-update-check`.
  No id, version, usage or machine data is sent; `curl` runs with `-q`, so a
  `~/.curlrc` cannot add headers or tokens.
- **When:** at most once every 24 hours, in a background process after an
  interactive terminal command, or when you run `ultracompress update`.
  A failed check waits a day too.
- **Never:** when stdout or stderr is not a terminal (agent hooks, pipes, JSON
  readers), when `CI` or `CLAUDECODE` is set, or when you turn it off.
- **Off switch:** `ULTRACOMPRESS_NO_UPDATE_CHECK=1`, or
  `"checkAutomatically": false` in the settings file below. In Pi,
  `"checkForUpdates": false` in `~/.pi/agent/ultracompress.json` also hides the
  footer line. `ultracompress update` still works when you run it yourself.

An update downloads the release archive and its `SHA256SUMS` from the same
GitHub release. Nothing about you is sent with those downloads either.

## Installers

When **you** run an installer, it downloads the release archive and
`SHA256SUMS` from GitHub over HTTPS, verifies the SHA-256 before installing,
and sends no usage data. The macOS binary is also signed and notarized.

## Your model provider

Your agent host may send data to a model provider you configured: prompts go to
that provider as usual, including the requests before and after compaction, and
snap-frame PNGs generated locally go to it when snap frames are enabled on a
supported provider. UltraCompress chooses no provider and contacts none itself.
The badges on the README are fetched by the site rendering it, not by the tool.

## Files on your machine

### The CLI's update files

| File | Contents |
| --- | --- |
| `update-check.json` in the cache folder | time of the last check, newest version and the first lines of its notes, the last failure cause, when a notice was shown |
| `config.json` in the settings folder | your choices: checks on or off, automatic installs on or off, the skipped version |
| `update.log` in the cache folder | only with automatic installs on: one line per install, kept under 256 KB |
| `<binary>.previous` next to the binary | the binary before the last update, removed after the new one has run once (7 days at most) |

The cache folder is `~/Library/Caches/ultracompress` (macOS),
`$XDG_CACHE_HOME/ultracompress` or `~/.cache/ultracompress` (Linux) or
`%LOCALAPPDATA%\ultracompress` (Windows); the settings folder is
`~/Library/Application Support/ultracompress`, `$XDG_CONFIG_HOME/ultracompress`
or `~/.config/ultracompress`, or `%APPDATA%\ultracompress`. Both files are
written atomically and readable only by you. Downloads go to a temporary folder
that is deleted whether the update succeeds or fails.

`ultracompress update --clean` removes all of these files.

### Agent adapters

- **Pi** writes `~/.pi/agent/ultracompress.json` (settings) and full
  pre-compaction snapshots in `<project>/.steak-pi/snaps/`. Snapshots can
  contain session text. Directories are `0700` and files `0600`. The newest
  `snapshot.maxCount` snapshots (10 by default) from the last
  `snapshot.maxAgeDays` days (14) are kept, within 64 MiB in total; the oldest
  go first, and the newest is kept even if it alone is larger. The folder is
  inside your project, so add `.steak-pi/` to its `.gitignore`. Delete them
  with `rm -rf <project>/.steak-pi/snaps` and `rm ~/.pi/agent/ultracompress.json`.
- With `debug: true`, **Pi** overwrites one diagnostics file at
  `~/.local/state/ultracompress/debug.json` (`$XDG_STATE_HOME` overrides the
  state root). It is `0600` in a `0700` directory and can contain a short
  summary preview. Delete it with `rm -rf ~/.local/state/ultracompress`.
- **Claude Code**: the plugin reads the local transcript and writes no files of
  its own; the transcript and logs belong to Claude Code.
- **Hermes** writes condensed turns, including tool output, to
  `$HERMES_HOME/ultracompress/<session>.jsonl` for recall. The directory is
  `0700`, the file `0600`, and the newest whole entries within 64 MiB are kept
  (the newest entry is kept even if it alone is larger). Set
  `archive_max_bytes` in the adapter's `settings.json` to change the budget.
  Delete the archive with `rm -rf "$HERMES_HOME/ultracompress"`.

To remove an install, run `ultracompress update --clean`, then delete
`~/.ultracompress` (or `%LOCALAPPDATA%\UltraCompress`) and the shim.
