"""Package this static example (Python 3.11+); no network or build steps.

Writes the release ZIP (`manifest.toml` at the archive root), a catalog v2
`catalog-v2.json` with its one tool entry, and the extracted package layout
`extracted/tools/<id>/any/` that the SDK's `vosu-package-check` reads.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tomllib
from urllib.parse import urlsplit
from zipfile import ZipFile, ZIP_DEFLATED

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--base-url', required=True, help='HTTPS directory containing the release assets')
parser.add_argument('--out', type=Path, default=Path('dist'))
args = parser.parse_args()
url = urlsplit(args.base_url)
if url.scheme != 'https' or not url.hostname or url.username or url.password or url.query or url.fragment:
    parser.error('--base-url must be an HTTPS directory URL without credentials, query or fragment')
root = Path(__file__).resolve().parent
manifest = tomllib.loads((root / 'manifest.toml').read_text())
args.out.mkdir(parents=True, exist_ok=True)
name = f"vosu-tool-{manifest['id']}-{manifest['version']}-any.zip"
archive = args.out / name
files = ['manifest.toml', 'settings.json', 'LICENSE']
files += [p.relative_to(root).as_posix() for p in sorted((root / 'web').rglob('*')) if p.is_file()]
extracted = args.out / 'extracted/tools' / manifest['id'] / 'any'
shutil.rmtree(extracted, ignore_errors=True)
with ZipFile(archive, 'w', compression=ZIP_DEFLATED) as package:
    for name_in_zip in files:
        path = root / name_in_zip
        if path.is_symlink():
            raise SystemExit(f'symlink not allowed: {path}')
        package.write(path, name_in_zip)
        (extracted / name_in_zip).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, extracted / name_in_zip)
body = archive.read_bytes()
tool = {key: manifest[key] for key in ('id', 'name', 'version', 'description', 'author')}
tool.update(
    homepage=manifest.get('homepage'), icon_url=None, banner_url=None, host=manifest['host'],
    requires=manifest.get('requires', []), optional=manifest.get('optional', []),
    replaces=manifest.get('replaces', []),
    packages={'any': {'url': args.base_url.rstrip('/') + '/' + name,
                      'sha256': hashlib.sha256(body).hexdigest(), 'size': len(body)}})
catalog = {'catalog_version': 2, 'name': 'Hello web tools',
           'published': '2026-11-25T00:00:00Z', 'tools': [tool]}
(args.out / 'catalog-v2.json').write_text(json.dumps(catalog, indent=2) + '\n')
print(archive)
print(args.out / 'catalog-v2.json')
print(extracted)
