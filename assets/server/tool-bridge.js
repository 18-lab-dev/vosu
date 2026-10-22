const id = decodeURIComponent(location.pathname.split('/')[2] || '');
const tool = metadata.find(tool => tool.id === id) || {id, name: id, version: ''};
// The session lives only in this document's memory. Any same-origin storage
// (sessionStorage, localStorage, IndexedDB, cookies) is readable by an overlay
// that later loads in this tab, so a reload deliberately drops write access
// until the tool is opened from the dashboard again.
let session = null;
const fragment = new URLSearchParams(location.hash.slice(1));
// The dashboard's colour mode when it opened this tab, else the system
// preference. Tools own their look: the bridge only reports the hint.
const hint = fragment.get('theme');
const colorScheme = hint === 'dark' || hint === 'light'
  ? hint : matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';

const ready = (async () => {
  const code = fragment.get('vosu-launch');
  if (code) {
    history.replaceState(null, '', location.pathname + location.search);
    const response = await fetch('/tools/_host/v1/session', {
      method: 'POST', headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({tool: id, code}), credentials: 'omit',
    });
    if (!response.ok) throw new Error('Tool launch code expired or was already used. Open the tool from the dashboard again.');
    session = (await response.json()).session;
  }
})();

export const vosu = Object.freeze({
  ready, tool, dashboardUrl, colorScheme,
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
