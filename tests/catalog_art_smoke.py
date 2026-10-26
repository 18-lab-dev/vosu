#!/usr/bin/env python3
"""Catalog card art through vosu against an isolated test-harness daemon.

A stand-in release host serves card art the way GitHub release downloads do:
a redirect to storage that answers `application/octet-stream`. The tool icon
is an SVG carrying a <script> and an onload handler. Checks that the daemon
proxies only listed art with an image type and inert-document headers, and,
with --playwright-module, that a headless browser renders the art on the
Tools and Available pages before and after installation while the same SVG
opened directly on the dashboard origin is sandboxed and runs nothing.
Only test ports 25150/25151 are used.
"""
import argparse
import functools
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import signal
import struct
import subprocess
import tempfile
import threading
import urllib.error
import urllib.parse
import urllib.request
import zipfile
import zlib
from tool_host_smoke import ROOT, DASHBOARD, curl, wait_for

ART_CSP = "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:"
HOSTILE_ICON = '''<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64"
     onload="window.__svgOnload = true; fetch('/api/tools?svg-onload')">
  <rect width="64" height="64" rx="6" fill="#e2702b"/>
  <path d="M14 20 L32 46 L50 20" fill="none" stroke="#fff6ec" stroke-width="7"/>
  <script>window.__svgScript = true; fetch('/api/tools?svg-script'); new Image().src = '/api/tools/catalog/sources?svg-image';</script>
</svg>
'''
BANNER = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 400" width="1200" height="400">
  <rect width="1200" height="400" fill="#1d2433"/>
  <path d="M0 300 C300 180 600 380 1200 220 L1200 400 L0 400 Z" fill="#e2702b"/>
</svg>
'''
INSTALLED_ICON = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" fill="#2b8ae2"/></svg>'


def png(width, height, rgb):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    row = b'\0' + bytes(rgb) * width
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(row * height)) + chunk(b'IEND', b''))


class ReleaseHost(SimpleHTTPRequestHandler):
    """`/download/<name>` redirects to `/storage/<name>`, served as octet-stream."""

    hits = []

    def log_message(self, *_args):
        pass

    def do_GET(self):
        path = urllib.parse.urlsplit(self.path).path
        self.hits.append(path)
        if path.startswith('/download/'):
            self.send_response(302)
            self.send_header('Location', '/storage/' + path.removeprefix('/download/'))
            self.end_headers()
            return
        if path.startswith('/storage/'):
            file = Path(self.directory) / 'art' / path.removeprefix('/storage/')
            if not file.is_file():
                self.send_error(404)
                return
            body = file.read_bytes()
            self.send_response(200)
            self.send_header('Content-Type', 'application/octet-stream')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()


def fetch(url):
    try:
        with urllib.request.urlopen(url, timeout=15) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/vosu')
    parser.add_argument('--evidence', type=Path, default=Path('/tmp/vosu-tools-evidence/catalog-art'))
    parser.add_argument('--playwright-module', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.evidence.mkdir(parents=True, exist_ok=True)
    evidence = []
    with tempfile.TemporaryDirectory(prefix='vosu-catalog-art-') as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
        for variable in ['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS']:
            environment.pop(variable, None)
        environment['PLAYWRIGHT_BROWSERS_PATH'] = os.environ.get(
            'PLAYWRIGHT_BROWSERS_PATH', str(Path.home() / '.cache/ms-playwright'))
        for variable, directory in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'),
                                    ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'),
                                    ('XDG_STATE_HOME', 'state'), ('XDG_RUNTIME_DIR', 'runtime')]:
            path = root / directory
            path.mkdir(mode=0o700)
            environment[variable] = str(path)
        config = root / 'config/vosu'
        config.mkdir()
        (config / 'vosu.toml').write_text('''ENABLE_AUTOUPDATE = false
OPEN_DASHBOARD_ON_STARTUP = false
DEBUG_LOG = false
SERVER_IP = "127.0.0.1"
SERVER_PORT = 25150
DASHBOARD_IP = "127.0.0.1"
DASHBOARD_PORT = 25151
ALLOWED_IPS = "127.0.0.1,localhost"
''')
        packages = root / 'packages'
        (packages / 'art').mkdir(parents=True)
        (packages / 'art/artful-icon.svg').write_text(HOSTILE_ICON)
        (packages / 'art/artful-banner.svg').write_text(BANNER)
        (packages / 'art/artlay-preview.png').write_bytes(png(320, 180, (226, 112, 43)))
        (packages / 'art/not-an-image.svg').write_text('<html><body>not art</body></html>')
        server = ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(ReleaseHost, directory=str(packages)))
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        base = f'http://tools.invalid:{port}'
        source = {'source': f'{base}/catalog.json', 'enabled': True}
        (config / 'tools').mkdir()
        (config / 'tools/catalog-sources.json').write_text(json.dumps([source]))
        environment['VOSU_TOOL_CATALOG_TEST_SOCKET'] = f'127.0.0.1:{port}'

        def package(archive):
            return {'url': f'{base}/{archive.name}', 'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
                    'size': archive.stat().st_size}

        tool_archive = packages / 'artful-1.0.0.zip'
        with zipfile.ZipFile(tool_archive, 'w', zipfile.ZIP_DEFLATED) as target:
            target.writestr('tool.toml', "manifest_version = 1\nid = 'artful'\nname = 'Artful'\nversion = '1.0.0'\n"
                                         "icon = 'web/icon.svg'\nbanner = 'web/banner.svg'\n[ui]\nroot = 'web'\n")
            target.writestr('web/index.html', '<!doctype html><title>Artful</title><p>Artful</p>')
            target.writestr('web/icon.svg', INSTALLED_ICON)
            target.writestr('web/banner.svg', BANNER)
        overlay_archive = packages / 'artlay-1.0.0.zip'
        with zipfile.ZipFile(overlay_archive, 'w', zipfile.ZIP_DEFLATED) as target:
            target.writestr('index.html', '<!doctype html><h1>Artlay</h1>')
            target.writestr('metadata.txt', 'Name: Artlay\nAuthor: 18-LAB\nVersion: 1.0.0\n')
        icon = f'{base}/download/artful-icon.svg'
        banner = f'{base}/download/artful-banner.svg'
        preview = f'{base}/download/artlay-preview.png'
        (packages / 'catalog.json').write_text(json.dumps({
            'catalog_version': 1, 'published': '2026-10-26T00:00:00Z',
            'tools': [{'id': 'artful', 'name': 'Artful', 'version': '1.0.0', 'description': 'Card art fixture.',
                       'author': '18-LAB', 'homepage': None, 'icon_url': icon, 'banner_url': banner,
                       'protocol': 1, 'interfaces': ['ui'], 'requires': [], 'packages': {'any': package(tool_archive)}}],
            'overlays': [{'id': 'artlay', 'name': 'Artlay', 'version': '1.0.0', 'description': 'Preview fixture.',
                          'author': '18-LAB', 'homepage': None, 'icon_url': None,
                          'banner_url': f'{base}/download/not-an-image.svg', 'resolution': [320, 180],
                          'preview': preview, 'requires_tools': [], 'package': package(overlay_archive)}],
        }))

        def art(url):
            return f'{DASHBOARD}/api/tools/catalog/art?' + urllib.parse.urlencode({'url': url})

        with (args.evidence / 'daemon.log').open('w') as log:
            daemon = subprocess.Popen([str(binary), '--update=false'], cwd=root, env=environment,
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: curl('/api/tools', dashboard=True)[0] == 200)
                status, page = curl('/tools', dashboard=True)
                assert status == 200
                context = json.loads(re.search(r'<script[^>]*id="vosu-context"[^>]*>(.*?)</script>', page, re.S).group(1))
                headers = {'Origin': DASHBOARD, 'X-Vosu-CSRF': context['csrf']}
                # Before the catalog is loaded nothing is listed.
                assert fetch(art(icon))[0] == 404
                status, response = curl('/api/tools/catalog', dashboard=True)
                assert status == 200 and json.loads(response)['tools'][0]['icon_url'] == icon, response

                for url, kind in [(icon, 'image/svg+xml'), (banner, 'image/svg+xml'), (preview, 'image/png')]:
                    status, response_headers, body = fetch(art(url))
                    assert status == 200 and response_headers['Content-Type'] == kind, (url, status, body[:200])
                    assert body == (packages / 'art' / url.rsplit('/', 1)[1]).read_bytes()
                for url, expected in [(f'{base}/download/not-an-image.svg', 502), (f'{base}/storage/artful-icon.svg', 404),
                                      ('https://example.com/icon.svg', 404)]:
                    status, response_headers, body = fetch(art(url))
                    assert status == expected, (url, status, body)
                status, response_headers, _ = fetch(art(icon))
                for name, value in [('Content-Security-Policy', ART_CSP), ('X-Content-Type-Options', 'nosniff'),
                                    ('Cross-Origin-Resource-Policy', 'same-origin'), ('Content-Disposition', 'inline')]:
                    assert response_headers[name] == value, (name, response_headers[name])
                assert 'Set-Cookie' not in response_headers
                fetched = [hit for hit in ReleaseHost.hits if hit.startswith('/storage/artful-icon')]
                assert len(fetched) == 1, ReleaseHost.hits
                evidence.append('Listed octet-stream art behind a redirect served typed (SVG, PNG); '
                                'cached per catalog revision; unlisted 404; non-image 502; inert-document headers')

                def browser(phase):
                    subprocess.run(['node', str(ROOT / 'tests/catalog_art_browser.mjs'),
                                    str(args.playwright_module.resolve()), str(args.evidence.resolve()), phase, icon],
                                   check=True, env=environment)

                if args.playwright_module:
                    browser('before')
                    evidence.append('Browser: Available tool icon and banner and catalog overlay preview render; '
                                    'the hostile SVG opened directly is sandboxed (opaque origin), no script or request ran')

                def action(path, body=None):
                    status, response = curl(path, dashboard=True, method='POST', headers=headers, data=body or {})
                    assert status == 200, (status, response)
                    job = json.loads(response)['job_id']

                    def completed():
                        status, response = curl(f'/api/tools/catalog/jobs/{job}', dashboard=True)
                        assert status == 200
                        result = json.loads(response)
                        assert result['status'] != 'failed', result
                        return result if result['status'] == 'completed' else None
                    return wait_for(completed, timeout=45)

                action('/api/tools/artful/install')
                action('/api/tools/catalog/overlays/artlay/install', {'install_tools': False})
                if args.playwright_module:
                    browser('after')
                    evidence.append('Browser after install: installed tool card shows its own icon and banner; '
                                    'the installed catalog overlay keeps its proxied preview')
                daemon.send_signal(signal.SIGTERM)
                assert daemon.wait(timeout=15) == 0
            finally:
                if daemon.poll() is None:
                    daemon.terminate()
                    try:
                        daemon.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait()
                server.shutdown()
    (args.evidence / 'smoke.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print('\n'.join(evidence))


if __name__ == '__main__':
    main()
