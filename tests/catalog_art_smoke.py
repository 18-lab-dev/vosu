#!/usr/bin/env python3
"""Catalog v2 card art through vosu against an isolated test-harness daemon.

A stand-in release host serves card art the way GitHub release downloads do:
a redirect to storage that answers `application/octet-stream`. The tool icon
is an SVG carrying a <script> and an onload handler. Checks that the daemon
proxies only art the validated v2 catalog snapshot lists, with an image type
and inert-document headers, and (required browser phase) that Chromium
renders the art on the Tools and Available pages before and after
plan-confirmed installation while the same SVG opened directly on the
dashboard origin is sandboxed and runs nothing. Only test ports
25150/25151 are used.
"""
import argparse
from contextlib import ExitStack
from http.server import SimpleHTTPRequestHandler
import json
import os
from pathlib import Path
import struct
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import zlib
from smoke_process import BINARY, DASHBOARD, Daemon, curl, package_server, wait_for
from catalog_v2_fixture import Catalog, api, dashboard_headers, facts, set_sources

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
    parser.add_argument('--binary', type=Path, default=BINARY)
    parser.add_argument('--evidence', type=Path, default=Path(tempfile.gettempdir()) / 'vosu-tools-evidence/catalog-art')
    parser.add_argument('--playwright-module', type=Path,
                        default=Path(os.environ['VOSU_PLAYWRIGHT_MODULE']) if os.environ.get('VOSU_PLAYWRIGHT_MODULE') else None)
    args = parser.parse_args()
    if not args.playwright_module:
        raise SystemExit('--playwright-module (or VOSU_PLAYWRIGHT_MODULE) is required: the browser phase is not optional')
    binary = args.binary.resolve(strict=True)
    args.evidence.mkdir(parents=True, exist_ok=True)
    evidence = []
    with tempfile.TemporaryDirectory(prefix='vosu-catalog-art-') as temporary, ExitStack() as contexts:
        root = Path(temporary)
        daemon = Daemon(binary, root)
        packages = root / 'packages'
        (packages / 'art').mkdir(parents=True)
        (packages / 'art/artful-icon.svg').write_text(HOSTILE_ICON)
        (packages / 'art/artful-banner.svg').write_text(BANNER)
        (packages / 'art/artlay-preview.png').write_bytes(png(320, 180, (226, 112, 43)))
        (packages / 'art/not-an-image.svg').write_text('<html><body>not art</body></html>')
        port = contexts.enter_context(package_server(ReleaseHost, packages))
        daemon.environment['VOSU_CATALOG_TEST_SOCKET'] = f'127.0.0.1:{port}'
        host_range, target = facts(binary)
        catalog = Catalog(packages, port, host_range, target)
        base = catalog.base
        source = catalog.source('art')
        icon = f'{base}/download/artful-icon.svg'
        banner = f'{base}/download/artful-banner.svg'
        preview = f'{base}/download/artlay-preview.png'
        tool = catalog.tool('artful', 'Artful', description='Card art fixture.', icon_url=icon, banner_url=banner,
                            manifest={'icon': 'web/icon.svg', 'banner': 'web/banner.svg'},
                            files={'web/icon.svg': INSTALLED_ICON, 'web/banner.svg': BANNER})
        overlay = catalog.overlay('artlay', 'Artlay', description='Preview fixture.', preview=preview,
                                  banner_url=f'{base}/download/not-an-image.svg')
        catalog.publish('art', tools=[tool], overlays=[overlay])

        def art(url):
            return f'{DASHBOARD}/api/catalog/art?' + urllib.parse.urlencode({'url': url})

        with daemon.run(args.evidence / 'daemon.log'):
            headers = dashboard_headers()
            set_sources(headers, [source])
            # Before the catalog is fetched nothing is listed.
            assert fetch(art(icon))[0] == 404
            view = api('refresh', headers, 'POST')
            assert view['sources'][0]['status'] == 'ok', view
            entries = {entry['id']: entry for entry in api('entries', headers)['entries']}
            assert entries['artful']['icon_url'] == icon, entries

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
                            'cached per catalog snapshot; unlisted 404; non-image 502; inert-document headers')

            def browser(phase):
                daemon.browser('catalog_art_browser.mjs', args.playwright_module, args.evidence.resolve(), phase, icon)

            browser('before')
            evidence.append('Browser: Available tool icon and banner and catalog overlay preview render; '
                            'the hostile SVG opened directly is sandboxed (opaque origin), no script or request ran')

            def install(kind, item):
                plan = api('install', headers, 'POST', {'kind': kind, 'id': item, 'source': source})
                assert plan['errors'] == [] and len(plan['adds']) == 1, plan
                job = api(f'plans/{plan["plan_id"]}/confirm', headers, 'POST', {'approve': []})['job_id']

                def completed():
                    status, response = curl(f'/api/catalog/jobs/{job}', dashboard=True)
                    assert status == 200
                    result = json.loads(response)
                    assert result['status'] != 'failed', result
                    return result if result['status'] == 'completed' else None
                return wait_for(completed, timeout=45)

            install('tool', 'artful')
            install('overlay', 'artlay')
            browser('after')
            evidence.append('Browser after plan-confirmed installs: installed tool card shows its own icon and banner; '
                            'the installed catalog overlay keeps its proxied preview')
    (args.evidence / 'smoke.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print('\n'.join(evidence))


if __name__ == '__main__':
    main()
