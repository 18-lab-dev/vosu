const id = decodeURIComponent(location.pathname.split('/')[2] || '');
const tool = metadata.find(tool => tool.id === id) || {id, name: id, version: ''};
// The session lives only in this document's memory. Any same-origin storage
// (sessionStorage, localStorage, IndexedDB, cookies) is readable by an overlay
// that later loads in this tab, so a reload deliberately drops write access
// until the tool is opened from the dashboard again.
let session = null;
const fragment = new URLSearchParams(location.hash.slice(1));
// The dashboard's colour mode when it opened this tab, else the system
// preference. The bridge applies it as data-theme so the shared theme.css
// renders in that mode; a tool may still switch it afterwards.
const hint = fragment.get('theme');
const colorScheme = hint === 'dark' || hint === 'light'
  ? hint : matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
document.documentElement.dataset.theme = colorScheme;

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


function actionError(code, message) {
  return Object.assign(new Error(message), {code});
}
const cancelled = () => actionError('cancelled', 'Action cancelled');
function sessionPath(value = 'active') {
  if (typeof value !== 'string' || !value) throw new TypeError('session must be a nonempty string or null for an action');
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
  const prefix = target === null ? '/api/v1' : '/api/v1/sessions/' + sessionPath(target);
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
  if (onProgress !== undefined) {
    if (typeof onProgress !== 'function') throw new TypeError('Progress listener must be a function');
    listeners.add(onProgress);
  }
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
  // Live API v1 session subscription frames that survive vosu restarts and
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
    if (typeof target !== 'string' || !target) {
      throw new TypeError('Subscription session must be a nonempty string');
    }
    const url = new URL('/api/v1/sessions/' + sessionPath(target) + '/subscribe', location.origin);
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
