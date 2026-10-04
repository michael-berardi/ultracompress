"""Integration tests against a real Hermes Agent checkout (skipped without one).

    HERMES_AGENT_DIR=~/.hermes/hermes-agent <hermes python> -m unittest discover -s hermes/tests -v

<hermes python> is the interpreter Hermes runs on (for example the one `hermes --print-runtime-command`
reports), so Hermes' own dependencies import. The test copies the adapter into a temporary
HERMES_HOME/plugins folder and loads it through Hermes' plugin loader, exactly as Hermes does.
"""
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ADAPTER = HERE.parent / "ultracompress"
HERMES_DIR = os.environ.get("HERMES_AGENT_DIR", "")
FIXTURE = json.loads((HERE / "fixture_conversation.json").read_text())


def _padded_conversation():
    """Fixture plus enough later traffic that Hermes has a middle window to condense."""
    msgs = [dict(m) for m in FIXTURE]
    for i in range(30):
        msgs.append({"role": "user", "content": f"Follow-up {i}: status please. " + "x" * 300})
        msgs.append({"role": "assistant", "content": f"Status {i}: still running. " + "y" * 2500})
    return msgs


@unittest.skipUnless(HERMES_DIR and Path(HERMES_DIR, "agent", "context_compressor.py").exists(),
                     "set HERMES_AGENT_DIR to a Hermes Agent checkout")
class EngineIntegration(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # A fresh HERMES_HOME has no install state, so Hermes may run its "finish install" tail on
        # import and rewrite the checkout's launchers to point at this temporary home's Python,
        # which breaks the real `hermes` command once the folder is deleted. Snapshot the launchers
        # and put them back afterwards.
        cls.launchers = {f: f.read_bytes() for f in Path(HERMES_DIR).expanduser().joinpath(".hermes", "bin").glob("*")
                         if f.is_file()}
        cls.home = tempfile.mkdtemp(prefix="uc-hermes-home-")
        os.environ["HERMES_HOME"] = cls.home
        shutil.copytree(ADAPTER, Path(cls.home, "plugins", "ultracompress"))
        sys.path.insert(0, str(Path(HERMES_DIR).expanduser()))
        import hermes_bootstrap  # noqa: F401  (Hermes' own import setup)
        from plugins.context_engine import discover_context_engines, load_context_engine
        cls.discovered = discover_context_engines()
        cls.load = staticmethod(load_context_engine)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.home, ignore_errors=True)
        rewritten = [f for f, data in cls.launchers.items() if f.read_bytes() != data]
        for f in rewritten:
            f.write_bytes(cls.launchers[f])
        if rewritten:
            print(f"\nrestored Hermes launchers rewritten during the test: {[str(f) for f in rewritten]}", file=sys.stderr)

    def engine(self, **settings):
        eng = self.load("ultracompress")
        self.assertIsNotNone(eng, "Hermes could not load the adapter")
        eng.uc_settings.update(settings)
        eng.bind_session_state(session_db=None, session_id="test-session")
        eng.update_model(model="test-model", context_length=272000, provider="test")
        return eng

    def test_discovered_and_named(self):
        names = [name for name, _desc, ok in self.discovered if ok]
        self.assertIn("ultracompress", names)
        self.assertEqual(self.engine().name, "ultracompress")

    def test_threshold_has_no_small_window_floor(self):
        eng = self.engine(threshold=0.30)
        eng.update_model(model="test-model", context_length=272000, provider="test")
        self.assertEqual(eng.threshold_tokens, int(272000 * 0.30))

    @unittest.skipUnless(os.environ.get("ULTRACOMPRESS_BIN") or shutil.which("ultracompress")
                         or Path("~/.ultraterm/bin/ultracompress").expanduser().is_file()
                         or Path("~/.local/bin/ultracompress").expanduser().is_file(),
                         "ultracompress binary not installed")
    def test_compact_then_recall(self):
        eng = self.engine()
        msgs = _padded_conversation()
        out = eng.compress([dict(m) for m in msgs], force=True)
        self.assertLess(len(out), len(msgs))
        carrier = next(m for m in out if "UltraCompress brief" in str(m.get("content")))
        text = carrier["content"]
        self.assertIn("[CONTEXT COMPACTION", text)          # Hermes' handoff prefix
        self.assertIn("Agent Replies", text)
        self.assertIn("Opened slot 2", text)                  # an early reply survived
        self.assertIn("Do 1 and 3", text)                     # the user's decision survived
        self.assertTrue(eng.last_uc_report.get("ok"), eng.last_uc_report)

        archive = Path(self.home, "ultracompress", "test-session.jsonl")
        self.assertTrue(archive.exists())
        self.assertEqual(oct(archive.stat().st_mode & 0o777), "0o600")
        hits = json.loads(eng.handle_tool_call("ultracompress_recall", {"query": "slot ready"}))
        self.assertTrue(hits.get("hits"), hits)
        self.assertIn("error", json.loads(eng.handle_tool_call("ultracompress_recall", {"query": ""})))
        self.assertIn("error", json.loads(eng.handle_tool_call("other_tool", {})))

    def test_archive_follows_session_rotation(self):
        eng = self.engine(archive_dir=str(Path(self.home, "rot")))
        old = Path(self.home, "rot", "test-session.jsonl")
        old.parent.mkdir(parents=True, exist_ok=True)
        old.write_text('{"type": "session", "version": 3, "id": "test-session"}\n')
        eng.on_session_start("rotated-session", old_session_id="test-session", boundary_reason="compression")
        moved = Path(self.home, "rot", "rotated-session.jsonl")
        self.assertTrue(moved.exists())
        self.assertEqual(moved.read_text(), old.read_text())

    def test_missing_binary_falls_back_to_builtin(self):
        from agent.context_compressor import ContextCompressor
        eng = self.engine(binary="/nonexistent/ultracompress")
        original = ContextCompressor._generate_summary
        ContextCompressor._generate_summary = lambda self, *a, **k: "BUILTIN-SUMMARY"
        try:
            env = os.environ.pop("ULTRACOMPRESS_BIN", None)
            # Point every standard location at nothing by asking for an explicit missing binary only.
            import importlib
            convert = importlib.import_module(type(eng).__module__ + ".convert")
            saved = convert.BINARY_CANDIDATES
            convert.BINARY_CANDIDATES = ()
            try:
                result = eng._generate_summary(_padded_conversation()[1:20])
            finally:
                convert.BINARY_CANDIDATES = saved
                if env is not None:
                    os.environ["ULTRACOMPRESS_BIN"] = env
        finally:
            ContextCompressor._generate_summary = original
        self.assertEqual(result, "BUILTIN-SUMMARY")
        self.assertFalse(eng.last_uc_report["ok"])
        self.assertIn("not found", eng.last_uc_report["error"])

    def test_tool_only_window_stays_local(self):
        from agent.context_compressor import ContextCompressor
        eng = self.engine()
        original = ContextCompressor._generate_summary
        ContextCompressor._generate_summary = lambda self, *a, **k: "BUILTIN-SUMMARY"
        turns = [{"role": "assistant", "content": "", "tool_calls": [
                     {"id": "c1", "type": "function", "function": {"name": "web_search", "arguments": "{}"}}]},
                 {"role": "tool", "tool_call_id": "c1", "content": "results"}]
        try:
            result = eng._generate_summary(turns)
        finally:
            ContextCompressor._generate_summary = original
        self.assertNotEqual(result, "BUILTIN-SUMMARY")
        self.assertIn("Tool steps condensed", result)
        self.assertTrue(eng.last_uc_report.get("ok"), eng.last_uc_report)
        self.assertTrue(eng.last_uc_report.get("toolOnly"))

    def test_builtin_mode_is_a_kill_switch(self):
        from agent.context_compressor import ContextCompressor
        eng = self.engine(mode="builtin")
        original = ContextCompressor._generate_summary
        ContextCompressor._generate_summary = lambda self, *a, **k: "BUILTIN-SUMMARY"
        try:
            self.assertEqual(eng._generate_summary(_padded_conversation()[1:20]), "BUILTIN-SUMMARY")
        finally:
            ContextCompressor._generate_summary = original


if __name__ == "__main__":
    unittest.main()
