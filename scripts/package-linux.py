#!/usr/bin/env python3
"""Build and inspect distro packages and a release-specific AUR source bundle."""
from __future__ import annotations

import argparse
import hashlib
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


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def render_aur(version, archive, output):
    paths = [archive, Path('packaging/linux/vosu.desktop'), Path('assets/branding/vosu-app-icon.svg'), Path('assets/branding/vosu-app-icon.png')]
    hashes = list(map(sha256, paths))
    template = Path('packaging/aur/PKGBUILD.in').read_text()
    for key, value in zip(['VERSION', 'ZIP_SHA256', 'DESKTOP_SHA256', 'SVG_SHA256', 'PNG_SHA256'], [version, *hashes]):
        template = template.replace(f'@{key}@', value)
    output.mkdir(parents=True, exist_ok=True)
    (output / 'PKGBUILD').write_text(template)
    shutil.copyfile('packaging/aur/vosu-bin.install', output / 'vosu-bin.install')
    sources = [f'vosu-linux-v{version}.zip::https://github.com/shiruviaa/vosu/releases/download/v{version}/vosu-linux-v{version}.zip',
               f'vosu.desktop::https://raw.githubusercontent.com/shiruviaa/vosu/v{version}/packaging/linux/vosu.desktop',
               f'vosu.svg::https://raw.githubusercontent.com/shiruviaa/vosu/v{version}/assets/branding/vosu-app-icon.svg',
               f'vosu-source.png::https://raw.githubusercontent.com/shiruviaa/vosu/v{version}/assets/branding/vosu-app-icon.png']
    lines = ['pkgbase = vosu-bin', '\tpkgdesc = Native osu! telemetry daemon with a local dashboard', f'\tpkgver = {version}', '\tpkgrel = 1', '\turl = https://github.com/shiruviaa/vosu', '\tinstall = vosu-bin.install', '\tarch = x86_64', '\tlicense = MIT', '\tlicense = Apache-2.0', '\tmakedepends = imagemagick']
    lines += [f'\tdepends = {dep}' for dep in ['glibc', 'libelf', 'zlib', 'libcap']]
    lines += ['\tprovides = vosu', '\tconflicts = vosu', '\toptions = !strip']
    lines += [f'\tsource = {source}' for source in sources]
    lines += [f'\tsha256sums = {digest}' for digest in hashes]
    lines += ['', 'pkgname = vosu-bin']
    (output / '.SRCINFO').write_text('\n'.join(lines) + '\n')


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
    with tempfile.TemporaryDirectory(prefix='vosu-aur-') as temporary:
        aur = Path(temporary)
        render_aur(version, archive, aur)
        with tarfile.open(args.output / f'vosu-bin-aur-v{version}.tar.gz', 'w:gz') as bundle:
            for name in ['PKGBUILD', '.SRCINFO', 'vosu-bin.install']:
                bundle.add(aur / name, arcname=name)


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
