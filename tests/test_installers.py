"""Guardrails for the one-line installers: static checks, shell behaviour, and PowerShell 7 logic in Docker."""
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest
import zipfile
import io
import tarfile

ROOT = Path(__file__).resolve().parent.parent
PS1 = ROOT / 'scripts' / 'install.ps1'
SH = ROOT / 'scripts' / 'install.sh'
SLUG = ROOT / 'scripts' / 'repo-slug.sh'
IMAGE = 'pwsh-install-test:7.4.6'


class StaticPs1Test(unittest.TestCase):
    text = PS1.read_text()

    def test_no_ascii_encoded_shims(self):
        self.assertNotRegex(self.text, r'(?i)-Encoding\s+Ascii')
        self.assertNotIn('Set-Content', self.text)

    def test_never_exits_the_users_shell(self):
        self.assertIsNone(re.search(r'(?m)^\s*exit\b', self.text))

    def test_body_is_a_function_called_last_and_removed(self):
        lines = [l for l in self.text.splitlines() if l.strip() and not l.lstrip().startswith('#')]
        self.assertTrue(lines[0].startswith('function Install-UltraCompress'))
        self.assertIn('Install-UltraCompress', lines[-1])
        self.assertIn('Remove-Item Function:', lines[-1])

    def test_quiet_progress_and_tls12(self):
        self.assertIn("$ProgressPreference = 'SilentlyContinue'", self.text)
        self.assertIn('Tls12', self.text)

    def test_paths_normalised(self):
        self.assertEqual(self.text.count('GetUnresolvedProviderPathFromPSPath'), 2)

    def test_captured_native_calls_run_under_continue(self):
        lines = self.text.splitlines()
        hits = [i for i, l in enumerate(lines) if '2>&1' in l]
        self.assertGreaterEqual(len(hits), 2)
        for i in hits:
            self.assertIn("'Continue'", lines[i - 1], lines[i])

    def test_cmd_shim_uses_oem_code_page_for_absolute_paths(self):
        self.assertIn('OEMCodePage', self.text)
        self.assertIn("'%~dp0..\\'", self.text)

    def test_user_path_is_written_through_the_registry(self):
        text = PS1.read_text()
        self.assertNotIn("SetEnvironmentVariable('Path'", text)
        self.assertNotIn("GetEnvironmentVariable('Path'", text)
        self.assertIn('DoNotExpandEnvironmentNames', text)

    def test_template_guard_keeps_its_shape(self):
        self.assertIn("-like '*github.com/@*@/*'", self.text)
        self.assertEqual(self.text.count('@RELEASE_REPO@'), 2)  # the comment and the one assignment
        self.assertIsNone(re.search(r'-like .*@RELEASE_REPO@', self.text))


class StaticShTest(unittest.TestCase):
    def test_guard_shape_and_marker(self):
        text = SH.read_text()
        self.assertIn('*github.com/@*@/*)', text)
        self.assertIn('ultracompress-installer-shim', text)
        self.assertEqual(text.count('"@RELEASE_REPO@"'), 1)

    def test_release_script_validates_slug(self):
        text = (ROOT / 'scripts' / 'build-release-cross.sh').read_text()
        self.assertIn('repo_slug', text)
        self.assertIn('A-Za-z0-9._-', SLUG.read_text())


class SlugTest(unittest.TestCase):
    def slug(self, remote):
        r = subprocess.run(['bash', '-c', '. "$1"; repo_slug "$2"', 'x', str(SLUG), remote],
                           capture_output=True, text=True, timeout=10)
        return r.returncode, r.stdout.strip()

    def test_accepted_forms(self):
        for remote in ('git@github.com:acme/tool.git', 'ssh://git@github.com/acme/tool.git',
                       'ssh://git@github.com/acme/tool', 'https://github.com/acme/tool.git',
                       'https://github.com/acme/tool', 'https://github.com/acme/tool/'):
            with self.subTest(remote=remote):
                self.assertEqual(self.slug(remote), (0, 'acme/tool'))

    def test_rejected_forms(self):
        for remote in ('ssh://git@github.com/acme', 'https://example.com/acme/tool', 'https://github.com/a/b/c',
                       'ssh://git@github.com/ac me/tool', 'https://github.com/acme/to|ol', '/srv/git/tool.git', ''):
            with self.subTest(remote=remote):
                self.assertNotEqual(self.slug(remote)[0], 0)


def fake_release(base: Path):
    """A release base with a fake `ultracompress` for this OS, served through file://."""
    exe = b'#!/bin/sh\necho "ultracompress 9.9.9"\n'
    if os.uname().sysname == 'Darwin':
        name = 'UltraCompress-macos-universal.zip'
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, 'w') as z:
            info = zipfile.ZipInfo('cli/ultracompress')
            info.external_attr = 0o755 << 16
            z.writestr(info, exe)
        data = buf.getvalue()
    else:
        arch = 'arm64' if os.uname().machine in ('aarch64', 'arm64') else 'x86_64'
        name = f'ultracompress-linux-{arch}.tar.gz'
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode='w:gz') as t:
            info = tarfile.TarInfo('ultracompress')
            info.size, info.mode = len(exe), 0o755
            t.addfile(info, io.BytesIO(exe))
        data = buf.getvalue()
    (base / name).write_bytes(data)
    (base / 'SHA256SUMS').write_text(f'{hashlib.sha256(data).hexdigest()}  {name}\n')


class ShimOwnershipTest(unittest.TestCase):
    def setUp(self):
        self.d = Path(tempfile.mkdtemp(prefix='uc-shim-'))
        self.addCleanup(shutil.rmtree, self.d, True)
        (self.d / 'base').mkdir()
        fake_release(self.d / 'base')
        self.home, self.bin = self.d / 'home', self.d / 'bin'
        self.bin.mkdir()
        self.shim = self.bin / 'ultracompress'

    def install(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith('ULTRACOMPRESS_')}
        env.update(HOME=str(self.d), ULTRACOMPRESS_HOME=str(self.home), ULTRACOMPRESS_BIN_DIR=str(self.bin),
                   ULTRACOMPRESS_RELEASE_BASE=(self.d / 'base').as_uri())
        r = subprocess.run(['sh', str(SH)], env=env, capture_output=True, text=True, timeout=60)
        return r

    def test_fresh_install_writes_marked_shim_and_upgrades_it(self):
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('# ultracompress-installer-shim', self.shim.read_text())
        self.assertEqual(self.install().returncode, 0)

    def test_foreign_file_is_left_alone(self):
        self.shim.write_text('#!/bin/sh\necho mine\n')
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.shim.read_text(), '#!/bin/sh\necho mine\n')
        self.assertIn('Left', r.stdout)
        self.assertEqual(len([l for l in r.stdout.splitlines() if l.startswith('Left')]), 1)
        self.assertTrue((self.home / 'ultracompress').exists())

    def test_foreign_file_with_similar_shape_is_left_alone(self):
        text = '#!/bin/sh\necho hi\nexec "/x/ultracompress" "$@"\n'
        self.shim.write_text(text)
        self.install()
        self.assertEqual(self.shim.read_text(), text)

    def test_symlink_is_left_alone(self):
        target = self.d / 'other'
        target.write_text('#!/bin/sh\necho other\n')
        self.shim.symlink_to(target)
        self.install()
        self.assertTrue(self.shim.is_symlink())
        self.assertEqual(target.read_text(), '#!/bin/sh\necho other\n')

    def test_0_6_1_shim_counts_as_ours(self):
        self.shim.write_text(f'#!/bin/sh\nexec "{self.home}/ultracompress" "$@"\n')
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('# ultracompress-installer-shim', self.shim.read_text())


@unittest.skipUnless(shutil.which('docker') and subprocess.run(
    ['docker', 'image', 'inspect', IMAGE], capture_output=True).returncode == 0, f'needs Docker and image {IMAGE}')
class Pwsh7Test(unittest.TestCase):
    def test_shim_helpers_and_parse(self):
        text = PS1.read_text()
        m = re.search(r'#region shim-helpers\n(.*?)#endregion', text, re.S)
        self.assertIsNotNone(m)
        (ROOT / 'dist').mkdir(exist_ok=True)
        d = Path(tempfile.mkdtemp(prefix='ps-unit-', dir=ROOT / 'dist'))
        self.addCleanup(shutil.rmtree, d, True)
        pm = re.search(r'#region path-helpers\n(.*?)#endregion', text, re.S)
        self.assertIsNotNone(pm)
        (d / 'helpers.ps1').write_text(m.group(1) + pm.group(1))
        (d / 'install.ps1').write_text(text)
        (d / 'test.ps1').write_text(r'''
$ErrorActionPreference = 'Stop'
. /t/helpers.ps1
$fail = 0
function Check($name, $ok) { if (-not $ok) { Write-Host "FAIL: $name"; $script:fail++ } else { Write-Host "ok: $name" } }
$errs = $null
[void][Management.Automation.Language.Parser]::ParseFile('/t/install.ps1', [ref]$null, [ref]$errs)
Check 'install.ps1 parses' ($errs.Count -eq 0)

$w = '/tmp/w'; New-Item -ItemType Directory $w | Out-Null
function Put($name, $text) { [IO.File]::WriteAllText("$w/$name", $text); "$w/$name" }
Check 'marked sh shim is ours' (Test-OwnShim (Put a "#!/bin/sh`n# $ShimMarker`nexec x`n"))
Check 'marked cmd shim is ours' (Test-OwnShim (Put b "@echo off`r`nrem $ShimMarker`r`n`"x`" %*`r`n"))
Check '0.6.1 sh shim is ours' (Test-OwnShim (Put c "#!/bin/sh`nexec `"/h/ultracompress`" `"`$@`"`n`n"))
Check '0.6.1 cmd shim is ours' (Test-OwnShim (Put d "@echo off`r`n`"%~dp0..\ultracompress.exe`" %*`r`n"))
Check 'user script is not ours' (-not (Test-OwnShim (Put e "#!/bin/sh`necho mine`n")))
Check 'user cmd with other target is not ours' (-not (Test-OwnShim (Put f "@echo off`r`n`"C:\mine\tool.exe`" %*`r`n")))
Check 'missing file is not ours' (-not (Test-OwnShim "$w/none"))
Check 'directory is not ours' (-not (Test-OwnShim $w))


$rel = Get-CmdShimText 'C:\Users\x\UC\ultracompress.exe' 'C:\Users\x\UC' 'C:\Users\x\UC\bin\'
Check 'own bin dir -> relative shim' ($rel.Text -like '*"%~dp0..\ultracompress.exe" %**' -and $rel.Text -notlike '*C:*')

$abs = Get-CmdShimText 'C:\Users\Jose\UC\ultracompress.exe' 'C:\Users\Jose\UC' 'D:\tools'
Check 'bin override -> absolute shim' ($abs.Text -like '*"C:\Users\Jose\UC\ultracompress.exe" %**')
Check 'relative shim is pure ASCII' (([Text.Encoding]::ASCII.GetString([Text.Encoding]::ASCII.GetBytes($rel.Text))) -eq $rel.Text)
$pct = Get-CmdShimText 'C:\a%b\ultracompress.exe' 'C:\a%b' 'D:\t'
Check 'percent doubled' ($pct.Text -like '*C:\a%%b*')

$thrown = $false
try { Write-ShimFile "$w/g.cmd" "@echo off`r`n`"C:\Users\Jos$([char]0xE9)\x.exe`" %*" ([Text.Encoding]::ASCII) } catch { $thrown = $true }
Check 'unrepresentable path throws instead of writing ?' ($thrown -and -not (Test-Path "$w/g.cmd"))
$latin = [Text.Encoding]::GetEncoding('iso-8859-1')
Write-ShimFile "$w/h.cmd" "@echo off`r`n`"C:\Users\Jos$([char]0xE9)\x.exe`" %*" $latin
Check 'representable path round-trips' ((Get-Content "$w/h.cmd" -Encoding ([Text.Encoding]::Latin1.WebName -as [string])) -ne $null -and ([IO.File]::ReadAllBytes("$w/h.cmd") -contains 0xE9))

$env:UCTEST_HOME = '/x/home'
Check 'literal entry present' (Test-PathHasEntry 'C:\a;D:\UC\bin' 'D:\UC\bin')
Check 'case and trailing backslash ignored' (Test-PathHasEntry 'C:\a;d:\uc\BIN\\' 'D:\UC\bin')
Check 'expanded entry present' (Test-PathHasEntry 'C:\a;%UCTEST_HOME%\bin' '/x/home\bin')
Check 'absent entry' (-not (Test-PathHasEntry 'C:\a;%UCTEST_HOME%\other;' 'D:\UC\bin'))
Check 'empty path' (-not (Test-PathHasEntry '' 'D:\UC\bin'))

$p = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath('rel/../x//y')
Check 'relative path normalised to absolute' ([IO.Path]::IsPathRooted($p))
if ($fail) { exit 1 }
''')
        r = subprocess.run(['docker', 'run', '--rm', '--name', 'ultracompress-test-pwsh-unit', '-v', f'{d}:/t:ro',
                            IMAGE, 'pwsh', '-NoProfile', '-File', '/t/test.ps1'],
                           capture_output=True, text=True, timeout=300)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


if __name__ == '__main__':
    unittest.main()
