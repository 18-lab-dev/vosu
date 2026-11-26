// Required real-browser/ABI smoke for the one tool bridge, driven by
// tool_host_smoke.py: dashboard launch, single-use exchange, invoke, progress,
// abort and the T1 counterexamples (bookmark/reload, replayed code, abort
// racing creation, reconnect and unsubscribe, the removed v1 bridge and
// token confinement).
import assert from 'node:assert/strict';
const [modulePath] = process.argv.slice(2);
if (!modulePath) throw new Error('A Playwright index.mjs is required');
const OVERLAY = 'http://127.0.0.1:25150';
const DASHBOARD = 'http://127.0.0.1:25151';
const {chromium} = await import(modulePath);
const browser = await chromium.launch({headless: true, executablePath: process.env.VOSU_CHROMIUM || undefined});
try {
  const context = await browser.newContext();
  context.setDefaultTimeout(15000);
  context.setDefaultNavigationTimeout(15000);
  const sockets = [];
  context.on('page', page => {
    page.on('pageerror', error => console.error(`browser page error: ${error.message}`));
    page.on('websocket', socket => sockets.push(socket.url()));
  });
  const requests = [];
  context.on('request', request => requests.push(request));
  const traffic = [];
  context.on('response', response => {
    if (response.url().includes('/api/v1/') || response.url().endsWith('/tools/_host/v1/session')) {
      traffic.push(response);
    }
  });
  const dashboard = await context.newPage();
  await dashboard.goto(`${DASHBOARD}/tools`);
  const popupReady = context.waitForEvent('page', {timeout: 15000});
  const launchReady = dashboard.waitForResponse(r => r.url().endsWith('/api/tools/fixture-v2/launch'), {timeout: 15000});
  await dashboard.locator('[data-tool-id="fixture-v2"] [data-tool-action="launch"]').click();
  const tool = await popupReady;
  const launch = await (await launchReady).json();
  const code = new URLSearchParams(new URL(launch.url).hash.slice(1)).get('vosu-launch');
  assert.ok(code, 'dashboard must supply a single-use code');
  await tool.waitForFunction(() => document.documentElement.dataset.ready === 'true', {timeout: 15000});
  const exchange = traffic.find(response => response.url().endsWith('/tools/_host/v1/session'));
  assert.equal(exchange.status(), 200);
  const token = (await exchange.json()).session;
  assert.ok(typeof token === 'string' && token.length >= 32, 'exchange returns a session');
  await tool.waitForFunction(() => document.querySelector('#subscription').textContent.includes('test.api.fixture'), null, {timeout: 15000});
  assert.equal(new URL(tool.url()).hash, '', 'bridge consumes and removes the code');
  assert.equal(await tool.evaluate(() => history.length), 1, 'no history entry keeps the code');
  const replay = await context.request.post(`${OVERLAY}/tools/_host/v1/session`, {data: {tool: 'fixture-v2', code}, timeout: 10000});
  assert.equal(replay.status(), 403, 'launch code must not be reusable');
  await tool.locator('#invoke').click();
  await tool.waitForFunction(() => document.querySelector('#result').textContent.includes('browser-6187'), {timeout: 15000});
  assert.deepEqual(JSON.parse(await tool.locator('#result').textContent()), {sentinel: 'browser-6187'});
  await tool.locator('#start').click();
  await tool.waitForFunction(() => document.querySelector('#progress').textContent.includes('fixture progress 7913'), {timeout: 15000});
  const progress = JSON.parse(await tool.locator('#progress').textContent());
  assert.equal(progress.fraction, 0.375);
  const job = await tool.locator('html').getAttribute('data-job');
  assert.ok(job);
  await tool.locator('#abort').click();
  await tool.waitForFunction(() => document.querySelector('#terminal').textContent === '"cancelled"', {timeout: 15000});
  console.log('v2 browser: dashboard launch, single-use exchange, invoke result, progress, abort and cancelled terminal passed');

  // Counterexample: an abort racing job creation must not lose the cancel.
  // The abort fires after the POST left and before its response arrived.
  const raced = await tool.evaluate(async () => {
    const {vosu} = await import('/tools/_host/v2/bridge.js');
    const controller = new AbortController();
    const native = window.fetch;
    window.fetch = (input, init) => {
      const pending = native(input, init);
      if (init?.method === 'POST' && String(input).includes('/actions/')) controller.abort();
      return pending;
    };
    try {
      const handle = await vosu.start('test.api.fixture.run', {mode: 'wait'}, {session: null, signal: controller.signal});
      try { await handle.done; return {id: handle.id, outcome: 'succeeded'}; } catch (error) { return {id: handle.id, outcome: error.code}; }
    } finally {
      window.fetch = native;
    }
  });
  assert.equal(raced.outcome, 'cancelled', 'abort during creation cancels the created job');
  console.log('v2 browser: abort racing job creation still cancels the created job');

  // Counterexamples: a reconnect never holds two sockets; unsubscribe never
  // reconnects (open, during backoff, or on a later online event).
  const session = await tool.locator('html').getAttribute('data-session');
  const reconnect = await tool.evaluate(async session => {
    const {vosu} = await import('/tools/_host/v2/bridge.js');
    const Native = window.WebSocket;
    const created = [];
    let maxLive = 0;
    window.WebSocket = class extends Native {
      constructor(...args) {
        super(...args);
        created.push(this);
        maxLive = Math.max(maxLive, created.filter(s => s.readyState <= Native.OPEN).length);
      }
    };
    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
    const until = async (predicate, what) => {
      const end = performance.now() + 10000;
      while (!predicate()) {
        if (performance.now() > end) throw new Error(`timed out waiting for ${what}`);
        await sleep(20);
      }
    };
    try {
      const frames = [];
      const statuses = [];
      const stop = vosu.subscribe([''], frame => frames.push(frame), status => statuses.push(status), {session});
      await until(() => frames.length === 1, 'initial frame');
      // A drop followed by repeated online events opens exactly one socket.
      created[0].close();
      await until(() => statuses.at(-1) === 'closed', 'drop');
      dispatchEvent(new Event('online'));
      dispatchEvent(new Event('online'));
      await until(() => frames.length === 2, 'frame after online reconnect');
      await sleep(1200);
      const afterOnline = created.length;
      // A drop without online reconnects once, after the backoff.
      created[1].close();
      await until(() => frames.length === 3, 'frame after backoff reconnect');
      const afterBackoff = created.length;
      stop();
      await sleep(1200);
      dispatchEvent(new Event('online'));
      await sleep(300);
      const afterStop = created.length;
      // Unsubscribe during the backoff cancels the pending reconnect.
      const pending = [];
      const stopPending = vosu.subscribe([''], () => {}, status => pending.push(status), {session});
      await until(() => pending.at(-1) === 'open', 'second subscription');
      created.at(-1).close();
      await until(() => pending.at(-1) === 'closed', 'second drop');
      stopPending();
      await sleep(1200);
      dispatchEvent(new Event('online'));
      await sleep(300);
      return {
        afterOnline, afterBackoff, afterStop, total: created.length, maxLive,
        open: created.filter(s => s.readyState <= Native.OPEN).length,
        initial: frames.map(frame => frame[0]),
      };
    } finally {
      window.WebSocket = Native;
    }
  }, session);
  assert.deepEqual(reconnect, {
    afterOnline: 2, afterBackoff: 3, afterStop: 3, total: 4, maxLive: 1, open: 0, initial: [0, 0, 0],
  });
  console.log('v2 browser: reconnects hold one socket at a time; unsubscribe never reconnects');

  // Counterexample: the token never reaches a read, artwork, settings, a URL,
  // a caller-supplied destination or browser storage.
  const confined = await tool.evaluate(async () => {
    const {vosu} = await import('/tools/_host/v2/bridge.js');
    const rejected = [];
    for (const actionId of ['https://evil.invalid/x', '//evil.invalid', '../jobs/x']) {
      try { await vosu.invoke(actionId, {}); } catch (error) { rejected.push(error.name); }
    }
    for (const session of ['..', '.', '']) {
      try { await vosu.invoke('test.api.fixture.run', {}, {session}); } catch (error) { rejected.push(error.name); }
    }
    const stop = vosu.subscribe([''], () => {}, undefined, {session: 'http://evil.invalid/'});
    stop();
    return {
      rejected, fetch: typeof vosu.fetch, settings: await vosu.settings(),
      local: Object.keys(localStorage), session: Object.keys(sessionStorage), cookies: document.cookie,
      databases: (await indexedDB.databases()).length, icon: document.querySelector('#icon').naturalWidth > 0,
    };
  });
  assert.deepEqual(confined, {
    rejected: Array(6).fill('TypeError'), fetch: 'undefined', settings: {},
    local: [], session: [], cookies: '', databases: 0, icon: true,
  });
  assert.deepEqual(await context.cookies(), []);

  // Snapshot the v2 API traffic before the read-only phase below.
  const jobs = [];
  for (const response of traffic) {
    if (!response.url().includes('/api/v1/')) continue;
    assert.ok([200, 202].includes(response.status()), `${response.url()}: ${response.status()}`);
    assert.equal(response.headers()['cache-control'], 'no-store');
    jobs.push({method: response.request().method(), url: response.url(), body: await response.json()});
  }
  assert.ok(jobs.some(r => r.body.result?.sentinel === 'browser-6187'), 'invoke result crosses HTTP');
  assert.ok(jobs.some(r => r.body.id === job && r.body.progress?.fraction === 0.375), 'nonzero progress crosses HTTP');
  assert.ok(jobs.some(r => r.method === 'DELETE' && r.url.endsWith('/jobs/' + job)), 'abort sends DELETE');
  assert.ok(jobs.some(r => r.body.id === job && r.body.state === 'cancelled'), 'plugin cancellation terminal crosses HTTP');
  assert.ok(jobs.some(r => r.method === 'DELETE' && r.url.endsWith('/jobs/' + raced.id)), 'raced abort sends DELETE');
  assert.ok(jobs.some(r => r.body.id === raced.id && r.body.state === 'cancelled'), 'raced job reaches cancelled');

  // Counterexample: a bookmark or reload has no write authority, and a
  // bookmarked launch URL replays a used code that is refused.
  const actionPosts = () => requests.filter(r => r.method() === 'POST' && /\/api\/v1\/(sessions\/[^/]+\/)?actions\//.test(new URL(r.url()).pathname)).length;
  const posted = actionPosts();
  await tool.reload();
  await tool.waitForFunction(() => document.documentElement.dataset.loaded === 'true', {timeout: 15000});
  assert.equal(await tool.locator('html').getAttribute('data-ready'), 'false');
  await tool.locator('#invoke').click();
  await tool.waitForFunction(() => document.querySelector('#result').textContent.includes('forbidden'), {timeout: 15000});
  const bookmark = await context.newPage();
  await bookmark.goto(launch.url);
  await bookmark.waitForFunction(() => document.documentElement.dataset.loaded === 'true', {timeout: 15000});
  assert.equal(await bookmark.locator('html').getAttribute('data-ready'), 'false');
  assert.match(await bookmark.locator('#launch').textContent(), /already used/);
  assert.equal(new URL(bookmark.url()).hash, '', 'a refused code is still removed from the URL');
  await bookmark.locator('#invoke').click();
  await bookmark.waitForFunction(() => document.querySelector('#result').textContent.includes('already used'), {timeout: 15000});
  assert.equal(actionPosts(), posted, 'a read-only page sends no action request');
  const replayed = traffic.filter(r => r.url().endsWith('/tools/_host/v1/session'));
  assert.deepEqual(replayed.map(r => r.status()), [200, 403]);
  for (const headers of [{}, {'X-Vosu-Tool-Session': 'not-a-session'}]) {
    const direct = await context.request.post(`${OVERLAY}/api/v1/actions/test.api.fixture.run?wait=0`, {headers, data: {arguments: {}}, timeout: 10000});
    assert.equal(direct.status(), 401, 'an action without a live session is unauthorized');
  }
  console.log('v2 browser: reload, bookmark and replayed launch URL are read-only; sessionless actions are 401');

  // Counterexample: the removed v1 bridge and its /websocket/v2/subscribe.
  for (const path of ['/tools/_host/v1/bridge.js', '/assets/tool-bridge-v1.js']) {
    assert.equal((await context.request.get(OVERLAY + path, {timeout: 10000})).status(), 404, path);
  }
  const bridge = await (await context.request.get(`${OVERLAY}/tools/_host/v2/bridge.js`, {timeout: 10000})).text();
  assert.equal(bridge.includes('/websocket/v2/subscribe'), false);
  assert.ok(sockets.length >= 6, `expected the bridge's sockets, saw ${sockets.length}`);
  for (const url of sockets) {
    assert.match(url, /^ws:\/\/127\.0\.0\.1:25150\/api\/v1\/sessions\/[^/]+\/subscribe$/, url);
  }

  // Every request: same two origins only; the code and token never in a URL,
  // Referer or body; the session header only on action and job routes.
  let authenticated = 0;
  for (const request of requests) {
    const url = new URL(request.url());
    assert.ok([OVERLAY, DASHBOARD].includes(url.origin), `unexpected destination ${url}`);
    const headers = await request.allHeaders();
    // A launch navigation carries the code in its fragment, which no request
    // sends; what reaches the wire must be free of both secrets.
    const sent = request.url().split('#')[0];
    for (const secret of [token, code]) {
      assert.equal(sent.includes(secret), false, `secret in URL ${sent}`);
      assert.equal((headers.referer || '').includes(secret), false, `secret in Referer of ${url}`);
    }
    const body = request.postData() || '';
    assert.equal(body.includes(token), false, `token in body of ${url}`);
    if (body.includes(code)) assert.equal(url.pathname, '/tools/_host/v1/session', `code in body of ${url}`);
    const header = headers['x-vosu-tool-session'];
    if (header === undefined) continue;
    assert.equal(header, token);
    assert.match(url.pathname, /^\/api\/v1\/((sessions\/[^/]+\/)?actions\/[^/]+|jobs\/[^/]+)$/, `session header sent to ${url}`);
    authenticated += 1;
  }
  assert.ok(authenticated >= 6, `expected authenticated action and job requests, saw ${authenticated}`);
  assert.ok(requests.some(r => new URL(r.url()).pathname === '/tools/fixture-v2/_icon'), 'artwork was requested');
  assert.ok(requests.some(r => new URL(r.url()).pathname === '/tools/fixture-v2/_settings'), 'settings were read');
  console.log('v2 browser: no v1 bridge or removed socket; the token stays in memory and on action and job routes only');
} finally {
  await browser.close();
}
