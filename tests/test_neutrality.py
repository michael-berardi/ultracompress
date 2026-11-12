"""UltraCompress ships no built-in model, provider or person choice.

UltraCompress is a neutral tool. What it ships names no model, provider or fallback
choice and no person's or client's name: those belong to the operator. This test
keeps it that way, modelled on Carl's neutrality test.

Not scanned, by design:

* LICENSE: legal attribution.
* The ``authors``/``author`` fields of Cargo.toml and plugin.json: package-metadata
  attribution. The rest of those files is scanned.
* The ``vendor/`` directory if present: third-party code.

A bare word such as ``fallback`` is not a fallback chain by itself. A chain cannot be
expressed without naming a model id or a provider route, which the model pattern
catches.
"""
import os
import re
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

SHIPPED_DIRS = ["crates", "docs", "scripts", "extension", "hermes", "claude-code", "tests", "test"]
SHIPPED_FILES = ["README.md", "CHANGELOG.md", "CONTRIBUTING.md", "SECURITY.md", "Cargo.toml"]
LEGAL = {"LICENSE", "THIRD-PARTY.md"}
TEXT_EXTENSIONS = {".md", ".rs", ".ts", ".js", ".mjs", ".py", ".toml", ".json", ".sh", ".yml", ".yaml", ".txt"}
SKIP_DIRS = {"node_modules", "target", ".git", "vendor"}

MODEL_CHOICE = re.compile(
    r"\b(?:gpt-\d[\w.-]*|deepseek[\w./-]*|opencode-go/|openrouter/[\w~]|:exacto\b"
    r"|mimo[-.]?v?\d|glm[-.]?\d|claude-[a-z]+-\d-\d)",
    re.IGNORECASE,
)
PERSONAL = re.compile(
    r"\b(?:Mike|Berardi|Felix|Mouseia|Liberty Design"
    r"|libertydesign)\b|/Users/[a-z]{2,}",
    re.IGNORECASE,
)


def _private_terms(path=None):
    path = path or os.environ.get("OSS_PRIVATE_TERMS") or os.path.expanduser("~/.config/oss-audit/names.txt")
    try:
        with open(path, encoding="utf-8") as handle:
            terms = [line.strip() for line in handle if line.strip() and not line.lstrip().startswith("#")]
    except FileNotFoundError:
        return None
    patterns = []
    for term in terms:
        escaped = re.escape(term)
        patterns.append((r"\b" if re.match(r"\w", term[0]) else "") + escaped
                        + (r"\b" if re.match(r"\w", term[-1]) else ""))
    return re.compile("|".join(patterns), re.IGNORECASE) if patterns else None


def _files():
    out = []
    for d in SHIPPED_DIRS:
        base = os.path.join(ROOT, d)
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = [n for n in dirnames if n not in SKIP_DIRS]
            for f in filenames:
                p = os.path.join(dirpath, f)
                if (os.path.splitext(f)[1].lower() in TEXT_EXTENSIONS
                    and p != os.path.abspath(__file__) and f not in LEGAL):
                    out.append(p)
    for f in SHIPPED_FILES:
        p = os.path.join(ROOT, f)
        if os.path.exists(p):
            out.append(p)
    return out


# The repository's own canonical URL is the one allowed use of the owner name, in docs only.
OWN_URL = re.compile(
    r"(?:github\.com|img\.shields\.io/github/(?:[\w.-]+/)*?[\w.-]+?)/michael-berardi/ultracompress(?![\w.-])")
DOC_SUFFIXES = (".md",)


def _hits(pattern):
    hits = []
    for p in _files():
        try:
            with open(p, encoding="utf-8", errors="replace") as handle:
                lines = handle.read().splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines, 1):
            if p.endswith(DOC_SUFFIXES):
                line = OWN_URL.sub("", line)
            if pattern.search(line):
                hits.append((os.path.relpath(p, ROOT), i, line.strip()[:140]))
    return hits


class NeutralityTest(unittest.TestCase):
    def test_private_terms_loader(self):
        with tempfile.TemporaryDirectory() as directory:
            terms = os.path.join(directory, "terms.txt")
            sample = os.path.join(directory, "sample.txt")
            with open(terms, "w", encoding="utf-8") as handle:
                handle.write("# comment\n\nzzqx-private-term\n")
            pattern = _private_terms(terms)
            with open(sample, "w", encoding="utf-8") as handle:
                handle.write("A ZZQX-private-term appears here")
            with open(sample, encoding="utf-8") as handle:
                self.assertIsNotNone(pattern.search(handle.read()))
            with open(sample, "w", encoding="utf-8") as handle:
                handle.write("A neutral sample")
            with open(sample, encoding="utf-8") as handle:
                self.assertIsNone(pattern.search(handle.read()))

    def test_ships_no_concrete_model_id_or_provider_preference(self):
        hits = _hits(MODEL_CHOICE)
        self.assertEqual(hits, [], "Model ids or routes found in shipped files:\n" + "\n".join(f"{f}:{l}: {t}" for f, l, t in hits))

    def test_ships_no_person_or_project_name(self):
        hits = _hits(PERSONAL)
        private = _private_terms()
        if private is not None:
            hits.extend(_hits(private))
        self.assertEqual(hits, [], "Personal/project names found in shipped files:\n" + "\n".join(f"{f}:{l}: {t}" for f, l, t in hits))

    def test_own_url_allowed_only_in_that_exact_form(self):
        ok = "https://github.com/michael-berardi/ultracompress/releases/latest"
        self.assertIsNone(PERSONAL.search(OWN_URL.sub("", ok)))
        for bad in ("https://github.com/michael-berardi/other", "michael-berardi",
                    "https://github.com/michael-berardi/ultracompress-x"):
            self.assertIsNotNone(PERSONAL.search(OWN_URL.sub("", bad)), bad)

    def test_readme_has_no_placeholder_owner_in_urls(self):
        with open(os.path.join(ROOT, "README.md"), encoding="utf-8") as handle:
            text = handle.read()
        url = re.compile(r"https?://[^\s)\]>\"']+")
        bad = [u for u in url.findall(text)
               if re.search(r"example-user|your-org|your-github-username|Alex/|OWNER|<owner>", u)]
        self.assertEqual(bad, [])

    def test_scans_the_shipped_tree_not_an_empty_set(self):
        files = _files()
        self.assertGreater(len(files), 10)
        rel = {os.path.relpath(p, ROOT) for p in files}
        for required in ("README.md", "Cargo.toml"):
            self.assertIn(required, rel)


if __name__ == "__main__":
    unittest.main()
