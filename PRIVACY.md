# Privacy

UltraCompress runs on your machine. It sends no telemetry, has no account, and
compaction, recall and snap frames make no network call. The model requests
before and after compaction are made by your agent host, not by UltraCompress.

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

## Files on your machine

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
