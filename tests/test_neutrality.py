"""UltraCompress ships no built-in model, provider or person choice.

UltraCompress is a neutral tool. What it ships names no model, provider or fallback
choice and no person's or client's name: those belong to the operator. This test
keeps it that way, modelled on Carl's neutrality test.

Not scanned, by design:

* LICENSE: legal attribution.
* tests/ and test/: synthetic fixtures and test code.
* The ``authors``/``author`` fields of Cargo.toml and plugin.json: package-metadata
  attribution. The rest of those files is scanned.
* The ``vendor/`` directory if present: third-party code.

A bare word such as ``fallback`` is not a fallback chain by itself. A chain cannot be
expressed without naming a model id or a provider route, which the model pattern
catches.
"""
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

SHIPPED_DIRS = ["crates", "docs", "scripts", "extension", "hermes", "claude-code"]
SHIPPED_FILES = ["README.md", "CHANGELOG.md", "CONTRIBUTING.md", "SECURITY.md", "Cargo.toml"]
LEGAL = {"LICENSE", "THIRD-PARTY.md"}
TEXT_EXTENSIONS = {".md", ".rs", ".ts", ".js", ".mjs", ".py", ".toml", ".json", ".sh", ".yml", ".yaml"}
SKIP_DIRS = {"node_modules", "target", ".git", "tests", "test", "vendor"}

MODEL_CHOICE = re.compile(
    r"\b(?:gpt-\d[\w.-]*|deepseek[\w./-]*|opencode-go/|openrouter/[\w~]|:exacto\b"
    r"|mimo[-.]?v?\d|glm[-.]?\d|claude-[a-z]+-\d-\d)",
    re.IGNORECASE,
)
PERSONAL = re.compile(
    r"\b(?:Mike|Berardi|Dana|Felix|acme|acme|Mouseia|Liberty Design"
    r"|libertydesign|acme)\b|/Users/[a-z]{2,}",
    re.IGNORECASE,
)


def _files():
    out = []
    for d in SHIPPED_DIRS:
        base = os.path.join(ROOT, d)
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = [n for n in dirnames if n not in SKIP_DIRS]
            for f in filenames:
                p = os.path.join(dirpath, f)
                if os.path.splitext(f)[1].lower() in TEXT_EXTENSIONS:
                    out.append(p)
    for f in SHIPPED_FILES:
        p = os.path.join(ROOT, f)
        if os.path.exists(p):
            out.append(p)
    return out


def _hits(pattern):
    hits = []
    for p in _files():
        try:
            lines = open(p, encoding="utf-8", errors="replace").read().splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines, 1):
            if pattern.search(line):
                hits.append((os.path.relpath(p, ROOT), i, line.strip()[:140]))
    return hits


class NeutralityTest(unittest.TestCase):
    def test_ships_no_concrete_model_id_or_provider_preference(self):
        hits = _hits(MODEL_CHOICE)
        self.assertEqual(hits, [], "Model ids or routes found in shipped files:\n" + "\n".join(f"{f}:{l}: {t}" for f, l, t in hits))

    def test_ships_no_person_or_project_name(self):
        hits = _hits(PERSONAL)
        self.assertEqual(hits, [], "Personal/project names found in shipped files:\n" + "\n".join(f"{f}:{l}: {t}" for f, l, t in hits))

    def test_scans_the_shipped_tree_not_an_empty_set(self):
        files = _files()
        self.assertGreater(len(files), 10)
        rel = {os.path.relpath(p, ROOT) for p in files}
        for required in ("README.md", "Cargo.toml"):
            self.assertIn(required, rel)


if __name__ == "__main__":
    unittest.main()
