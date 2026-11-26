"""Standalone checks for the Hermes raw-archive privacy guards."""
import importlib.util
import json
import stat
import tempfile
import unittest
from pathlib import Path

# convert.py has no Hermes imports; load it directly so this suite runs without a
# Hermes Agent checkout (importing ultracompress.__init__ would need `agent`).
_CONVERT = Path(__file__).resolve().parents[1] / "ultracompress" / "convert.py"
_SPEC = importlib.util.spec_from_file_location("uc_convert", _CONVERT)
_CONVERT_MODULE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_CONVERT_MODULE)
private_dir = _CONVERT_MODULE.private_dir
trim_jsonl_archive = _CONVERT_MODULE.trim_jsonl_archive


class ArchivePrivacyTest(unittest.TestCase):
    def test_private_dir_tightens_an_existing_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp, "state", "ultracompress")
            target.mkdir(parents=True)
            target.chmod(0o777)
            private_dir(target)
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o700)

    def test_retention_keeps_header_and_newest_whole_entries(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp, "session.jsonl")
            header = {"type": "session", "version": 3, "id": "s"}
            entries = [{"id": f"m{i}", "text": "x" * 100} for i in range(5)]
            archive.write_text("\n".join(json.dumps(x) for x in [header, *entries]) + "\n")
            archive.chmod(0o600)
            limit = len(json.dumps(header)) + 1 + 2 * (len(json.dumps(entries[0])) + 1)
            kept = trim_jsonl_archive(archive, limit)
            self.assertLessEqual(kept, limit)
            lines = [json.loads(line) for line in archive.read_text().splitlines()]
            self.assertEqual(lines[0], header)
            self.assertEqual([e["id"] for e in lines[1:]], ["m3", "m4"])
            self.assertEqual(stat.S_IMODE(archive.stat().st_mode), 0o600)
            self.assertFalse(archive.with_name("session.jsonl.trim").exists())

    def test_unicode_line_separators_never_split_an_entry(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp, "session.jsonl")
            entries = [{"id": f"m{i}", "text": "a\u2028b\u2029c\x85d" + "x" * 100} for i in range(4)]
            archive.write_text("".join(json.dumps(e, ensure_ascii=False) + "\n" for e in entries), encoding="utf-8")
            one = len((json.dumps(entries[0], ensure_ascii=False) + "\n").encode("utf-8"))
            trim_jsonl_archive(archive, 2 * one)
            lines = archive.read_text(encoding="utf-8").split("\n")
            kept = [json.loads(line) for line in lines if line]
            self.assertEqual([e["id"] for e in kept], ["m2", "m3"])

    def test_newest_entry_is_kept_even_when_larger_than_the_budget(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp, "session.jsonl")
            header = {"type": "session", "version": 3, "id": "s"}
            entries = [{"id": "old", "text": "x"}, {"id": "big", "text": "y" * 5000}]
            archive.write_text("".join(json.dumps(x) + "\n" for x in [header, *entries]))
            trim_jsonl_archive(archive, 200)
            lines = [json.loads(line) for line in archive.read_text().splitlines()]
            self.assertEqual(lines[0], header)
            self.assertEqual([e["id"] for e in lines[1:]], ["big"])

    def test_archive_below_budget_is_untouched(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp, "session.jsonl")
            text = '{"type": "session", "version": 3, "id": "s"}\n'
            archive.write_text(text)
            self.assertEqual(trim_jsonl_archive(archive, 1024), len(text))
            self.assertEqual(archive.read_text(), text)


if __name__ == "__main__":
    unittest.main()
