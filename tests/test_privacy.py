"""Privacy regression guards: no telemetry packages/endpoints or default network calls."""
import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

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

BANNED_ENDPOINTS = re.compile(
    r"(?i)(sentry\.io|api\.segment\.io|api\.mixpanel\.com|app\.posthog\.com|"
    r"api\.amplitude\.com|api\.datadoghq\.com|google-analytics\.com|"
    r"stats\.g\.doubleclick\.net|analytics\.google\.com|api\.heap\.io|"
    r"https?://[^\s\"')]+)"
)


def _text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


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
                self.assertIsNone(BANNED_ENDPOINTS.search(text))
                self.assertNotRegex(text, r"\b(std::net|TcpStream|urllib\.request|XMLHttpRequest|WebSocket)\b")

    def test_installers_verify_checksums_before_installing(self):
        sh = _text(ROOT / "scripts" / "install.sh")
        ps1 = _text(ROOT / "scripts" / "install.ps1")
        self.assertIn("SHA256SUMS", sh)
        self.assertIn("checksum mismatch", sh)
        self.assertRegex(sh, r"sha256sum|shasum")
        self.assertIn("SHA256SUMS", ps1)
        self.assertIn("checksum mismatch", ps1)
        self.assertIn("Get-FileHash", ps1)

    def test_readme_states_privacy_behaviour(self):
        readme = _text(ROOT / "README.md")
        self.assertIn("## Privacy", readme)
        privacy = readme.split("## Privacy", 1)[1].split("\n## ", 1)[0]
        for phrase in ("sends nothing over the network", "no telemetry", "0700", "0600",
                       "64 MiB", "rm -rf", "SHA-256", "model provider"):
            self.assertIn(phrase, privacy)

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
        self.assertNotIn("/tmp/ultracompress-debug.json", debug)
        self.assertIn("trim_jsonl_archive", hermes)
        self.assertIn("archive_max_bytes", _text(ROOT / "hermes" / "ultracompress" / "__init__.py"))

    def test_pi_adapter_reports_its_package_version(self):
        package = _text(ROOT / "extension" / "package.json")
        index = _text(ROOT / "extension" / "index.ts")
        version = re.search(r'"version":\s*"([^"]+)"', package).group(1)
        self.assertIn(f'ADAPTER_VERSION = "{version}"', index)


if __name__ == "__main__":
    unittest.main()
