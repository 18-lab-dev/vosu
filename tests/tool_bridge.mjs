import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../assets/server/tool-bridge.js', import.meta.url), 'utf8');
const running = (revision = 3, progress = {fraction: 0.2, message: 'reading'}) => ({
  id: 'job-19', state: 'running', revision, progress, result: null, error: null,
});
const succeeded = {...running(8), state: 'succeeded', result: {imported: 17}};
const reply = (value, status = 200) => ({ok: status < 400, status, json: async () => value});
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return {promise, resolve};
};
async function client(handler, hash = '#vosu-launch=single-use-secret&theme=light', pendingLaunch) {
  const requests = [];
  const sockets = [];
  const replaced = [];
  // Order of history rewrites and requests, to prove the fragment is gone
  // before the first asynchronous step.
  const events = [];
  const window = new EventTarget();
  class Socket extends EventTarget {
    constructor(url) { super(); this.url = String(url); this.closed = false; sockets.push(this); }
    send(value) { this.sent = value; }
    close() { this.drop(); }
    // A server-side drop or a client close: both end in exactly one `close`.
    drop() {
      if (this.closed) return;
      this.closed = true;
      this.dispatchEvent(new Event('close'));
    }
  }
  const forbidden = name => ({get() { throw new Error(`bridge touched ${name}`); }, set() { throw new Error(`bridge wrote ${name}`); }});
  const document = {documentElement: {dataset: {}}};
  Object.defineProperty(document, 'cookie', forbidden('document.cookie'));
  const globals = {
    location: {pathname: '/tools/org.fixture.browser/', search: '', hash, origin: 'http://localhost:24050', protocol: 'http:'},
    history: {replaceState(...args) { replaced.push(args); events.push('replaceState'); }},
    document,
    matchMedia: () => ({matches: false}),
    URL, URLSearchParams, Headers, WebSocket: Socket,
    setTimeout, clearTimeout, queueMicrotask,
    addEventListener: window.addEventListener.bind(window),
    removeEventListener: window.removeEventListener.bind(window),
    fetch: async (path, options = {}) => {
      const request = {path: String(path), ...options};
      requests.push(request);
      events.push('fetch ' + request.path);
      if (request.path === '/tools/_host/v1/session') return pendingLaunch || reply({session: 'private-session-token'});
      return handler(request);
    },
  };
  for (const name of ['localStorage', 'sessionStorage', 'indexedDB', 'caches']) {
    Object.defineProperty(globals, name, forbidden(name));
  }
  const context = vm.createContext(globals);
  const module = new vm.SourceTextModule('const metadata = []; const dashboardUrl = "http://localhost:24051";\n' + source, {context});
  await module.link(() => { throw new Error('Unexpected import'); });
  await module.evaluate();
  if (!pendingLaunch) await module.namespace.vosu.ready;
  return {vosu: module.namespace.vosu, requests, sockets, replaced, events, window};
}
const options = {timeout: 3000};

test('exact action session routing, authenticated requests and result', options, async () => {
  for (const [settings, prefix] of [
    [{}, '/api/v1/sessions/active'],
    [{session: 'session 47'}, '/api/v1/sessions/session%2047'],
    [{session: null}, '/api/v1'],
  ]) {
    const c = await client(request => request.method === 'POST' ? reply(running(), 202) : reply(succeeded));
    const result = await c.vosu.invoke('org.fixture.import', {file: 'sentinel'}, settings);
    assert.equal(result.imported, 17);
    const [, post, poll] = c.requests;
    assert.equal(post.path, prefix + '/actions/org.fixture.import?wait=0');
    assert.equal(post.body, '{"arguments":{"file":"sentinel"}}');
    assert.equal(poll.path, '/api/v1/jobs/job-19?after=3&wait=30000');
    for (const request of [post, poll]) {
      assert.equal(request.headers.get('X-Vosu-Tool-Session'), 'private-session-token');
      assert.equal(request.credentials, 'omit');
      assert.equal(request.cache, 'no-store');
      assert.equal(request.redirect, 'error');
      assert.equal(request.path.includes('private-session-token'), false);
    }
    assert.deepEqual(c.replaced[0], [null, '', '/tools/org.fixture.browser/']);
  }
});

test('late progress registration replays current progress and ignores unchanged progress', options, async () => {
  const poll = deferred();
  let polls = 0;
  const c = await client(request => {
    if (request.method === 'POST') return reply(running());
    polls += 1;
    return polls === 1 ? poll.promise : reply({...succeeded, progress: {fraction: 1, message: 'done'}});
  });
  const handle = await c.vosu.start('org.fixture.import', {});
  const seen = [];
  handle.onProgress(progress => seen.push(progress.fraction));
  poll.resolve(reply(running(4)));
  await handle.done;
  assert.deepEqual(seen, [0.2, 1]);
});

test('failed and cancelled jobs reject rather than resolve', options, async () => {
  for (const [state, code] of [['failed', 'plugin_crashed'], ['cancelled', 'cancelled']]) {
    const c = await client(() => reply({...running(), state, error: {code, message: 'fixture failure'}}));
    await assert.rejects(c.vosu.invoke('org.fixture.import', {}), error => error.code === code);
  }
});

test('already aborted signal never posts a job', options, async () => {
  const c = await client(() => { throw new Error('Unexpected action request'); });
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(c.vosu.invoke('org.fixture.import', {}, {signal: controller.signal}), error => error.code === 'cancelled');
  assert.equal(c.requests.length, 1);
});

test('abort during POST preserves the response and cancels the discovered job', options, async () => {
  const posted = deferred();
  const response = deferred();
  const c = await client(request => {
    if (request.method === 'POST') { posted.resolve(request); return response.promise; }
    return reply({...running(6), state: 'cancelled'});
  });
  const controller = new AbortController();
  const result = c.vosu.invoke('org.fixture.import', {}, {signal: controller.signal});
  const post = await posted.promise;
  assert.equal(post.signal, undefined);
  controller.abort();
  response.resolve(reply(running(), 202));
  await assert.rejects(result, error => error.code === 'cancelled');
  assert.equal(c.requests.filter(request => request.method === 'DELETE').length, 1);
  assert.equal(c.requests.find(request => request.method === 'DELETE').path, '/api/v1/jobs/job-19');
});

test('abort during polling sends DELETE; terminal completion removes abort listener', options, async () => {
  const polling = deferred();
  const response = deferred();
  const deleted = deferred();
  const c = await client(request => {
    if (request.method === 'POST') return reply(running());
    if (request.method === 'DELETE') { deleted.resolve(request); return reply(running()); }
    polling.resolve();
    return response.promise;
  });
  const controller = new AbortController();
  const handle = await c.vosu.start('org.fixture.import', {}, {signal: controller.signal});
  await polling.promise;
  controller.abort();
  await deleted.promise;
  response.resolve(reply({...running(9), state: 'cancelled'}));
  await assert.rejects(handle.done, error => error.code === 'cancelled');
  await handle.cancel();
  assert.equal(c.requests.filter(request => request.method === 'DELETE').length, 1);
  const complete = await client(() => reply(succeeded));
  const after = new AbortController();
  const completed = await complete.vosu.start('org.fixture.import', {}, {signal: after.signal});
  await completed.done;
  after.abort();
  await completed.cancel();
  assert.equal(complete.requests.filter(request => request.method === 'DELETE').length, 0);
});

test('HTTP failure and absent launch authority reject without leaking credentials', options, async () => {
  const c = await client(() => reply({error: {code: 'forbidden', message: 'capability denied'}}, 403));
  await assert.rejects(c.vosu.invoke('org.fixture.import', {}), error => error.code === 'forbidden');
  const readOnly = await client(() => { throw new Error('Unexpected request'); }, '');
  await assert.rejects(readOnly.vosu.start('org.fixture.import', {}), error => error.code === 'forbidden');
  assert.equal(readOnly.requests.length, 0);
});

test('subscription uses the API v1 session route and sends the caller pointers', options, async () => {
  for (const [settings, session] of [[{}, 'active'], [{session: 'session 47'}, 'session%2047']]) {
    const c = await client(() => { throw new Error('Unexpected request'); });
    const frames = [];
    const unsubscribe = c.vosu.subscribe(['/vosu.play/combo'], frame => frames.push(frame), undefined, settings);
    const socket = c.sockets[0];
    assert.equal(socket.url, 'ws://localhost:24050/api/v1/sessions/' + session + '/subscribe');
    socket.dispatchEvent(new Event('open'));
    assert.equal(socket.sent, '["/vosu.play/combo"]');
    socket.dispatchEvent(new MessageEvent('message', {data: '[0,7,[[0,51]]]'}));
    assert.equal(frames[0][2][0][1], 51);
    unsubscribe();
  }
});

test('the bridge exports exactly the frozen surface: no fetch, no legacy subscription route', options, async () => {
  const c = await client(() => { throw new Error('Unexpected request'); });
  assert.deepEqual(Object.keys(c.vosu).sort(),
    ['canWrite', 'colorScheme', 'dashboardUrl', 'invoke', 'ready', 'settings', 'start', 'subscribe', 'tool']);
  assert.equal(c.vosu.fetch, undefined);
  assert.ok(Object.isFrozen(c.vosu));
  assert.equal(source.includes('/websocket/v2/subscribe'), false);
  assert.equal(source.includes(')}/api/'), false, 'no per-tool /api/ proxy URL');
  const unsubscribe = c.vosu.subscribe(['/vosu.client/name'], () => {});
  assert.equal(c.sockets[0].url, 'ws://localhost:24050/api/v1/sessions/active/subscribe');
  unsubscribe();
});

test('subscription rejects invalid options synchronously without opening a socket', options, async () => {
  const c = await client(() => { throw new Error('Unexpected request'); });
  for (const invalid of [null, [], false, 'active', {unknown: true}, {[Symbol('key')]: true}]) {
    assert.throws(() => c.vosu.subscribe([''], () => {}, undefined, invalid), /[Oo]ption/);
  }
  for (const session of [undefined, null, '', '.', '..', 0, false, {}, []]) {
    assert.throws(() => c.vosu.subscribe([''], () => {}, undefined, {session}), /session.*nonempty string/);
  }
  assert.equal(c.sockets.length, 0);
});

test('subscription frames and statuses reach the caller unchanged', options, async () => {
  const c = await client(() => { throw new Error('Unexpected request'); });
  const frames = [];
  const statuses = [];
  const stop = c.vosu.subscribe(['/vosu.play/combo'], frame => frames.push(JSON.stringify(frame)), status => statuses.push(status));
  c.sockets[0].dispatchEvent(new Event('open'));
  for (const data of ['[0,7,[[0,51]]]', 'not json', '[1,8,[[0,52]],[]]', '[1,9,[],[0]]', '[2,1,\"fixture error\"]']) {
    c.sockets[0].dispatchEvent(new MessageEvent('message', {data}));
  }
  stop();
  await Promise.resolve();
  assert.equal(c.sockets[0].sent, '["/vosu.play/combo"]');
  assert.deepEqual(frames, ['[0,7,[[0,51]]]', '[1,8,[[0,52]],[]]', '[1,9,[],[0]]', '[2,1,"fixture error"]']);
  assert.deepEqual(statuses, ['connecting', 'open', 'closed']);
});

test('progress listener slots are bounded and unsubscribe releases them', options, async () => {
  const poll = deferred();
  const c = await client(request => request.method === 'POST' ? reply(running()) : poll.promise);
  const handle = await c.vosu.start('org.fixture.import', {});
  const first = handle.onProgress(() => {});
  for (let i = 1; i < 256; i += 1) handle.onProgress(() => {});
  assert.throws(() => handle.onProgress(() => {}), /Too many progress listeners/);
  first();
  assert.doesNotThrow(() => handle.onProgress(() => {}));
  poll.resolve(reply(succeeded));
  await handle.done;
});

test('polling failure does not turn cancel into a no-op for a running job', options, async () => {
  const c = await client(request => {
    if (request.method === 'POST') return reply(running());
    if (request.method === 'DELETE') return reply({...running(9), state: 'cancelled'});
    throw new Error('network fixture failure');
  });
  const handle = await c.vosu.start('org.fixture.import', {});
  await assert.rejects(handle.done, /network fixture failure/);
  await handle.cancel();
  assert.equal(c.requests.filter(request => request.method === 'DELETE').length, 1);
});

test('abort while launch exchange is pending never posts an action', options, async () => {
  const launch = deferred();
  const c = await client(() => { throw new Error('Unexpected action request'); }, '#vosu-launch=single-use-secret', launch.promise);
  const controller = new AbortController();
  const result = c.vosu.invoke('org.fixture.import', {}, {signal: controller.signal});
  controller.abort();
  launch.resolve(reply({session: 'private-session-token'}));
  await assert.rejects(result, error => error.code === 'cancelled');
  assert.equal(c.requests.length, 1);
});

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const isTypeError = error => error.name === 'TypeError';
const live = sockets => sockets.filter(socket => !socket.closed).length;

test('the launch fragment is cleared before any asynchronous work', options, async () => {
  const launch = deferred();
  const c = await client(() => { throw new Error('Unexpected request'); }, '#vosu-launch=single-use-secret&theme=dark', launch.promise);
  // Module evaluation has returned; the exchange has not answered yet.
  assert.deepEqual(c.replaced, [[null, '', '/tools/org.fixture.browser/']]);
  assert.deepEqual(c.events, ['replaceState', 'fetch /tools/_host/v1/session']);
  const exchange = c.requests[0];
  assert.deepEqual(JSON.parse(exchange.body), {tool: 'org.fixture.browser', code: 'single-use-secret'});
  assert.equal(exchange.credentials, 'omit');
  assert.equal(exchange.cache, 'no-store');
  assert.equal(exchange.redirect, 'error');
  assert.equal(c.vosu.colorScheme, 'dark');
  assert.equal(c.vosu.canWrite, false);
  launch.resolve(reply({session: 'private-session-token'}));
  await c.vosu.ready;
  assert.equal(c.vosu.canWrite, true);
});

test('a bookmark or reload has no write authority and sends no action', options, async () => {
  // Bookmark or reload of the tool URL: no fragment, nothing to exchange.
  const bookmark = await client(() => { throw new Error('Unexpected request'); }, '');
  assert.deepEqual(bookmark.replaced, []);
  assert.equal(bookmark.vosu.canWrite, false);
  await assert.rejects(bookmark.vosu.invoke('org.fixture.import', {}), error => error.code === 'forbidden');
  assert.equal(bookmark.requests.length, 0);
  // Bookmarked launch URL: the replayed code is refused, the fragment is
  // still removed, and actions reject with the launch error before any POST.
  const replay = await client(() => { throw new Error('Unexpected request'); }, '#vosu-launch=used-code', Promise.resolve(reply({}, 403)));
  assert.deepEqual(replay.replaced, [[null, '', '/tools/org.fixture.browser/']]);
  await assert.rejects(replay.vosu.ready, /expired or was already used/);
  assert.equal(replay.vosu.canWrite, false);
  await assert.rejects(replay.vosu.invoke('org.fixture.import', {}), /expired or was already used/);
  assert.deepEqual(replay.requests.map(request => request.path), ['/tools/_host/v1/session']);
});

test('the session token never reaches reads, URLs or caller-chosen destinations', options, async () => {
  const c = await client(request => {
    if (request.path.endsWith('/_settings')) return reply({values: {label: 'x'}});
    return reply(succeeded);
  });
  assert.deepEqual(await c.vosu.settings(), {label: 'x'});
  const settings = c.requests.find(request => request.path.endsWith('/_settings'));
  assert.equal(settings.path, '/tools/org.fixture.browser/_settings');
  assert.equal(settings.headers, undefined);
  assert.equal(settings.credentials, 'omit');
  for (const actionId of ['https://evil.example/x', '//evil.example', '../jobs/x', 'Org.fixture', '.hidden', 'a/b', 'a?b', 'a#b']) {
    await assert.rejects(c.vosu.invoke(actionId, {}), isTypeError);
  }
  for (const session of ['', '.', '..', 7, {}]) {
    await assert.rejects(c.vosu.invoke('org.fixture.import', {}, {session}), isTypeError);
  }
  await c.vosu.invoke('org.fixture.import', {}, {session: 'https://evil.example/'});
  const unsubscribe = c.vosu.subscribe([''], () => {}, undefined, {session: 'https://evil.example/'});
  unsubscribe();
  assert.equal(c.sockets[0].url, 'ws://localhost:24050/api/v1/sessions/https%3A%2F%2Fevil.example%2F/subscribe');
  const authenticated = c.requests.filter(request => request.headers?.get?.('X-Vosu-Tool-Session'));
  assert.deepEqual(authenticated.map(request => request.path), [
    '/api/v1/sessions/https%3A%2F%2Fevil.example%2F/actions/org.fixture.import?wait=0',
  ]);
  for (const request of c.requests) {
    assert.ok(request.path.startsWith('/'), request.path);
    assert.equal(request.path.includes('private-session-token'), false);
    assert.equal(request.path.includes('single-use-secret'), false);
  }
  assert.equal(c.sockets[0].url.includes('private-session-token'), false);
});

test('a reconnect never holds two sockets, even when online fires repeatedly', options, async () => {
  const c = await client(() => { throw new Error('Unexpected request'); });
  const frames = [];
  const stop = c.vosu.subscribe(['/vosu.play/combo'], frame => frames.push(JSON.stringify(frame)));
  c.sockets[0].dispatchEvent(new Event('open'));
  // A connected subscription ignores online: there is nothing to retry.
  c.window.dispatchEvent(new Event('online'));
  assert.equal(c.sockets.length, 1);
  // Server drop, then online during the backoff: exactly one new socket.
  c.sockets[0].drop();
  assert.equal(live(c.sockets), 0);
  c.window.dispatchEvent(new Event('online'));
  c.window.dispatchEvent(new Event('online'));
  assert.equal(c.sockets.length, 2);
  assert.equal(live(c.sockets), 1);
  // The cancelled backoff timer never fires a third socket.
  await sleep(600);
  assert.equal(c.sockets.length, 2);
  // The replacement re-sends the same subscription; the dropped socket's late
  // events are ignored.
  c.sockets[1].dispatchEvent(new Event('open'));
  assert.equal(c.sockets[1].sent, '["/vosu.play/combo"]');
  c.sockets[0].dispatchEvent(new MessageEvent('message', {data: '[1,99,[[0,1]],[]]'}));
  c.sockets[1].dispatchEvent(new MessageEvent('message', {data: '[0,1,[[0,3]]]'}));
  assert.deepEqual(frames, ['[0,1,[[0,3]]]']);
  // A drop with no online event reconnects once, after the backoff.
  c.sockets[1].drop();
  await sleep(700);
  assert.equal(c.sockets.length, 3);
  assert.equal(live(c.sockets), 1);
  stop();
  assert.equal(live(c.sockets), 0);
});

test('unsubscribe never reconnects', options, async () => {
  for (const phase of ['open', 'connecting', 'backoff']) {
    const c = await client(() => { throw new Error('Unexpected request'); });
    const statuses = [];
    const stop = c.vosu.subscribe([''], () => {}, status => statuses.push(status));
    if (phase === 'open') c.sockets[0].dispatchEvent(new Event('open'));
    if (phase === 'backoff') c.sockets[0].drop();
    stop();
    stop();
    assert.equal(live(c.sockets), 0);
    c.window.dispatchEvent(new Event('online'));
    await sleep(600);
    assert.equal(c.sockets.length, 1, phase);
    assert.equal(statuses.at(-1), 'closed', phase);
  }
});
