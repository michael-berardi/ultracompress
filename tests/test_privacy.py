"""Privacy regression guards: no telemetry, one documented network request, private bounded storage."""
import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

UPDATE_RS = ROOT / "crates" / "ultracompress-cli" / "src" / "update.rs"

RUNTIME_FILES = (
    sorted((ROOT / "crates").glob("*/src/*.rs"))
    + sorted((ROOT / "extension" / "src").glob("*.ts"))
    + sorted((ROOT / "hermes" / "ultracompress").glob("*.py"))
    + [ROOT / "extension" / "index.ts", ROOT / "claude-code" / "hooks" / "ultracompress.mjs"]
)

BANNED_PACKAGES = re.compile(
    r"(?i)(reqwest|ureq|sentry|bugsnag|posthog|@segment/|segmentio|mixpanel|amplitude|"
    r"datadog|opentelemetry|google-analytics|\bgtag\.js\b|googletagmanager|"
    r"firebase|plausible|heap\.io)"
)

TELEMETRY_ENDPOINTS = re.compile(
    r"(?i)(sentry\.io|api\.segment\.io|api\.mixpanel\.com|app\.posthog\.com|"
    r"api\.amplitude\.com|api\.datadoghq\.com|google-analytics\.com|"
    r"stats\.g\.doubleclick\.net|analytics\.google\.com|api\.heap\.io)"
)

URL = re.compile(r"https?://[^\s\"')]+")

# The update check is the only request: the public release record of this repository.
UPDATE_URLS = {"https://github.com/", "https://api.github.com/repos/{slug}/releases/latest"}


def _text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _without_rust_tests(text: str) -> str:
    """Runtime code only: unit tests (fixtures with example URLs) follow `#[cfg(test)]`."""
    return text.split("#[cfg(test)]", 1)[0]


def _section(text: str, heading: str) -> str:
    return text.split(heading, 1)[1].split("\n## ", 1)[0]


class PrivacyTest(unittest.TestCase):
    def test_no_telemetry_dependencies_in_manifests(self):
        for manifest in (ROOT / "Cargo.toml", ROOT / "Cargo.lock"):
            with self.subTest(manifest=manifest.name):
                self.assertIsNone(BANNED_PACKAGES.search(_text(manifest)))
        package = json.loads(_text(ROOT / "extension" / "package.json"))
        declared = set(package.get("dependencies", {})) | set(package.get("devDependencies", {}))
        lock = json.loads(_text(ROOT / "extension" / "package-lock.json"))
        installed = {name for name in lock.get("packages", {}) if name.startswith("node_modules/")}
        for name in declared | installed:
            with self.subTest(package=name):
                self.assertIsNone(BANNED_PACKAGES.search(name))

    def test_runtime_has_no_http_clients_or_telemetry_endpoints(self):
        for path in RUNTIME_FILES:
            with self.subTest(path=str(path.relative_to(ROOT))):
                text = _text(path)
                self.assertIsNone(BANNED_PACKAGES.search(text))
                self.assertIsNone(TELEMETRY_ENDPOINTS.search(text))
                self.assertNotRegex(text, r"\b(std::net|TcpStream|urllib\.request|XMLHttpRequest|WebSocket)\b")
                self.assertNotRegex(text, r"\bfetch\(")
                urls = set(URL.findall(_without_rust_tests(text) if path.suffix == ".rs" else text))
                if path == UPDATE_RS:
                    self.assertEqual(urls, UPDATE_URLS)
                else:
                    self.assertEqual(urls, set())

    def test_update_check_is_anonymous_https_with_opt_out(self):
        update = _without_rust_tests(_text(UPDATE_RS))
        self.assertIn('Command::new("curl")', update)
        self.assertIn('"-q"', update)  # ~/.curlrc cannot add headers or tokens
        self.assertIn('"=https"', update)
        self.assertIn("ULTRACOMPRESS_NO_UPDATE_CHECK", update)
        self.assertIn("ultracompress-update-check", update)

    def test_installers_verify_checksums_before_installing(self):
        sh = _text(ROOT / "scripts" / "install.sh")
        ps1 = _text(ROOT / "scripts" / "install.ps1")
        self.assertIn("SHA256SUMS", sh)
        self.assertIn("checksum mismatch", sh)
        self.assertRegex(sh, r"sha256sum|shasum")
        self.assertIn("SHA256SUMS", ps1)
        self.assertIn("checksum mismatch", ps1)
        self.assertIn("Get-FileHash", ps1)

    def test_readme_privacy_is_short_and_links_the_details(self):
        readme = _text(ROOT / "README.md")
        self.assertEqual(readme.count("\n## Privacy\n"), 1)
        privacy = _section(readme, "\n## Privacy\n")
        lines = [line for line in privacy.splitlines() if line.strip()]
        self.assertLessEqual(len(lines), 6, privacy)
        for phrase in ("No telemetry", "update check", "ULTRACOMPRESS_NO_UPDATE_CHECK=1", "Stored locally",
                       "(PRIVACY.md)"):
            self.assertIn(phrase, privacy)

    def test_privacy_md_states_the_details(self):
        privacy = _text(ROOT / "PRIVACY.md")
        for phrase in ("no telemetry", "api.github.com", "at most once every 24 hours",
                       "ULTRACOMPRESS_NO_UPDATE_CHECK=1", "`CI`", "SHA256SUMS", "SHA-256", "model provider",
                       "~/.pi/agent/ultracompress.json", ".steak-pi/snaps", "~/.local/state/ultracompress",
                       "$HERMES_HOME/ultracompress", "archive_max_bytes", "0700", "0600", "64 MiB", "rm -rf",
                       "update --clean"):
            self.assertIn(phrase, privacy)

    def test_no_doc_denies_the_update_check(self):
        docs = [ROOT / "README.md", ROOT / "PRIVACY.md", ROOT / "SECURITY.md", ROOT / "claude-code" / "README.md",
                ROOT / "hermes" / "README.md", ROOT / "docs" / "updates.md"]
        for path in docs:
            with self.subTest(path=str(path.relative_to(ROOT))):
                text = _text(path).lower()
                for wrong in ("no update check", "makes no update check", "sends nothing over the network",
                              "never checks for updates"):
                    self.assertNotIn(wrong, text)

    def test_private_storage_and_retention_guards_present(self):
        snapshot = _text(ROOT / "extension" / "src" / "snapshot.ts")
        settings = _text(ROOT / "extension" / "src" / "settings.ts")
        debug = _text(ROOT / "extension" / "src" / "debug.ts")
        hermes = _text(ROOT / "hermes" / "ultracompress" / "convert.py")
        for text in (snapshot, settings, debug):
            self.assertIn("0o700", text)
            self.assertIn("0o600", text)
        self.assertIn("pruneSnaps", snapshot)
        self.assertIn("MAX_SNAPS_BYTES", snapshot)
        self.assertNotIn("ultracompress-debug.json", debug + _text(ROOT / "extension" / "index.ts"))
        self.assertIn("trim_jsonl_archive", hermes)
        self.assertIn("archive_max_bytes", _text(ROOT / "hermes" / "ultracompress" / "__init__.py"))
        self.assertIn("0o600", _without_rust_tests(_text(UPDATE_RS)))

    def test_adapters_report_their_own_versions(self):
        index = _text(ROOT / "extension" / "index.ts")
        self.assertIn("ultracompress ${packageVersion()}", index)  # read from package.json, never hard-coded
        plugin_yaml = _text(ROOT / "hermes" / "ultracompress" / "plugin.yaml")
        init = _text(ROOT / "hermes" / "ultracompress" / "__init__.py")
        yaml_version = re.search(r"^version:\s*(\S+)", plugin_yaml, re.M).group(1)
        self.assertIn(f'__version__ = "{yaml_version}"', init)


if __name__ == "__main__":
    unittest.main()
