// Required real-browser/ABI smoke for the one tool bridge, driven by
// tool_host_smoke.py: dashboard launch, single-use exchange, invoke, progress,
// abort, the T1 counterexamples (bookmark/reload, replayed code, abort
// racing creation, reconnect and unsubscribe, the removed v1 bridge and
// token confinement) and B0 job artifacts (a streamed, ranged and downloaded
// sessionless artifact; another tab and a revoked tab refused).
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {browserReceipt, watchBrowser} from './browser_diagnostics.mjs';
const [modulePath] = process.argv.slice(2);
if (!modulePath) throw new Error('A Playwright index.mjs is required');
const OVERLAY = 'http://127.0.0.1:25150';
const DASHBOARD = 'http://127.0.0.1:25151';
const {chromium} = await import(pathToFileURL(modulePath).href);
const browser = await chromium.launch({headless: true, executablePath: process.env.VOSU_CHROMIUM || undefined});
const cases = [];
const passed = (name) => { cases.push(name); console.log(name); };
const failures = [];
const diagnostics = [];
try {
  const context = await browser.newContext();
  watchBrowser(context, diagnostics);
  context.setDefaultTimeout(15000);
  context.setDefaultNavigationTimeout(15000);
  const sockets = [];
  context.on('page', page => {
    page.on('pageerror', error => failures.push(`page error: ${error.message}`));
    page.on('console', message => {
      // Chromium reports intentionally refused HTTP responses in the console.
      // Their status and JSON are asserted in their own scenario below.
      if (message.type() === 'error' && !message.text().startsWith('Failed to load resource: the server responded with a status of ')) {
        failures.push(`console error: ${message.text()}`);
      }
    });
    page.on('websocket', socket => sockets.push(socket.url()));
  });
  const requests = [];
  context.on('request', request => requests.push(request));
  context.on('requestfailed', request => {
    if (request.failure()?.errorText !== 'net::ERR_ABORTED') {
      failures.push(`network: ${new URL(request.url()).pathname}: ${request.failure()?.errorText}`);
    }
  });
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
  passed('M7-T01 / X0 dashboard launch, single-use exchange, invoke, progress and cancel');

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
  passed('M7-T05 abort racing job creation still cancels the created job');

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
  passed('M7-T05 reconnects hold one socket at a time; unsubscribe never reconnects');

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
    assert.deepEqual((await direct.json()).error, {code: 'unauthorized', message: 'Action credential required'});
  }
  passed('M7-T01/T05 reload, bookmark and replayed launch URL are read-only; sessionless actions are 401');

  // Exercise the live action with a valid token for the wrong tool, not
  // merely an unknown action or an arbitrary fabricated token.
  const csrf = await dashboard.evaluate(() => JSON.parse(document.getElementById('vosu-context').textContent).csrf);
  async function launchReader() {
    const response = await context.request.post(`${DASHBOARD}/api/tools/fixture-reader/launch`, {
      headers: {Origin: DASHBOARD, 'X-Vosu-CSRF': csrf}, data: {},
    });
    assert.equal(response.status(), 200);
    const code = new URLSearchParams(new URL((await response.json()).url).hash.slice(1)).get('vosu-launch');
    assert.ok(code);
    return code;
  }
  const redeem = (tool, code) => context.request.post(`${OVERLAY}/tools/_host/v1/session`, {data: {tool, code}});
  const wrongToolCode = await launchReader();
  const wrongTool = await redeem('fixture-v2', wrongToolCode);
  assert.equal(wrongTool.status(), 403, 'another tool cannot redeem the code');
  assert.deepEqual(await wrongTool.json(), {error: 'invalid or expired launch code'});
  assert.equal((await redeem('fixture-reader', wrongToolCode)).status(), 403, 'a failed exchange also consumes the code');
  const readerCode = await launchReader();
  const readerExchange = await redeem('fixture-reader', readerCode);
  assert.equal(readerExchange.status(), 200);
  const readerToken = (await readerExchange.json()).session;
  const startAction = (sessionToken, originHeaders = {}) => context.request.post(`${OVERLAY}/api/v1/actions/test.api.fixture.run?wait=0`, {
    headers: {'X-Vosu-Tool-Session': sessionToken, ...originHeaders},
    data: {arguments: {sentinel: 'origin-control'}},
  });
  const forbidden = async (response, message) => {
    assert.equal(response.status(), 403);
    assert.deepEqual((await response.json()).error, {code: 'forbidden', message});
  };
  await forbidden(await startAction(readerToken, {Origin: OVERLAY}), 'Action capability required');
  for (const origin of [{Origin: DASHBOARD}, {Referer: `${DASHBOARD}/tools`}]) {
    await forbidden(await startAction(token, origin), 'Cross-origin action forbidden');
  }
  const sameOrigin = await startAction(token, {Origin: OVERLAY, Referer: `${OVERLAY}/tools/fixture-v2/`});
  assert.equal(sameOrigin.status(), 202);
  assert.ok((await sameOrigin.json()).id);
  passed('M7-T02 same-origin control, cross-origin action refusal and live session missing capability');

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

  // B0 (AF.12): the fixture tool downloads a protected, sessionless,
  // multi-chunk artifact through the bridge; another tab and a revoked tab
  // are refused. Two fresh launches: the owner and another tab.
  const launchTool = async () => {
    const opened = context.waitForEvent('page', {timeout: 15000});
    const launched = dashboard.waitForResponse(r => r.url().endsWith('/api/tools/fixture-v2/launch'), {timeout: 15000});
    await dashboard.locator('[data-tool-id="fixture-v2"] [data-tool-action="launch"]').click();
    const page = await opened;
    const url = (await (await launched).json()).url;
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'true', {timeout: 15000});
    return {page, code: new URLSearchParams(new URL(url).hash.slice(1)).get('vosu-launch')};
  };
  const owner = await launchTool();
  const other = await launchTool();
  // Mirrors crates/vosu-plugin-testkit/src/fixture.rs `ARTIFACT_*`.
  const ARTIFACT_SIZE = 2_621_447;
  const ARTIFACT_NAME = 'fixture-artifact-7913.bin';
  const artifactByte = offset => (offset * 31 + 7) % 251;
  const made = await owner.page.evaluate(async () => {
    const {vosu} = await import('/tools/_host/v2/bridge.js');
    const byte = offset => (offset * 31 + 7) % 251;
    const handle = await vosu.start('test.api.fixture.run', {mode: 'artifact'}, {session: null});
    const result = await handle.done;
    const listed = await handle.artifacts();
    const [artifact] = listed;
    window.fixtureArtifact = artifact;
    // Streamed, never read whole.
    const response = await artifact.fetch();
    const reader = response.body.getReader();
    let total = 0;
    let mismatch = -1;
    for (;;) {
      const {done, value} = await reader.read();
      if (done) break;
      for (let i = 0; mismatch < 0 && i < value.length; i += 1) {
        if (value[i] !== byte(total + i)) mismatch = total + i;
      }
      total += value.length;
    }
    const head = await artifact.fetch({method: 'HEAD'});
    const ranged = await artifact.fetch({range: {start: 1048570, end: 1048580}});
    // The File System Access route pipes the body into the chosen file.
    const native = window.showSaveFilePicker;
    let saved = null;
    window.showSaveFilePicker = async ({suggestedName}) => ({
      async createWritable() {
        saved = {name: suggestedName, size: 0, closed: false};
        return new WritableStream({
          write(chunk) { saved.size += chunk.byteLength; },
          close() { saved.closed = true; },
        });
      },
    });
    try { await artifact.download(); } finally { window.showSaveFilePicker = native; }
    return {
      job: handle.id, result, count: listed.length,
      artifact: {id: artifact.id, name: artifact.name, type: artifact.type, size: artifact.size},
      status: response.status, total, mismatch,
      cache: response.headers.get('cache-control'),
      disposition: response.headers.get('content-disposition'),
      head: [head.status, head.headers.get('content-length')],
      ranged: [ranged.status, ranged.headers.get('content-range'), [...new Uint8Array(await ranged.arrayBuffer())]],
      relisted: (await vosu.artifacts(handle.id)).map(value => value.id),
      saved,
    };
  });
  assert.deepEqual(made.result, {mode: 'artifact'});
  assert.equal(made.count, 1);
  assert.deepEqual(made.artifact, {id: made.artifact.id, name: ARTIFACT_NAME, type: 'application/octet-stream', size: ARTIFACT_SIZE});
  assert.deepEqual([made.status, made.total, made.mismatch], [200, ARTIFACT_SIZE, -1]);
  assert.equal(made.cache, 'no-store');
  assert.match(made.disposition, /^attachment;.*fixture-artifact-7913\.bin/);
  assert.deepEqual(made.head, [200, String(ARTIFACT_SIZE)]);
  assert.deepEqual(made.ranged, [206, `bytes 1048570-1048580/${ARTIFACT_SIZE}`,
    Array.from({length: 11}, (_, i) => artifactByte(1048570 + i))]);
  assert.deepEqual(made.relisted, [made.artifact.id]);
  assert.deepEqual(made.saved, {name: ARTIFACT_NAME, size: ARTIFACT_SIZE, closed: true});
  // Without the picker: a Blob saved through <a download>.
  const downloaded = owner.page.waitForEvent('download', {timeout: 15000});
  await owner.page.evaluate(async () => {
    const native = window.showSaveFilePicker;
    window.showSaveFilePicker = undefined;
    try { await window.fixtureArtifact.download(); } finally { window.showSaveFilePicker = native; }
  });
  const download = await downloaded;
  assert.equal(download.suggestedFilename(), ARTIFACT_NAME);
  const file = await readFile(await download.path());
  assert.equal(file.length, ARTIFACT_SIZE);
  assert.ok(file.every((value, offset) => value === artifactByte(offset)), 'downloaded bytes');
  passed(`B0: tool streamed, ranged and downloaded a ${ARTIFACT_SIZE}-byte sessionless artifact`);

  // Counterexample: another tab, and no session at all, cannot read it.
  const artifactUrl = `${OVERLAY}/api/v1/jobs/${encodeURIComponent(made.job)}/artifacts/${encodeURIComponent(made.artifact.id)}`;
  const foreign = await other.page.evaluate(async ({job, url}) => {
    const {vosu} = await import('/tools/_host/v2/bridge.js');
    let listed;
    try { listed = await vosu.artifacts(job); } catch (error) { listed = error.code; }
    return {listed, bare: (await fetch(url, {credentials: 'omit'})).status};
  }, {job: made.job, url: artifactUrl});
  assert.deepEqual(foreign, {listed: 'unknown_job', bare: 401});
  const exchanged = [];
  for (const response of traffic.filter(r => r.url().endsWith('/tools/_host/v1/session') && r.status() === 200)) {
    exchanged.push((await response.json()).session);
  }
  assert.equal(exchanged.length, 3, 'the first launch, the owner and the other tab');
  const otherRead = await context.request.get(artifactUrl, {headers: {'X-Vosu-Tool-Session': exchanged[2]}, timeout: 10000});
  assert.equal(otherRead.status(), 404);
  assert.equal((await otherRead.json()).error.code, 'unknown_job');
  // Counterexample: disabling the tool revokes the owner's launch session,
  // and the artifact with it.
  const disabled = await dashboard.evaluate(async csrf => (await fetch('/api/tools/fixture-v2/disable', {
    method: 'POST', headers: {'Content-Type': 'application/json', 'X-Vosu-CSRF': csrf}, body: '{}',
  })).status, csrf);
  assert.equal(disabled, 200);
  const revoked = await owner.page.evaluate(async () => {
    const outcomes = [];
    for (const options of [undefined, {method: 'HEAD'}, {range: {start: 0, end: 0}}]) {
      try { await window.fixtureArtifact.fetch(options); outcomes.push('read'); } catch (error) { outcomes.push(error.code); }
    }
    return outcomes;
  });
  assert.deepEqual(revoked, ['unauthorized', 'http_error', 'unauthorized']);
  const ownerRead = await context.request.get(artifactUrl, {headers: {'X-Vosu-Tool-Session': exchanged[1]}, timeout: 10000});
  assert.equal(ownerRead.status(), 401);
  passed('M7-T02/T03 another tab, a sessionless request and a revoked tab cannot read the artifact');

  // Every request: same two origins only; the codes and tokens never in a URL,
  // Referer or body; the session header only on action, job and artifact routes.
  const codes = [code, owner.code, other.code, readerCode, wrongToolCode];
  let authenticated = 0;
  for (const request of requests) {
    const url = new URL(request.url());
    assert.ok([OVERLAY, DASHBOARD].includes(url.origin), `unexpected destination ${url}`);
    const headers = await request.allHeaders();
    // A launch navigation carries the code in its fragment, which no request
    // sends; what reaches the wire must be free of both secrets.
    const sent = request.url().split('#')[0];
    for (const secret of [...exchanged, readerToken, ...codes]) {
      assert.equal(sent.includes(secret), false, `secret in URL ${sent}`);
      assert.equal((headers.referer || '').includes(secret), false, `secret in Referer of ${url}`);
    }
    const body = request.postData() || '';
    for (const secret of exchanged) assert.equal(body.includes(secret), false, `token in body of ${url}`);
    if (codes.some(value => body.includes(value))) assert.equal(url.pathname, '/tools/_host/v1/session', `code in body of ${url}`);
    const header = headers['x-vosu-tool-session'];
    if (header === undefined) continue;
    assert.ok(exchanged.includes(header), `unknown session header to ${url}`);
    assert.match(url.pathname, /^\/api\/v1\/((sessions\/[^/]+\/)?actions\/[^/]+|jobs\/[^/]+(\/artifacts\/[^/]+)?)$/, `session header sent to ${url}`);
    authenticated += 1;
  }
  assert.ok(authenticated >= 6, `expected authenticated action and job requests, saw ${authenticated}`);
  assert.ok(requests.some(r => /\/artifacts\/[^/]+$/.test(new URL(r.url()).pathname) && r.headers()['x-vosu-tool-session'] === exchanged[1]),
    'the artifact reads carried the owner session');
  assert.ok(requests.some(r => new URL(r.url()).pathname === '/tools/fixture-v2/_icon'), 'artwork was requested');
  assert.ok(requests.some(r => new URL(r.url()).pathname === '/tools/fixture-v2/_settings'), 'settings were read');
  passed('M7-R02/T05 removed bridge/socket and launch secret confinement');
  assert.ok(cases.length >= 8, 'named bridge browser scenarios were omitted');
  for (const secret of [...exchanged, readerToken, ...codes, csrf]) {
    assert.ok(!JSON.stringify(diagnostics).includes(secret), 'browser diagnostics leaked a launch secret');
  }
  assert.deepEqual(failures, []);
  console.log(`bridge browser: ${cases.length} named scenarios passed`);
  browserReceipt('tool_host_v2_browser.mjs', cases.length, diagnostics);
} finally {
  await browser.close();
}
