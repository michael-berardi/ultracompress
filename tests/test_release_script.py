"""Fast, offline guardrails for the release entry point."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from test_neutrality import PERSONAL

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'scripts' / 'release.sh'
VARIABLES = ('ULTRACOMPRESS_SIGNING_IDENTITY', 'ULTRACOMPRESS_INSTALLER_IDENTITY', 'NOTARY_PROFILE', 'ULTRACOMPRESS_PKG_IDENTIFIER')


class ReleaseScriptTest(unittest.TestCase):
    def run_script(self, cwd, env):
        return subprocess.run(['bash', str(SCRIPT)], cwd=cwd, env=env, capture_output=True, text=True, timeout=10)

    def test_required_credentials(self):
        for missing in VARIABLES:
            with self.subTest(missing=missing):
                env = os.environ.copy()
                env.update({name: 'placeholder' for name in VARIABLES})
                del env[missing]
                result = self.run_script(ROOT, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(missing, result.stderr)

    def test_dirty_tree_refused(self):
        with tempfile.TemporaryDirectory() as d:
            subprocess.run(['git', 'init', '-q', d], check=True)
            Path(d, 'untracked').write_text('dirty')
            env = os.environ.copy()
            env.update({name: 'placeholder' for name in VARIABLES})
            result = self.run_script(d, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('dirty tree', result.stderr)

    def test_untagged_head_refused(self):
        with tempfile.TemporaryDirectory() as d:
            subprocess.run(['git', 'init', '-q', d], check=True)
            Path(d, 'Cargo.toml').write_text('[workspace.package]\nversion = "1.2.3"\n')
            subprocess.run(['git', '-C', d, 'add', 'Cargo.toml'], check=True)
            subprocess.run(['git', '-C', d, '-c', 'user.name=release', '-c',
                            'user.email=release@example.invalid', 'commit', '-qm', 'fixture'], check=True)
            env = os.environ.copy()
            env.update({name: 'placeholder' for name in VARIABLES})
            result = self.run_script(d, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('HEAD is not the v1.2.3 tag', result.stderr)

    def test_archives_disable_resource_forks(self):
        text = SCRIPT.read_text()
        commands = [line for line in text.splitlines() if 'ditto -c -k' in line]
        self.assertGreaterEqual(len(commands), 4)
        self.assertTrue(all('--norsrc' in line for line in commands))
        self.assertNotRegex(text, r'(?m)^\s*zip\s')

    def test_no_personal_names(self):
        self.assertIsNone(PERSONAL.search(SCRIPT.read_text()))


if __name__ == '__main__':
    unittest.main()
