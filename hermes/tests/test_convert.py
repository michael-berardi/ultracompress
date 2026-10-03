"""Standalone tests for the Hermes adapter's pure helpers (no Hermes install needed).

    python3 -m unittest discover -s hermes/tests -v
"""
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

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


class FindBinary(unittest.TestCase):
    def test_explicit_then_env(self):
        with tempfile.TemporaryDirectory() as tmp:
            exe = Path(tmp) / "uc"
            exe.write_text("#!/bin/sh\n")
            self.assertEqual(convert.find_binary(str(exe), env={}), str(exe))
            self.assertEqual(convert.find_binary("", env={"ULTRACOMPRESS_BIN": str(exe)}), str(exe))
            self.assertNotEqual(convert.find_binary("/nonexistent/uc", env={}), "/nonexistent/uc")


@unittest.skipUnless(os.environ.get("ULTRACOMPRESS_BIN") or convert.find_binary(), "ultracompress binary not installed")
class CliContract(unittest.TestCase):
    """The converted fixture is accepted by the real CLI and produces a brief."""

    def test_compact_accepts_entries(self):
        import subprocess
        payload = {"entries": convert.to_pi_entries(convert.conversation_view(FIXTURE)),
                   "keepUserTurns": 0, "smartKeepTail": False}
        proc = subprocess.run([convert.find_binary(), "compact", "--policy", "vcc", "--keep", "0"],
                              input=json.dumps(payload), capture_output=True, text=True, timeout=30)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        out = json.loads(proc.stdout)
        self.assertIn("Do 1 and 3", out["summary"])
        self.assertEqual(out["stats"]["kept_messages"], 0)


if __name__ == "__main__":
    unittest.main()
