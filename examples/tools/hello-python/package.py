"""Package this dependency-free example (Python 3.11+); no network or build steps."""
import argparse
import hashlib
import json
from pathlib import Path
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
manifest = tomllib.loads((root / 'tool.toml').read_text())
args.out.mkdir(parents=True, exist_ok=True)
name = f"vosu-tool-{manifest['id']}-{manifest['version']}-any.zip"
archive = args.out / name
files = ['tool.toml', 'main.py', 'settings.json', 'LICENSE']
files += [p.relative_to(root).as_posix() for p in sorted((root / 'web').rglob('*')) if p.is_file()]
with ZipFile(archive, 'w', compression=ZIP_DEFLATED) as package:
    for name_in_zip in files:
        path = root / name_in_zip
        if path.is_symlink():
            raise SystemExit(f'symlink not allowed: {path}')
        package.write(path, name_in_zip)
body = archive.read_bytes()
tool = {key: manifest[key] for key in ('id', 'name', 'version', 'description', 'author')}
tool.update(protocol=1, interfaces=['ui', 'service', 'cli'], requires=[], packages={
    'any': {'url': args.base_url.rstrip('/') + '/' + name,
            'sha256': hashlib.sha256(body).hexdigest(), 'size': len(body)}})
catalog = {'catalog_version': 1, 'name': 'Hello Python tools',
           'published': '2026-10-24T00:00:00Z', 'tools': [tool]}
(args.out / 'catalog.json').write_text(json.dumps(catalog, indent=2) + '\n')
print(archive)
print(args.out / 'catalog.json')
