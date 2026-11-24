import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../assets/server/tool-bridge.js', import.meta.url), 'utf8');
const v1Source = await readFile(new URL('../assets/server/tool-bridge-v1.js', import.meta.url), 'utf8');
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
async function client(handler, asset = source, hash = '#vosu-launch=single-use-secret&theme=light', pendingLaunch) {
  const requests = [];
  const sockets = [];
  const replaced = [];
  class Socket extends EventTarget {
    constructor(url) { super(); this.url = String(url); sockets.push(this); }
    send(value) { this.sent = value; }
    close() { this.dispatchEvent(new Event('close')); }
  }
  const context = vm.createContext({
    location: {pathname: '/tools/org.fixture.browser/', search: '', hash, origin: 'http://localhost:24050', protocol: 'http:'},
    history: {replaceState(...args) { replaced.push(args); }},
    document: {documentElement: {dataset: {}}},
    matchMedia: () => ({matches: false}),
    URL, URLSearchParams, Headers, WebSocket: Socket,
    setTimeout, clearTimeout, queueMicrotask,
    addEventListener() {}, removeEventListener() {},
    fetch: async (path, options = {}) => {
      const request = {path: String(path), ...options};
      requests.push(request);
      if (request.path === '/tools/_host/v1/session') return pendingLaunch || reply({session: 'private-session-token'});
      return handler(request);
    },
  });
  const module = new vm.SourceTextModule('const metadata = []; const dashboardUrl = "http://localhost:24051";\n' + asset, {context});
  await module.link(() => { throw new Error('Unexpected import'); });
  await module.evaluate();
  if (!pendingLaunch) await module.namespace.vosu.ready;
  return {vosu: module.namespace.vosu, requests, sockets, replaced};
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
  const readOnly = await client(() => { throw new Error('Unexpected request'); }, source, '');
  await assert.rejects(readOnly.vosu.start('org.fixture.import', {}), error => error.code === 'forbidden');
  assert.equal(readOnly.requests.length, 0);
});

test('v2 subscription uses API v1 and preserves selected pointer wire shape', options, async () => {
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

test('preserved v1 subscription stays on the legacy route without action methods', options, async () => {
  const c = await client(() => { throw new Error('Unexpected request'); }, v1Source);
  const unsubscribe = c.vosu.subscribe(['/menu'], () => {});
  assert.equal(c.sockets[0].url, 'ws://localhost:24050/websocket/v2/subscribe');
  assert.equal(c.vosu.invoke, undefined);
  assert.equal(c.vosu.start, undefined);
  unsubscribe();
});

test('subscription rejects invalid options synchronously without opening a socket', options, async () => {
  const c = await client(() => { throw new Error('Unexpected request'); });
  for (const invalid of [null, [], false, 'active', {unknown: true}, {[Symbol('key')]: true}]) {
    assert.throws(() => c.vosu.subscribe([''], () => {}, undefined, invalid), /[Oo]ption/);
  }
  for (const session of [undefined, null, '', 0, false, {}, []]) {
    assert.throws(() => c.vosu.subscribe([''], () => {}, undefined, {session}), /session.*nonempty string/);
  }
  assert.equal(c.sockets.length, 0);
});

test('default subscription frame delivery matches v1 byte for byte', options, async () => {
  const observed = [];
  for (const asset of [v1Source, source]) {
    const c = await client(() => { throw new Error('Unexpected request'); }, asset);
    const frames = [];
    const statuses = [];
    const stop = c.vosu.subscribe(['/vosu.play/combo'], frame => frames.push(JSON.stringify(frame)), status => statuses.push(status));
    c.sockets[0].dispatchEvent(new Event('open'));
    for (const data of ['[0,7,[[0,51]]]', '[1,8,[[0,52]],[]]', '[1,9,[],[0]]', '[2,1,\"fixture error\"]']) {
      c.sockets[0].dispatchEvent(new MessageEvent('message', {data}));
    }
    stop();
    await Promise.resolve();
    observed.push({sent: c.sockets[0].sent, frames, statuses});
  }
  assert.deepEqual(observed[0], observed[1]);
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
  const c = await client(() => { throw new Error('Unexpected action request'); }, source, '#vosu-launch=single-use-secret', launch.promise);
  const controller = new AbortController();
  const result = c.vosu.invoke('org.fixture.import', {}, {signal: controller.signal});
  controller.abort();
  launch.resolve(reply({session: 'private-session-token'}));
  await assert.rejects(result, error => error.code === 'cancelled');
  assert.equal(c.requests.length, 1);
});
