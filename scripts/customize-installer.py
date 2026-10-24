#!/usr/bin/env python3
"""Apply Beebeeb update policy to a cargo-dist 0.31 shell installer.

Only build/release hosts need Python. Published installers remain POSIX shell.
Anchor counts fail closed when a cargo-dist upgrade changes the template.
"""
import argparse
from pathlib import Path

MARKER = '# BEEBEEB_INSTALLER_UPDATE_POLICY_V1'


def customize(source: str) -> str:
    payload = Path(__file__).with_name('installer-update.sh').read_text()
    if MARKER in source:
        # Validate all hooks even on an idempotent invocation.
        for hook in ('beebeeb_prepare_install\n', 'beebeeb_verify_install "$_install_temp/bb"\n', 'beebeeb_verify_install "$_install_dir/bb"\n'):
            if source.count(hook) != 1:
                raise ValueError(f'customized installer has missing/duplicate hook: {hook.strip()}')
        if source.count(f'{MARKER}\n{payload}\ninstall() {{\n') != 1:
            raise ValueError('customized installer policy differs from committed policy')
        return source
    substitutions = [
        ('install() {\n', f'{MARKER}\n{payload}\ninstall() {{\n'),
        ('    get_architecture || return 1\n', '    beebeeb_prepare_install\n\n    get_architecture || return 1\n'),
        ('    for _bin_name in $_bins; do\n        ensure mv "$_install_temp/$_bin_name"',
         '    beebeeb_verify_install "$_install_temp/bb"\n\n    for _bin_name in $_bins; do\n        ensure mv "$_install_temp/$_bin_name"'),
        ('    say "everything\'s installed!"\n', '    beebeeb_verify_install "$_install_dir/bb"\n    say "everything\'s installed!"\n'),
    ]
    for before, after in substitutions:
        if source.count(before) != 1:
            raise ValueError(f'cargo-dist template changed: expected exactly one {before.strip()!r}')
        source = source.replace(before, after, 1)
    return source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('installer', type=Path)
    parser.add_argument('--check', action='store_true', help='verify policy is already applied without changing the file')
    args = parser.parse_args()
    source = args.installer.read_text()
    updated = customize(source)
    if source != updated:
        if args.check:
            raise ValueError('installer update policy is missing')
        args.installer.write_text(updated)
    print('installer update policy: 4 hooks verified')


if __name__ == '__main__':
    main()
