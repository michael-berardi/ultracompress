"""Engine tests for the parts that do not need Hermes: the wiring around the CLI.

Hermes' own compressor is replaced with a stand-in when no Hermes checkout is importable, so the
engine's own decisions (archive before the CLI, empty summaries, fallback guards, the recall tool)
are testable on a plain interpreter. When a Hermes checkout is on the path its class is used and
its model-calling summary is patched out, so these tests never make a provider call.

    python3 -m unittest discover -s hermes/tests -v
"""
import json
import os
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

ADAPTER_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ADAPTER_DIR))

STUB_SUMMARY = "[CONTEXT COMPACTION — REFERENCE ONLY]"


def _install_stub_hermes():
    """Provide agent.context_compressor when Hermes is not installed. Returns True if stubbed."""
    try:
        import agent.context_compressor  # noqa: F401
        return False
    except Exception:
        pass

    class ContextCompressor:
        def __init__(self, **kwargs):
            self.kwargs = kwargs
            self._previous_summary = None
            self._last_summary_error = None

        def _effective_threshold_percent(self, context_length, threshold_percent):
            return 0.75 if context_length < 512_000 else threshold_percent

        def _generate_summary(self, turns, focus_topic=None, memory_context="", bypass_cooldown=False):
            return "BUILTIN-SUMMARY"

        def _strip_summary_prefix(self, text):
            return text

        def _with_summary_prefix(self, text):
            return STUB_SUMMARY + "\n\n" + text

        def _augment_summary_lean(self, summary, turns):
            return summary

        def _clear_compression_failure_cooldown(self):
            pass

        def on_session_start(self, session_id, **kwargs):
            self._session_id = session_id

        def get_status(self):
            return {}

    module = types.ModuleType("agent.context_compressor")
    module.ContextCompressor = ContextCompressor
    module._redact_compaction_text = lambda text: text
    module.is_compaction_summary_message = lambda m: str(m.get("content", "")).startswith(
        "[CONTEXT COMPACTION")
    package = types.ModuleType("agent")
    package.context_compressor = module
    sys.modules.setdefault("agent", package)
    sys.modules["agent.context_compressor"] = module
    return True


STUBBED = _install_stub_hermes()
import ultracompress  # noqa: E402  (after the stub is in place)
import ultracompress.convert as convert  # noqa: E402


def _proc(stdout="", returncode=0, stderr=""):
    return types.SimpleNamespace(returncode=returncode, stdout=stdout, stderr=stderr)


TURNS = [
    {"role": "user", "content": "Compact this window please." + "x" * 400},
    {"role": "assistant", "content": "", "tool_calls": [
        {"id": "c1", "type": "function",
         "function": {"name": "write_file", "arguments": json.dumps({"path": "src/a.rs"})}}]},
    {"role": "tool", "tool_call_id": "c1", "content": "written"},
    {"role": "assistant", "content": "Done: src/a.rs is written."},
]


class EngineFixture(unittest.TestCase):
    """Builds an engine with a temporary archive and no dependency on a real binary."""

    def engine(self, **settings):
        with mock.patch.object(ultracompress, "_read_config", return_value={}):
            eng = ultracompress.UltraCompressEngine()
        eng.uc_settings.update(settings)
        eng._session_id = "test-session"
        return eng

    def tmp_archive(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        return directory.name


class Threshold(EngineFixture):
    def test_setting_wins_only_when_set(self):
        self.assertEqual(ultracompress.resolve_threshold({"threshold": 0.40}, {"threshold": 0.75}), 0.40)
        self.assertEqual(ultracompress.resolve_threshold({}, {"threshold": 0.75}), 0.75)
        self.assertEqual(ultracompress.resolve_threshold({"threshold": None}, {"threshold": 0.75}), 0.75)
        self.assertEqual(ultracompress.resolve_threshold({}, {}), 0.30)

    def test_engine_uses_the_configured_threshold(self):
        with mock.patch.object(ultracompress, "_read_config",
                               return_value={"compression": {"threshold": 0.75}}):
            eng = ultracompress.UltraCompressEngine()
        self.assertEqual(eng.uc_settings.get("threshold"), None)
        if STUBBED:
            self.assertEqual(eng.kwargs["threshold_percent"], 0.75)
        else:  # a real Hermes compressor keeps the resolved value on the instance
            self.assertAlmostEqual(getattr(eng, "threshold_percent", 0.75), 0.75)


class ArchiveBeforeCli(EngineFixture):
    def test_a_failed_cli_still_leaves_the_window_in_the_archive(self):
        archive_dir = self.tmp_archive()
        eng = self.engine(archive_dir=archive_dir, binary="/nonexistent/ultracompress")
        with mock.patch.dict(os.environ, {"ULTRACOMPRESS_BIN": ""}), \
                mock.patch.object(convert, "BINARY_CANDIDATES", ()):
            with self.assertRaises(RuntimeError):
                eng._ultracompress_brief([dict(t) for t in TURNS])
        archive = Path(archive_dir, "test-session.jsonl")
        self.assertTrue(archive.exists(), "a CLI failure must not lose the raw history")
        self.assertIn("src/a.rs is written", archive.read_text())
        self.assertEqual(oct(archive.stat().st_mode & 0o777), "0o600")

    def test_known_window_is_archived_once_per_brief(self):
        archive_dir = self.tmp_archive()
        eng = self.engine(archive_dir=archive_dir)
        with mock.patch.object(ultracompress.subprocess, "run",
                               return_value=_proc(json.dumps({"summary": "[Session Goal]\n- ship it",
                                                              "stats": {}}))):
            eng._ultracompress_brief([dict(t) for t in TURNS])
        lines = Path(archive_dir, "test-session.jsonl").read_text().splitlines()
        self.assertEqual(len(lines), 1 + len(TURNS))  # the session header plus one entry per turn


class EmptySummary(EngineFixture):
    def test_empty_cli_summary_stays_local(self):
        eng = self.engine(archive_dir=self.tmp_archive())
        with mock.patch.object(ultracompress.subprocess, "run",
                               return_value=_proc(json.dumps({"summary": "", "stats": {"kept_messages": 0}}))):
            brief, report = eng._ultracompress_brief([dict(t) for t in TURNS])
        self.assertEqual(brief, convert.EMPTY_WINDOW_LINE)
        self.assertTrue(report.get("emptyWindow"))
        self.assertEqual(report.get("kept_messages"), 0)

    def test_empty_cli_summary_does_not_call_the_model(self):
        eng = self.engine(archive_dir=self.tmp_archive())
        with mock.patch.object(ultracompress.subprocess, "run",
                               return_value=_proc(json.dumps({"summary": "", "stats": {}}))):
            summary = eng._generate_summary([dict(t) for t in TURNS])
        self.assertIn(convert.EMPTY_WINDOW_LINE, summary)
        self.assertNotIn("BUILTIN-SUMMARY", summary)
        self.assertTrue(eng.last_uc_report.get("ok"), eng.last_uc_report)


class FallbackGuard(EngineFixture):
    def test_a_failure_after_the_cli_still_falls_back(self):
        """Everything after the CLI call is inside the guard, not just the CLI call itself."""
        eng = self.engine(archive_dir=self.tmp_archive())
        with mock.patch.object(ultracompress.subprocess, "run",
                               return_value=_proc(json.dumps({"summary": "[Session Goal]\n- ship it",
                                                              "stats": {}}))), \
                mock.patch.object(eng, "_augment_summary_lean", side_effect=AttributeError("renamed")):
            summary = eng._generate_summary([dict(t) for t in TURNS])
        self.assertEqual(summary, "BUILTIN-SUMMARY")
        self.assertFalse(eng.last_uc_report["ok"])
        self.assertIn("renamed", eng.last_uc_report["error"])

    def test_status_line_does_not_reserialise_the_window(self):
        eng = self.engine(archive_dir=self.tmp_archive())
        with mock.patch.object(ultracompress.subprocess, "run",
                               return_value=_proc(json.dumps({"summary": "[Session Goal]\n- ship it",
                                                              "stats": {}}))), \
                mock.patch.object(ultracompress, "turn_chars", wraps=convert.turn_chars) as counter:
            eng._generate_summary([dict(t) for t in TURNS])
        counter.assert_called_once()
        self.assertEqual(eng.last_uc_report["raw_tokens_est"], convert.turn_chars(TURNS) // 3)


class RecallTool(EngineFixture):
    def engine_with_archive(self, **settings):
        archive_dir = self.tmp_archive()
        eng = self.engine(archive_dir=archive_dir, **settings)
        Path(archive_dir, "test-session.jsonl").write_text(
            json.dumps({"type": "session", "version": 3, "id": "test-session"}) + "\n")
        return eng

    def test_schema_offers_paging(self):
        schema = self.engine().get_tool_schemas()[0]
        self.assertIn("page", schema["parameters"]["properties"])
        self.assertEqual(schema["parameters"]["required"], ["query"])
        self.assertIn("10", schema["description"])

    def test_page_is_passed_and_the_result_states_the_totals(self):
        eng = self.engine_with_archive()
        answer = {"query": "ingest", "hits": [{"entry_id": "m1"}], "total": 23, "page": 3,
                  "page_count": 3, "searched_messages": 40}
        with mock.patch.object(ultracompress.subprocess, "run",
                               return_value=_proc(json.dumps(answer))) as run:
            result = json.loads(eng.handle_tool_call("ultracompress_recall",
                                                     {"query": "ingest", "page": 3}))
        cmd = run.call_args[0][0]
        self.assertIn("--per-page", cmd)
        self.assertEqual(cmd[cmd.index("--per-page") + 1], "10")
        self.assertEqual(cmd[cmd.index("--page") + 1], "3")
        self.assertEqual(result["hits"], [{"entry_id": "m1"}])
        self.assertIn("23 hit(s)", result["result"])
        self.assertIn("3 page(s)", result["result"])

    def test_no_page_argument_means_no_page_flag(self):
        eng = self.engine_with_archive()
        answer = {"query": "x", "hits": [], "total": 0, "page": 1, "page_count": 1}
        with mock.patch.object(ultracompress.subprocess, "run",
                               return_value=_proc(json.dumps(answer))) as run:
            result = json.loads(eng.handle_tool_call("ultracompress_recall", {"query": "x"}))
        self.assertNotIn("--page", run.call_args[0][0])
        self.assertIn("0 hit(s)", result["result"])

    def test_bad_page_is_an_error_result(self):
        eng = self.engine_with_archive()
        for page in ("two", 0, -1):
            self.assertIn("error", json.loads(eng.handle_tool_call("ultracompress_recall",
                                                                   {"query": "x", "page": page})))

    def test_oserror_and_decoding_failures_return_error_json(self):
        eng = self.engine_with_archive()
        for failure in (PermissionError("not executable"), FileNotFoundError("gone"),
                        UnicodeDecodeError("utf-8", b"\xff", 0, 1, "invalid")):
            with mock.patch.object(ultracompress.subprocess, "run", side_effect=failure):
                result = json.loads(eng.handle_tool_call("ultracompress_recall", {"query": "x"}))
            self.assertIn("error", result, failure)

    def test_none_args_are_guarded(self):
        eng = self.engine_with_archive()
        self.assertIn("error", json.loads(eng.handle_tool_call("ultracompress_recall", None)))

    def test_recall_honours_the_timeout_setting(self):
        eng = self.engine_with_archive(timeout_seconds=7)
        answer = {"query": "x", "hits": [], "total": 0, "page": 1, "page_count": 1}
        with mock.patch.object(ultracompress.subprocess, "run",
                               return_value=_proc(json.dumps(answer))) as run:
            eng.handle_tool_call("ultracompress_recall", {"query": "x"})
        self.assertEqual(run.call_args[1]["timeout"], 7.0)

    def test_timeout_message_names_the_setting(self):
        eng = self.engine_with_archive(timeout_seconds=7)
        with mock.patch.object(ultracompress.subprocess, "run",
                               side_effect=ultracompress.subprocess.TimeoutExpired("uc", 7)):
            result = json.loads(eng.handle_tool_call("ultracompress_recall", {"query": "x"}))
        self.assertIn("7 s", result["error"])


if __name__ == "__main__":
    unittest.main()
