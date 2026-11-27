"""Update-contract rules for the Hermes adapter (no Hermes imports, no network by default).

The adapter starts the one anonymous check in a daemon thread from a live CLI
session and reports the result in `get_status()`. Failed checks are cached with
their cause; nothing is printed into agent turns or JSON tool results.

The cache is shared with the CLI and Pi: one `update.json` in the platform cache
directory, snake_case keys and integer Unix seconds. An old Pi cache (camelCase
keys, milliseconds) is migrated when it is read.
"""
from __future__ import annotations

import json
import math
import os
import re
import tempfile
import time
import urllib.request
from pathlib import Path
from typing import Any, Callable, Dict, Optional

VERSION = "0.8.3"
REPOSITORY = "https://github.com/michael-berardi/ultracompress"
CHECK_INTERVAL = 24 * 60 * 60
CHECK_TIMEOUT = 4.0
CACHE_FILE = "update.json"
USER_AGENT = "ultracompress-update-check"
OPT_OUT_ENV = "ULTRACOMPRESS_NO_UPDATE_CHECK"

DEFAULT_STATE: Dict[str, Any] = {
    "checked_at": 0,
    "last_error": None,
    "release": None,
    "skipped_version": None,
    "shown_version": None,
    "shown_at": 0,
    "auto_attempted_version": None,
}


def parse_version(text: str) -> Optional[tuple]:
    body = text[1:] if text.startswith("v") else text
    if not re.fullmatch(r"\d+\.\d+\.\d+", body):
        return None
    return tuple(int(part) for part in body.split("."))


def is_newer(latest: str, current: str) -> bool:
    new, old = parse_version(latest), parse_version(current)
    return bool(new and old and new > old)


def latest_release_url(repository: str = REPOSITORY) -> str:
    prefix = "https://github.com/"
    if not repository.startswith(prefix):
        raise ValueError(f"repository metadata is not a GitHub URL: {repository}")
    match = re.fullmatch(r"([^/]+)/([^/]+?)(?:\.git)?/?", repository[len(prefix):])
    if not match:
        raise ValueError(f"repository metadata has no owner/repo: {repository}")
    return f"https://api.github.com/repos/{match.group(1)}/{match.group(2)}/releases/latest"


def parse_release(body: str) -> Dict[str, str]:
    data = json.loads(body)
    if not isinstance(data, dict):
        raise ValueError("release response is not an object")
    if data.get("draft") is True:
        raise ValueError("latest release is a draft; ignored")
    if data.get("prerelease") is True:
        raise ValueError("latest release is a prerelease; ignored")
    tag = str(data.get("tag_name") or "")
    if not parse_version(tag):
        raise ValueError(f"release tag '{tag}' is not a stable x.y.z version")
    notes = data.get("body") or ""
    lines = [line for line in str(notes).splitlines() if line.strip()][:5]
    return {"version": tag[1:] if tag.startswith("v") else tag, "notes": "\n".join(lines)[:600]}


def sys_platform() -> str:
    import sys
    return sys.platform


def cache_path(state_dir: str = "", env: Optional[Dict[str, str]] = None,
               platform: Optional[str] = None, home: Optional[Path] = None) -> Path:
    """The same cache root as the CLI and Pi, with the same file name."""
    if state_dir:
        return Path(state_dir) / CACHE_FILE
    environ = os.environ if env is None else env
    current = platform or sys_platform()
    if environ.get("XDG_CACHE_HOME"):
        root = Path(environ["XDG_CACHE_HOME"])
    elif current == "win32" and environ.get("LOCALAPPDATA"):
        root = Path(environ["LOCALAPPDATA"])
    elif current == "darwin":
        root = (home or Path.home()) / "Library" / "Caches"
    else:
        root = (home or Path.home()) / ".cache"
    return root / "ultracompress" / CACHE_FILE


def now_seconds() -> int:
    return int(time.time())


def _stamp(data: Dict[str, Any], key: str, legacy: str) -> int:
    value = data.get(key, data.get(legacy))
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return 0
    if not math.isfinite(value) or value <= 0:
        return 0
    milliseconds = key not in data and legacy in data
    return int(value / 1000 if milliseconds else value)


def _text(data: Dict[str, Any], key: str, legacy: str) -> Optional[str]:
    value = data.get(key, data.get(legacy))
    return value if isinstance(value, str) else None


def normalise_state(data: Dict[str, Any]) -> Dict[str, Any]:
    """Read the canonical seconds schema, migrating the old Pi millisecond file."""
    release = data.get("release")
    parsed_release = None
    if isinstance(release, dict) and isinstance(release.get("version"), str):
        parsed_release = {
            "version": release["version"],
            "notes": release.get("notes") if isinstance(release.get("notes"), str) else "",
        }
    return {
        "checked_at": _stamp(data, "checked_at", "checkedAt"),
        "last_error": _text(data, "last_error", "lastError"),
        "release": parsed_release,
        "skipped_version": _text(data, "skipped_version", "skippedVersion"),
        "shown_version": _text(data, "shown_version", "shownVersion"),
        "shown_at": _stamp(data, "shown_at", "shownAt"),
        "auto_attempted_version": _text(data, "auto_attempted_version", "autoAttemptedVersion"),
    }


def load_state(path: Optional[Path] = None) -> Dict[str, Any]:
    target = path or cache_path()
    try:
        data = json.loads(target.read_text(encoding="utf-8"))
        if isinstance(data, dict):
            return normalise_state(data)
    except (OSError, ValueError):
        pass
    return dict(DEFAULT_STATE)


def save_state(state: Dict[str, Any], path: Optional[Path] = None) -> None:
    """Atomic, owner-only write of one canonical state object."""
    target = path or cache_path()
    target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        os.chmod(target.parent, 0o700)
    except OSError:
        pass
    fd, tmp = tempfile.mkstemp(prefix=f".{target.name}.", dir=target.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(normalise_state(state), handle, indent=2)
            handle.write("\n")
        os.chmod(tmp, 0o600)
        os.replace(tmp, target)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def check_due(state: Dict[str, Any], now: Optional[float] = None) -> bool:
    now = now_seconds() if now is None else now
    checked = state.get("checked_at") or 0
    return not checked or now - float(checked) >= CHECK_INTERVAL


def gate(env: Optional[Dict[str, str]] = None, interactive: bool = True) -> bool:
    environ = os.environ if env is None else env
    if environ.get(OPT_OUT_ENV) == "1":
        return False
    if environ.get("CI"):
        return False
    if environ.get("PI_HEADLESS") == "1" or environ.get("HERMES_HEADLESS") == "1":
        return False
    if environ.get("HERMES_PRINT_MODE") == "1" or environ.get("ULTRACOMPRESS_AGENT_MODE") == "1":
        return False
    return interactive


def fetch_release(url: str = "") -> Dict[str, str]:
    """The only network call in the adapter: one anonymous GET, no identifiers."""
    request = urllib.request.Request(
        url or latest_release_url(),
        headers={"User-Agent": USER_AGENT, "Accept": "application/vnd.github+json"},
        method="GET",
    )
    with urllib.request.urlopen(request, timeout=CHECK_TIMEOUT) as response:
        return parse_release(response.read(64 * 1024).decode("utf-8"))


def run_check(state: Dict[str, Any], path: Optional[Path] = None,
              fetcher: Optional[Callable[[], Dict[str, str]]] = None) -> Dict[str, Any]:
    """Check when due; a failure is recorded with its cause, never raised."""
    result = dict(state)
    now = now_seconds()
    if not check_due(result, now):
        return result
    try:
        result["release"] = (fetcher or fetch_release)()
        result["last_error"] = None
    except Exception as exc:  # network and parse failures are both recorded
        result["last_error"] = str(exc) or exc.__class__.__name__
    result["checked_at"] = now
    try:
        save_state(result, path)
    except OSError:
        pass
    return result


def showable(state: Dict[str, Any], release: Dict[str, str], current: str = VERSION,
             now: Optional[float] = None) -> bool:
    now = now_seconds() if now is None else now
    version = release["version"]
    if not is_newer(version, current):
        return False
    if state.get("skipped_version") == version:
        return False
    shown_at = state.get("shown_at") or 0
    if state.get("shown_version") == version and now - float(shown_at) < CHECK_INTERVAL:
        return False
    return True


def mark_shown(state: Dict[str, Any], version: str, now: Optional[float] = None) -> None:
    state["shown_version"] = version
    state["shown_at"] = now_seconds() if now is None else int(now)


def skip(state: Dict[str, Any], version: Optional[str]) -> None:
    state["skipped_version"] = version


def notice(release: Dict[str, str], current: str = VERSION) -> str:
    return (f"ultracompress {release['version']} available (you have {current}) · run: ultracompress update\n"
            "turn on automatic updates: ultracompress update --auto on · skip: ultracompress update --skip")


def check_report(release: Optional[Dict[str, str]], error: Optional[str], current: str = VERSION) -> str:
    if release and is_newer(release["version"], current):
        return f"ultracompress {release['version']} available (you have {current}) · run: ultracompress update"
    if error:
        return f"ultracompress update check failed: {error}"
    return f"ultracompress is up to date ({current})"
