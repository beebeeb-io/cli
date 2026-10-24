#!/usr/bin/env python3
"""Run the native bb executable extracted from its final release archive.

Cross-target archives are deliberately skipped; their matching release runners
perform this check. Extraction writes only the exact binary to a scratch dir.
"""
import argparse
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tarfile
import tempfile
import zipfile


def check(directory):
    machine = platform.machine().lower()
    machine = {'arm64': 'aarch64', 'amd64': 'x86_64'}.get(machine, machine)
    system = platform.system()
    suffix = {'Darwin': 'apple-darwin', 'Linux': 'unknown-linux-musl',
              'Windows': 'pc-windows-msvc'}.get(system)
    if suffix is None:
        raise ValueError(f'unsupported release runner: {system}/{machine}')
    target = f'{machine}-{suffix}'
    is_windows = system == 'Windows'
    archive = directory / f'beebeeb-cli-{target}{".zip" if is_windows else ".tar.xz"}'
    if not archive.exists():
        expected_targets = os.environ.get('BEEBEEB_EXPECTED_TARGETS', '').split()
        if expected_targets and target not in expected_targets:
            print(f'packaged version: 0 native archives; skipped cross-target runner {target}; expected {expected_targets}')
            return False
        raise ValueError(f'missing expected native archive: {archive.name}')
    expected = re.search(r'^version\s*=\s*"([^"]+)"',
                         Path('Cargo.toml').read_text(), re.M).group(1)
    name = 'bb.exe' if is_windows else 'bb'
    with tempfile.TemporaryDirectory(prefix='bb-packaged-version-') as scratch:
        root = Path(scratch)
        binary = root / name
        if is_windows:
            with zipfile.ZipFile(archive) as bundle:
                members = [entry for entry in bundle.namelist() if entry.split('/')[-1] == name]
                if len(members) != 1:
                    raise ValueError(f'{archive}: expected 1 bb.exe, found {len(members)}')
                binary.write_bytes(bundle.read(members[0]))
        else:
            with tarfile.open(archive) as bundle:
                members = [entry for entry in bundle.getmembers() if entry.name.split('/')[-1] == name and entry.isfile()]
                if len(members) != 1:
                    raise ValueError(f'{archive}: expected 1 bb, found {len(members)}')
                binary.write_bytes(bundle.extractfile(members[0]).read())
            binary.chmod(0o755)
        env = dict(os.environ, BB_NO_UPDATE='1', HOME=str(root),
                   XDG_CONFIG_HOME=str(root / '.config'), XDG_DATA_HOME=str(root / '.data'),
                   APPDATA=str(root / 'AppData'), LOCALAPPDATA=str(root / 'LocalAppData'))
        result = subprocess.run([str(binary), '--version'], env=env,
                                text=True, capture_output=True, timeout=30)
        actual = result.stdout.strip()
        if result.returncode != 0 or actual != f'bb {expected}':
            raise ValueError(f'{archive.name}: exit {result.returncode}, expected bb {expected}, got {actual!r}; {result.stderr}')
    print(f'packaged version: 1 passed, 0 failed ({archive.name}, bb {expected})')
    return target


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--self-test', action='store_true',
                        help='also prove a deliberately wrong release version is rejected')
    args = parser.parse_args()
    directory = args.directory.resolve()
    native_target = check(directory)
    if native_target and args.self_test:
        # Run this same guard against the unchanged native archive while the
        # scratch source metadata deliberately claims a different release.
        with tempfile.TemporaryDirectory(prefix='bb-packaged-version-red-') as scratch:
            Path(scratch, 'Cargo.toml').write_text('[package]\nversion = "0.0.0-deliberate-mutation"\n')
            red = subprocess.run([sys.executable, str(Path(__file__).resolve()), str(directory)],
                                 cwd=scratch, text=True, capture_output=True)
            if red.returncode == 0 or 'expected bb 0.0.0-deliberate-mutation' not in red.stderr:
                raise ValueError('packaged-version guard failed to reject deliberately wrong release metadata')
            missing = Path(scratch, 'missing')
            missing.mkdir()
            env = dict(os.environ, BEEBEEB_EXPECTED_TARGETS=native_target)
            red = subprocess.run([sys.executable, str(Path(__file__).resolve()), str(missing)],
                                 cwd=scratch, env=env, text=True, capture_output=True)
            if red.returncode == 0 or 'missing expected native archive' not in red.stderr:
                raise ValueError('packaged-version guard accepted missing expected native archive')
            print('packaged version red controls: 2 rejected, 0 accepted')
