import { vosu } from '/tools/_host/v1/bridge.js';
const $ = id => document.getElementById(id);
const skills = ['stream','jumpstream','handstream','stamina','jackspeed','chordjack','technical'];
let before = null, job = null, generation = 0;
const number = n => Number.isFinite(n) ? n.toFixed(2) : '—';
function error(e) { $('error').hidden = false; $('error').textContent = e.message || String(e); }
async function api(path, options) { const r = await vosu.fetch(path, options); if (!r.ok) { const e = await r.json().catch(() => ({})); throw new Error(e.error || `Request failed (${r.status})`); } return r.status === 204 ? null : r.json(); }
function text(tag, value, cls) { const e = document.createElement(tag); e.textContent = value; if (cls) e.className = cls; return e; }
function profile(p) {
  $('overall').textContent = number(p.ratings.overall);
  $('play-count').textContent = `${Object.values(p.play_counts).reduce((a,b)=>a+b,0)} recorded plays · ${p.rated_scores} rated scores`;
  $('dan').textContent = p.dan?.label || 'UNRANKED';
  const maximum = Math.max(10, ...skills.map(k => p.ratings[k] || 0));
  $('skills').replaceChildren(...skills.map(k => { const e = text('div','','skill'), label = document.createElement('div'); label.append(text('span',k.toUpperCase()),text('span',number(p.ratings[k]))); const bar = text('div','','bar'), fill = document.createElement('i'); fill.style.width = `${Math.max(0,Math.min(100,(p.ratings[k] || 0)/maximum*100))}%`; bar.append(fill); e.append(label,bar); return e; }));
  const point = (i,r) => { const a = -Math.PI/2+i*2*Math.PI/7; return `${120+Math.cos(a)*r},${120+Math.sin(a)*r}`; };
  const radar = $('radar'); radar.replaceChildren();
  function svg(tag,attrs) { const e = document.createElementNS('http://www.w3.org/2000/svg',tag); for(const [k,v] of Object.entries(attrs))e.setAttribute(k,v); return e; }
  for(const r of [25,50,75,100])radar.append(svg('polygon',{points:skills.map((_,i)=>point(i,r)).join(' '),fill:'none',stroke:'var(--rule, #343537)','stroke-width':1}));
  skills.forEach((_,i) => {const [x,y]=point(i,100).split(',');radar.append(svg('line',{x1:120,y1:120,x2:x,y2:y,stroke:'var(--rule, #343537)'}));});
  radar.append(svg('polygon',{points:skills.map((k,i)=>point(i,100*(p.ratings[k]||0)/maximum)).join(' '),fill:'var(--accent, #ff963f)','fill-opacity':'.12',stroke:'var(--accent, #ff963f)','stroke-width':2}));
  const trend = [...p.trend].reverse(), chart = svg('svg',{viewBox:'0 0 300 80',role:'img','aria-label':'Overall rating over recorded changes'});
  chart.append(svg('line',{x1:0,y1:72,x2:300,y2:72,stroke:'var(--rule, #343537)'}));
  if(trend.length){const lo = Math.min(...trend.map(t=>t.overall)), hi = Math.max(...trend.map(t=>t.overall)); const points = trend.map((t,i)=>`${trend.length===1?150:i/(trend.length-1)*300},${65-(t.overall-lo)/Math.max(1,hi-lo)*55}`).join(' ');chart.append(svg('polyline',{points,fill:'none',stroke:'var(--accent, #ff963f)','stroke-width':2}));} $('trend').replaceChildren(chart);
}
function current(c) { $('session-plays').replaceChildren(document.createTextNode(`${c.plays} `),text('span','PLAYS')); $('delta').textContent = `${c.rating_deltas.overall >= 0?'+':''}${number(c.rating_deltas.overall)}`; $('last-play').textContent = c.last_play?.title || '—'; $('connection').textContent = c.connection.toUpperCase(); }
function row(p) {
  const r = document.createElement('tr'), title = text('td',p.title); title.append(text('small',`${new Date(p.timestamp).toLocaleString()} / ${p.client} / ${p.mode}${p.keys?` ${p.keys}K`:''}`)); r.append(title);
  const result = text('td',p.outcome.toUpperCase()); if(p.outcome==='passed')result.className='outcome'; r.append(result,text('td',`${number(p.rate)}×`),text('td',number(p.difficulty?.msd?.overall ?? p.stars)),text('td',number(p.difficulty?.ssr?.overall ?? p.pp)),text('td',`${number(p.accuracy)}%`));
  const model = text('td',p.mode==='mania'?`${number(p.wife.value*100)}%`:'—'); model.append(text('small',p.mode==='mania'?(p.wife.model.includes('offsets')?'WIFE3 / OFFSETS':'APPROX / JUDGMENTS'):'NATIVE PP')); if(p.calculation_error){model.title=p.calculation_error;model.append(text('small','CALC UNAVAILABLE'));} r.append(model);
  const actions = document.createElement('td'); if(vosu.canWrite){const b=text('button','DELETE','delete'); b.addEventListener('click',async()=>{if(!confirm(`Delete this recorded play?\n${p.title}`))return;try{await api(`v1/plays/${p.id}`,{method:'DELETE'});await history();}catch(e){error(e);}});actions.append(b);}r.append(actions); return r;
}
async function history(append=false) { const epoch = append?generation:++generation; const q = new URLSearchParams({limit:'30'}); if($('mode').value)q.set('mode',$('mode').value);if(append&&before)q.set('before',before);const h=await api(`v1/history?${q}`);if(epoch!==generation)return;if(!append)$('history').replaceChildren();$('history').append(...h.plays.map(row));before=h.before;$('more').hidden=h.plays.length<30;$('empty').hidden=$('history').children.length>0; }
async function pollJob(id) { try {const j=await api(`v1/jobs/${id}`);if(job!==id)return;$('progress').value=j.progress;$('job-message').textContent=`${j.state.toUpperCase()} / ${j.message}${j.result?` · ${j.result.inserted??0} inserted, ${j.result.calculation_errors??0} calculation errors`:''}`;if(j.state==='running'||j.state==='queued')setTimeout(()=>pollJob(id),750);else{job=null;$('cancel').hidden=true;$('import').disabled=!vosu.canWrite;await history();}}catch(e){job=null;$('import').disabled=!vosu.canWrite;error(e);} }
$('mode').addEventListener('change',()=>history().catch(error));$('more').addEventListener('click',()=>history(true).catch(error));
$('import').addEventListener('click',async()=>{try{$('import').disabled=true;const j=await api('v1/import',{method:'POST'});job=j.job;$('job').hidden=false;$('cancel').hidden=false;pollJob(job);}catch(e){$('import').disabled=!vosu.canWrite;error(e);}});
$('cancel').addEventListener('click',async()=>{if(job)try{await api(`v1/jobs/${job}`,{method:'DELETE'});}catch(e){error(e);}});
await vosu.ready;$('access').textContent=vosu.canWrite?'LOCAL WORKSPACE':'READ ONLY';$('import').disabled=!vosu.canWrite;if(vosu.canWrite)$('import-note').textContent='Read-only source databases. Import can be cancelled; already imported scores remain.';
try { const [p,c] = await Promise.all([api('v1/profile'),api('v1/current')]);profile(p);current(c);await history(); } catch(e){error(e);}
let delay=1000;
function connect(){const url=new URL('api/v1/stream',location.href);url.protocol=location.protocol==='https:'?'wss:':'ws:';const ws=new WebSocket(url);ws.onmessage=e=>{try{const v=JSON.parse(e.data);profile(v.profile);current(v.current);history().catch(error);delay=1000;}catch(e){error(e);}};ws.onclose=()=>{$('connection').textContent='RECONNECTING';setTimeout(connect,delay);delay=Math.min(30000,delay*2);};ws.onerror=()=>ws.close();}
connect();
