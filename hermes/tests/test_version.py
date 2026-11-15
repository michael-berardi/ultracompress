"""Adapter metadata must identify the version reported by the runtime."""
import ast
from pathlib import Path
import unittest


class VersionTest(unittest.TestCase):
    def test_manifest_matches_runtime(self):
        adapter = Path(__file__).resolve().parents[1] / "ultracompress"
        tree = ast.parse((adapter / "__init__.py").read_text())
        runtime = next(ast.literal_eval(node.value) for node in tree.body
                       if isinstance(node, ast.Assign)
                       and any(isinstance(target, ast.Name) and target.id == "__version__"
                               for target in node.targets))
        manifest = next(line.split(":", 1)[1].strip()
                        for line in (adapter / "plugin.yaml").read_text().splitlines()
                        if line.startswith("version:"))
        self.assertEqual(manifest, runtime)
