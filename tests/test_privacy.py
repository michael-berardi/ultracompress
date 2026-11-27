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
    r"(?i)(reqwest|sentry|bugsnag|posthog|@segment/|segmentio|mixpanel|amplitude|"
    r"datadog|opentelemetry|google-analytics|\bgtag\.js\b|googletagmanager|"
    r"firebase|plausible|heap\.io)"
)

# One HTTP client is allowed by the 2026-11-26 update contract, and only in the
# CLI's update module and its manifests. Everything else stays banned.
UPDATE_HTTP_PACKAGE = re.compile(r"(?i)\bureq\b")
UPDATE_FILES = {
    "Cargo.toml", "Cargo.lock", "crates/ultracompress-cli/Cargo.toml",
    "crates/ultracompress-cli/src/update.rs",
}

BANNED_ENDPOINTS = re.compile(
    r"(?i)(sentry\.io|api\.segment\.io|api\.mixpanel\.com|app\.posthog\.com|"
    r"api\.amplitude\.com|api\.datadoghq\.com|google-analytics\.com|"
    r"stats\.g\.doubleclick\.net|analytics\.google\.com|api\.heap\.io|"
    r"https?://[^\s\"')]+)"
)

# The only hosts allowed in the three update modules: the repo's own GitHub
# metadata and its public releases API. No other URL may appear at runtime.
UPDATE_ALLOWED_HOSTS = re.compile(
    r"https://(?:api\.)?github\.com/[^\s\"'`)]*"
)
UPDATE_MODULES = {
    "crates/ultracompress-core/src/update.rs",
    "crates/ultracompress-cli/src/update.rs",
    "extension/src/update.ts",
    "extension/src/metadata.ts",
    "hermes/ultracompress/update.py",
}


def _text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


class PrivacyTest(unittest.TestCase):
    def test_no_telemetry_dependencies_in_manifests(self):
        for manifest in (ROOT / "Cargo.toml", ROOT / "Cargo.lock"):
            with self.subTest(manifest=manifest.name):
                text = _text(manifest)
                self.assertIsNone(BANNED_PACKAGES.search(text))
                if manifest.name == "Cargo.lock":
                    self.assertIsNotNone(UPDATE_HTTP_PACKAGE.search(text))
        cli_manifest = _text(ROOT / "crates/ultracompress-cli/Cargo.toml")
        self.assertIsNotNone(UPDATE_HTTP_PACKAGE.search(cli_manifest))
        package = json.loads(_text(ROOT / "extension" / "package.json"))
        declared = set(package.get("dependencies", {})) | set(package.get("devDependencies", {}))
        lock = json.loads(_text(ROOT / "extension" / "package-lock.json"))
        installed = {name for name in lock.get("packages", {}) if name.startswith("node_modules/")}
        for name in declared | installed:
            with self.subTest(package=name):
                self.assertIsNone(BANNED_PACKAGES.search(name))

    def test_runtime_has_no_http_clients_or_telemetry_endpoints(self):
        for path in RUNTIME_FILES:
            rel = str(path.relative_to(ROOT))
            with self.subTest(path=rel):
                text = _text(path)
                self.assertIsNone(BANNED_PACKAGES.search(text))
                if rel not in UPDATE_FILES:
                    self.assertIsNone(UPDATE_HTTP_PACKAGE.search(text))
                if rel in UPDATE_MODULES:
                    # Strip only the tool's own GitHub hosts; a different host stays visible.
                    text = UPDATE_ALLOWED_HOSTS.sub("", text)
                self.assertIsNone(BANNED_ENDPOINTS.search(text))
                clients = r"\b(std::net|TcpStream|XMLHttpRequest|WebSocket)\b"
                if rel != "hermes/ultracompress/update.py":
                    clients += r"|\burllib\.request\b"
                self.assertNotRegex(text, clients)
                if rel == "crates/ultracompress-cli/src/update.rs":
                    self.assertIn("USER_AGENT", text)
                    self.assertNotRegex(text, r"(?i)(Authorization|api[_-]?key|client[_-]?secret)")
                if rel == "crates/ultracompress-core/src/update.rs":
                    self.assertIn('USER_AGENT: &str = "ultracompress-update-check"', text)

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
        # The README keeps the short form the contract asks for: between three
        # and six lines that point at the detail rather than repeating it.
        lines = [line for line in privacy.splitlines() if line.strip()]
        self.assertGreaterEqual(len(lines), 3)
        self.assertLessEqual(len(lines), 6)
        self.assertIn("[PRIVACY.md](PRIVACY.md)", privacy)
        self.assertIn("no telemetry", privacy)
        self.assertIn("makes no automatic network traffic", privacy)

    def test_privacy_md_holds_the_detail_and_the_file_inventory(self):
        privacy = _text(ROOT / "PRIVACY.md")
        for phrase in ("makes no automatic network traffic", "no telemetry", "0700", "0600", "64 MiB",
                       "SHA-256", "model provider", "$TMPDIR", "rm -rf"):
            with self.subTest(phrase=phrase):
                self.assertIn(phrase, privacy)
        # Every local write named by the contract has a documented lifetime.
        for path in ("ultracompress.json", ".steak-pi/snaps", "debug.json",
                     "update.json", "check.claim", "install.log", "install/<tag>",
                     "ultracompress.previous", "config.json", "<session>.jsonl"):
            with self.subTest(path=path):
                self.assertIn(path, privacy)
        self.assertIn("Lifetime", privacy)
        self.assertIn("at most 30 days", privacy)
        self.assertIn("next snapshot write", privacy)
        self.assertIn("7 days", privacy)
        # The split must keep the platform-specific install cleanup steps.
        self.assertIn("<install dir>", privacy)
        self.assertIn("%LOCALAPPDATA%\\UltraCompress", privacy)
        self.assertIn("Remove-Item", privacy)
        # The refresh hand-offs point at the detail, not at a section summary.
        for adapter in (ROOT / "hermes" / "README.md", ROOT / "claude-code" / "README.md"):
            with self.subTest(adapter=adapter.name):
                self.assertIn("PRIVACY.md", _text(adapter))

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
        self.assertIn("MAX_SNAP_AGE_MS", snapshot)
        self.assertNotIn("/tmp/ultracompress-debug.json", debug)
        self.assertIn("trim_jsonl_archive", hermes)
        self.assertIn("archive_max_bytes", _text(ROOT / "hermes" / "ultracompress" / "__init__.py"))

    def test_rollback_copy_and_install_debris_have_retention(self):
        update = _text(ROOT / "crates" / "ultracompress-cli" / "src" / "update.rs")
        main = _text(ROOT / "crates" / "ultracompress-cli" / "src" / "main.rs")
        self.assertIn("ROLLBACK_MAX_AGE_SECS", update)
        self.assertIn("STAGE_MAX_AGE_SECS", update)
        self.assertIn("DEBRIS_MAX_AGE_SECS", update)
        self.assertIn("fn sweep_local_data", update)
        self.assertIn("record_rollback", update)
        # With no sidecar there is no known install time: the old binary's mtime
        # must not be used to age out the rollback copy.
        self.assertIn("ROLLBACK_OBSERVED_VERSION", update)
        self.assertNotRegex(update, r"expired\(modified_secs\(&previous\)")
        # The sweep runs on the successful path, not inside the check itself.
        self.assertIn("sweep_local_data", main)
        # The update check keeps its single request and fixed user agent.
        self.assertIn("USER_AGENT", update)
        self.assertNotRegex(update, r"(?i)(Authorization|api[_-]?key|client[_-]?secret)")

    def test_update_contract_marker_and_docs(self):
        docs = _text(ROOT / "docs" / "updates.md")
        self.assertIn("<!-- update-contract: v1 -->", docs)
        readme = _text(ROOT / "README.md")
        self.assertIn("[Updates](docs/updates.md)", readme)
        self.assertIn("## Privacy", readme)
        changelog = _text(ROOT / "CHANGELOG.md")
        self.assertIn("update contract", changelog.lower())

    def test_all_component_versions_are_aligned(self):
        import json
        import re
        workspace = re.search(r'(?m)^version = "([^"]+)"', _text(ROOT / "Cargo.toml")).group(1)
        pi = json.loads(_text(ROOT / "extension" / "package.json"))["version"]
        cc = json.loads(_text(ROOT / "claude-code/.claude-plugin/plugin.json"))["version"]
        hermes = re.search(r'(?m)^version:\s*(\S+)', _text(ROOT / "hermes/ultracompress/plugin.yaml")).group(1)
        runtime = re.search(r'__version__ = "([^"]+)"', _text(ROOT / "hermes/ultracompress/__init__.py")).group(1)
        index = re.search(r'ADAPTER_VERSION = "([^"]+)"', _text(ROOT / "extension/index.ts")).group(1)
        self.assertEqual({workspace, pi, cc, hermes, runtime, index}, {workspace})

    def test_pi_adapter_reports_its_package_version(self):
        package = _text(ROOT / "extension" / "package.json")
        index = _text(ROOT / "extension" / "index.ts")
        version = re.search(r'"version":\s*"([^"]+)"', package).group(1)
        self.assertIn(f'ADAPTER_VERSION = "{version}"', index)


if __name__ == "__main__":
    unittest.main()
