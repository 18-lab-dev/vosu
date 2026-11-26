// The vosu 2.0 tool bridge: the ES module served as GET /tools/_host/v2/bridge.js,
// and the only bridge. Its exported surface and behavior are frozen in
// docs/tools/protocol.md ("Browser bridge"); change both together. The tool
// host prepends `const metadata = [{id, name, version}, ...]` and
// `const dashboardUrl = "<origin>" | null` to these bytes.
//
// Authority: the dashboard opens a tool at
// `/tools/<id>/#vosu-launch=<code>&theme=<dark|light>`. The bridge removes that
// fragment synchronously, before any asynchronous work, then exchanges the
// single-use code for a launch session. The session lives only in this
// module's memory: never in cookies, localStorage, sessionStorage, IndexedDB,
// a URL or a query string, since every overlay later loaded in this tab shares
// that storage. A reload or bookmark therefore has read access only. The
// session is sent as `X-Vosu-Tool-Session` solely on the API v1 action and job
// routes this module builds itself; reads, settings, subscriptions, artwork
// and any URL a caller supplies never carry it.
const id = decodeURIComponent(location.pathname.split('/')[2] || '');
const tool = metadata.find(tool => tool.id === id) || {id, name: id, version: ''};
let session = null;
const fragment = new URLSearchParams(location.hash.slice(1));
const code = fragment.get('vosu-launch');
if (code !== null) history.replaceState(null, '', location.pathname + location.search);
// The dashboard's colour mode when it opened this tab, else the system
// preference. The bridge applies it as data-theme so the shared theme.css
// renders in that mode; a tool may still switch it afterwards.
const hint = fragment.get('theme');
const colorScheme = hint === 'dark' || hint === 'light'
  ? hint : matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
document.documentElement.dataset.theme = colorScheme;

const ready = (async () => {
  if (!code) return;
  const response = await fetch('/tools/_host/v1/session', {
    method: 'POST', headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({tool: id, code}),
    credentials: 'omit', cache: 'no-store', redirect: 'error',
  });
  if (!response.ok) throw new Error('Tool launch code expired or was already used. Open the tool from the dashboard again.');
  const value = await response.json();
  if (typeof value?.session !== 'string' || !value.session) throw new Error('Tool launch returned no session');
  session = value.session;
})();

function actionError(code, message) {
  return Object.assign(new Error(message), {code});
}
const cancelled = () => actionError('cancelled', 'Action cancelled');
// One route segment: a session id or "active". Dot segments would be
// normalized away and silently retarget the request.
function sessionSegment(value, what) {
  if (typeof value !== 'string' || !value || value === '.' || value === '..') {
    throw new TypeError(`${what} must be a nonempty string session id or "active"`);
  }
  return encodeURIComponent(value);
}
async function jobRequest(path, method = 'GET', body) {
  const headers = new Headers({'X-Vosu-Tool-Session': session});
  if (body !== undefined) headers.set('Content-Type', 'application/json');
  const response = await fetch(path, {
    method, headers, credentials: 'omit', cache: 'no-store', redirect: 'error',
    ...(body === undefined ? {} : {body: JSON.stringify(body)}),
  });
  const value = await response.json();
  if (!response.ok) throw actionError(value.error?.code || 'http_error', value.error?.message || 'Action request failed');
  return value;
}
async function start(actionId, args, {session: target = 'active', signal, onProgress} = {}) {
  if (typeof actionId !== 'string' || !/^[a-z0-9][a-z0-9._-]*$/.test(actionId)) throw new TypeError('Invalid action id');
  const prefix = target === null ? '/api/v1' : '/api/v1/sessions/' + sessionSegment(target, 'Action session');
  if (onProgress !== undefined && typeof onProgress !== 'function') throw new TypeError('Progress listener must be a function');
  if (signal?.aborted) throw cancelled();
  await ready;
  if (signal?.aborted) throw cancelled();
  if (!session) throw actionError('forbidden', 'Open the tool from the dashboard to invoke actions');
  // Never pass the caller's signal to POST: its response is the only way to
  // discover the job to cancel when abort races creation.
  let job = null;
  let terminal = false;
  let settled = false;
  let abortRequested = false;
  let cancellation = null;
  const listeners = new Set();
  if (onProgress !== undefined) listeners.add(onProgress);
  const cancel = () => {
    if (terminal) return Promise.resolve(job);
    abortRequested = true;
    if (!job) return Promise.resolve(null);
    if (!cancellation) cancellation = jobRequest('/api/v1/jobs/' + encodeURIComponent(job.id), 'DELETE');
    return cancellation;
  };
  const abort = () => { cancel().catch(() => {}); };
  signal?.addEventListener('abort', abort, {once: true});
  try {
    job = await jobRequest(prefix + '/actions/' + encodeURIComponent(actionId) + '?wait=0', 'POST', {arguments: args});
  } catch (error) {
    signal?.removeEventListener('abort', abort);
    throw error;
  }
  let progressKey;
  const publish = () => {
    const key = JSON.stringify(job.progress);
    if (key === progressKey) return;
    progressKey = key;
    const progress = job.progress;
    for (const listener of listeners) queueMicrotask(() => listener(progress));
  };
  const done = (async () => {
    try {
      while (true) {
        publish();
        if (job.state !== 'running') {
          terminal = true;
          if (job.state === 'succeeded') return job.result;
          if (job.state === 'cancelled') throw cancelled();
          throw actionError(job.error?.code || 'invalid_job', job.error?.message || 'Action failed');
        }
        if (abortRequested) await cancel();
        job = await jobRequest('/api/v1/jobs/' + encodeURIComponent(job.id) + '?after=' + encodeURIComponent(job.revision) + '&wait=30000');
      }
    } finally {
      settled = true;
      signal?.removeEventListener('abort', abort);
      listeners.clear();
    }
  })();
  // Background handles need not install their done observer in the same turn.
  done.catch(() => {});
  return Object.freeze({
    id: job.id, done, cancel,
    onProgress(fn) {
      if (typeof fn !== 'function') throw new TypeError('Progress listener must be a function');
      if (!settled) {
        if (!listeners.has(fn) && listeners.size >= 256) throw new RangeError('Too many progress listeners');
        listeners.add(fn);
      }
      const progress = job.progress;
      queueMicrotask(() => fn(progress));
      return () => listeners.delete(fn);
    },
  });
}

export const vosu = Object.freeze({
  ready, tool, dashboardUrl, colorScheme,
  start,
  async invoke(actionId, args, options) {
    const handle = await start(actionId, args, options);
    return handle.done;
  },
  get canWrite() { return Boolean(session); },
  // Effective settings.json values (defaults applied), password-type fields
  // excluded. A public read: no session is sent. Re-read for fresh values.
  async settings() {
    const response = await fetch(`/tools/${encodeURIComponent(id)}/_settings`, {credentials: 'omit', cache: 'no-store'});
    if (!response.ok) throw new Error(`Tool settings unavailable (HTTP ${response.status})`);
    return (await response.json()).values;
  },
  // Live API v1 field subscription frames that survive vosu restarts and
  // network drops: one socket at a time, the subscription re-sent on every
  // reconnect, capped exponential backoff with jitter between attempts.
  // `onStatus` (optional) receives "connecting" | "open" | "closed".
  // Returns an unsubscribe function that stops reconnecting.
  subscribe(fields, onFrame, onStatus, options = {}) {
    if (!options || typeof options !== 'object' || Array.isArray(options)) {
      throw new TypeError('Subscription options must be an object');
    }
    if (Reflect.ownKeys(options).some(key => key !== 'session')) {
      throw new TypeError('Unknown subscription option; only session is supported');
    }
    const target = Object.hasOwn(options, 'session') ? options.session : 'active';
    const url = new URL('/api/v1/sessions/' + sessionSegment(target, 'Subscription session') + '/subscribe', location.origin);
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
