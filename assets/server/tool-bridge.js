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
  // Effective settings.json values (defaults applied), password-type fields
  // excluded. Re-read whenever fresh values are needed.
  async settings() {
    const response = await fetch(`/tools/${encodeURIComponent(id)}/_settings`, {credentials: 'omit', cache: 'no-store'});
    if (!response.ok) throw new Error(`Tool settings unavailable (HTTP ${response.status})`);
    return (await response.json()).values;
  },
  // Live `/websocket/v2/subscribe` frames that survive vosu restarts and
  // network drops: one socket at a time, the subscription re-sent on every
  // reconnect, capped exponential backoff with jitter between attempts.
  // `onStatus` (optional) receives "connecting" | "open" | "closed".
  // Returns an unsubscribe function that stops reconnecting.
  subscribe(fields, onFrame, onStatus) {
    const url = new URL('/websocket/v2/subscribe', location.origin);
    url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const request = JSON.stringify(fields);
    let socket = null;
    let timer = 0;
    let attempt = 0;
    let stopped = false;
    let state = null;
    const report = (next) => {
      if (next === state) return;
      state = next;
      if (onStatus) queueMicrotask(() => onStatus(next));
    };
    const connect = () => {
      timer = 0;
      if (stopped || socket) return;
      report('connecting');
      const current = new WebSocket(url);
      let openedAt = 0;
      socket = current;
      current.addEventListener('open', () => {
        if (socket !== current) return;
        openedAt = Date.now();
        current.send(request);
        report('open');
      });
      current.addEventListener('message', (event) => {
        if (socket !== current) return;
        let frame;
        try { frame = JSON.parse(event.data); } catch { return; }
        onFrame(frame);
      });
      // Every failed or dropped connection ends in `close`, also after `error`.
      current.addEventListener('close', () => {
        if (socket !== current) return;
        socket = null;
        if (stopped) return;
        report('closed');
        // Only a connection that stayed up resets the backoff, so a server
        // that rejects the subscription (its error frame reaches onFrame) is
        // retried ever more slowly rather than in a tight loop.
        if (openedAt && Date.now() - openedAt >= 10_000) attempt = 0;
        const ceiling = Math.min(30_000, 500 * 2 ** attempt);
        attempt += 1;
        timer = setTimeout(connect, ceiling / 2 + Math.random() * (ceiling / 2));
      });
    };
    // Coming back online retries at once instead of waiting out the backoff.
    const online = () => {
      if (stopped || socket) return;
      clearTimeout(timer);
      attempt = 0;
      connect();
    };
    addEventListener('online', online);
    connect();
    return () => {
      if (stopped) return;
      stopped = true;
      clearTimeout(timer);
      removeEventListener('online', online);
      const current = socket;
      socket = null;
      current?.close();
      report('closed');
    };
  },
});
