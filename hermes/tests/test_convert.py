"""Standalone tests for the Hermes adapter's pure helpers (no Hermes install needed).

    python3 -m unittest discover -s hermes/tests -v
"""
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ultracompress"))
import convert  # noqa: E402

FIXTURE = json.loads((Path(__file__).with_name("fixture_conversation.json")).read_text())


class ToPiEntries(unittest.TestCase):
    def test_roles_and_chain(self):
        entries = convert.to_pi_entries(FIXTURE)
        roles = [e["message"]["role"] for e in entries]
        self.assertNotIn("system", roles)
        self.assertEqual(roles[:3], ["user", "assistant", "toolResult"])
        for prev, cur in zip(entries, entries[1:]):
            self.assertEqual(cur["parentId"], prev["id"])
        self.assertIsNone(entries[0]["parentId"])

    def test_tool_call_arguments_parsed(self):
        call = convert.to_pi_entries(FIXTURE)[1]["message"]["content"][0]
        self.assertEqual(call["type"], "toolCall")
        self.assertEqual(call["arguments"], {"title": "slot test"})

    def test_untrusted_envelope_removed(self):
        result = convert.to_pi_entries(FIXTURE)[2]["message"]
        text = result["content"][0]["text"]
        self.assertEqual(json.loads(text)["slot"], 2)
        self.assertEqual(result["toolName"], "open_terminal")

    def test_unparseable_arguments_kept_raw(self):
        msg = [{"role": "assistant", "content": "", "tool_calls": [
            {"id": "x", "function": {"name": "f", "arguments": "{not json"}}]}]
        self.assertEqual(convert.to_pi_entries(msg)[0]["message"]["content"][0]["arguments"], {"raw": "{not json"})

    def test_content_part_lists(self):
        msg = [{"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "image_url"}, {"type": "text", "text": "b"}]}]
        self.assertEqual(convert.to_pi_entries(msg)[0]["message"]["content"][0]["text"], "ab")


class ConversationView(unittest.TestCase):
    def test_default_drops_tool_noise(self):
        view = convert.conversation_view(FIXTURE)
        self.assertFalse(any(m["role"] == "tool" for m in view))
        self.assertFalse(any(m.get("tool_calls") for m in view))
        self.assertIn("It works. Opened slot 2", json.dumps(view))

    def test_tool_result_chars_keeps_heads(self):
        view = convert.conversation_view(FIXTURE, tool_result_chars=20)
        tools = [m for m in view if m["role"] == "tool"]
        self.assertTrue(tools and all(len(m["content"]) <= 20 for m in tools))

    def test_carriers_skipped(self):
        carrier = {"role": "user", "content": "[CONTEXT COMPACTION] old"}
        view = convert.conversation_view([carrier] + FIXTURE,
                                         is_carrier=lambda m: str(m.get("content")).startswith("[CONTEXT COMPACTION"))
        self.assertNotIn("[CONTEXT COMPACTION] old", json.dumps(view))


class Sections(unittest.TestCase):
    def test_replies_newest_first_and_bounded(self):
        out = convert.agent_replies_section(FIXTURE, per_reply=30, total=200)
        self.assertTrue(out.startswith("\n\n" + convert.REPLIES_HEADING))
        self.assertLess(out.index("Queued both"), out.index("Three proposals"))
        self.assertIn(" …", out)

    def test_replies_carry_forward_survives_new_burst(self):
        first = convert.agent_replies_section(FIXTURE)
        burst = [{"role": "assistant", "content": "filler " * 400} for _ in range(30)]
        second = convert.agent_replies_section(burst, previous_summary=first)
        self.assertIn("Opened slot 2", second)

    def test_users_carry_forward(self):
        prev = convert.USERS_HEADING + "\n> earlier request"
        merged = convert.carry_forward_users("brief\n\n" + convert.USERS_HEADING + "\n> new request", prev)
        self.assertIn("> new request", merged)
        self.assertIn("> earlier request", merged)
        self.assertEqual(convert.carry_forward_users("brief", ""), "brief")

    def test_section_extraction(self):
        text = "## A\none\n## B\ntwo"
        self.assertEqual(convert.section(text, "## A"), "one")
        self.assertEqual(convert.section(text, "## B"), "two")
        self.assertEqual(convert.section(text, "## C"), "")


    def test_tool_only_brief_counts_tools_and_keeps_previous(self):
        turns = [{"role": "assistant", "content": "", "tool_calls": [
                     {"function": {"name": "mcp__overseer__overseer_status"}},
                     {"function": {"name": "mcp__overseer__overseer_status"}}]},
                 {"role": "tool", "content": "{}"},
                 {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "web_search"}}]}]
        brief = convert.tool_only_brief(turns, "Earlier: Morgan approved P-1.")
        self.assertTrue(brief.startswith("Earlier: Morgan approved P-1."))
        self.assertIn("3 call(s)", brief)
        self.assertIn("mcp__overseer__overseer_status ×2", brief)
        self.assertIn("web_search", brief)
        self.assertNotIn("Earlier", convert.tool_only_brief(turns))
        composed = convert.compose_brief("Earlier: Morgan approved P-1.")
        again = convert.tool_only_brief(turns, convert.section(composed, convert.BRIEF_HEADING))
        self.assertNotIn(convert.BRIEF_INTRO, again)
        self.assertTrue(again.startswith("Earlier: Morgan approved P-1."))

class RetainedToolCalls(unittest.TestCase):
    """File edits and commits survive into the brief; read-only calls and results do not."""

    @staticmethod
    def _call(name, arguments, call_id="c"):
        return {"id": call_id, "type": "function",
                "function": {"name": name, "arguments": json.dumps(arguments)}}

    def _kept(self, turns):
        blocks = [b for e in convert.to_pi_entries(convert.conversation_view(turns))
                  for b in e["message"]["content"] if b["type"] == "toolCall"]
        return blocks

    def test_write_edit_and_git_commit_kept_read_only_dropped(self):
        turns = [
            {"role": "user", "content": "Fix the parser and commit it."},
            {"role": "assistant", "content": "", "tool_calls": [
                self._call("read_file", {"path": "src/a.rs"}, "c1")]},
            {"role": "tool", "tool_call_id": "c1", "content": "fn main() {}"},
            {"role": "assistant", "content": "Rewriting it now.", "tool_calls": [
                self._call("write_file", {"path": "src/a.rs", "content": "x" * 500}, "c2"),
                self._call("str_replace_editor", {"file_path": "src/b.rs", "old_string": "y"}, "c3")]},
            {"role": "tool", "tool_call_id": "c2", "content": "written"},
            {"role": "assistant", "content": "", "tool_calls": [
                self._call("terminal", {"command": "git add -A && git commit -m 'fix parser'"}, "c4")]},
            {"role": "assistant", "content": "", "tool_calls": [
                self._call("bash", {"command": "cargo build --release"}, "c5")]},
        ]
        blocks = self._kept(turns)
        # The CLI only builds its Files And Changes / Commits sections from these names.
        self.assertEqual([b["name"] for b in blocks], ["write", "edit", "bash"])
        self.assertEqual(blocks[0]["arguments"], {"path": "src/a.rs"})
        self.assertEqual(blocks[1]["arguments"], {"path": "src/b.rs"})
        self.assertEqual(blocks[2]["arguments"], {"command": "git add -A && git commit -m 'fix parser'"})
        rendered = json.dumps(blocks)
        self.assertNotIn("read_file", rendered)
        self.assertNotIn("cargo build", rendered)
        self.assertNotIn("written", rendered)          # tool results stay out of the brief
        view = convert.conversation_view(turns)
        self.assertFalse(any(m["role"] == "tool" for m in view))
        self.assertIn("Rewriting it now.", json.dumps(view))   # the text next to a kept call survives

    def test_arguments_reduced_and_bounded(self):
        command = "git commit -m '" + "x" * 900 + "'"
        blocks = self._kept([{"role": "assistant", "content": "", "tool_calls": [
            self._call("bash", {"command": command, "timeout": 60000, "cwd": "/tmp"})]}])
        self.assertEqual(list(blocks[0]["arguments"]), ["command"])
        self.assertEqual(len(blocks[0]["arguments"]["command"]), convert.TOOL_ARGUMENT_CHARS)

    def test_names_are_matched_case_insensitively(self):
        blocks = self._kept([{"role": "assistant", "content": "", "tool_calls": [
            self._call("Write", {"file_path": "src/a.rs"}),
            self._call("BASH", {"command": "git commit -m done"}),
            self._call("Patch", {"path": "src/b.rs"})]}])
        self.assertEqual([b["name"] for b in blocks], ["write", "bash", "edit"])

    def test_alias_table_covers_harness_spellings(self):
        self.assertEqual(convert.canonical_tool_name("write_file"), "write")
        self.assertEqual(convert.canonical_tool_name("patch"), "edit")
        self.assertEqual(convert.canonical_tool_name("str_replace_editor"), "edit")
        self.assertEqual(convert.canonical_tool_name("terminal"), "bash")
        self.assertEqual(convert.canonical_tool_name("shell"), "bash")
        self.assertEqual(convert.canonical_tool_name("execute"), "bash")
        self.assertEqual(convert.canonical_tool_name("web_search"), "web_search")

    def test_call_without_a_path_is_dropped(self):
        self.assertEqual(self._kept([{"role": "assistant", "content": "", "tool_calls": [
            self._call("edit", {"old_string": "a", "new_string": "b"})]}]), [])

    def test_tool_result_chars_mode_keeps_calls_untouched(self):
        turns = [{"role": "assistant", "content": "", "tool_calls": [
            self._call("read_file", {"path": "src/a.rs"})]},
                 {"role": "tool", "tool_call_id": "c", "content": "body"}]
        view = convert.conversation_view(turns, tool_result_chars=20)
        self.assertEqual(view[0]["tool_calls"][0]["function"]["name"], "read_file")


class TurnChars(unittest.TestCase):
    def test_counts_text_and_arguments_without_serialising(self):
        turns = [{"role": "user", "content": "hello"},
                 {"role": "assistant", "content": "ok", "tool_calls": [
                     {"function": {"name": "edit", "arguments": json.dumps({"path": "src/a.rs"})}}]}]
        with mock.patch.object(convert.json, "dumps", side_effect=AssertionError("serialised")):
            chars = convert.turn_chars(turns)
        self.assertGreaterEqual(chars, len("hello") + len("ok") + len("src/a.rs"))
        self.assertEqual(convert.turn_chars([]), 0)


class EmptyWindow(unittest.TestCase):
    def test_local_line_instead_of_a_model_summary(self):
        self.assertEqual(convert.empty_window_brief(), convert.EMPTY_WINDOW_LINE)

    def test_previous_brief_is_kept_without_the_intro(self):
        previous = convert.compose_brief("Earlier goal: ship it.")
        brief = convert.empty_window_brief(convert.section(previous, convert.BRIEF_HEADING))
        self.assertTrue(brief.startswith("Earlier goal: ship it."))
        self.assertNotIn(convert.BRIEF_INTRO, brief)
        self.assertIn(convert.EMPTY_WINDOW_LINE, brief)


class FindBinary(unittest.TestCase):
    def test_explicit_then_env(self):
        with tempfile.TemporaryDirectory() as tmp:
            exe = Path(tmp) / "uc"
            exe.write_text("#!/bin/sh\n")
            exe.chmod(0o755)
            self.assertEqual(convert.find_binary(str(exe), env={}), str(exe))
            self.assertEqual(convert.find_binary("", env={"ULTRACOMPRESS_BIN": str(exe)}), str(exe))
            self.assertNotEqual(convert.find_binary("/nonexistent/uc", env={}), "/nonexistent/uc")

    def test_file_without_the_executable_bit_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            data = Path(tmp) / "uc"
            data.write_text("#!/bin/sh\n")
            data.chmod(0o644)
            with mock.patch.object(convert, "BINARY_CANDIDATES", ()):
                self.assertIsNone(convert.find_binary(str(data), env={}))
                self.assertIsNone(convert.find_binary("", env={"ULTRACOMPRESS_BIN": str(data)}))


def cli_binary() -> str:
    """The binary the CLI-contract tests run against.

    $ULTRACOMPRESS_BIN wins, so the tests can be pointed at a frozen copy while the installed
    binary is being rebuilt."""
    return convert.find_binary() or ""


@unittest.skipUnless(cli_binary(), "ultracompress binary not installed (or not executable)")
class CliContract(unittest.TestCase):
    """The converted fixture is accepted by the real CLI and produces a brief."""

    def _compact(self, messages, tool_result_chars: int = 0):
        import subprocess
        payload = {"entries": convert.to_pi_entries(convert.conversation_view(messages, tool_result_chars)),
                   "keepUserTurns": 0, "smartKeepTail": False}
        proc = subprocess.run([cli_binary(), "compact", "--policy", "vcc", "--keep", "0"],
                              input=json.dumps(payload), capture_output=True, text=True, timeout=60)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout)

    def test_compact_accepts_entries(self):
        out = self._compact(FIXTURE)
        self.assertIn("Do 1 and 3", out["summary"])
        self.assertEqual(out["stats"]["kept_messages"], 0)

    def test_brief_records_files_and_commits(self):
        """Without the retained tool calls the CLI cannot build these two sections at all."""
        messages = [
            {"role": "user", "content": "Log the ingest queue depth and commit it."},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "c1", "function": {"name": "write", "arguments": json.dumps(
                    {"path": "src/ingest/log.rs", "content": "// logging"})}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "wrote src/ingest/log.rs"},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "c2", "function": {"name": "edit", "arguments": json.dumps(
                    {"path": "src/ingest/worker.rs", "old_string": "a", "new_string": "b"})}}]},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "c3", "function": {"name": "bash", "arguments": json.dumps(
                    {"command": "git commit -am 'log queue depth'"})}}]},
            {"role": "tool", "tool_call_id": "c3", "content": "[main 1a2b3c4] log queue depth"},
            {"role": "assistant", "content": "Committed as 1a2b3c4."},
        ]
        summary = self._compact(messages)["summary"]
        self.assertIn("[Files And Changes]", summary)
        self.assertIn("log.rs", summary)
        self.assertIn("[Commits]", summary)
        self.assertIn("log queue depth", summary)


if __name__ == "__main__":
    unittest.main()
