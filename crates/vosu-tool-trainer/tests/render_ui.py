#!/usr/bin/env python3
"""Render the real trainer UI with a deliberately mocked host, headlessly."""
import argparse
import http.server
import json
import pathlib
import subprocess
import tempfile
import threading
import urllib.parse

parser = argparse.ArgumentParser()
parser.add_argument("--firefox", default="firefox")
parser.add_argument("--output", type=pathlib.Path, required=True)
args = parser.parse_args()
web = pathlib.Path(__file__).resolve().parents[1] / "web"
args.output.mkdir(parents=True, exist_ok=True)
current = {"client":"stable","beatmap":{"artist":"18-LAB","title":"Synthetic / Signal Study","version":"Training Sequence","mode":"mania","bpm":144,"hp":6,"cs":4,"ar":9,"od":8},"difficulty":{"msd":{"overall":23.15},"dan":{"label":"Reform 7","experimental":True}}}
generated = {"maps":[{"id":"smoke","name":"[Training Sequence] 1.3x (187.2bpm) AR10 OD9.2","export":False,"files":[],"warnings":[]}]}

class Host(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = urllib.parse.urlsplit(self.path).path
        kind = "text/plain"
        if path == "/tools/_host/v1/bridge.js":
            kind = "text/javascript"
            fixtures = json.dumps({"current":current,"generated":generated})
            content = (f"const fixtures={fixtures};" + """const p=new URLSearchParams(location.search); document.documentElement.dataset.theme=p.get('theme')||'dark'; export const vosu={ready:Promise.resolve(),canWrite:p.get('readonly')!=='1',fetch:async(path)=>new Response(JSON.stringify(path.startsWith('v1/current')?fixtures.current:fixtures.generated),{headers:{'Content-Type':'application/json'}})};""").encode()
        elif path == "/tools/_host/v1/theme.css":
            kind = "text/css"
            content = b':root[data-theme="light"]{color-scheme:light;--bg:#f2f1ec;--bg-raised:#e7e6e0;--rule:#c9c9c2;--fg:#171d24;--muted:#606772;--accent:#175fad;--on-accent:#fff}'
        elif path.startswith("/tools/trainer/") and "/api/" not in path:
            name = path.removeprefix("/tools/trainer/") or "index.html"
            file = (web / name).resolve()
            if not file.is_relative_to(web) or not file.is_file():
                self.send_error(404)
                return
            content = file.read_bytes()
            kind = {".html":"text/html",".js":"text/javascript",".css":"text/css"}.get(file.suffix,"text/plain")
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type",kind)
        self.send_header("Content-Length",str(len(content)))
        self.end_headers()
        self.wfile.write(content)

    def log_message(self, *_):
        pass

server = http.server.ThreadingHTTPServer(("127.0.0.1",25451),Host)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    for theme, readonly in [("dark",False),("light",False),("dark",True)]:
        name = f"trainer-{theme}{'-readonly' if readonly else ''}.png"
        with tempfile.TemporaryDirectory(prefix="trainer-firefox-") as profile:
            result = subprocess.run([args.firefox,"--headless","--no-remote","--profile",profile,"--window-size","1440,1600","--screenshot",str((args.output/name).resolve()),f"http://127.0.0.1:25451/tools/trainer/?theme={theme}&readonly={int(readonly)}"],capture_output=True,text=True,timeout=60)
            if result.returncode:
                raise RuntimeError(result.stderr)
            screenshot = args.output/name
            assert screenshot.is_file() and screenshot.stat().st_size > 10000
            print(screenshot.resolve())
finally:
    server.shutdown()
