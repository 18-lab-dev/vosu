// Required real-browser/ABI smoke, driven by tool_host_smoke.py.
import assert from 'node:assert/strict';
const [modulePath] = process.argv.slice(2);
if (!modulePath) throw new Error('A Playwright index.mjs is required');
const {chromium} = await import(modulePath);
const browser = await chromium.launch({headless: true, executablePath: process.env.VOSU_CHROMIUM || undefined});
try {
  const context = await browser.newContext();
  context.setDefaultTimeout(15000);
  context.on('page', page => page.on('pageerror', error => console.error(`browser page error: ${error.message}`)));
  context.setDefaultNavigationTimeout(15000);
  const traffic = [];
  context.on('response', response => {
    if (response.url().includes('/api/v1/') || response.url().endsWith('/tools/_host/v1/session')) {
      traffic.push(response);
    }
  });
  const dashboard = await context.newPage();
  await dashboard.goto('http://127.0.0.1:25151/tools');
  const popupReady = context.waitForEvent('page', {timeout: 15000});
  const launchReady = dashboard.waitForResponse(r => r.url().endsWith('/api/tools/fixture-v2/launch'), {timeout: 15000});
  await dashboard.locator('[data-tool-id="fixture-v2"] [data-tool-action="launch"]').click();
  const tool = await popupReady;
  const launch = await (await launchReady).json();
  const code = new URLSearchParams(new URL(launch.url).hash.slice(1)).get('vosu-launch');
  assert.ok(code, 'dashboard must supply a single-use code');
  await tool.waitForFunction(() => document.documentElement.dataset.ready === 'true', {timeout: 15000});
  await tool.waitForFunction(() => document.querySelector('#subscription').textContent.includes('test.api.fixture'), null, {timeout: 15000});
  assert.equal(new URL(tool.url()).hash, '', 'bridge consumes and removes the code');
  const replay = await context.request.post('http://127.0.0.1:25150/tools/_host/v1/session', {data: {tool: 'fixture-v2', code}, timeout: 10000});
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
  assert.deepEqual(await tool.evaluate(() => ({local: Object.keys(localStorage), session: Object.keys(sessionStorage), cookies: document.cookie})), {local: [], session: [], cookies: ''});
  console.log('v2 browser: dashboard launch, single-use exchange, invoke result, progress, abort and cancelled terminal passed');
} finally {
  await browser.close();
}
