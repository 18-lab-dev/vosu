// D20 removed-route notice checks for tests/dashboard_smoke.py (M7-N01,
// M7-N02, M7-G01–G03), run after dashboard_browser.mjs against the same
// isolated daemon on 25150/25151:
//   node tests/dashboard_notices_browser.mjs <playwright index.mjs> <evidence dir> <overlay root>
// 1. A real 1.x overlay page opens the removed /websocket/v2/subscribe and
//    fetches the removed /api/ingame; both get 404 and nothing upgrades.
// 2. The dashboard's production status refresh shows the notice: the
//    overlay's hostile name as text, the count, the age, the fixed guide
//    link, the unknown /api/ingame bucket without a name, and the overlay
//    still installed. The vosulay action states truthfully that its catalog
//    source is absent and offers a retry, never a plan.
// 3. With the vosulay entry listed (catalog routes answered by the test),
//    Install opens the ordinary plan review: the exact install request, and
//    confirmation only after the listed approval; nothing is confirmed here.
// 4. With vosulay installed, the notice offers launch and manage.
// 5. Removing the overlay folder drops its name and link at the next status
//    refresh; the unknown bucket stays.
import fs from 'node:fs';
import path from 'node:path';
import {browserReceipt, watchBrowser} from './browser_diagnostics.mjs';

const [modulePath, evidence, overlayRoot] = process.argv.slice(2);
if (!modulePath || !evidence || !overlayRoot) {
  throw new Error('Usage: node dashboard_notices_browser.mjs <playwright index.mjs> <evidence directory> <overlay root>');
}
const {pathToFileURL} = await import('node:url');
const { chromium } = await import(pathToFileURL(modulePath).href);
fs.mkdirSync(evidence, { recursive: true });
const OVERLAY = 'http://127.0.0.1:25150';
const DASHBOARD = 'http://127.0.0.1:25151';
const GUIDE = 'https://github.com/18-lab-dev/vosu/blob/main/docs/v2/migration-from-1x.md#overlays-that-used-removed-apis';
const HOSTILE_ID = 'hostile-overlay';
const HOSTILE_NAME = '<img src=x onerror="window.__d20=1">Hostile & "quoted"';
const VOSULAY = { kind: 'tool', id: 'vosulay-editor', source: '18-lab-dev/vosulay' };
const report = {};
const failures = [];
const diagnostics = [];
const check = (condition, message) => { if (!condition) failures.push(message); };

const browser = await chromium.launch({ headless: true, executablePath: process.env.VOSU_CHROMIUM || undefined });

async function openRouted(route, routes = []) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  watchBrowser(context, diagnostics);
  const page = await context.newPage();
  page.on('pageerror', (error) => failures.push(`${route}: page error ${error.message}`));
  for (const [url, handler] of routes) await page.route(url, handler);
  await page.goto(DASHBOARD + route, { waitUntil: 'domcontentloaded' });
  return { context, page };
}

const ready = (page, selector, timeout = 8000) => page.waitForSelector(selector, { state: 'attached', timeout }).then(() => true, () => false);

function notice(page) {
  return page.evaluate((id) => {
    const region = document.querySelector('#removed-routes');
    const block = (feature) => region?.querySelector(`[data-feature="${feature}"]`);
    const row = (feature) => [...(block(feature)?.querySelectorAll('[data-overlay-id]') ?? [])].find((item) => item.dataset.overlayId === id);
    const describe = (feature) => {
      const known = row(feature);
      const unknown = block(feature)?.querySelector('[data-role="unknown"]');
      return block(feature) ? {
        name: known?.querySelector('[data-role="overlay-link"]')?.textContent ?? null,
        href: known?.querySelector('[data-role="overlay-link"]')?.getAttribute('href') ?? null,
        attempts: known?.querySelector('[data-role="attempts"]')?.textContent ?? null,
        badge: known?.querySelector('.badge')?.textContent ?? null,
        unknown: unknown?.textContent ?? null,
        guide: block(feature).querySelector('[data-role="guide"]')?.href ?? null,
      } : null;
    };
    return {
      hidden: region ? region.hidden : null,
      features: [...(region?.querySelectorAll('[data-feature]') ?? [])].map((element) => element.dataset.feature),
      v2: describe('selected-v2-feed'),
      ingame: describe('ingame-page'),
      guides: [...(region?.querySelectorAll('a[target="_blank"]') ?? [])].map((link) => link.href),
      injected: region ? region.querySelectorAll('img, script, iframe').length : null,
      executed: window.__d20 ?? null,
      buttons: [...(region?.querySelectorAll('button') ?? [])].map((button) => button.textContent),
      action: region?.querySelector('[data-role="ingame-action"]')?.dataset.state ?? null,
      actionText: region?.querySelector('[data-role="ingame-action"]')?.textContent ?? null,
    };
  }, HOSTILE_ID);
}

// 1. A 1.x overlay opened directly, as a browser source would.
{
  const context = await browser.newContext();
  watchBrowser(context, diagnostics);
  const page = await context.newPage();
  const sockets = [];
  const handshakes = [];
  page.on('console', (message) => {
    if (message.text().includes('/websocket/v2/subscribe')) handshakes.push(message.text());
  });
  page.on('websocket', (socket) => sockets.push(socket.url()));
  await page.goto(`${OVERLAY}/${HOSTILE_ID}/index.html`, { waitUntil: 'domcontentloaded' });
  const done = await page.waitForFunction(() => document.body.dataset.socket === 'closed' && document.body.dataset.ingame,
    null, { timeout: 10000 }).then(() => true, () => false);
  const result = await page.evaluate(() => ({ ...document.body.dataset }));
  report.overlay = { result, sockets, handshakes };
  check(done && result.socket === 'closed' && result.opened !== 'true', `overlay: the removed WebSocket must fail without opening ${JSON.stringify(result)}`);
  check(result.ingame === '404', `overlay: /api/ingame must be 404 ${JSON.stringify(result)}`);
  check(sockets.some((url) => url.includes('/websocket/v2/subscribe')), `overlay: the page really attempted the removed WebSocket ${JSON.stringify(sockets)}`);
  check(handshakes.some((message) => message.includes('Unexpected response code: 404')),
    `M7-N01: the real upgrade attempt received HTTP 404 ${JSON.stringify(handshakes)}`);
  await context.close();
}

// 2. The notice through the production status refresh.
{
  const requests = [];
  const { context, page } = await openRouted('/');
  page.on('request', (request) => requests.push(`${request.method()} ${new URL(request.url()).pathname}`));
  check(await ready(page, `#removed-routes [data-feature="selected-v2-feed"] [data-overlay-id="${HOSTILE_ID}"]`), 'notice: the selected-feed notice must name the overlay');
  check(await ready(page, '#removed-routes [data-role="ingame-action"][data-state="unavailable"]'), 'notice: the vosulay action must settle');
  const shown = await notice(page);
  report.notice = shown;
  check(shown.hidden === false, `notice: visible ${JSON.stringify(shown)}`);
  check(shown.v2?.name === HOSTILE_NAME, `notice: the name is shown as text, escaped ${JSON.stringify(shown.v2)}`);
  check(shown.injected === 0 && shown.executed === null, `notice: the name injects nothing ${JSON.stringify(shown)}`);
  check(shown.v2?.href === `/settings?overlay=${HOSTILE_ID}`, `notice: links the installed overlay ${JSON.stringify(shown.v2)}`);
  check(/^1 request · (just now|\d+ s ago)$/.test(shown.v2?.attempts ?? ''), `notice: one genuine request (previews uncounted) and its age ${JSON.stringify(shown.v2)}`);
  check(shown.v2?.badge?.startsWith('Enabled'), `notice: the overlay stays enabled ${JSON.stringify(shown.v2)}`);
  check(shown.guides.length === shown.features.length, `notice: one guide link per notice ${JSON.stringify(shown.guides)}`);
  check(shown.v2?.guide === GUIDE && shown.ingame?.guide === GUIDE, `notice: the fixed guide link ${JSON.stringify(shown)}`);
  check(shown.guides.every((href) => href === GUIDE), `notice: no other external link ${JSON.stringify(shown.guides)}`);
  check(shown.ingame?.name === HOSTILE_NAME, `notice: /api/ingame from the overlay page is attributed by its own path ${JSON.stringify(shown.ingame)}`);
  check(/^Unidentified clients: \d+ requests? · /.test(shown.ingame?.unknown ?? '') && !shown.ingame.unknown.includes(HOSTILE_NAME),
    `notice: the in-game client's requests are an unnamed bucket ${JSON.stringify(shown.ingame)}`);
  check(shown.features.join() === 'selected-v2-feed,legacy-skin-index,ingame-page', `notice: every removed feature, in order ${JSON.stringify(shown.features)}`);
  check(shown.action === 'unavailable' && shown.actionText.includes('18-lab-dev/vosulay') && shown.buttons.join() === 'Retry',
    `notice: no vosulay source means retry guidance, and no other control ${JSON.stringify(shown)}`);
  const card = await page.evaluate((id) => [...document.querySelectorAll('.counter-card')].some((element) => element.dataset.overlayId === id), HOSTILE_ID);
  check(card, 'notice: the overlay is still installed and listed');
  await page.locator('#removed-routes').screenshot({ path: `${evidence}/removed-routes-notice.png` });
  await page.locator('#removed-routes button', { hasText: 'Retry' }).click();
  check(await ready(page, '#removed-routes [data-role="ingame-action"][data-state="unavailable"] button'), 'notice: retry settles again');
  check(requests.includes('POST /api/catalog/refresh'), `notice: Retry refreshes the catalog ${JSON.stringify(requests)}`);
  check(!requests.some((request) => request.startsWith('POST /api/catalog/install')), 'notice: no plan without an entry');
  await context.close();
}

// 3. The listed vosulay entry: Install starts the ordinary plan review.
{
  const installs = [];
  const confirms = [];
  const { context, page } = await openRouted('/', [
    [`${DASHBOARD}/api/tools`, (route) => route.fulfill({ json: { tools: [] } })],
    [`${DASHBOARD}/api/catalog/entries`, (route) => route.fulfill({ json: { fetched: true, entries: [{ ...VOSULAY, name: 'vosulay editor', version: '1.0.0' }] } })],
    [`${DASHBOARD}/api/catalog/sources`, (route) => route.fulfill({ json: { sources: [{ source: VOSULAY.source, enabled: true, status: 'ok', entries: 2 }] } })],
    [`${DASHBOARD}/api/catalog/install`, (route) => {
      installs.push({ method: route.request().method(), body: route.request().postDataJSON() });
      route.fulfill({ json: { plan_id: 'plan-n1', errors: [], adds: [
        { id: 'vosu.vosulay', kind: 'plugin', version: '1.0.0', source: VOSULAY.source, reason: 'dependency', native: true, runtime: 'process', trust: 'official', needs_approval: true },
        { id: VOSULAY.id, kind: 'tool', version: '1.0.0', source: VOSULAY.source, reason: 'requested' },
      ] } });
    }],
    [`${DASHBOARD}/api/catalog/plans/**`, (route) => { confirms.push(route.request().url()); route.abort(); }],
  ]);
  check(await ready(page, '#removed-routes [data-action="install-vosulay"]'), 'install: a listed entry offers Install');
  await page.click('#removed-routes [data-action="install-vosulay"]');
  const reviewed = await ready(page, 'dialog.catalog-plan[open]');
  const review = await page.evaluate(() => {
    const dialog = document.querySelector('dialog.catalog-plan[open]');
    const confirm = [...(dialog?.querySelectorAll('button') ?? [])].find((button) => button.textContent.startsWith('Install'));
    return { approvals: [...(dialog?.querySelectorAll('[data-approve]') ?? [])].map((box) => box.dataset.approve), disabled: confirm?.disabled ?? null };
  });
  report.install = { installs, review };
  check(reviewed && installs.length === 1 && installs[0].method === 'POST' && JSON.stringify(installs[0].body) === JSON.stringify(VOSULAY),
    `install: the exact vosulay-editor install request ${JSON.stringify(installs)}`);
  check(review.approvals.join() === 'vosu.vosulay' && review.disabled === true, `install: confirmation waits for the listed approval ${JSON.stringify(review)}`);
  await page.check('dialog.catalog-plan[open] [data-approve="vosu.vosulay"]');
  check(await page.evaluate(() => [...document.querySelectorAll('dialog.catalog-plan[open] button')].find((button) => button.textContent.startsWith('Install'))?.disabled === false),
    'install: the approval enables confirmation');
  await page.click('dialog.catalog-plan[open] button:has-text("Cancel")');
  await page.waitForTimeout(300);
  check(confirms.length === 0, `install: nothing confirms on its own ${JSON.stringify(confirms)}`);
  await page.locator('#removed-routes').screenshot({ path: `${evidence}/removed-routes-install.png` });
  await context.close();
}

// 4. Installed: launch and manage instead of install.
{
  const { context, page } = await openRouted('/', [
    [`${DASHBOARD}/api/tools`, (route) => route.fulfill({ json: { tools: [{ id: VOSULAY.id, name: 'vosulay editor', status: 'running' }] } })],
  ]);
  check(await ready(page, '#removed-routes [data-action="launch-vosulay"]'), 'installed: Launch is offered');
  const manage = await page.evaluate(() => [...document.querySelectorAll('#removed-routes [data-role="ingame-action"] a')].map((link) => [link.textContent, link.getAttribute('href')]));
  check(JSON.stringify(manage) === JSON.stringify([['Manage', '/tools']]), `installed: Manage opens the Tools page ${JSON.stringify(manage)}`);
  report.installed = manage;
  check(!(await page.$('#removed-routes [data-action="install-vosulay"]')), 'installed: no Install');
  await context.close();
}

// 5. Uninstalling drops the overlay's name and link at the next refresh.
{
  const { context, page } = await openRouted('/');
  check(await ready(page, `#removed-routes [data-overlay-id="${HOSTILE_ID}"]`), 'uninstall: named before removal');
  fs.rmSync(path.join(overlayRoot, HOSTILE_ID), { recursive: true, force: true });
  const dropped = await page.waitForFunction((id) => ![...document.querySelectorAll('#removed-routes [data-overlay-id]')].some((row) => row.dataset.overlayId === id),
    HOSTILE_ID, { timeout: 8000 }).then(() => true, () => false);
  const after = await notice(page);
  report.uninstalled = after;
  check(dropped && after.v2 === null && after.ingame?.name === null && after.ingame?.unknown, `uninstall: no stale link, the unknown bucket stays ${JSON.stringify(after)}`);
  await context.close();
}

await browser.close();
report.failures = failures;
fs.writeFileSync(`${evidence}/removed-routes.json`, `${JSON.stringify(report, null, 2)}\n`);
if (failures.length) {
  console.error(failures.join('\n'));
  process.exit(1);
}
browserReceipt('dashboard_notices_browser.mjs', Object.keys(report).length - 1, diagnostics);
