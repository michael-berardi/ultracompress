# UltraCompress updates

<!-- update-contract: v1 -->

UltraCompress tells you when a newer public release exists. Checks are on by
default in interactive use, anonymous, at most once per 24 hours per machine,
and never part of your command's output. Installing is always opt-in.

## What the check sends

There is exactly one GET per check, to the public GitHub "latest release"
record of this repository (`/repos/<owner>/<repo>/releases/latest`). The
owner/repo comes from the package's own repository metadata. The request uses
the fixed user agent `ultracompress-update-check` and sends no install id,
version, usage event, machine fingerprint, credential, or query parameter.
Drafts, prereleases, and tags that are not stable `x.y.z` are ignored. Only a
strictly newer stable version produces a notice.

The check has a four-second ceiling and runs after your command has finished
(in a detached worker for the CLI, in a daemon thread for Hermes, and from the
Pi session-start hook). It never sits on a startup path in front of your work.
The result is cached atomically in a `0600` file in the platform cache
directory; a failed check records its cause there, is never printed as noise,
and is not retried in a loop. An exclusive, short-lived claim file in the same
directory means quick successive commands share one in-flight check instead of
starting several anonymous requests.

### One shared cache

The CLI, Pi and Hermes all use `<cache root>/ultracompress/update.json`, where
the root is `$XDG_CACHE_HOME`, `%LOCALAPPDATA%` on Windows,
`~/Library/Caches` on macOS, or `~/.cache` otherwise. The file has one schema:
`checked_at`, `shown_at`, `last_error`, `release`, `skipped_version`,
`shown_version` and `auto_attempted_version`. Timestamps are **integer Unix
seconds** (not milliseconds) and `CHECK_INTERVAL_SECS = 86_400` everywhere, so
whichever adapter checks first suppresses the check in the others for 24 hours.
A cache from an older Pi adapter that used camelCase keys and milliseconds is
converted on read; it is rewritten in the shared format on the next check.

## When checks are off

Checks are off automatically when stdout/stderr are not a terminal (CLI), in
`CI`, in a headless/print/agent-mode session, in a Pi session without a live
UI, or when you opt out:

```sh
ULTRACOMPRESS_NO_UPDATE_CHECK=1 ultracompress stats --session session.jsonl
```

The same opt-out works for the Pi and Hermes adapters. The adapters also read
`updates.checkEnabled` in `~/.pi/agent/ultracompress.json` (Pi) and
`updates.checkEnabled` in the Hermes adapter's `settings.json`.

## CLI commands

```sh
ultracompress update --check          # report only: 0 current, 10 available, 1 failed
ultracompress update                  # current → new, release notes, then install on a TTY
ultracompress update --skip           # hide this version's notice until a newer one
ultracompress update --auto on        # opt in to background installation
ultracompress update --auto off       # opt back out
ultracompress update --auto status    # report the persisted choice
ultracompress update --rollback       # restore the one kept previous binary
```

`update --check` prints one line and never installs. In a terminal, `update`
asks `Install <new> now? [Y/n]`, then asks `Install future updates
automatically? [y/N]` only once after a successful install; answering `n` is
remembered. Without a TTY, an explicit `update` installs without questions
because the command itself is consent.

A notice appears at most once per 24 hours per version. Skipping a version
hides that version only; a newer release notifies again. The notice reads:

```text
ultracompress 0.8.4 available (you have 0.8.3) · run: ultracompress update
turn on automatic updates: ultracompress update --auto on · skip: ultracompress update --skip
```

For Pi, the same notice arrives as a session-start notification, and
`/ultracompress-update [--check|--skip|--auto on|off|status]` gives the same
controls. Hermes shows the available version and the last check cause in the
context engine's status; a live CLI session also logs the notice once. Neither
adapter puts it inside JSON tool output.

## Automatic installation (opt-in only)

Automatic installation is off by default and is stored in the CLI config
(`~/.config/ultracompress/config.json`, or the platform config directory), so
it survives updates. When it is on, a discovered release is installed in a
detached, low-priority worker; the new binary applies on the next start. The
worker writes to `install.log` in the update cache directory. That log is one
bounded file (256 KB maximum; the oldest lines are dropped), written `0600`.
On platforms that support it (`nice`), the auto-install worker runs at reduced
priority so it never competes with your command. The download, archive and
unpacked staging tree under the shared cache is removed after both success and
failure; only the rollback copy of the binary is kept, and it has a lifetime
(see [Retention in contract §4a](#retention-in-contract-4a)). A failure leaves
the current binary working, records the cause, and falls back to the notice. An
environment variable never turns auto-install on.

## Install safety

The CLI fetches `SHA256SUMS` and the platform release asset and verifies the
SHA-256 before replacing anything. It checks the downloaded binary's own
`--version`, swaps it atomically, and keeps exactly one `*.previous` copy for
`update --rollback`, which the retention sweep later removes. Package-manager installs name the command with the new
release version; staged downloads and unpacked trees are always cleaned up. Package-manager installs (Cargo, Homebrew, npm) are
detected and are never overwritten: the command names the right manager
command instead.

### Retention in contract §4a

Contract §4a asks for the rollback copy to be deleted after the new version has
run successfully once, and at the latest within seven days. Retained state now
carries a lifetime:

- A successful swap writes `*.previous` plus a `*.previous.json` sidecar holding
  the installed version string and one Unix second. The first successful
  command run by the replacement binary removes both; if the replacement never
  runs, the copy expires seven days after the swap. A copy found without a
  sidecar gets one on first observation, with version `observed` and the Unix
  second it was seen, starting the same seven-day clock: the binary mtime
  belongs to the old binary and is never used to expire it early.
  `update --rollback` restores the copy and drops the sidecar, after which the
  age rule runs from the next observation.
- Swap temp files beside the binary (`.ultracompress-new-*`, `*.rollback-tmp`),
  half-written `install.log` temp files, and abandoned stage trees under
  `<cache>/ultracompress/install/` are swept by the same pass: an hour for temp
  files, seven days for a stage tree. The shared `update.json`, the bounded
  `install.log` and the opt-in config stay.
- The sweep runs after a successful command and removes local files only. What
  the check sends, when it runs, its gates and its cache format are unchanged.
- `ultracompress update --auto off` still only writes `auto_install = false` to
  the config; it does not delete the rollback copy, which the sweep handles on
  age.

## Privacy documentation layout (round-5 N5, resolved)

The 2026-11-26 contract asks for a short Privacy notice. `README.md` now keeps
the short form: three lines that state the single anonymous check, the absence of
telemetry, the owner-only permissions and the bounded retention, and that link
[`PRIVACY.md`](../PRIVACY.md). All detail lives there as one table of every local
write with its contents, its lifetime and its deletion command, so the short
form no longer competes with the detailed one.

Snapshot pruning now also expires full-session snapshots after 30 days, always
keeping the newest. The age limit is applied on the next snapshot write, so an
idle project keeps older snapshots until then. The Hermes recall archive keeps
its `archive_max_bytes` bound. Neither change alters retention of the shared
update cache.

## Compatibility notes

The Rust core is a pure library: it owns the version comparison, cache,
gates, and persistence rules but opens no connection. Snapshot pruning and
the Hermes recall archive bounds are unchanged by the update work.
