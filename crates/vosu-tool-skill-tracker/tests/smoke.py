#!/usr/bin/env python3
"""Read-only integration smoke. Requires Python aiohttp; run after cargo build.
Creates fake osu data and an isolated SDK-launched service; never touches real data.
"""
import asyncio
import hashlib
import json
import os
from pathlib import Path
import tempfile
import aiohttp
from aiohttp import web

WORKSPACE = Path(__file__).resolve().parents[3]
SKILLS = ['overall','stream','jumpstream','handstream','stamina','jackspeed','chordjack','technical']

async def main():
    with tempfile.TemporaryDirectory(prefix='vosu-tracker-smoke-') as temp:
        root = Path(temp)
        songs = root / 'osu' / 'Songs'
        folder = songs / 'fixture'
        folder.mkdir(parents=True)
        content = b'osu file format v14\n[General]\nMode:3\n[Difficulty]\nCircleSize:4\n[HitObjects]\n64,192,1000,1,0,0:0:0:0:\n'
        checksum = hashlib.md5(content).hexdigest()
        mapfile = folder / 'fixture.osu'
        mapfile.write_bytes(content)
        connected = asyncio.Event()
        start = asyncio.Event()
        context_read = asyncio.Event()
        selected = []
        async def context(request):
            context_read.set()
            return web.json_response({'client':'stable','songs_dir':str(songs),'beatmap':{'checksum':checksum,'mode':'mania','folder':str(folder),'file':str(mapfile),'audio':None,'background':None}})
        async def calculate(request):
            assert request.headers['X-Vosu-Tool-Token'] == 'smoke-token'
            assert await request.read() == content
            assert float(request.query['score']) > .99
            return web.json_response({'checksum':checksum,'mode':'mania','keys':4,'notes':10,'rate':float(request.query['rate']),'msd':dict.fromkeys(SKILLS,20.0),'ssr':dict.fromkeys(SKILLS,19.0),'score':float(request.query['score']),'dan':{'system':'fixture','label':'Fixture 5','value':5.0,'experimental':True},'calculator':{'msd':'mock','dan':'mock'},'unsupported':None})
        async def subscription(request):
            ws=web.WebSocketResponse()
            await ws.prepare(request)
            selected.extend(await ws.receive_json())
            connected.set()
            await start.wait()
            values = [{'number':2,'name':'play'},'stable',{'checksum':checksum,'title':'Smoke map','version':'4K','mode':{'name':'mania'},'time':{'live':1000},'stats':{'cs':{'converted':4},'stars':{'live':4.5},'hitWindow':{'perfect':16,'great':40,'good':73,'ok':103,'meh':127}}},{'mode':{'name':'mania'},'score':1000000,'accuracy':100.0,'failed':False,'hits':{'geki':10},'hitOffsets':[0]*10,'mods':{'number':0,'rate':1.0},'combo':{'max':10},'pp':{'current':100}}, {},False,1,{}]
            await ws.send_json([0,1,list(enumerate(values))])
            await asyncio.wait_for(context_read.wait(),5)
            # Preserve final play data across transition; results refresh the score.
            result={'hits':{'geki':10},'score':1000000,'accuracy':100.0,'maxCombo':10,'pp':{'current':100}}
            await ws.send_json([1,2,[[0,{'number':7,'name':'resultScreen'}],[4,result]],[]])
            async for _ in ws:
                pass
            return ws
        mock=web.Application()
        mock.router.add_get('/tools/_host/v1/context',context)
        mock.router.add_post('/tools/difficulty/api/v1/calculate',calculate)
        mock.router.add_get('/websocket/v2/subscribe',subscription)
        runner=web.AppRunner(mock,shutdown_timeout=2)
        await runner.setup()
        await web.TCPSite(runner,'127.0.0.1',25550).start()
        env=os.environ.copy()
        for key in ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','XDG_STATE_HOME','XDG_RUNTIME_DIR']:
            path=root/key
            path.mkdir(mode=0o700)
            env[key]=str(path)
        env.update(VOSU_TOOL_PROTOCOL='1',VOSU_TOOL_ID='skill-tracker',VOSU_TOOL_TOKEN='smoke-token',VOSU_URL='http://127.0.0.1:25550',VOSU_TOOL_DATA_DIR=str(root/'data'),VOSU_TOOL_CACHE_DIR=str(root/'cache'))
        proc=await asyncio.create_subprocess_exec(str(WORKSPACE/'target/debug/vosu-skill-tracker'),env=env,stdin=asyncio.subprocess.PIPE,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.PIPE)
        try:
            line=(await asyncio.wait_for(proc.stdout.readline(),10)).decode().strip()
            assert line.startswith('vosu-tool-ready '), line
            base='http://127.0.0.1:'+line.split()[-1]
            async with aiohttp.ClientSession(headers={'X-Vosu-Tool-Token':'smoke-token'}) as client:
                async with client.get(base+'/healthz') as r: assert 200<=r.status<300
                async with aiohttp.ClientSession() as anonymous:
                    async with anonymous.get(base+'/v1/profile') as r: assert r.status==401
                await asyncio.wait_for(connected.wait(),5)
                async with client.ws_connect(base+'/v1/stream') as stream:
                    snapshot=await stream.receive_json(timeout=5)
                    assert snapshot['profile']['rated_scores']==0
                    start.set()
                    for _ in range(10):
                        snapshot=await stream.receive_json(timeout=5)
                        if snapshot['profile']['rated_scores']==1: break
                    assert snapshot['profile']['rated_scores']==1, snapshot
                async with client.get(base+'/v1/profile') as r: profile=await r.json()
                async with client.get(base+'/v1/history') as r: history=await r.json()
                assert profile['play_counts']['mania']==1
                assert profile['ratings']['overall']>0
                play=history['plays'][0]
                assert play['wife']['model']=='wife3-j4-offsets'
                assert play['difficulty']['ssr']['overall']==19
                assert play['outcome']=='passed'
                assert snapshot['current']['plays']==1
                evidence={'profile':profile,'history':history,'stream':snapshot,'selected_fields':selected}
                # Optional real-daemon probe is read-only, bounded, and never required.
                try:
                    async with client.ws_connect('http://127.0.0.1:24050/websocket/v2/subscribe',timeout=aiohttp.ClientWSTimeout(ws_receive=3)) as live:
                        await live.send_json(selected)
                        frame=await live.receive_json(timeout=3)
                        evidence['live_probe']={'kind':frame[0],'present_ids':[pair[0] for pair in frame[2]],'selected_fields':selected}
                except (aiohttp.ClientError,asyncio.TimeoutError,TypeError) as e:
                    evidence['live_probe']={'unavailable':str(e)}
                output=WORKSPACE/'target/skill-tracker-smoke.json'
                output.write_text(json.dumps(evidence,indent=2))
                print(json.dumps({'result':'passed','evidence':str(output),'ratings':profile['ratings'],'live_probe':evidence['live_probe']}))
                async with client.delete(base+f'/v1/plays/{play["id"]}') as r: assert r.status==204
                async with client.get(base+'/v1/history') as r: assert not (await r.json())['plays']
        finally:
            proc.stdin.close()
            await asyncio.wait_for(proc.wait(),5)
            errors=(await proc.stderr.read()).decode()
            if errors: print(errors)
            await runner.cleanup()

if __name__=='__main__':
    asyncio.run(main())
