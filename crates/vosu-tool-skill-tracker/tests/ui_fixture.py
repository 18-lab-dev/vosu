#!/usr/bin/env python3
"""Headless screenshot fixture with mocked host bridge/data, on assigned port 25551."""
import json
from pathlib import Path
from aiohttp import web

ROOT=Path(__file__).resolve().parents[1]
SKILLS=['stream','jumpstream','handstream','stamina','jackspeed','chordjack','technical']
PROFILE={'ratings':dict(zip(['overall']+SKILLS,[24.67,26.1,28.7,25.6,23.2,21.8,22.9,24.4])),'play_counts':{'mania':142,'osu':12},'rated_scores':86,'dan':{'label':'Reform 7','value':7.4,'experimental':True},'trend':[{'timestamp':f'2026-10-{21-i:02}T10:00:00Z','overall':24.67-i*.23-(i%3)*.08}for i in range(14)]}
PLAYS=[]
for i,(title,msd,ssr,acc,rate,outcome) in enumerate([('Camellia — crystallized [4K Hyper]',26.8,27.12,98.42,1.1,'passed'),('xi — FREEDOM DiVE [Another]',28.3,26.48,96.87,1.0,'passed'),('Feryquitous — Dstorv [Insane]',25.1,24.52,97.61,1.2,'passed'),('Laur — Vindication [Challenge]',30.2,25.74,91.23,1.0,'failed')]):
    PLAYS.append({'id':142-i,'title':title,'timestamp':f'2026-10-21T10:{42-i*7}:00Z','client':'lazer','mode':'mania','keys':4,'rate':rate,'outcome':outcome,'accuracy':acc,'wife':{'value':acc/100-.012,'model':'wife3-j4-offsets' if i<2 else 'wife3-j4-judgment-midpoints'},'difficulty':{'msd':{'overall':msd},'ssr':{'overall':ssr}},'calculation_error':None})
CURRENT={'plays':8,'rating_deltas':dict.fromkeys(['overall']+SKILLS,.18),'last_play':PLAYS[0],'connection':'connected'}
async def bridge(request):
    return web.Response(text="export const vosu={ready:Promise.resolve(),canWrite:new URLSearchParams(location.search).get('write')==='1',fetch:(path,options)=>fetch('/api/'+path,options)};",content_type='text/javascript')
async def theme(request):
    return web.Response(text=":root{--bg:#111214;--fg:#e7e6e1;--fg-dim:#939597;--accent:#ff963f;--on-accent:#111214;--rule:#303236;--rule-strong:#505256;--font-body:'Arial',sans-serif;--font-display:'Arial Narrow','Arial',sans-serif;--font-mono:monospace}:root[data-theme=light]{--bg:#f2f1ec;--fg:#252930;--fg-dim:#636971;--accent:#175fad;--on-accent:#fff;--rule:#d1d0ca;--rule-strong:#a1a2a0}",content_type='text/css')
async def data(request):
    path=request.match_info['path']
    if path=='v1/profile':return web.json_response(PROFILE)
    if path=='v1/current':return web.json_response(CURRENT)
    if path=='v1/history':return web.json_response({'plays':[p for p in PLAYS if not request.query.get('mode') or p['mode']==request.query['mode']],'before':139})
    if path=='v1/stream':
        ws=web.WebSocketResponse();await ws.prepare(request);await ws.send_json({'profile':PROFILE,'current':CURRENT})
        async for _ in ws:pass
        return ws
    return web.json_response({'error':'fixture route unavailable'},status=404)
async def index(request):return web.FileResponse(ROOT/'web/index.html')
app=web.Application()
app.router.add_get('/tools/_host/v1/bridge.js',bridge)
app.router.add_get('/tools/_host/v1/theme.css',theme)
app.router.add_route('*','/api/{path:.*}',data)
app.router.add_get('/',index)
app.router.add_static('/',ROOT/'web')
if __name__=='__main__':web.run_app(app,host='127.0.0.1',port=25551)
