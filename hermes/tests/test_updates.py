"""Standalone update-contract checks for the Hermes adapter (no host checkout, no network)."""
import importlib.util
import json
import os
import stat
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "hermes" / "ultracompress" / "update.py"
SPEC = importlib.util.spec_from_file_location("uc_update", MODULE_PATH)
UPDATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPDATE)


class HermesUpdateTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name, "cache", "update.json")

    def test_version_comparison_and_stable_releases(self):
        self.assertTrue(UPDATE.is_newer("0.6.9", UPDATE.VERSION))
        self.assertFalse(UPDATE.is_newer("0.6.8", UPDATE.VERSION))
        self.assertFalse(UPDATE.is_newer("garbage", UPDATE.VERSION))
        self.assertFalse(UPDATE.is_newer("0.6.8-rc.1", UPDATE.VERSION))
        release = UPDATE.parse_release(json.dumps({"tag_name": "v0.6.9", "draft": False,
                                                   "prerelease": False, "body": "A\n\nB"}))
        self.assertEqual(release, {"version": "0.6.9", "notes": "A\nB"})
        with self.assertRaises(ValueError):
            UPDATE.parse_release(json.dumps({"tag_name": "v0.6.8", "prerelease": True}))
        with self.assertRaises(ValueError):
            UPDATE.parse_release(json.dumps({"tag_name": "v0.6.8", "draft": True}))
        with self.assertRaises(ValueError):
            UPDATE.parse_release(json.dumps({"tag_name": "nightly"}))

    def test_source_comes_from_package_metadata(self):
        self.assertEqual(
            UPDATE.latest_release_url(),
            "https://api.github.com/repos/michael-berardi/ultracompress/releases/latest",
        )
        with self.assertRaises(ValueError):
            UPDATE.latest_release_url("https://example.invalid/o/r")

    def test_daily_cache_failure_cause_and_private_write(self):
        def broken():
            raise OSError("upstream unavailable")

        state = UPDATE.run_check(dict(UPDATE.DEFAULT_STATE), self.path, broken)
        self.assertIn("upstream unavailable", state["last_error"])
        self.assertFalse(UPDATE.check_due(state))
        self.assertEqual(UPDATE.load_state(self.path)["last_error"], "upstream unavailable")
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(self.path.parent.stat().st_mode), 0o700)
        calls = []
        cached = UPDATE.run_check(state, self.path, lambda: calls.append(1) or {"version": "0.6.9", "notes": ""})
        self.assertEqual(calls, [])
        self.assertEqual(cached["last_error"], "upstream unavailable")
        self.assertTrue(UPDATE.check_due(cached, cached["checked_at"] + UPDATE.CHECK_INTERVAL + 1))

    def test_notice_frequency_skip_and_report(self):
        release = {"version": "0.6.9", "notes": "notes"}
        state = dict(UPDATE.DEFAULT_STATE)
        now = 1_700_000_000.0
        self.assertTrue(UPDATE.showable(state, release, UPDATE.VERSION, now))
        UPDATE.mark_shown(state, release["version"], now)
        self.assertFalse(UPDATE.showable(state, release, UPDATE.VERSION, now + 10))
        self.assertTrue(UPDATE.showable(state, release, UPDATE.VERSION, now + UPDATE.CHECK_INTERVAL))
        UPDATE.skip(state, release["version"])
        self.assertFalse(UPDATE.showable(state, release, UPDATE.VERSION, now + UPDATE.CHECK_INTERVAL * 2))
        self.assertTrue(UPDATE.showable(state, {"version": "0.7.0", "notes": ""}, UPDATE.VERSION, now))
        text = UPDATE.notice(release, UPDATE.VERSION)
        self.assertIn(f"ultracompress 0.6.9 available (you have {UPDATE.VERSION})", text)
        self.assertIn("update --auto on", text)
        self.assertIn("update --skip", text)
        self.assertNotIn("notes", text)
        self.assertIn("HTTP 503", UPDATE.check_report(None, "HTTP 503", UPDATE.VERSION))

    def test_gates_ci_headless_agent_and_opt_out(self):
        self.assertTrue(UPDATE.gate({}, interactive=True))
        for env in ({"CI": "1"}, {"HERMES_HEADLESS": "1"}, {"HERMES_PRINT_MODE": "1"},
                    {"ULTRACOMPRESS_AGENT_MODE": "1"}, {UPDATE.OPT_OUT_ENV: "1"}):
            with self.subTest(env=env):
                self.assertFalse(UPDATE.gate(env, interactive=True))
        self.assertFalse(UPDATE.gate({}, interactive=False))

    def test_shared_cache_fixture_uses_seconds_and_one_file_name(self):
        fixture = ROOT / "tests" / "fixtures" / "update-cache.json"
        state = UPDATE.load_state(fixture)
        self.assertEqual(UPDATE.CACHE_FILE, "update.json")
        self.assertEqual(UPDATE.CHECK_INTERVAL, 86_400)
        self.assertEqual(state["checked_at"], 1_700_000_000)
        self.assertEqual(state["shown_at"], 1_700_000_000)
        self.assertEqual(state["release"]["version"], "9.9.9")
        self.assertFalse(UPDATE.check_due(state, 1_700_000_000 + UPDATE.CHECK_INTERVAL - 1))
        self.assertTrue(UPDATE.check_due(state, 1_700_000_000 + UPDATE.CHECK_INTERVAL))

    def test_legacy_pi_milliseconds_are_migrated_to_canonical_keys(self):
        legacy = {
            "checkedAt": 1_700_000_000_000,
            "shownAt": 1_700_000_000_000,
            "shownVersion": "9.9.9",
            "release": {"version": "9.9.9", "notes": "notes"},
        }
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text(json.dumps(legacy), encoding="utf-8")
        state = UPDATE.load_state(self.path)
        self.assertEqual(state["checked_at"], 1_700_000_000)
        self.assertFalse(UPDATE.check_due(state, 1_700_000_000 + UPDATE.CHECK_INTERVAL - 1))
        UPDATE.save_state(state, self.path)
        raw = json.loads(self.path.read_text(encoding="utf-8"))
        self.assertEqual(raw["checked_at"], 1_700_000_000)
        self.assertNotIn("checkedAt", raw)
        self.assertEqual(set(raw), set(UPDATE.DEFAULT_STATE))

    def test_cache_path_matches_the_other_adapters(self):
        self.assertEqual(
            UPDATE.cache_path(env={"XDG_CACHE_HOME": "/tmp/xdg"}, platform="linux",
                              home=Path("/home/u")),
            Path("/tmp/xdg/ultracompress/update.json"),
        )
        self.assertEqual(
            UPDATE.cache_path(env={}, platform="darwin", home=Path("/Users/u")),
            Path("/Users/u/Library/Caches/ultracompress/update.json"),
        )
        self.assertEqual(
            UPDATE.cache_path(env={"LOCALAPPDATA": "C:/Cache"}, platform="win32",
                              home=Path("C:/Users/u")),
            Path("C:/Cache/ultracompress/update.json"),
        )
        self.assertEqual(
            UPDATE.cache_path(env={}, platform="linux", home=Path("/home/u")),
            Path("/home/u/.cache/ultracompress/update.json"),
        )

    def test_manifest_and_runtime_versions_stay_aligned(self):
        adapter = MODULE_PATH.parent
        manifest = (adapter / "plugin.yaml").read_text()
        runtime = (adapter / "__init__.py").read_text()
        self.assertIn(f"version: {UPDATE.VERSION}", manifest)
        self.assertIn(f'__version__ = "{UPDATE.VERSION}"', runtime)


if __name__ == "__main__":
    unittest.main()
