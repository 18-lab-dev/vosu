#!/usr/bin/env python3
"""Exercise the real daemon's web-only tool host with isolated XDG roots and curl.

A 2.0 tool is a static web package served by the bundled `vosu.toolhost`
(A7.2). The legacy `tests/fixtures/tool-host` `tool.toml` package is listed
`disabled` with `needs-2.0-update`, never parsed, run, proxied or served, and
its files are byte-identical afterwards. `tests/tool_bridge.mjs` first checks
the one bridge module in isolation. The required browser phase
(`tests/tool_host_v2_browser.mjs`) drives the `fixture-v2` web tool through the
dashboard launch, the single-use session exchange and the API v1 bridge, plus
the bridge's authority, reconnect and removed-route counterexamples and a
protected job artifact download (B0) refused to another tab and a revoked tab.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from smoke_process import BINARY, DASHBOARD, NODE, ROOT, Daemon, api_plugin, check_ports, curl, target, wait_for

NEEDS_UPDATE = 'needs-2.0-update'

def receipts(directory):
    """SHA-256 of every file below `directory`, keyed by relative path."""
    return {path.relative_to(directory).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(directory.rglob('*')) if path.is_file()}


def tool_view(tool_id):
    status, body = curl(f'/api/tools/{tool_id}', dashboard=True)
    assert status == 200, (tool_id, status, body)
    return json.loads(body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=BINARY)
    parser.add_argument('--playwright-module', type=Path,
                        default=os.environ.get('VOSU_PLAYWRIGHT_MODULE'))
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    if args.playwright_module is None or not args.playwright_module.is_file():
        parser.error('required browser phase needs --playwright-module or VOSU_PLAYWRIGHT_MODULE')
    check_ports()
    bridge = subprocess.run([NODE, '--experimental-vm-modules', '--test', '--test-reporter=tap', str(ROOT / 'tests/tool_bridge.mjs')],
                            cwd=ROOT, check=True, timeout=120, capture_output=True, text=True)
    print(bridge.stdout, end='')
    assert re.search(r'^# pass [1-9][0-9]*$', bridge.stdout, re.M), 'no named bridge unit tests passed'
    playwright = args.playwright_module.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='vosu-tool-smoke-') as temporary:
        root = Path(temporary)
        daemon = Daemon(binary, root, allowed='127.0.0.1,localhost,absolute')
        tool = root / 'fixture'
        shutil.copytree(ROOT / 'tests/fixtures/tool-host', tool)
        legacy_receipts = receipts(tool)
        v2_tool = root / 'fixture-v2'
        shutil.copytree(ROOT / 'tests/fixtures/tool-host-v2', v2_tool)
        malformed = root / 'malformed-v2'
        shutil.copytree(tool, malformed)
        (malformed / 'manifest.toml').write_text('invalid v2')
        malformed_receipt = receipts(malformed)
        reader = root / 'fixture-reader'
        shutil.copytree(v2_tool, reader)
        manifest = reader / 'manifest.toml'
        manifest.write_text(manifest.read_text().split('[[requires]]')[0].replace("'fixture-v2'", "'fixture-reader'"))
        plugin = api_plugin(root)
        with target(root) as candidate:
            daemon.discover(candidate)
            command = ('--tool', str(tool), '--tool', str(v2_tool), '--tool', str(malformed),
                       '--tool', str(reader), '--plugin', str(plugin))
            with daemon.run(root / 'daemon.log', *command):
                status, _ = curl('/tools')
                assert status == 302
                print('curl GET /tools -> 302 (dashboard redirect)')
                status, page = curl('/tools', dashboard=True)
                assert status == 200 and 'tools-root' in page
                context = json.loads(re.search(r'<script[^>]*id="vosu-context"[^>]*>(.*?)</script>', page, re.S).group(1))
                management = {'Origin': DASHBOARD, 'X-Vosu-CSRF': context['csrf']}

                # Legacy `tool.toml`: listed, diagnosed, never presented or run.
                view = tool_view('fixture')
                assert view['status'] == 'disabled' and view['status_detail'] == NEEDS_UPDATE, view
                assert view['enabled'] is False and view['ui_url'] is None, view
                print(f'curl GET dashboard /api/tools/fixture -> disabled {NEEDS_UPDATE}')
                for action in ['enable', 'trust', 'launch']:
                    status, body = curl(f'/api/tools/fixture/{action}', dashboard=True, method='POST',
                                        headers=management, data={})
                    assert status == 400 and json.loads(body)['error'] == NEEDS_UPDATE, (action, status, body)
                print(f'curl POST dashboard /api/tools/fixture/{{enable,trust,launch}} -> 400 {NEEDS_UPDATE}')
                for method, path in [('GET', '/tools/fixture/'), ('GET', '/tools/fixture/api/v1/read'),
                                     ('POST', '/tools/fixture/api/v1/write'),
                                     ('GET', '/tools/fixture/_settings')]:
                    status, _ = curl(path, method=method, data={} if method == 'POST' else None)
                    assert status == 404, (method, path, status)
                print('curl legacy UI, settings and /api/* proxy paths -> 404 (no service, no proxy)')
                for path in ['/tools/_host/v1/self', '/tools/_host/v1/settings',
                             '/tools/_host/v1/context', '/tools/_host/v1/overlays',
                             '/tools/_host/v1/bridge.js', '/tools/_host/v1/cli',
                             '/assets/tool-bridge-v1.js', '/assets/tool-bridge.js',
                             '/assets/design.html']:
                    status, _ = curl(path)
                    assert status == 404, (path, status)
                print('curl deleted /tools/_host/v1/{self,settings,context,overlays,bridge.js,cli} '
                      'and /assets/{tool-bridge-v1.js,tool-bridge.js,design.html} -> 404')

                # A broken manifest with byte-identical legacy input is invalid,
                # not a legacy package and never a fallback to its service.
                invalid = tool_view('malformed-v2')
                assert invalid['status'] == 'invalid' and invalid['status_detail'] != NEEDS_UPDATE, invalid
                assert invalid['ui_url'] is None, invalid
                invalid_status, _ = curl('/tools/malformed-v2/')
                assert invalid_status == 404, invalid_status
                print('M7-T04 legacy and malformed-manifest precedence')

                # The v2 web tool: static UI, explicit api/* 404, 503 when not servable.
                wait_for(lambda: tool_view('fixture-v2')['status'] == 'running')
                status, body = curl('/tools/fixture-v2/')
                assert status == 200 and '<title>M3 browser fixture</title>' in body
                status, _ = curl('/tools/fixture-v2/api/x')
                assert status == 404
                status, bridge = curl('/tools/_host/v2/bridge.js')
                assert status == 200 and 'export' in bridge
                print('curl GET /tools/fixture-v2/ -> 200; /tools/fixture-v2/api/x -> 404; v2 bridge -> 200')
                status, _ = curl('/api/tools/fixture-v2/disable', dashboard=True, method='POST',
                                 headers=management, data={})
                assert status == 200
                wait_for(lambda: curl('/tools/fixture-v2/')[0] == 503)
                status, _ = curl('/api/tools/fixture-v2/enable', dashboard=True, method='POST',
                                 headers=management, data={})
                assert status == 200
                wait_for(lambda: curl('/tools/fixture-v2/')[0] == 200)
                print('disable fixture-v2 -> /tools/fixture-v2/ 503; enable -> 200')
                def identified():
                    _, body = curl('/api/v1/sessions')
                    return next((s for s in json.loads(body)['sessions'] if s['plugin']['id'] == 'test.api.fixture'), None)
                session = wait_for(identified)
                assert session['pid'] == candidate[0] and session['variant'] == 'synthetic', session
                _, body = curl('/api/plugins', dashboard=True)
                plugin_view = next(p for p in json.loads(body)['plugins'] if p['id'] == 'test.api.fixture')
                assert plugin_view['trust'] == 'development' and plugin_view['runtime'] == 'in-process', plugin_view
                print('M7-X0 loaded development in-process plugin and identified API v1 state')
                daemon.browser('tool_host_v2_browser.mjs', playwright)
        assert not (daemon.data / 'tool-data/fixture').exists(), 'legacy application data was created'
        assert receipts(tool) == legacy_receipts, 'legacy package files changed'
        assert receipts(malformed) == malformed_receipt, 'malformed package files changed'
        print('M7-T04 clean daemon stop and unchanged source package receipts')


if __name__ == '__main__':
    main()
