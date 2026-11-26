"""Shared catalog-v2 fixtures for the isolated browser smokes.

A `--features test-harness` daemon started with VOSU_CATALOG_TEST_SOCKET
serves static `https://catalog.invalid:<port>/…/catalog-v2.json` sources,
their packages and art from one exact loopback listener over plain HTTP
(src/server/catalog_api/network.rs). Catalogs still pass the daemon's one
SDK validation; packages still pass hash/size/ZIP/manifest verification.
"""
import hashlib
import io
import json
import re
import zipfile

from smoke_process import DASHBOARD, curl

HOST = 'catalog.invalid'
# docs/v2/catalog-v2.md "Trust": the exact normative warning.
TRUST_WARNING = ('Plugins from this source can run in-process, with the same memory access as vosu itself. '
                 'Only trust sources you control or know well.')


def toml_value(value):
    if isinstance(value, bool):
        return 'true' if value else 'false'
    if isinstance(value, (int, float)):
        return str(value)
    return json.dumps(value)


def manifest(fields, tables=(), arrays=()):
    """A tiny TOML writer for flat manifests plus named tables/arrays of tables."""
    lines = [f'{key} = {toml_value(value)}' for key, value in fields.items()]
    for name, table in tables:
        lines.append(f'[{name}]')
        lines += [f'{key} = {toml_value(value)}' for key, value in table.items()]
    for name, rows in arrays:
        for row in rows:
            lines.append(f'[[{name}]]')
            lines += [f'{key} = {toml_value(value)}' for key, value in row.items()]
    return '\n'.join(lines) + '\n'


class Catalog:
    """Packages and catalogs written into the served `packages` directory."""

    def __init__(self, root, port, host_range, target):
        self.root = root
        self.base = f'https://{HOST}:{port}'
        self.host_range = host_range
        self.target = target

    def source(self, name):
        return f'{self.base}/{name}/catalog-v2.json'

    def package(self, name, files):
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, 'w', zipfile.ZIP_DEFLATED) as archive:
            for path, body in files.items():
                archive.writestr(path, body)
        data = buffer.getvalue()
        (self.root / f'{name}.zip').write_bytes(data)
        return {'url': f'{self.base}/{name}.zip', 'sha256': hashlib.sha256(data).hexdigest(), 'size': len(data)}

    def tool(self, tool_id, name, version='1.0.0', **extra):
        description = extra.pop('description', 'A catalog v2 tool fixture.')
        text = manifest({'manifest_version': 1, 'kind': 'tool', 'id': tool_id, 'name': name, 'version': version,
                         'host': self.host_range, 'description': description, 'author': '18-LAB',
                         **extra.pop('manifest', {})}, tables=[('ui', {'root': 'web'})])
        package = self.package(f'{tool_id}-{version}', {
            'manifest.toml': text,
            'web/index.html': f'<!doctype html><title>{name}</title><p>{name} {version}</p>',
            **extra.pop('files', {}),
        })
        return {'id': tool_id, 'name': name, 'version': version, 'host': self.host_range,
                'description': description, 'author': '18-LAB', 'packages': {'any': package}, **extra}

    def overlay(self, overlay_id, name, version='1.0.0', **extra):
        description = extra.pop('description', 'A catalog v2 overlay fixture.')
        text = manifest({'manifest_version': 1, 'kind': 'overlay', 'id': overlay_id, 'name': name,
                         'version': version, 'host': self.host_range, 'description': description,
                         'author': '18-LAB', 'resolution': [640, 120]})
        package = self.package(f'{overlay_id}-{version}', {
            'manifest.toml': text,
            'index.html': f'<!doctype html><h1>{name} {version}</h1>',
        })
        return {'id': overlay_id, 'name': name, 'version': version, 'host': self.host_range,
                'description': description, 'author': '18-LAB', 'resolution': [640, 120],
                'packages': {'any': package}, **extra}

    def plugin(self, plugin_id, runtime, artifact, requires=(), provides=(), matcher=None, settings=None,
               *, linux_user_namespaces=False, extra_files=None):
        library = f'lib/{artifact.name}'
        fields = {'manifest_version': 1, 'kind': 'plugin', 'id': plugin_id, 'name': plugin_id, 'version': '1.0.0',
                  'host': self.host_range, 'abi_version': 1, 'runtime': runtime}
        files = {library: artifact.read_bytes(), **(extra_files or {})}
        if settings is not None:
            fields['settings_schema'] = 'settings.schema.json'
            files['settings.schema.json'] = json.dumps(settings)
        tables = [(f'targets.{self.target}', {'library': library})]
        if linux_user_namespaces:
            del fields['runtime']
            tables.extend([('runtime', {'mode': runtime}), ('runtime.linux', {'user_namespaces': True})])
        if matcher is not None:
            tables.append(('match', matcher))
        text = manifest(fields, tables=tables,
                        arrays=[('requires', list(requires)), ('provides', [
                            {key: value for key, value in provide.items() if key in ('id', 'version')}
                            for provide in provides])])
        package = self.package(f'{plugin_id}-1.0.0', {'manifest.toml': text, **files})
        entry = {'id': plugin_id, 'name': plugin_id, 'version': '1.0.0', 'host': self.host_range, 'runtime': runtime,
                 'requires': list(requires), 'provides': list(provides), 'packages': {self.target: package}}
        if linux_user_namespaces:
            entry['linux_user_namespaces'] = True
        if matcher is not None:
            entry['matcher'] = matcher
        return entry

    def publish(self, name, tools=(), overlays=(), plugins=()):
        directory = self.root / name
        directory.mkdir(exist_ok=True)
        (directory / 'catalog-v2.json').write_text(json.dumps({
            'catalog_version': 2, 'name': f'{name} fixture', 'published': '2026-11-24T00:00:00Z',
            'tools': list(tools), 'overlays': list(overlays), 'plugins': list(plugins)}))


def dashboard_headers():
    status, page = curl('/tools', dashboard=True)
    assert status == 200
    context = json.loads(re.search(r'<script[^>]*id="vosu-context"[^>]*>(.*?)</script>', page, re.S).group(1))
    return {'Origin': DASHBOARD, 'X-Vosu-CSRF': context['csrf']}


def api(path, headers, method='GET', data=None, expect=200):
    status, body = curl(f'/api/catalog/{path}', dashboard=True, method=method,
                        headers=headers if method != 'GET' else None, data=data)
    assert status == expect, (method, path, status, body)
    return json.loads(body) if body else None


def set_sources(headers, sources):
    return api('sources', headers, 'POST', {'sources': [{'source': source, 'enabled': True} for source in sources]})


def facts(binary):
    """`(host range, target)` for this executable's catalog entries."""
    import subprocess
    version = subprocess.check_output([str(binary), '--version'], text=True, timeout=10).strip().split()[-1]
    import platform
    target = 'x86_64-pc-windows-msvc' if platform.system() == 'Windows' else 'x86_64-unknown-linux-gnu'
    return f'={version}', target
