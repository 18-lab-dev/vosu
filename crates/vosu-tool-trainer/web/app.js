import { vosu } from '/tools/_host/v1/bridge.js';
const exportNote = document.createElement('p');
exportNote.className = 'muted';
exportNote.textContent = 'Lazer exports include available assets only. Hash-only custom hitsounds, storyboard images or video may be omitted; warnings list missing files. Gameplay timing and processed audio are preserved.';
document.querySelector('.map').after(exportNote);
const $ = id => document.getElementById(id);
let current = null, active = null, busy = false, revision = 0;
const overridden = new Set();
const arMs = ar => ar < 5 ? 1800 - 120 * ar : 1200 - 150 * (ar - 5);
const msAr = ms => ms > 1200 ? (1800 - ms) / 120 : 5 + (1200 - ms) / 150;
const bounded = x => Math.max(0, Math.min(10, x));
async function api(path, options) { const r = await vosu.fetch(path, options); const v = await r.json(); if (!r.ok) throw new Error(v.error || r.statusText); return v; }
for (const key of ['hp','cs','ar','od']) {
  const row = document.createElement('div'); row.className = 'stat';
  row.innerHTML = `<label for="${key}">${key.toUpperCase()}</label><input id="${key}" type="range" min="0" max="10" step="0.1"><output id="${key}Value">5.0</output>${['ar','od'].includes(key) ? `<label class="lock"><input type="checkbox" id="lock_${key}"> LOCK</label>` : '<span></span>'}`;
  $('stats').append(row);
  $(key).oninput = () => { overridden.add(key); preview(); };
  if ($(`lock_${key}`)) $(`lock_${key}`).onchange = () => { overridden.delete(key); preview(); };
}
function preview() {
  const rate = +$('rate').value;
  $('rateValue').innerHTML = `${rate.toFixed(2)}<span>×</span>`;
  $('bpmValue').textContent = current ? (current.bpm * rate).toFixed(1) : '—';
  if (!current) return;
  const mode = current.mode;
  const [base, step] = mode === 'taiko' ? [50,3] : mode === 'mania' ? [64,3] : [80,6];
  const stats = {};
  for (const k of ['hp','cs','ar','od']) {
    let v = current[k];
    if (overridden.has(k)) v = +$(k).value;
    else if (k === 'ar' && !$('lock_ar').checked) v = msAr(arMs(v) / rate);
    else if (k === 'od' && !$('lock_od').checked) v = (base - (base - step * v) / rate) / step;
    v = bounded(v); stats[k] = v;
    $(k).value = v; $(k+'Value').textContent = v.toFixed(1);
  }
  $('preview').textContent = Object.entries(stats).map(([k,v]) => `${k.toUpperCase()} ${v.toFixed(1)}`).join(' / ');
}
async function refresh() {
  try {
    const seq = ++revision;
    const v = await api(`v1/current?rate=${$('rate').value}`);
    if (seq !== revision) return;
    current = v.beatmap;
    $('title').textContent = current ? `${current.artist} — ${current.title}` : 'Waiting for osu!';
    $('subtitle').textContent = current ? `[${current.version}] / ${current.mode.toUpperCase()} / ${v.client}` : 'Select a beatmap in your client.';
    const d = v.difficulty;
    $('difficulty').textContent = d?.msd ? `MSD ${d.msd.overall.toFixed(2)}${d.dan ? ` / ${d.dan.label} (experimental)` : ''}` : d?.unsupported || '';
    preview(); $('generate').disabled = !vosu.canWrite || !current || busy;
  } catch (e) { $('status').textContent = e.message; }
}
async function list() {
  const v = await api('v1/generated'); $('generated').replaceChildren();
  if (!v.maps.length) $('generated').textContent = 'No generated maps yet.';
  for (const entry of v.maps) {
    const row = document.createElement('div'); row.className = 'entry';
    const text = document.createElement('div'); text.textContent = entry.name;
    const detail = document.createElement('small'); detail.textContent = entry.export ? `LAZER IMPORT / ${entry.files[0].path}` : 'STABLE / Refresh your song list with F5'; text.append(detail);
    if (entry.warnings?.length) { const warning = document.createElement('p'); warning.className = 'notice'; warning.textContent = entry.warnings.join(' · '); text.append(warning); }
    const remove = document.createElement('button'); remove.textContent = 'DELETE'; remove.disabled = !vosu.canWrite;
    remove.onclick = async () => { if (!confirm(`Delete the generated copy “${entry.name}”? Original files are never removed.`)) return; try { await api(`v1/delete/${entry.id}`,{method:'POST'}); await list(); } catch(e) { $('status').textContent = e.message; } };
    row.append(text,remove); $('generated').append(row);
  }
}
$('rate').oninput = () => { preview(); $('target').value = ''; };
$('rate').onchange = refresh;
$('target').onchange = () => { if (!current) return; const rate = +$('target').value / current.bpm; if (rate < .5 || rate > 2) { $('status').textContent = 'Target BPM must give a rate between 0.5× and 2×.'; return; } $('rate').value = rate; preview(); refresh(); };
$('reset').onclick = () => { overridden.clear(); $('lock_ar').checked = false; $('lock_od').checked = false; preview(); };
$('refresh').onclick = () => { refresh(); list().catch(e => { $('status').textContent = e.message; }); };
$('cancel').onclick = async () => { if (active) await api(`v1/jobs/${active}`,{method:'DELETE'}); };
$('generate').onclick = async () => {
  const options = {rate:+$('rate').value,pitch_preserving:$('pitch').checked,lock_ar:$('lock_ar').checked,lock_od:$('lock_od').checked};
  if ($('target').value) options.target_bpm = +$('target').value;
  for (const k of overridden) options[k] = +$(k).value;
  busy = true; $('generate').disabled = true; $('cancel').hidden = false;
  try {
    const v = await api('v1/generate',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(options)}); active = v.job;
    while (active) { const j = await api(`v1/jobs/${active}`); $('status').textContent = [j.message, ...(j.result?.warnings || [])].join(' · '); $('progress').value = j.progress; if (['done','failed','cancelled'].includes(j.state)) { active = null; break; } await new Promise(r => setTimeout(r,400)); }
    await list();
  } catch(e) { $('status').textContent = e.message; }
  finally { busy = false; active = null; $('cancel').hidden = true; $('generate').disabled = !vosu.canWrite || !current; }
};
await vosu.ready;
$('readonly').hidden = vosu.canWrite;
await refresh();
try { await list(); } catch(e) { $('status').textContent = e.message; }
