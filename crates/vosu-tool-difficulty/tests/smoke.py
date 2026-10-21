#!/usr/bin/env python3
"""Run against an isolated binary; never contacts the user's vosu or Songs."""
import concurrent.futures
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

root = Path(__file__).resolve().parents[1]
source = Path(sys.argv[2]) if len(sys.argv) > 2 else root / 'tests/fixtures/cyclic.osu'
with tempfile.TemporaryDirectory(prefix='vosu-difficulty-smoke-') as tmp:
    tmp = Path(tmp)
    chart = tmp / 'map.osu'
    chart.write_bytes(source.read_bytes())
    token = 'ab' * 32
    class Host(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            assert self.path == '/tools/_host/v1/context'
            assert self.headers.get('X-Vosu-Tool-Token') == token
            self.send_response(200)
            self.end_headers()
            self.wfile.write(json.dumps({'client':'stable','songs_dir':str(tmp),'beatmap':{'checksum':hashlib.md5(chart.read_bytes()).hexdigest(),'mode':'mania','folder':str(tmp),'file':str(chart),'audio':None,'background':None}}).encode())
        def log_message(self, *_):
            pass
    host = http.server.ThreadingHTTPServer(('127.0.0.1',25350),Host)
    threading.Thread(target=host.serve_forever,daemon=True).start()
    env = os.environ.copy()
    for key in ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','XDG_STATE_HOME','XDG_RUNTIME_DIR']:
        env[key] = str(tmp)
    env.update(VOSU_TOOL_PROTOCOL='1',VOSU_TOOL_ID='difficulty',VOSU_TOOL_TOKEN=token,VOSU_URL='http://127.0.0.1:25350',VOSU_TOOL_DATA_DIR=str(tmp/'data'),VOSU_TOOL_CACHE_DIR=str(tmp/'cache'))
    proc = subprocess.Popen([sys.argv[1]],stdin=subprocess.PIPE,stdout=subprocess.PIPE,env=env,text=True)
    try:
        handshake=proc.stdout.readline().strip()
        assert handshake.startswith('vosu-tool-ready '),handshake
        print(handshake)
        base='http://127.0.0.1:'+handshake.split()[1]
        def request(path,body=None,auth=True):
            req=urllib.request.Request(base+path,data=body,headers={'X-Vosu-Tool-Token':token} if auth else {})
            try:
                with urllib.request.urlopen(req,timeout=30) as r:
                    data=r.read()
                    return r.status,json.loads(data) if data else None
            except urllib.error.HTTPError as e:
                return e.code,e.read()
        assert request('/healthz',auth=False)[0]==401
        print('unauthenticated: 401')
        start=time.perf_counter()
        status,d=request('/v1/calculate?rate=1&score=0.93',chart.read_bytes())
        assert status==200,(status,d)
        assert d['msd'] and d['ssr'] and d['mode']=='mania'
        print('POST',source.name,'notes',d['notes'],'MSD',d['msd']['overall'],'seconds',round(time.perf_counter()-start,4))
        md5=d['checksum']
        assert request('/v1/beatmap/'+md5+'?score=0.93')[1]==d
        assert request('/v1/beatmap/current?score=0.93')[1]==d
        status,table=request('/v1/beatmap/'+md5+'/rates')
        assert status==200 and len(table['rates'])==14
        assert table['rates'][0]['rate']==0.7 and table['rates'][-1]['rate']==2.0
        print('cached lookup/current/rates: 200, 14 rates')
        assert request('/v1/beatmap/missing')[0]==404
        assert request('/v1/calculate?rate=2.01',chart.read_bytes())[0]==400
        assert request('/v1/calculate',b'x'*(1024*1024+1))[0]==413
        dense=('osu file format v14\n[General]\nMode:3\n[Difficulty]\nCircleSize:4\n[HitObjects]\n'+''.join(f'{64+(i%4)*128},192,{1000+i*15},1,0\n' for i in range(20000))).encode()
        start=time.perf_counter()
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            results=list(pool.map(lambda _:request('/v1/calculate?rate=1.25',dense),range(8)))
        assert all(s==200 and r==results[0][1] for s,r in results)
        print('dense 20000 notes, 8 coalesced requests seconds',round(time.perf_counter()-start,4))
        proc.stdin.close()
        assert proc.wait(timeout=5)==0
        print('stdin EOF: exit 0')
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        host.shutdown()
