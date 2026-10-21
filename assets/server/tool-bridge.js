const id = decodeURIComponent(location.pathname.split('/')[2] || '');
const tool = metadata.find(tool => tool.id === id) || {id, name: id, version: ''};
let session = null;
const storageKey = `vosu-tool-session:${epoch}:${id}`;
try { session = sessionStorage.getItem(storageKey); } catch {}

const ready = (async () => {
  const fragment = new URLSearchParams(location.hash.slice(1));
  const theme = fragment.get('theme');
  document.documentElement.dataset.theme = ['dark', 'light'].includes(theme)
    ? theme : matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  const code = fragment.get('vosu-launch');
  if (code) {
    history.replaceState(null, '', location.pathname + location.search);
    const response = await fetch('/tools/_host/v1/session', {
      method: 'POST', headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({tool: id, code}), credentials: 'omit',
    });
    if (!response.ok) throw new Error('Tool launch code expired or was already used. Open the tool from the dashboard again.');
    session = (await response.json()).session;
    try { sessionStorage.setItem(storageKey, session); } catch {}
  }
})();

export const vosu = Object.freeze({
  ready, tool, dashboardUrl,
  get canWrite() { return Boolean(session); },
  async fetch(path, options = {}) {
    await ready;
    const base = new URL(`/tools/${encodeURIComponent(id)}/api/`, location.origin);
    const url = new URL(path, base);
    if (url.origin !== base.origin || !url.pathname.startsWith(base.pathname)) {
      throw new TypeError('vosu.fetch path must stay inside this tool API');
    }
    const headers = new Headers(options.headers);
    if (session) headers.set('X-Vosu-Tool-Session', session);
    return fetch(url, {...options, headers, credentials: 'omit'});
  },
  subscribe(fields, onFrame) {
    const url = new URL('/websocket/v2/subscribe', location.origin);
    url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const socket = new WebSocket(url);
    socket.addEventListener('open', () => socket.send(JSON.stringify(fields)));
    socket.addEventListener('message', event => onFrame(JSON.parse(event.data)));
    return () => socket.close();
  },
});
