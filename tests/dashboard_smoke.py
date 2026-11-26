#!/usr/bin/env python3
"""Check the dashboard in a real browser against an isolated test daemon.

Runs tests/dashboard_browser.mjs with an installed playwright index.mjs:
text baselines in the header and status strip (within 0.5 px), keyboard
reachability of every card control on the Overlays, Available and Tools
grids, the 2.0 Tools page (Ready, Blocked and legacy Disabled cards, launching
`fixture-v2` through the single-use exchange, and no call to a deleted
service-era tool route), that the dashboard's own previews are not counted as
connected browser sources, the status strip's session join, the update notice
(minor, major with consent, unsafe release URLs) and Settings filled from
SettingsView or replaced by an alert when that read fails. Then, with
tests/dashboard_notices_browser.mjs, the D20 removed-route notice (M7-N01,
M7-G01–G03): a real 1.x overlay page requests the removed WebSocket and
`/api/ingame`, and the dashboard shows the notice through its own status
refresh, with one daemon WARN for `/api/ingame`. Only test ports
25150/25151 are used.
"""
import argparse
from http.server import SimpleHTTPRequestHandler
import contextlib
import json
from pathlib import Path
import shutil
import tempfile
from smoke_process import BINARY, ROOT, Daemon, api_plugin, curl, package_server, target, wait_for
from catalog_v2_fixture import Catalog, api, dashboard_headers, facts, set_sources

# A classic inline script in <body> that opens a socket immediately, like
# most overlays: the preview marker must already be in place.
OVERLAY = '''<!doctype html>
<html><head><meta charset="utf-8"><title>{name}</title></head>
<body><p>{name}</p>
<script>new WebSocket(`ws://${{location.host}}/websocket/commands?l=${{encodeURIComponent(location.pathname.split('/')[1])}}`);</script>
</body></html>
'''

# A 1.x overlay that still uses two removed routes. Its name is hostile
# markup, which the dashboard must show as text.
HOSTILE_NAME = '<img src=x onerror="window.__d20=1">Hostile & "quoted"'
HOSTILE = '''<!doctype html>
<html><head><meta charset="utf-8"><title>1.x overlay</title></head>
<body><p>1.x overlay</p>
<script>
const socket = new WebSocket(`ws://${location.host}/websocket/v2/subscribe?l=hostile-overlay`);
socket.onopen = () => { document.body.dataset.opened = 'true'; };
socket.onclose = () => { document.body.dataset.socket = 'closed'; };
fetch('/api/ingame').then((response) => { document.body.dataset.ingame = String(response.status); });
</script></body></html>
'''
INGAME_WARNING = '/api/ingame was removed in vosu 2.0'


def removed_routes(static, args, daemon):
    """D20: real removed requests, then the notice in the browser."""
    folder = static / 'hostile-overlay'
    folder.mkdir()
    (folder / 'index.html').write_text(HOSTILE)
    (folder / 'metadata.txt').write_text(f'Name: {HOSTILE_NAME}\nAuthor: 1.x\nVersion: 1.0.0\nResolution: 400x100\n')
    # The upstream in-game client carries no identity: the unknown bucket.
    # Every exact request is the ordinary 404; a near miss is not recorded.
    for method in ['GET', 'POST', 'PUT']:
        expected = curl('/api/definitely-unknown', method=method)
        assert curl('/api/ingame?sentinel=never-retain', method=method) == expected, method
        assert expected[0] == 404, expected
    assert curl('/api/ingame/')[0] == 404
    assert curl('/files/skin/?l=overlay-1')[0] == 404
    daemon.browser('dashboard_notices_browser.mjs', args.playwright_module, args.evidence, static)
    status, body = curl('/api/dashboard/status', dashboard=True)
    assert status == 200 and 'ingame' not in json.loads(body), body
    assert 'never-retain' not in body, body


def stage_osu(daemon, pid):
    # The bundled producer reads the same input and uses the same ABI
    # publication ingress as M5's staged osu! process fixture.
    data = daemon.data / 'plugins/vosu.osu'
    data.mkdir(parents=True)
    (data / 'parity.json').write_text(json.dumps({
        'input': str(ROOT / 'tests/fixtures/oracle/stable-play-osu-fl/input.json'),
        'chart_root': str(data), 'receipts': str(data), 'pids': [pid],
    }), encoding='utf-8')
    return data / f'published-{pid}'


def verify_live_sessions(synthetic_pid, osu_pid, osu_receipt):
    def sessions():
        status, body = curl('/api/v1/sessions')
        assert status == 200, body
        found = {s['pid']: s for s in json.loads(body)['sessions']}
        if set(found) == {synthetic_pid, osu_pid} and osu_receipt.is_file():
            return found
        return None

    live = wait_for(sessions)
    synthetic, osu = live[synthetic_pid], live[osu_pid]
    assert synthetic['plugin']['id'] == 'test.api.fixture' and synthetic['variant'] == 'synthetic', synthetic
    assert osu['plugin']['id'] == 'vosu.osu' and osu['variant'] == 'stable', osu
    assert synthetic['session'] != osu['session']
    state = {}
    for session in (synthetic, osu):
        status, body = curl(f'/api/v1/sessions/{session["session"]}/state')
        assert status == 200, body
        state[session['plugin']['id']] = json.loads(body)
    file_url = state['test.api.fixture']['test.api.fixture']['resources']['file']['url']
    assert curl(file_url) == (200, (ROOT / 'tests/fixtures/api-v1-plugin/fixture.osu').read_bytes().decode())
    assert 'vosu.osu' not in state['test.api.fixture']
    assert state['vosu.osu']['vosu.play']['accuracy'] == 1
    assert 'test.api.fixture' not in state['vosu.osu']
    assert 'internal' not in state['vosu.osu']['vosu.osu']
    print('M7-X0 identifiable testkit and harness-driven osu! state from distinct ABI providers')


def verify_live_daemon(binary, root):
    # Keep the existing visual baseline a comparison of the idle one-row
    # status strip. The mixed-provider positive control has its own daemon.
    root.mkdir()
    daemon = Daemon(binary, root)
    with target(root) as synthetic, target(root, 'osu!') as osu:
        receipt = stage_osu(daemon, osu[0])
        daemon.discover(synthetic, osu)
        with daemon.run(root / 'daemon.log', '--plugin', str(api_plugin(root))):
            verify_live_sessions(synthetic[0], osu[0], receipt)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=BINARY)
    parser.add_argument('--evidence', type=Path, default=Path(tempfile.gettempdir()) / 'vosu-dashboard-evidence')
    parser.add_argument('--playwright-module', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.evidence.mkdir(parents=True, exist_ok=True)
    args.evidence = args.evidence.resolve()
    with tempfile.TemporaryDirectory(prefix='vosu-dashboard-smoke-') as temporary, contextlib.ExitStack() as contexts:
        root = Path(temporary)
        verify_live_daemon(binary, root / 'live-state')
        static = root / 'static'
        for index in range(6):
            folder = static / f'overlay-{index}'
            folder.mkdir(parents=True)
            (folder / 'index.html').write_text(OVERLAY.format(name=f'overlay-{index}'))
            (folder / 'metadata.txt').write_text(f'Name: Overlay {index}\nAuthor: 18-LAB\nVersion: 1.0.0\nResolution: 400x100\n')
        daemon = Daemon(binary, root, allowed='127.0.0.1,localhost,absolute', static=static)
        # The ready tool requires the live fixture action and state. A second
        # ready tool needs no capabilities; the blocked tool names a missing action.
        tools = []
        v2_manifest = (ROOT / 'tests/fixtures/tool-host-v2/manifest.toml').read_text()
        for tool_id in ['fixture-v2', 'fixture-v2-two', 'fixture-v2-blocked']:
            tool = root / 'tools' / tool_id
            shutil.copytree(ROOT / 'tests/fixtures/tool-host-v2', tool)
            text = v2_manifest.replace("id = 'fixture-v2'", f"id = '{tool_id}'")
            if tool_id != 'fixture-v2':
                text = text.replace("name = 'M3 browser fixture'", f"name = '{tool_id}'")
            if tool_id == 'fixture-v2-two':
                text = text.split('[[requires]]')[0]
            if tool_id == 'fixture-v2-blocked':
                text = text.replace('action:test.api.fixture.run', 'action:test.api.fixture.missing')
            (tool / 'manifest.toml').write_text(text)
            tools += ['--tool', str(tool)]
        legacy = root / 'tools' / 'fixture'
        shutil.copytree(ROOT / 'tests/fixtures/tool-host', legacy)
        tools += ['--tool', str(legacy)]
        # A catalog-v2 source with two overlays and two tools, so the
        # Available and Tools pages have catalog cards beside the
        # (unreachable) community catalog. Needs a `--features test-harness`
        # build; packages are listed but never downloaded here.
        packages = root / 'packages'
        packages.mkdir()
        port = contexts.enter_context(package_server(SimpleHTTPRequestHandler, packages))
        host_range, triple = facts(binary)
        catalog = Catalog(packages, port, host_range, triple)
        catalog.publish('keyboard',
                        tools=[catalog.tool(f'catalog-tool-{index}', f'Catalog tool {index}',
                                            description='A catalog tool for keyboard checks.') for index in range(2)],
                        overlays=[catalog.overlay(f'source-overlay-{index}', f'Source overlay {index}',
                                                  description='A catalog-source overlay for keyboard checks.')
                                  for index in range(2)])
        daemon.environment['VOSU_CATALOG_TEST_SOCKET'] = f'127.0.0.1:{port}'
        daemon.discover(contexts.enter_context(target(root)))
        tools += ['--plugin', str(api_plugin(root))]
        with daemon.run(args.evidence / 'daemon.log', *tools):
            headers = dashboard_headers()
            set_sources(headers, [catalog.source('keyboard')])
            assert api('refresh', headers, 'POST')['sources'][0]['status'] == 'ok'
            # Unknown routes: styled for a person, plain for scripts.
            for dashboard in [False, True]:
                status, body = curl('/definitely-missing/', dashboard=dashboard, headers={'Accept': 'text/html'})
                assert status == 404 and 'Page not found' in body and '/assets/system.css' in body, body
                assert curl('/definitely-missing/', dashboard=dashboard) == (404, 'Not Found')
            # The full visual corpus includes four real settings fade/expiry
            # cycles and screenshots at several widths. Keep per-operation
            # Playwright bounds; give the aggregate enough time on the VM.
            daemon.browser('dashboard_browser.mjs', args.playwright_module, args.evidence, timeout=240)
            removed_routes(static, args, daemon)
        # One WARN per daemon process in each sink, never its query.
        logs = [args.evidence / 'daemon.log', *root.rglob('*.log')]
        counts = [path.read_text(errors='replace').count(INGAME_WARNING) for path in logs]
        assert max(counts) == 1 and all(count in (0, 1) for count in counts), list(zip(logs, counts))
        assert not any('never-retain' in path.read_text(errors='replace') for path in logs)
    print((args.evidence / 'dashboard.json').read_text())


if __name__ == '__main__':
    main()
