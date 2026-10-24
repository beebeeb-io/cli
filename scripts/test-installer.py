#!/usr/bin/env python3
"""Exercise the actual generated installer against offline archives and PATHs.

Pass --unmodified to prove the regression against the old published installer.
No real package manager, executable, account configuration or network is used.
"""
import argparse
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('installer', type=Path)
parser.add_argument('--unmodified', action='store_true')
parser.add_argument('--mutation', choices=['omit-preflight', 'omit-verification'])
args = parser.parse_args()
original = args.installer.read_text()
version = re.search(r'^APP_VERSION="([^"]+)"', original, re.M).group(1)
spec = importlib.util.spec_from_file_location('customize', ROOT / 'customize-installer.py')
customizer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(customizer)


def executable(path, source):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(source)
    path.chmod(0o755)


def binary(version_text):
    return '#!/bin/sh\nprintf "%s\\n" "bb ' + version_text + '"\n'


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='bb-installer-test-')
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.home = self.base / 'home'
        self.tools = self.base / 'tools'
        self.first = self.base / 'first'
        for path in (self.home, self.tools, self.first, self.base / 'tmp'):
            path.mkdir()
        self.archive = self.base / 'fixture.tar.xz'
        with tarfile.open(self.archive, 'w:xz') as archive:
            info = tarfile.TarInfo('beebeeb-cli/bb')
            content = binary(version).encode()
            info.size, info.mode = len(content), 0o755
            archive.addfile(info, io.BytesIO(content))
        self.env = {key: value for key, value in os.environ.items()
                    if key not in ('CARGO_HOME', 'APP_URL', 'XDG_CONFIG_HOME', 'GITHUB_PATH')
                    and not key.startswith(('BEEBEEB_', 'CARGO_DIST_', 'INSTALLER_'))}
        self.env.update(HOME=str(self.home), SHELL='/bin/sh', TMPDIR=str(self.base / 'tmp'),
                        CARGO_HOME=str(self.home / '.cargo'),
                        XDG_CONFIG_HOME=str(self.home / '.config'),
                        PATH=f'{self.first}:{self.tools}:/usr/bin:/bin:/usr/sbin:/sbin',
                        BB_FIXTURE_ARCHIVE=str(self.archive),
                        BB_FIXTURE_BREW_PREFIX=str(self.base / 'brew'),
                        BB_FIXTURE_BREW_LOG=str(self.base / 'brew.log'),
                        BB_FIXTURE_VERSION=version,
                        BEEBEEB_CLI_NO_MODIFY_PATH='1',
                        BEEBEEB_CLI_DOWNLOAD_URL='https://fixture.invalid')
        executable(self.tools / 'mktemp', '''#!/bin/sh
# macOS mktemp need not honor TMPDIR when no template is supplied.
if [ "$#" -eq 1 ] && [ "$1" = -d ]; then
  exec /usr/bin/mktemp -d "$TMPDIR/download.XXXXXXXXXX"
elif [ "$#" -eq 0 ]; then
  exec /usr/bin/mktemp "$TMPDIR/file.XXXXXXXXXX"
fi
exec /usr/bin/mktemp "$@"
''')
        executable(self.tools / 'curl', '''#!/bin/sh
if [ "$1" = --version ]; then echo curl-fixture; exit 0; fi
while [ "$#" -gt 0 ]; do
  if [ "$1" = -o ]; then cp "$BB_FIXTURE_ARCHIVE" "$2"; exit; fi
  shift
done
exit 1
''')
        # Keep generated platform detection, tar unpacking, checksum verification,
        # file replacement and receipt writing. Only archive source is mocked.
        source = original if args.unmodified else customizer.customize(original)
        # A global-only dry run may have no platform manifests/checksums yet.
        # Supply the fixture archive's known checksum in each archive branch;
        # keep the generated verifier itself unchanged.
        fixture_hash = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        source = re.sub(r'(_zip_ext="[^"\n]+"\n)',
                        lambda m: m[1] + f'            _checksum_style="sha256"\n            _checksum_value="{fixture_hash}"\n', source)
        source = re.sub(r'(_checksum_value=")[0-9a-fA-F]{64}("[\r\n])',
                        lambda m: m[1] + hashlib.sha256(self.archive.read_bytes()).hexdigest() + m[2], source)
        if args.mutation == 'omit-preflight':
            source = source.replace('    beebeeb_prepare_install\n', '    : # deliberate mutation: omit preflight\n')
        if args.mutation == 'omit-verification':
            source = re.sub(r'    beebeeb_verify_install \"\$_install_(?:temp|dir)/bb\"\n', '    : # deliberate mutation: omit verification\n', source)
        self.installer = self.base / 'installer.sh'
        self.installer.write_text(source)

    def run_installer(self, success=True):
        result = subprocess.run(['/bin/sh', str(self.installer)], env=self.env,
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stdout

    def assert_version(self, path, expected=version):
        result = subprocess.run([str(path), '--version'], env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f'bb {expected}')

    def brew(self, stale_regular=False):
        prefix = Path(self.env['BB_FIXTURE_BREW_PREFIX'])
        installed = prefix / 'Cellar/bb/0.0.1/bin/bb'
        executable(installed, binary('0.0.1'))
        active = prefix / 'bin/bb'
        active.parent.mkdir(parents=True)
        if stale_regular:
            executable(active, binary('0.0.1'))
        else:
            active.symlink_to(installed)
        self.env['PATH'] = f'{active.parent}:{self.env["PATH"]}'
        executable(self.tools / 'brew', '''#!/bin/sh
printf '%s\\n' "$*" >> "$BB_FIXTURE_BREW_LOG"
case "$*" in
  --prefix) echo "$BB_FIXTURE_BREW_PREFIX" ;;
  'list --formula beebeeb-io/tap/bb') exit 0 ;;
  'update --quiet') [ "${BB_FIXTURE_BREW_FAIL:-}" != update ] || exit 4 ;;
  'upgrade beebeeb-io/tap/bb')
    [ "${BB_FIXTURE_BREW_FAIL:-}" != upgrade ] || exit 2
    [ "${BB_FIXTURE_BREW_OLD:-}" != 1 ] || exit 0
    mkdir -p "$BB_FIXTURE_BREW_PREFIX/Cellar/bb/new/bin"
    printf '#!/bin/sh\\necho "bb %s"\\n' "$BB_FIXTURE_VERSION" > "$BB_FIXTURE_BREW_PREFIX/Cellar/bb/new/bin/bb"
    chmod +x "$BB_FIXTURE_BREW_PREFIX/Cellar/bb/new/bin/bb" ;;
  'link --overwrite beebeeb-io/tap/bb')
    [ "${BB_FIXTURE_BREW_FAIL:-}" != link ] || exit 3
    [ "${BB_FIXTURE_BREW_OLD:-}" != 1 ] || exit 0
    rm -f "$BB_FIXTURE_BREW_PREFIX/bin/bb"
    ln -s "$BB_FIXTURE_BREW_PREFIX/Cellar/bb/new/bin/bb" "$BB_FIXTURE_BREW_PREFIX/bin/bb" ;;
  *) exit 9 ;;
esac
''')
        return active, installed

    def test_existing_earlier_path_is_updated(self):
        active = self.first / 'bb'
        executable(active, binary('0.13.2'))
        self.run_installer()
        self.assert_version(active)
        self.assertFalse((self.home / '.cargo/bin/bb').exists(), 'must not install a shadowed second copy')
        self.assertFalse((self.home / '.profile').exists())

    def test_existing_path_with_spaces(self):
        active = self.base / 'space dir/bin/bb'
        executable(active, binary('0.13.2'))
        self.env['PATH'] = f'{active.parent}:{self.env["PATH"]}'
        self.run_installer()
        self.assert_version(active)

    def test_clean_install_and_repeat(self):
        active = self.home / '.cargo/bin/bb'
        self.run_installer()
        self.assert_version(active)
        self.env['PATH'] = f'{active.parent}:{self.env["PATH"]}'
        self.run_installer()
        self.assert_version(active)

    def test_explicit_cargo_destination_is_honored(self):
        active = self.first / 'bb'
        executable(active, binary('0.13.2'))
        destination = self.base / 'chosen'
        self.env['BEEBEEB_CLI_INSTALL_DIR'] = str(destination)
        self.run_installer()
        self.assert_version(destination / 'bin/bb')
        self.assert_version(active, '0.13.2')

    def test_explicit_unmanaged_destination_is_honored(self):
        active = self.first / 'bb'
        executable(active, binary('0.13.2'))
        destination = self.base / 'chosen-flat'
        self.env['BEEBEEB_CLI_UNMANAGED_INSTALL'] = str(destination)
        self.run_installer()
        self.assert_version(destination / 'bb')
        self.assert_version(active, '0.13.2')

    def test_homebrew_symlink_is_preserved(self):
        active, old = self.brew()
        self.run_installer()
        self.assertTrue(active.is_symlink())
        self.assert_version(active)
        self.assert_version(old, '0.0.1')
        log = Path(self.env['BB_FIXTURE_BREW_LOG']).read_text()
        self.assertIn('update --quiet', log)
        self.assertIn('upgrade beebeeb-io/tap/bb', log)
        self.assertLess(log.index('update --quiet'), log.index('upgrade beebeeb-io/tap/bb'))
        self.assertIn('link --overwrite beebeeb-io/tap/bb', log)
        self.assertFalse((self.home / '.cargo/bin/bb').exists())

    def test_homebrew_stale_regular_binary_is_relinked(self):
        active, old = self.brew(stale_regular=True)
        self.run_installer()
        self.assertTrue(active.is_symlink())
        self.assert_version(active)
        self.assert_version(old, '0.0.1')

    def test_homebrew_upgrade_failure_is_reported(self):
        active, _ = self.brew()
        for failing_step in ('update', 'upgrade'):
            with self.subTest(failing_step=failing_step):
                self.env['BB_FIXTURE_BREW_FAIL'] = failing_step
                self.run_installer(False)
                self.assert_version(active, '0.0.1')
                log = Path(self.env['BB_FIXTURE_BREW_LOG']).read_text()
                self.assertNotIn('link --overwrite', log)

    def test_homebrew_link_failure_is_reported(self):
        active, _ = self.brew()
        self.env['BB_FIXTURE_BREW_FAIL'] = 'link'
        self.run_installer(False)
        self.assert_version(active, '0.0.1')

    def test_homebrew_formula_lag_is_reported(self):
        self.brew()
        self.env['BB_FIXTURE_BREW_OLD'] = '1'
        self.assertIn('update did not take effect', self.run_installer(False))

    def test_unrelated_bb_is_not_overwritten(self):
        active = self.first / 'bb'
        executable(active, '#!/bin/sh\necho "unrelated tool 3.2.1"\n')
        before = active.read_bytes()
        self.assertIn('not Beebeeb', self.run_installer(False))
        self.assertEqual(active.read_bytes(), before)
        self.assertFalse((self.home / '.cargo/bin/bb').exists())

    def test_unrecognized_symlink_is_not_overwritten(self):
        target = self.base / 'elsewhere/bb'
        executable(target, binary('0.13.2'))
        active = self.first / 'bb'
        active.symlink_to(target)
        self.assertIn('symlink', self.run_installer(False))
        self.assertTrue(active.is_symlink())
        self.assert_version(target, '0.13.2')

    @unittest.skipIf(hasattr(os, 'geteuid') and os.geteuid() == 0, 'root bypasses directory permissions')
    def test_unwritable_existing_directory_is_reported(self):
        active = self.first / 'bb'
        executable(active, binary('0.13.2'))
        self.first.chmod(0o555)
        self.addCleanup(self.first.chmod, 0o755)
        self.assertIn('not writable', self.run_installer(False))
        self.assert_version(active, '0.13.2')

    def test_wrong_downloaded_binary_is_rejected(self):
        # Correctly hashed archive containing the wrong version must not claim
        # success. This catches installer/download miswiring after checksums.
        with tarfile.open(self.archive, 'w:xz') as archive:
            info = tarfile.TarInfo('beebeeb-cli/bb')
            content = binary('0.0.1').encode()
            info.size, info.mode = len(content), 0o755
            archive.addfile(info, io.BytesIO(content))
        source = self.installer.read_text()
        source = re.sub(r'(_checksum_value=")[0-9a-fA-F]{64}("[\r\n])',
                        lambda m: m[1] + hashlib.sha256(self.archive.read_bytes()).hexdigest() + m[2], source)
        self.installer.write_text(source)
        self.assertIn('update did not take effect', self.run_installer(False))
        self.assertFalse((self.home / '.cargo/bin/bb').exists())

    def test_corrupt_download_is_rejected(self):
        with self.archive.open('ab') as archive:
            archive.write(b'corrupted')
        self.assertIn('checksum mismatch', self.run_installer(False))
        self.assertFalse((self.home / '.cargo/bin/bb').exists())

    def test_customizer_is_idempotent_and_refuses_template_drift(self):
        patched = customizer.customize(original)
        self.assertEqual(customizer.customize(patched), patched)
        with self.assertRaises(ValueError):
            customizer.customize('unrecognized cargo-dist template')
        with self.assertRaises(ValueError):
            customizer.customize(patched.replace('    beebeeb_prepare_install\n', ''))
        with self.assertRaises(ValueError):
            customizer.customize(patched.replace('ensure brew upgrade', 'ensure brew outdated'))
        # The packaging assertion must reject an uncustomized installer without
        # modifying it, then accept the customized bytes.
        candidate = self.base / 'check-installer.sh'
        unchecked = original.replace(customizer.MARKER, '# deliberately removed policy marker', 1)
        candidate.write_text(unchecked)
        red = subprocess.run([os.sys.executable, str(ROOT / 'customize-installer.py'),
                              '--check', str(candidate)], capture_output=True)
        self.assertNotEqual(red.returncode, 0)
        self.assertEqual(candidate.read_text(), unchecked)
        candidate.write_text(patched)
        green = subprocess.run([os.sys.executable, str(ROOT / 'customize-installer.py'),
                                '--check', str(candidate)], capture_output=True)
        self.assertEqual(green.returncode, 0, green.stderr)


unittest.main(argv=[__file__], verbosity=2)
