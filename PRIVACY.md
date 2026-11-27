# Privacy and local data

UltraCompress compacts conversations locally: the brief is built by a Rust
binary with no model call, and the tool has no telemetry. This page is the full
statement; the [README](README.md#privacy) keeps the short form.

- **One automatic check, clear purpose.** UltraCompress makes no automatic network traffic
  except one anonymous check of this repository's public GitHub
  latest-release record, at most once per 24 hours per machine. If you opt into
  automatic updates, the background worker also downloads the release archive
  and `SHA256SUMS` without waiting for a command. Fixed
  `ultracompress-update-check` user agent; no install id, version, usage event,
  machine data, or credentials; nothing in your command's output.
- **No telemetry at all.** No analytics, crash reporting, unique install ID,
  machine fingerprint, or experiment assignment, and no dependency that reports
  on its own.
- **Your agent host is separate.** Prompts go to the model provider you
  configured, as usual, and locally generated snap-frame PNGs go to it when
  snapshots are enabled on a supported provider. UltraCompress chooses no
  provider and contacts none itself.
- **Installers you run** download the release archive and `SHA256SUMS` from
  GitHub over HTTPS, verify the SHA-256 before installing anything, and send no
  usage data. The macOS binary is signed and notarized.
- **Checks are off** in CI, non-TTY, and agent/headless sessions. Opt out with
  `ULTRACOMPRESS_NO_UPDATE_CHECK=1` or `updates.checkEnabled: false`. See
  [Updates](docs/updates.md) for the check and install contract.

## Local files, lifetimes, and deletion

Every path below is one of the tool's own writes. Permissions are owner-only:
directories `0700`, files `0600`. Cache root is `~/Library/Caches` on macOS,
`%LOCALAPPDATA%` on Windows, and `$XDG_CACHE_HOME` or `~/.cache` elsewhere;
config root is the platform config directory (`~/.config` on Linux/macOS with
`$XDG_CONFIG_HOME` honored). `<install dir>` below is where the installer put
the binary: `~/.ultracompress` on macOS/Linux and
`%LOCALAPPDATA%\UltraCompress` on Windows, or the `ULTRACOMPRESS_HOME` value
used at install time. Its shim lives in `~/.local/bin` (or `<install dir>\bin`
on Windows, or `ULTRACOMPRESS_BIN_DIR` if set).

| File | Contents | Lifetime | Delete |
| --- | --- | --- | --- |
| `~/.pi/agent/ultracompress.json` (Pi) | Settings only: policy, keep counts, update switches, `debug`. No session text. | Overwritten in place when you change a setting; kept until deleted. | `rm ~/.pi/agent/ultracompress.json` |
| `<project>/.steak-pi/snaps/snap-*.json` (Pi) | Full pre-compaction session text; the restorable safety net. | Pruned after every snapshot write to the newest 20 files within 64 MiB and at most 30 days old; the newest is always kept. The 30-day age limit applies on the next snapshot write, so an idle project keeps older snapshots until then. | `rm -rf <project>/.steak-pi` |
| `~/.local/state/ultracompress/debug.json` (Pi, `debug: true` only) | One diagnostics object, which can include a short summary preview. | Overwritten on each event, never appended; kept until the next event or deletion. | `rm -rf ~/.local/state/ultracompress` |
| `$HERMES_HOME/ultracompress/<session>.jsonl` (Hermes) | Condensed turns with tool output, for raw-history recall. | Trimmed after each write to the newest whole entries within `archive_max_bytes` (64 MiB default). | `rm -rf "$HERMES_HOME/ultracompress"` |
| `<cache root>/ultracompress/update.json` | Check timestamps, last error cause, the public latest-release record, skipped/shown versions, one auto-install attempt marker. No identifiers, no session text. | Rewritten in place per check; kept until you delete it. | `rm -rf <cache root>/ultracompress` |
| `<cache root>/ultracompress/check.claim` | One in-flight marker so quick commands share a single check. | Seconds: removed by the process that wrote it, reclaimable after a 30-second TTL. | Removed with the cache folder. |
| `<cache root>/ultracompress/install.log` | Auto-install outcome lines, cause only. | Bounded at 256 KB (oldest lines dropped); kept until deleted. | Removed with the cache folder. |
| `<cache root>/ultracompress/install/<tag>/` | The release archive, `SHA256SUMS`, and the unpacked tree for one version. | Removed at the end of every install, success or failure; a set abandoned by an interrupted install is swept after 7 days. | Removed with the cache folder. |
| `<install dir>/ultracompress.previous` | One rollback copy of the binary. | Deleted automatically once the replacement binary completes its first successful run, or seven days after the swap (or after the first sweep that observes a copy without a sidecar). The old binary's mtime is never used as its install time. | Removed automatically; `update --rollback` restores it first. |
| `<install dir>/ultracompress.previous.json` | The installed version string (or `observed` for a copy first seen without a sidecar) and one Unix second; nothing else. | Deleted with the rollback copy; a copy found without one gets a new sidecar recording when it was first observed. | Removed automatically. |
| `<config root>/ultracompress/config.json` | Your opt-in auto-install answer and whether you were asked. | Overwritten in place; kept until deleted. | `rm -rf <config root>/ultracompress` |

Claude Code is read-only here: the plugin reads the local transcript and writes
no files of its own; its transcript and logs belong to Claude Code.

Nothing else is written. UltraCompress puts no session text, credentials, or
downloads in `$TMPDIR`/`/tmp`; binary swaps use temp files beside the installed
binary and in the shared cache, and stale ones older than an hour are removed by the next successful command. The
only write inside your project is `.steak-pi/snaps/`, so add `.steak-pi/` to
that project's `.gitignore`.

## Automatic cleanup

After every successful command the CLI sweeps its own leftovers: the rollback
copy, the version sidecar, swap temp files, abandoned stage trees, and log temp
files. The sweep removes local files only; it does not change what the update
check sends, when it runs, or what it caches. Every file above also has a count,
byte, or age bound, so none of them grows without limit.

To remove everything the tool ever wrote, delete:

- macOS/Linux: the `~/.ultracompress` install folder and the
  `~/.local/bin/ultracompress` shim
  (`rm -rf ~/.ultracompress ~/.local/bin/ultracompress`).
- Windows: the `%LOCALAPPDATA%\UltraCompress` install folder and its
  `bin\ultracompress.cmd` shim
  (`Remove-Item -Recurse -Force "$env:LOCALAPPDATA\UltraCompress"`); also
  remove the shim's PATH entry if you added one.
- On every platform: the Pi snapshot folder, the Pi settings file, the Hermes
  archive, and the cache and config folders listed above. Use the
  `ULTRACOMPRESS_HOME`/`ULTRACOMPRESS_BIN_DIR` paths instead when you
  overrode them at install time.
