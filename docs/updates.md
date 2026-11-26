<!-- update-contract: v1 -->
# Updates

UltraCompress tells you when a newer stable release exists and installs it
with one command. Automatic installs are opt-in and off by default. What the
check sends and which files it keeps: [PRIVACY.md](../PRIVACY.md).

## The notice

After an interactive command finishes, at most once a day per version, on stderr:

```text
ultracompress 0.9.0 available (you have 0.8.0) · run: ultracompress update
turn on automatic updates: ultracompress update --auto on · skip: ultracompress update --skip
```

The check runs in a background process after the command, with a 4-second
timeout, at most once every 24 hours; the notice appears on a later command.
Only stable `x.y.z` releases that are newer than yours count; drafts,
prereleases and other tags are ignored.

There is no notice and no check when stdout or stderr is not a terminal, which
covers agent hooks, pipes and JSON output, when `CI` or `CLAUDECODE` is set,
with `ULTRACOMPRESS_NO_UPDATE_CHECK=1`, or with `"checkAutomatically": false`
in `config.json`.

Agent hosts:

- **Pi:** the notice is a footer line in interactive sessions
  (`ultracompress 0.9.0 available (you have 0.8.0) · run: /ultracompress-update`).
  `/ultracompress-update` installs; `/ultracompress-update check`, `skip` and
  `auto on|off|status` run the commands below. `"checkForUpdates": false` in
  `~/.pi/agent/ultracompress.json` hides the line.
- **Claude Code and Hermes:** their hooks run the CLI without a terminal, so
  they never show a notice. Run `ultracompress update --check` in a terminal.

## Commands

| Command | What it does |
| --- | --- |
| `ultracompress update` | Checks now, shows current → new and the first lines of the release notes, asks `Install 0.9.0 now? [Y/n]` on a terminal, then installs. Without a terminal it installs without asking: running the command is consent. |
| `ultracompress update --check` | Reports only. Exit 0: up to date. Exit 10: update available. Exit 1: the check failed; the cause is printed and kept in the cache. |
| `ultracompress update --skip` | No notice for this version; a newer release notifies again. |
| `ultracompress update --auto on\|off\|status` | Turns automatic installs on or off, or shows the setting. `off` also deletes the install log. |
| `ultracompress update --rollback` | Puts back the binary from before the last update and skips the version you rolled back from. |
| `ultracompress update --clean` | Removes the update cache, settings, log and rollback copy, for uninstalling. |
| `ultracompress update --yes` | Installs without asking (used by `/ultracompress-update` in Pi). |

After the first successful install on a terminal you are asked once:
`Install future updates automatically? [y/N]`. The answer is kept; after "n"
the question does not come back.

## Automatic installs

With `update --auto on`, the background check that finds a new release also
installs it, at low priority, and logs one line per step to `update.log`
(kept under 256 KB). The new version runs from the next command; a command
that is running is not touched. If the install fails, the current binary keeps
working, `update --check` shows the cause, and the notice returns. One failed
version is not retried automatically; `ultracompress update` installs it by hand.

## Install safety

- The release archive for your platform and `SHA256SUMS` come from the same
  GitHub release. The archive's SHA-256 must match before it is unpacked, and
  the unpacked binary must run and report the new version before anything is
  replaced. The macOS binary is also signed and notarized by the release build.
- The new binary replaces the old one with one rename. The old one is kept as
  `<binary>.previous` for `update --rollback` and removed after the new
  version has run successfully once, or after 7 days. There is never more than
  one.
- Downloads go to a temporary folder that is deleted on success and on failure.
- Installs owned by a package manager are not overwritten: under Homebrew the
  command is `brew upgrade ultracompress`, under `~/.cargo/bin` it is
  `cargo install --locked --git <repository> --tag v<new> ultracompress`.
  `update --check` names the command and `update` runs it.

Network access uses `curl`, which the installers already need, so the CLI has
no HTTP library.

## Testing against a fake release

`ULTRACOMPRESS_UPDATE_API` points the check at another release record (it also
allows plain `http://`); `ULTRACOMPRESS_CACHE_DIR` and
`ULTRACOMPRESS_CONFIG_DIR` move the state files. The tests in
`crates/ultracompress-cli/tests/update.rs` serve a fake release from a loopback
server and never contact GitHub.
