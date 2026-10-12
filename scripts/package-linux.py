#!/usr/bin/env python3
"""Build and inspect distro packages and the Linux release archive."""
from __future__ import annotations

import argparse
import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

SPEC = importlib.util.spec_from_file_location('package_release', Path(__file__).with_name('package-release.py'))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


def run(*command, **kwargs):
    return subprocess.check_output(command, text=True, **kwargs)


def check_glibc(binary):
    symbols = run('objdump', '-T', str(binary))
    versions = {tuple(map(int, value.split('.'))) for value in re.findall(r'GLIBC_(\d+(?:\.\d+)+)', symbols)}
    if not versions or max(versions) > (2, 35):
        raise ValueError(f'GLIBC baseline exceeds 2.35 (or no versioned symbols): {sorted(versions)}')
    print(f'maximum required GLIBC: {".".join(map(str, max(versions)))}')


def package_names(version):
    return [f'vosu_{version}_amd64.deb', f'vosu-{version}-1.x86_64.rpm', f'vosu-{version}-1-x86_64.pkg.tar.zst']


FILES = {'usr/bin/vosu', 'usr/share/applications/vosu.desktop',
         'usr/share/icons/hicolor/scalable/apps/vosu.svg',
         'usr/share/icons/hicolor/256x256/apps/vosu.png', 'usr/share/licenses/vosu/LICENSE'}


def tar_entries(command):
    with subprocess.Popen(command, stdout=subprocess.PIPE) as stream:
        with tarfile.open(fileobj=stream.stdout, mode='r|') as archive:
            entries = {item.name.removeprefix('./'): item.mode for item in archive
                       if item.isfile() and not item.name.removeprefix('./').startswith('.')}
        if stream.wait() != 0:
            raise ValueError(f'package archive inspection failed: {command}')
    return entries


def verify_package(path, version):
    if path.suffix == '.deb':
        entries = tar_entries(['dpkg-deb', '--fsys-tarfile', str(path)])
        metadata = run('dpkg-deb', '-f', str(path), 'Package', 'Version', 'Architecture', 'Depends')
        if f'Package: vosu\nVersion: {version}-1\n' not in metadata or 'Architecture: amd64' not in metadata:
            raise ValueError(f'incorrect Debian metadata: {metadata}')
        expected = FILES | {'usr/share/doc/vosu/copyright'}
    elif path.suffix == '.rpm':
        entries = {fields[0].lstrip('/'): int(fields[4], 8) & 0o777
                   for line in run('rpm', '-qp', '--dump', str(path)).splitlines()
                   if (fields := line.split())}
        metadata = run('rpm', '-qp', '--qf', '%{NAME} %{VERSION} %{RELEASE} %{ARCH}', str(path))
        if metadata != f'vosu {version} 1 x86_64':
            raise ValueError(f'incorrect RPM metadata: {metadata}')
        expected = FILES
    else:
        entries = tar_entries(['bsdtar', '-cf', '-', '@' + str(path)])
        metadata = run('bsdtar', '-xOf', str(path), '.PKGINFO')
        if f'pkgname = vosu\n' not in metadata or f'pkgver = {version}-1\n' not in metadata or 'arch = x86_64' not in metadata:
            raise ValueError(f'incorrect Arch metadata: {metadata}')
        expected = FILES
    if set(entries) != expected or entries['usr/bin/vosu'] != 0o755:
        raise ValueError(f'incorrect package files or executable mode: {entries}')
    print(f'verified {path}: {metadata.strip()}')


def build(args):
    version = release.version_from_tag(args.tag, Path('Cargo.toml'))
    check_glibc(args.binary)
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='vosu-package-') as temporary:
        stage = Path(temporary)
        shutil.copyfile(args.binary, stage / 'vosu')
        (stage / 'vosu').chmod(0o755)
        run('convert', 'assets/branding/vosu-app-icon.png', '-resize', '256x256', str(stage / 'vosu.png'))
        environment = dict(os.environ, VERSION=version, STAGE=str(stage))
        for packager, name in zip(['deb', 'rpm', 'archlinux'], package_names(version)):
            run(args.nfpm, 'package', '--config', 'packaging/linux/nfpm.yaml', '--packager', packager, '--target', str(args.output / name), env=environment)
            verify_package(args.output / name, version)
    archive = args.output / f'vosu-linux-v{version}.zip'
    release.verify_archive(archive, 'linux')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    baseline = commands.add_parser('check-glibc')
    baseline.add_argument('--binary', type=Path, required=True)
    create = commands.add_parser('build')
    create.add_argument('--binary', type=Path, required=True)
    create.add_argument('--tag', required=True)
    create.add_argument('--output', type=Path, required=True)
    create.add_argument('--nfpm', default='nfpm')
    args = parser.parse_args()
    if args.command == 'check-glibc':
        check_glibc(args.binary)
    else:
        build(args)


if __name__ == '__main__':
    main()
