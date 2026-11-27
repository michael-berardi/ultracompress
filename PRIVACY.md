# Privacy and local data

UltraCompress builds the brief locally in a Rust binary, with no model call and
no telemetry: no analytics, crash reports, account, install ID or machine
fingerprint, and no dependency that reports on its own. The [README](README.md#privacy)
keeps the short summary; this page is the full statement.

## What goes over the network

- **One anonymous check.** UltraCompress makes no automatic network traffic
  apart from that check. After an interactive command, at most once every 24 hours, a detached
  process makes one HTTPS GET to `api.github.com/repos/<owner>/<repo>/releases/latest`
  with the fixed user agent `ultracompress-update-check`. It sends no version,
  usage event, machine data or credential; `curl -q` ignores `~/.curlrc`, so a
  config file cannot add headers or tokens. The result is one small cache file
  and never appears in the command's output.
- **Gates and opt-out.** The check does not run in a pipe, hook, JSON reader or
  non-terminal session, nor with `CI` or `CLAUDECODE` set. Turn it off with
  `ULTRACOMPRESS_NO_UPDATE_CHECK=1`, `"checkAutomatically": false` in the CLI
  config, or `"checkForUpdates": false` in Pi's settings. A manual
  `ultracompress update` still works. See [Updates](docs/updates.md).
- **Installs you ask for.** An update, or an installer you run yourself,
  downloads the release archive and `SHA256SUMS` from the same GitHub release
  over HTTPS, verifies the SHA-256 before installing, and sends no usage data.
  Automatic installs are opt-in and off by default.
- **Your model provider is separate.** Your agent host sends prompts to the
  provider you configured, as usual, and locally rendered snap-frame PNGs go to
  it when snap frames are enabled on a supported provider. UltraCompress chooses
  no provider and contacts none itself. README badges are fetched by the site
  rendering them, not by the tool.

## Local files, lifetimes and deletion

Every path below is one of this tool's own writes. Where the tool creates a
directory it is `0700` and new JSON files are `0600` on Unix; an existing loose
Pi directory is tightened on the next snapshot write. The cache root is
`~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows, and `$XDG_CACHE_HOME`
or `~/.cache` elsewhere; the config root is the platform config directory
(`~/.config`, with `$XDG_CONFIG_HOME` honored, or `%APPDATA%` on Windows).
`<install dir>` is where the installer put the binary: `~/.ultracompress` on
macOS/Linux, `%LOCALAPPDATA%\UltraCompress` on Windows, or the
`ULTRACOMPRESS_HOME` used at install time.

| File | Contents | Lifetime | Delete |
| --- | --- | --- | --- |
| `~/.pi/agent/ultracompress.json` (Pi) | Settings only: policy, keep counts, snapshot limits, the summary budget, `checkForUpdates` and `debug`. No session text. | Rewritten when a setting changes; kept until deleted. | `rm ~/.pi/agent/ultracompress.json` |
| `<project>/.steak-pi/snaps/snap-*.json` (Pi) | Full pre-compaction session text, the restorable safety net; can be large. | After each snapshot write, older files are pruned to the newest `snapshot.maxCount` (10 by default), within `snapshot.maxAgeDays` (14 days by default, configurable) and 64 MiB total; the newest is always kept. An idle project keeps older snapshots until its next write. | `rm -rf <project>/.steak-pi` |
| `~/.local/state/ultracompress/debug.json` (Pi, `debug: true` only) | One overwritten diagnostics object, which can include a short summary preview. | Never appended; kept until the next event or deletion. `$XDG_STATE_HOME` overrides the state root. | `rm -rf ~/.local/state/ultracompress` |
| `$HERMES_HOME/ultracompress/<session>.jsonl` (Hermes) | Condensed turns with tool output, for raw-history recall. | Trimmed after each write to the newest whole entries within `archive_max_bytes` (64 MiB default); the newest entry is kept. | `rm -rf "$HERMES_HOME/ultracompress"` |
| `<cache root>/ultracompress/update-check.json` (CLI) | Check timestamp, newest public release and notes, last failure cause, notice and auto-install markers, and the version the last self-update installed. No identifiers or session text. | Rewritten in place per check; kept until deleted. Older builds called this `update.json`. | `ultracompress update --clean` or `rm -rf <cache root>/ultracompress` |
| `<config root>/ultracompress/config.json` (CLI) | Your choices: `checkAutomatically`, `installAutomatically`, skipped version, whether the opt-in question was asked. | Rewritten in place; kept until deleted. | `ultracompress update --clean` or `rm -rf <config root>/ultracompress` |
| `<cache root>/ultracompress/update.log` (CLI) | One line per automatic-install outcome, cause only. | Oldest lines dropped above 256 KB; `update --auto off` deletes it. | `ultracompress update --clean` |
| `<install dir>/ultracompress.previous` | One rollback copy of the binary, for `update --rollback`. It keeps the installed binary's executable mode. | Deleted after the replacement's first successful command, or 7 days after the swap; a copy without a sidecar starts its 7 days when first observed, never from the old binary's mtime. | Removed automatically; `update --rollback` restores it first. |
| `<install dir>/ultracompress.previous.json` | The installed version string (or `observed` for a copy first seen without a sidecar) and one Unix second; nothing else. `0600` on Unix. | Deleted with the rollback copy, including on rollback. | Removed automatically or with `ultracompress update --clean`. |
| Install temp files: `<install dir>/ultracompress.next`, `.rollback`, state-file `.tmp` siblings and a `ultracompress-update-*` folder in the system temp directory | Staged binary or archive during an install; no session text. | Removed at the end of an install, success or failure. Stale swap and `.tmp` files are swept after an hour by the next successful command. | Swept automatically; otherwise remove with the install/cache folders. |

Claude Code is read-only here: the plugin reads the local transcript and writes
no files of its own; its transcript and logs belong to Claude Code. Nothing else
is written by UltraCompress. Binary swaps use temp files beside the installed
binary; downloads go to a private system-temp folder that is deleted whether
the install succeeds or fails. The only write inside your project is
`.steak-pi/snaps/`, so add `.steak-pi/` to that project's `.gitignore`.

## Automatic cleanup and full removal

After every successful command the CLI sweeps only its own leftovers: the
rollback copy once its replacement has run, plus stale swap and state temp
files. The check's timing, gates and cache format are unchanged. Every file
above has a count, byte or age bound, so none grows without limit.

To remove the update state, run `ultracompress update --clean`. To remove the
installation:

- macOS/Linux: delete `~/.ultracompress` and the `~/.local/bin/ultracompress`
  shim (`rm -rf ~/.ultracompress ~/.local/bin/ultracompress`).
- Windows: delete `%LOCALAPPDATA%\UltraCompress` and its `bin\ultracompress.cmd`
  shim (`Remove-Item -Recurse -Force "$env:LOCALAPPDATA\UltraCompress"`), plus
  any PATH entry you added.
- On every platform: delete the Pi snapshot folder, the Pi settings file, the
  optional Pi debug folder, the Hermes archive, and the cache and config
  folders listed above. Use `ULTRACOMPRESS_HOME`/`ULTRACOMPRESS_BIN_DIR` when
  you overrode the install paths.
