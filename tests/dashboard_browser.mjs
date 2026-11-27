// Browser checks for tests/dashboard_smoke.py (isolated daemon on 25150/25151):
//   node tests/dashboard_browser.mjs <playwright index.mjs> <evidence dir>
// 1. Text baselines in the header and the status strip agree within 0.5 px.
// 2. Every control of every card on the Overlays, Available and Tools grids
//    is reachable by keyboard: arrows between cards, Tab into a card.
// 3. The dashboard's own previews are not counted as connected sources,
//    while the same overlay opened directly is.
// 4. Every page's head (title, search) sits at the same place, and Settings'
//    save state is empty at rest, shows "Saved" after a save and then
//    disappears (at once with reduced motion), and keeps "Unsaved changes"
//    while a network field waits for Apply.
// 5. The status strip joins core diagnostics with API v1 session summaries.
// 6. The update notice follows DashboardStatus.update: minor and major copy,
//    major consent, and a release link only for an http(s) URL.
// 7. Settings fills from SettingsView without saving anything, and a failed
//    SettingsView read replaces the form with an alert.
// 8. Tools are 2.0 web tools: Ready, Blocked and legacy Disabled cards, a
//    launch through the single-use exchange, and no request to a deleted
//    service-era tool route from any page.
import fs from 'node:fs';
import {browserReceipt, watchBrowser} from './browser_diagnostics.mjs';

const [modulePath, evidence] = process.argv.slice(2);
if (!modulePath || !evidence) throw new Error('Usage: node dashboard_browser.mjs <playwright index.mjs> <evidence directory>');
const {pathToFileURL} = await import('node:url');
const { chromium } = await import(pathToFileURL(modulePath).href);
fs.mkdirSync(evidence, { recursive: true });
const OVERLAY = 'http://127.0.0.1:25150';
const DASHBOARD = 'http://127.0.0.1:25151';
const TOLERANCE = 0.5;
// Service-era tool routes deleted in 2.0 (A7.2); the dashboard must never call them.
const DELETED_TOOL_ROUTE = /^\/api\/tools\/[^/]+\/(start|stop|restart|open-data|clear-cache|logs|autostart)$/;
const report = { baselines: [], keyboard: [], previews: {}, heads: [], saveState: [], toolStatuses: [] };
const failures = [];
const diagnostics = [];
const check = (condition, message) => { if (!condition) failures.push(message); };

const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.VOSU_CHROMIUM || undefined,
  args: ['--host-resolver-rules=MAP tools.invalid 127.0.0.1'],
});

async function open(path, { theme = 'dark', width = 1280, height = 900, reducedMotion = 'no-preference' } = {}) {
  const context = await browser.newContext({
    viewport: { width, height }, reducedMotion,
    storageState: { cookies: [], origins: [{ origin: DASHBOARD, localStorage: [{ name: 'vosu.theme.v1', value: theme }] }] },
  });
  watchBrowser(context, diagnostics);
  const page = await context.newPage();
  page.on('pageerror', (error) => failures.push(`${path}: page error ${error.message}`));
  // The 2.0 Tools page never calls a deleted service-era tool route.
  page.on('request', (request) => {
    const route = new URL(request.url()).pathname;
    if (DELETED_TOOL_ROUTE.test(route)) failures.push(`${path}: requested deleted ${request.method()} ${route}`);
  });
  await page.goto(DASHBOARD + path, { waitUntil: 'networkidle' });
  await page.waitForTimeout(800);
  return { context, page };
}

// The y of each element's first text line's baseline: a zero-height
// inline-block at the end of a span wrapped around its first text node sits
// on that line's baseline.
function measureBaselines(page, selectors) {
  return page.evaluate((list) => list.map((selector) => {
    const element = document.querySelector(selector);
    if (!element) return [selector, null];
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT, {
      acceptNode: (node) => (node.textContent.trim() ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP),
    });
    const text = walker.nextNode();
    if (!text) return [selector, null];
    const wrapper = document.createElement('span');
    const probe = document.createElement('span');
    probe.style.cssText = 'display:inline-block;width:0;height:0;vertical-align:baseline';
    text.replaceWith(wrapper);
    wrapper.append(text, probe);
    const y = probe.getBoundingClientRect().bottom;
    wrapper.replaceWith(text);
    return [selector, Math.round(y * 100) / 100];
  }), selectors);
}

async function baselines(name, page, selectors) {
  const values = await measureBaselines(page, selectors);
  const ys = values.map(([, y]) => y);
  check(ys.every((y) => y !== null), `${name}: missing element ${JSON.stringify(values)}`);
  const spread = Math.max(...ys) - Math.min(...ys);
  report.baselines.push({ name, spread: Math.round(spread * 100) / 100, values });
  check(spread <= TOLERANCE, `${name}: baselines differ by ${spread.toFixed(2)} px ${JSON.stringify(values)}`);
}

for (const theme of ['dark', 'light']) {
  for (const width of [1280, 1920]) {
    const { context, page } = await open('/', { theme, width });
    await baselines(`header ${theme} ${width}`, page,
      ['.brand-name', '.brand-sub', '.nav-link[aria-current] .nav-label', '.nav-link:not([aria-current]) .nav-label', 'a.header-link']);
    // The update chip is the cell that once sat lower; show it as an update
    // would, with the notice put off.
    await page.evaluate(() => {
      document.querySelector('#update-notice').hidden = true;
      const chip = document.querySelector('.status-update');
      chip.hidden = false;
      chip.textContent = '9.9.9 available';
    });
    await baselines(`status strip ${theme} ${width}`, page,
      ['.status-cell .status-key', '#attachment-status', '.status-cell--version .status-value', '.status-update']);
    await page.screenshot({ path: `${evidence}/header-${theme}-${width}.png`, clip: { x: 0, y: 0, width, height: 120 } });
    await context.close();
  }
}

// Walks one card grid with the keyboard only (after focusing its single
// Tab stop): Tab through each card's controls, back with Shift+Tab, then
// ArrowRight to the next card.
async function walkGrid(name, page, gridSelector, cardSelector) {
  const cards = () => page.evaluate(([grid, card]) => {
    const container = document.querySelector(grid);
    return container ? [...container.children].filter((item) => item.matches(card) && !item.hidden).length : 0;
  }, [gridSelector, cardSelector]);
  const count = await cards();
  const result = { name, cards: count, reached: 0, controls: 0 };
  report.keyboard.push(result);
  check(count > 1, `${name}: expected more than one card, found ${count}`);
  if (!count) return;
  const state = (index) => page.evaluate(([grid, card, at]) => {
    const items = [...document.querySelector(grid).children].filter((item) => item.matches(card) && !item.hidden);
    const current = items[at];
    const controls = [...current.querySelectorAll('a[href], button, input, select, textarea')]
      .filter((element) => element.tabIndex >= 0 && element.getClientRects().length && !element.closest('[popover]'));
    controls.forEach((element, position) => { element.dataset.walk = `${at}:${position}`; });
    return { onCard: document.activeElement === current, controls: controls.length };
  }, [gridSelector, cardSelector, index]);
  const active = () => page.evaluate(() => document.activeElement?.dataset.walk ?? null);
  // Enter the grid through its one Tab stop.
  await page.evaluate(([grid, card]) => [...document.querySelector(grid).children].find((item) => item.matches(card) && item.tabIndex === 0)?.focus(), [gridSelector, cardSelector]);
  for (let index = 0; index < count; index += 1) {
    const { onCard, controls } = await state(index);
    check(onCard, `${name}: card ${index + 1} did not take focus`);
    if (!onCard) break;
    result.reached += 1;
    const seen = new Set();
    for (let step = 0; step < controls; step += 1) {
      await page.keyboard.press('Tab');
      const walk = await active();
      if (walk?.startsWith(`${index}:`)) seen.add(walk);
    }
    result.controls += seen.size;
    check(seen.size === controls, `${name}: card ${index + 1} reached ${seen.size} of ${controls} controls by Tab`);
    for (let step = 0; step < controls; step += 1) await page.keyboard.press('Shift+Tab');
    if (index < count - 1) await page.keyboard.press('ArrowRight');
  }
  check(result.reached === count, `${name}: reached ${result.reached} of ${count} cards`);
}

{
  const { context, page } = await open('/', { height: 1400 });
  await walkGrid('Overlays', page, '#counter-results', '.counter-card');
  await context.close();
}
{
  const { context, page } = await open('/available', { height: 1400 });
  await page.waitForSelector('.catalog-card--source', { timeout: 15000 }).catch(() => {});
  await walkGrid('Available', page, '#counter-results', '.counter-card');
  await context.close();
}
{
  const { context, page } = await open('/tools', { height: 1400 });
  await page.waitForSelector('#tools-root > .tool-card');
  await walkGrid('Tools installed', page, '#tools-root', '.tool-card');
  await page.waitForSelector('.tools-catalog .tools-root > .tool-card', { timeout: 15000 }).catch(() => {});
  await walkGrid('Tools available', page, '.tools-catalog .tools-root', '.tool-card');
  await context.close();
}

// 2.0 web tools: a servable tool is Ready and opens through the single-use
// launch exchange, an unmet `requires` is Blocked, and a 1.x `tool.toml`
// package is Disabled with needs-2.0-update. No card or drawer offers a
// service control.
for (const theme of ['dark', 'light']) {
  const { context, page } = await open('/tools', { theme, width: 1920, height: 1080 });
  await page.waitForSelector('#tools-root > .tool-card');
  const card = (id) => page.locator(`#tools-root > [data-tool-id="${id}"]`);
  const label = (id) => card(id).locator('.tool-card__status > span').nth(1);
  const expectations = [['fixture-v2', 'Ready'], ['fixture-v2-two', 'Ready'], ['fixture-v2-blocked', 'Blocked'], ['fixture', 'Disabled']];
  for (const [id, expected] of expectations) {
    check(await label(id).textContent() === expected, `${theme}: ${id} must show ${expected}`);
  }
  const views = await page.evaluate(async () => (await fetch('/api/tools').then((r) => r.json())).tools);
  check(views.find((v) => v.id === 'fixture-v2').status === 'running', `${theme}: fixture-v2 must be servable`);
  check(views.find((v) => v.id === 'fixture-v2-blocked').status === 'blocked', `${theme}: unmet requires must block`);
  check(views.find((v) => v.id === 'fixture').status_detail === 'needs-2.0-update', `${theme}: legacy package must need the 2.0 update`);
  check(await card('fixture-v2-blocked').locator('[data-tool-action="launch"]').count() === 0, `${theme}: a blocked tool must not offer Open`);
  check(await card('fixture').locator('[data-tool-action="launch"], [data-tool-action="enable"], [data-tool-action="trust"]').count() === 0,
    `${theme}: a legacy package offers no launch, enable or trust`);
  report.toolStatuses.push({ theme, labels: await Promise.all(expectations.map(async ([id]) => [id, await label(id).textContent()])) });
  await page.screenshot({ path: `${evidence}/tool-statuses-${theme}-1920.png` });
  await page.setViewportSize({ width: 1440, height: 1080 });
  await page.screenshot({ path: `${evidence}/tool-statuses-${theme}-1440.png` });

  const popup = context.waitForEvent('page', { timeout: 15000 });
  const launched = page.waitForResponse((response) => response.url().endsWith('/api/tools/fixture-v2/launch'), { timeout: 15000 });
  await card('fixture-v2').locator('[data-tool-action="launch"]').click();
  const tool = await popup;
  check((await launched).status() === 200, `${theme}: launch must succeed`);
  await tool.waitForFunction(() => document.documentElement.dataset.ready === 'true', null, { timeout: 15000 });
  check(new URL(tool.url()).pathname === '/tools/fixture-v2/', `${theme}: launch must open the tool UI, got ${tool.url()}`);
  check(new URL(tool.url()).hash === '', `${theme}: the bridge must consume the launch fragment`);
  check(await tool.title() === 'M3 browser fixture', `${theme}: the tool page must load`);
  await tool.close();

  await card('fixture-v2-blocked').locator('[data-tool-action="details"]').click();
  const drawer = page.locator('dialog.tool-details');
  check(await drawer.locator('.tool-status').textContent() === 'Blocked', `${theme}: Details must show the blocked status`);
  check(await drawer.locator('.tool-dependency[data-satisfied="false"]').count() > 0, `${theme}: Details must list the unmet requirement`);
  check(await page.locator('[data-tool-action="start"], [data-tool-action="stop"], [data-tool-action="restart"], [data-tool-action="logs"], [data-tool-action="data"]').count() === 0,
    `${theme}: no service controls`);
  await context.close();
}

// Cover every 2.0 status label without changing the host status contract.
{
  const { context, page } = await open('/tools');
  const base = await page.evaluate(async () => (await fetch('/api/tools').then((r) => r.json())).tools[0]);
  const labels = { running: 'Ready', blocked: 'Blocked', untrusted: 'Untrusted', disabled: 'Disabled', invalid: 'Invalid' };
  const tools = Object.keys(labels).map((status) => ({
    ...base, id: `status-${status}`, name: `web ${status}`, status,
    trusted: status !== 'untrusted', enabled: status !== 'disabled',
  }));
  await page.route('**/api/tools', (route) => route.fulfill({ json: { tools } }));
  await page.reload();
  for (const view of tools) {
    const card = page.locator(`#tools-root > [data-tool-id="${view.id}"]`);
    await card.waitFor();
    check(await card.locator('.tool-card__status > span').nth(1).textContent() === labels[view.status], `${view.id}: expected ${labels[view.status]}`);
    check(await card.getAttribute('data-status') === view.status, `${view.id}: raw status changed`);
  }
  await context.close();
}

// Previews: the Overlays page loads six live previews that each open a
// socket; none may count. The same overlay opened directly does.
{
  const { context, page } = await open('/', { height: 1400 });
  await page.waitForTimeout(2500);
  const status = () => page.evaluate(async () => (await (await fetch('/api/dashboard/status')).json()).connections ?? {});
  const previewFrames = await page.locator('.preview-stage iframe').count();
  const withPreviews = await status();
  report.previews.frames = previewFrames;
  report.previews.connectionsWithPreviews = withPreviews;
  check(previewFrames >= 6, `previews: expected 6 preview frames, found ${previewFrames}`);
  check(Object.keys(withPreviews).length === 0, `previews: dashboard previews were counted ${JSON.stringify(withPreviews)}`);
  const source = await context.newPage();
  await source.goto(`${OVERLAY}/overlay-0/`);
  await page.waitForTimeout(1500);
  const withSource = await status();
  report.previews.connectionsWithSource = withSource;
  check(withSource['overlay-0'] === 1, `previews: a directly opened overlay must count once ${JSON.stringify(withSource)}`);
  await page.screenshot({ path: `${evidence}/overlays-previews.png` });
  await context.close();
}

// Page heads: every page puts its title and search at the same place, and
// Settings starts its content as close under the head as the others.
for (const width of [1280, 1920]) {
  const heads = [];
  for (const path of ['/', '/available', '/tools', '/settings']) {
    const { context, page } = await open(path, { width });
    heads.push({ path, ...await page.evaluate(() => {
      const box = (selector) => document.querySelector(selector)?.getBoundingClientRect();
      const head = box('.page-head');
      return {
        titleTop: Math.round(box('#page-title').top * 100) / 100,
        searchTop: Math.round(box('.search-form').top * 100) / 100,
        headHeight: Math.round(head.height * 100) / 100,
        headBottom: Math.round(head.bottom * 100) / 100,
        gapBelow: Math.round((document.querySelector('.page-head').nextElementSibling.getBoundingClientRect().top - head.bottom) * 100) / 100,
        statusShown: Boolean(document.querySelector('.page-head .settings-status:not([hidden])')),
        networkHint: document.body.innerText.includes('Network changes need Apply'),
      };
    }) });
    await context.close();
  }
  report.heads.push({ width, heads });
  for (const key of ['titleTop', 'searchTop', 'headHeight', 'gapBelow']) {
    const values = heads.map((head) => head[key]);
    check(Math.max(...values) - Math.min(...values) <= TOLERANCE, `heads ${width}: ${key} differs ${JSON.stringify(values)}`);
  }
  const settings = heads.find((head) => head.path === '/settings');
  check(!settings.statusShown, `heads ${width}: the save state must be empty at rest`);
  check(!settings.networkHint, `heads ${width}: the network hint copy must be gone`);
}

// Save state lifecycle on Settings. Where the site header is pinned, the
// state stays in view (in the head row, or in the status strip once the head
// is scrolled away); at every width it never overlaps a control.
for (const [width, reducedMotion] of [[390, 'no-preference'], [1280, 'no-preference'], [1920, 'no-preference'], [1280, 'reduce']]) {
  const { context, page } = await open('/settings', { width, reducedMotion });
  const state = () => page.evaluate(() => {
    const status = document.querySelector('.settings-status');
    const box = status.getBoundingClientRect();
    const header = document.querySelector('.site-header');
    const pinnedHeader = getComputedStyle(header).position === 'sticky';
    const inPage = status.closest('.page-head') !== null;
    const inView = !status.hidden && box.bottom <= innerHeight && box.top >= 0
      && (inPage ? box.top >= header.getBoundingClientRect().bottom - 0.5 : true);
    // Page content scrolled under the pinned header is hidden by the header
    // itself, so only its part below the header counts. Containers of the
    // state (the main landmark, the strip) are not covered by it.
    const headerBottom = pinnedHeader ? header.getBoundingClientRect().bottom : -Infinity;
    const overlaps = status.hidden ? [] : [...document.querySelectorAll('a[href], button, input, select, textarea, summary, [tabindex], .status-cell')]
      .filter((element) => element !== status && !status.contains(element) && !element.contains(status) && element.getClientRects().length)
      .filter((element) => {
        const other = element.getBoundingClientRect();
        const top = header.contains(element) ? other.top : Math.max(other.top, headerBottom);
        return other.width > 0 && other.bottom - top > 0 && box.left < other.right - 0.5 && other.left < box.right - 0.5
          && box.top < other.bottom - 0.5 && top < box.bottom - 0.5;
      })
      .map((element) => element.dataset.id || element.className || element.tagName);
    return { shown: !status.hidden, text: status.textContent, fading: status.classList.contains('is-fading'), place: inPage ? 'head' : 'strip', pinnedHeader, inView, overlaps };
  });
  const debug = page.locator('[data-id="DEBUG_LOG"]');
  const steps = {};
  await debug.click();
  await page.waitForFunction(() => document.querySelector('.settings-status')?.textContent === 'Saved', null, { timeout: 5000 }).catch(() => {});
  steps.afterSave = await state();
  if (reducedMotion === 'no-preference') {
    await page.waitForTimeout(2150);
    steps.fading = await state();
  }
  await page.waitForTimeout(reducedMotion === 'reduce' ? 2300 : 600);
  steps.later = await state();
  await debug.click();
  await page.waitForTimeout(3000);
  const port = page.locator('[data-id="SERVER_PORT"]');
  const original = await port.inputValue();
  await port.fill('25999');
  // Scrolling to the field moves the state on the next intersection callback.
  await page.waitForTimeout(300);
  steps.networkEdited = await state();
  await page.screenshot({ path: `${evidence}/save-state-unsaved-${width}-${reducedMotion}.png` });
  await page.waitForTimeout(3000);
  steps.networkLater = await state();
  await port.fill(original);
  steps.networkRestored = await state();
  report.saveState.push({ width, reducedMotion, ...steps });
  const label = `save state (${width}, ${reducedMotion})`;
  check(steps.afterSave.shown && steps.afterSave.text === 'Saved', `${label}: "Saved" must show after a save ${JSON.stringify(steps.afterSave)}`);
  for (const [name, step] of Object.entries(steps)) {
    check(step.overlaps.length === 0, `${label}: ${name} overlaps ${JSON.stringify(step.overlaps)}`);
    if (step.shown && step.pinnedHeader) check(step.inView, `${label}: ${name} must be in view ${JSON.stringify(step)}`);
  }
  if (steps.fading) check(steps.fading.shown && steps.fading.fading, `${label}: "Saved" must fade after about 2 s ${JSON.stringify(steps.fading)}`);
  check(!steps.later.shown, `${label}: "Saved" must disappear ${JSON.stringify(steps.later)}`);
  check(steps.networkEdited.text === 'Unsaved changes' && steps.networkLater.shown, `${label}: an edited network field must keep "Unsaved changes" ${JSON.stringify(steps)}`);
  check(!steps.networkRestored.shown, `${label}: restoring the field must clear the state ${JSON.stringify(steps.networkRestored)}`);
  await context.close();
}

// Status strip (S0): core diagnostics joined by PID with API v1 session
// summaries read from the overlay origin. The real daemon answers first
// (no game: an empty list, read cross-origin without credentials); then
// scripted responses cover focus moving between two sessions, unknown
// fields, a failed summary read and a detach.
{
  const { context, page } = await open('/');
  const sessionReads = [];
  const sockets = [];
  page.on('websocket', (socket) => sockets.push(socket.url()));
  page.on('request', (request) => {
    if (request.url() === `${OVERLAY}/api/v1/sessions`) sessionReads.push(request.headers());
  });
  const real = await page.waitForResponse((response) => response.url() === `${OVERLAY}/api/v1/sessions`, { timeout: 6000 }).catch(() => null);
  check(real?.status() === 200, `status: the overlay-origin session read must succeed, got ${real?.status()}`);
  check(sessionReads.length > 0 && sessionReads.every((headers) => !('x-vosu-csrf' in headers) && !('cookie' in headers) && !('authorization' in headers)),
    `status: session reads must carry no CSRF, cookie or credential ${JSON.stringify(sessionReads)}`);
  const instance = (pid, client, poll, precise, version = '2026.1') => ({ pid, client, version, state: 'reading', pollIntervalMs: poll, precisePollIntervalMs: precise });
  const session = (pid, game, name, variant, focused, version = '2026.1') => ({ game, variant, name, version, pid, session: `s${pid}`, focused, plugin: { id: `vosu.${game}`, version: '2.0.0' } });
  let scene = {
    status: { available: true, connections: {}, instances: [instance(101, 'lazer', 150, 10), instance(202, 'other', 999, 77)] },
    sessions: { sessions: [session(202, 'rhythm', 'Rhythm', 'beta', false), session(101, 'osu', 'osu!', 'lazer', true)] },
  };
  await page.route(`${DASHBOARD}/api/dashboard/status`, (route) => route.fulfill({ json: scene.status }));
  await page.route(`${OVERLAY}/api/v1/sessions`, (route) => (scene.sessions === null
    ? route.fulfill({ status: 500, json: { error: { code: 'internal' } }, headers: { 'access-control-allow-origin': '*' } })
    : route.fulfill({ json: scene.sessions, headers: { 'access-control-allow-origin': '*' } })));
  const strip = () => page.evaluate(() => {
    const cell = (name) => document.querySelector(`[data-status-cell="${name}"]`);
    return {
      attachment: document.querySelector('#attachment-status').textContent,
      client: cell('client').hidden ? null : cell('client').querySelector('.status-value').textContent,
      poll: cell('poll').hidden ? null : cell('poll').querySelector('.status-value').textContent,
    };
  });
  const settle = async (predicate, label) => {
    const ok = await page.waitForFunction(predicate, null, { timeout: 6000 }).then(() => true, () => false);
    const value = await strip();
    report.status = [...(report.status ?? []), { label, ...value }];
    check(ok, `status: ${label} ${JSON.stringify(value)}`);
    return value;
  };
  await settle(() => document.querySelector('[data-status-cell="client"] .status-value').textContent.includes('PID 202'), 'two sessions');
  const both = await strip();
  check(both.client === 'osu! lazer 2026.1 · PID 101 (focused) | Rhythm beta 2026.1 · PID 202', `status: labels follow each PID ${JSON.stringify(both)}`);
  check(both.poll === '150 ms / precise 10 ms', `status: only the osu session's own interval shows ${JSON.stringify(both)}`);
  // Focus moves to a second osu session between reads: its own interval follows it.
  scene = { ...scene, status: { ...scene.status, instances: [instance(101, 'lazer', 150, 10), instance(202, 'stable', 333, 7)] },
    sessions: { sessions: [session(101, 'osu', 'osu!', 'lazer', false), session(202, 'osu', 'osu!', 'stable', true)] } };
  await settle(() => document.querySelector('[data-status-cell="poll"] .status-value').textContent.startsWith('333'), 'focus moved');
  // Unknown values are shown as unknown, never as zero or "null".
  scene = { status: { available: true, connections: {}, instances: [{ pid: 303, client: 'lazer', version: null, state: 'reading', pollIntervalMs: null, precisePollIntervalMs: null }] },
    sessions: { sessions: [{ ...session(303, 'osu', 'osu!', 'lazer', true), version: '' }] } };
  const unknown = await settle(() => document.querySelector('[data-status-cell="client"] .status-value').textContent.includes('PID 303'), 'unknown fields');
  check(unknown.client === 'osu! lazer version unknown · PID 303' && unknown.poll === null, `status: unknown fields ${JSON.stringify(unknown)}`);
  // A failed summary read keeps core's own text and invents no game.
  scene = { status: { available: true, connections: {}, instances: [instance(404, 'stable', 150, 10)] }, sessions: null };
  const failed = await settle(() => document.querySelector('[data-status-cell="client"] .status-value').textContent.includes('PID 404'), 'summaries unavailable');
  check(failed.client === 'stable 2026.1 · PID 404' && failed.poll === null, `status: failed summaries ${JSON.stringify(failed)}`);
  // Detach: nothing from the previous poll remains.
  scene = { status: { available: true, connections: {}, instances: [] }, sessions: { sessions: [] } };
  const detached = await settle(() => document.querySelector('[data-status-cell="client"]').hidden, 'detached');
  check(detached.client === null && detached.poll === null && detached.attachment.startsWith('Not detected'), `status: detach ${JSON.stringify(detached)}`);
  check(sockets.length === 0, `status: the dashboard must open no state subscription ${JSON.stringify(sockets)}`);
  await context.close();
}

// A page whose routes are installed before its first request.
async function openRouted(path, routes) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  watchBrowser(context, diagnostics);
  const page = await context.newPage();
  page.on('pageerror', (error) => failures.push(`${path}: page error ${error.message}`));
  for (const [url, handler] of routes) await page.route(url, handler);
  await page.goto(DASHBOARD + path, { waitUntil: 'domcontentloaded' });
  return { context, page };
}

// Update notice (D0): DashboardStatus.update fills the notice once per
// version. A minor update shows the plain copy; a major one shows the
// consent block, installs only after consent, and links its release notes
// only for an http(s) URL. "Later" hides that version across reloads.
{
  report.updateNotice = [];
  let update = { version: '9.1.0', major: false, release_url: null };
  const updates = [];
  const { context, page } = await openRouted('/', [
    [`${DASHBOARD}/api/dashboard/status`, (route) => route.fulfill({ json: { available: true, connections: {}, instances: [], update } })],
    [`${DASHBOARD}/api/runUpdates`, (route) => { updates.push(route.request().postData()); return route.fulfill({ json: { status: 'latest' } }); }],
  ]);
  const notice = (label) => page.evaluate(() => {
    const element = document.querySelector('#update-notice');
    const chip = document.querySelector('.status-update');
    return {
      hidden: element.hidden,
      version: element.dataset.version,
      major: element.dataset.major,
      copy: !element.querySelector(':scope > .update-notice__copy').hidden,
      majorBlock: !element.querySelector('.update-notice__major').hidden,
      versions: [...document.querySelectorAll('[data-role="new-version"]')].map((node) => node.textContent),
      href: element.querySelector('[data-role="release-url"]').getAttribute('href'),
      chip: chip.hidden ? null : chip.textContent.trim(),
    };
  }).then((value) => { report.updateNotice.push({ label, ...value }); return value; });
  const shown = (version) => page.waitForFunction((v) => document.querySelector('#update-notice')?.dataset.version === v, version, { timeout: 6000 })
    .then(() => true, () => false);

  check(await shown('9.1.0'), 'update: a minor notice must appear');
  const minor = await notice('minor');
  check(!minor.hidden && minor.copy && !minor.majorBlock && minor.major === 'false', `update: minor shows the plain copy ${JSON.stringify(minor)}`);
  check(minor.versions.every((value) => value === '9.1.0') && minor.chip === '9.1.0 available', `update: minor version text ${JSON.stringify(minor)}`);
  check(minor.href === null, `update: a minor notice links nowhere ${JSON.stringify(minor)}`);
  await page.locator('[data-action="dismiss-update"]').click();
  check(await page.locator('#update-notice').isHidden(), 'update: "Later" must hide the notice');
  await page.reload({ waitUntil: 'domcontentloaded' });
  check(await shown('9.1.0'), 'update: the reloaded page must read the notice again');
  const later = await notice('minor after Later and reload');
  check(later.hidden && later.chip === '9.1.0 available', `update: a dismissed version stays hidden, its chip stays ${JSON.stringify(later)}`);

  const release = 'https://github.com/18-lab-dev/vosu/releases/tag/v10.0.0';
  update = { version: '10.0.0', major: true, release_url: release };
  check(await shown('10.0.0'), 'update: a major notice must appear');
  const major = await notice('major');
  check(!major.hidden && !major.copy && major.majorBlock && major.major === 'true', `update: major shows the consent block ${JSON.stringify(major)}`);
  check(major.href === release, `update: major links its release notes ${JSON.stringify(major)}`);
  const link = page.locator('[data-role="release-url"]');
  check((await link.getAttribute('target')) === '_blank' && (await link.getAttribute('rel'))?.includes('noopener'), 'update: the release link must open apart, without an opener');
  const install = page.locator('#update-notice [data-action="app-update"]');
  await install.click();
  await page.waitForTimeout(400);
  check(updates.length === 0, `update: a major update must not start without consent ${JSON.stringify(updates)}`);

  // A later unsafe release URL removes the earlier safe link, and none of
  // these ever becomes a link.
  for (const [index, unsafe] of ['javascript:alert(1)', 'data:text/html,<b>x</b>', '//evil.example/notes', 'file:///etc/passwd', 42, null].entries()) {
    const version = `11.0.${index}`;
    update = { version, major: true, release_url: unsafe };
    check(await shown(version), `update: notice ${version} must appear`);
    const value = await notice(`unsafe ${JSON.stringify(unsafe)}`);
    check(value.majorBlock && value.href === null, `update: ${JSON.stringify(unsafe)} must not become a link ${JSON.stringify(value)}`);
  }
  // A hostile version string is text, never markup.
  const hostile = '12.0.0"><img src=x onerror="window.__vosuPwned=1"><script>window.__vosuPwned=2</script>';
  update = { version: hostile, major: true, release_url: release };
  check(await shown(hostile), 'update: a hostile version must still show as text');
  const escaped = await notice('hostile version');
  const injected = await page.evaluate(() => ({
    pwned: window.__vosuPwned ?? null,
    elements: document.querySelectorAll('#update-notice img, #update-notice script, .status-update img, .status-update script').length,
  }));
  check(escaped.versions.every((value) => value === hostile) && injected.pwned === null && injected.elements === 0,
    `update: the version must be inserted as text ${JSON.stringify({ escaped, injected })}`);
  check(await page.locator('[data-count="installed"]').first().textContent() === '6', 'update: the installed count must follow the overlay list');

  const requested = page.waitForRequest(`${DASHBOARD}/api/runUpdates`, { timeout: 6000 }).catch(() => null);
  await page.locator('[data-role="major-update-consent"]').check();
  await install.click();
  await requested;
  check(updates.length === 1 && JSON.parse(updates[0] ?? '{}').consent === true, `update: consent must send one consenting request ${JSON.stringify(updates)}`);
  await page.screenshot({ path: `${evidence}/update-notice.png`, clip: { x: 0, y: 0, width: 1280, height: 260 } });
  await context.close();
}

// Settings (D0): the shell carries no values; they arrive from core's
// SettingsView, fill every control without saving, and never include the
// dashboard switch. A failed read replaces the form with an alert.
{
  report.settings = {};
  const saves = [];
  const recordSaves = [`${DASHBOARD}/api/settingsSave`, (route) => { saves.push(route.request().postData()); return route.fulfill({ json: { status: 'updated' } }); }];
  const filled = (page) => page.waitForFunction(() => {
    const form = document.querySelector('#runtime-settings');
    return form && !form.inert && !form.hasAttribute('aria-busy');
  }, null, { timeout: 6000 }).then(() => true, () => false);
  const controls = (page) => page.evaluate(() => Object.fromEntries([...document.querySelectorAll('#runtime-settings [data-id]')]
    .map((control) => [control.dataset.id, control.type === 'checkbox' ? control.checked : control.value])));

  {
    const { context, page } = await openRouted('/settings', [recordSaves]);
    check(await filled(page), 'settings: the form must be filled and enabled');
    const view = await page.evaluate(async () => {
      const { csrf } = JSON.parse(document.querySelector('#vosu-context').textContent);
      return (await fetch('/api/settings', { headers: { Accept: 'application/json', 'X-Vosu-CSRF': csrf } })).json();
    });
    const shown = await controls(page);
    report.settings.filled = shown;
    check(Object.keys(view.values ?? {}).length === 11, `settings: SettingsView must carry 11 keys ${JSON.stringify(view)}`);
    for (const [key, value] of Object.entries(view.values ?? {})) {
      // SettingsView has a platform-independent shape, but the control
      // starts a systemd user service and must not appear on Windows.
      if (key === 'START_AT_LOGIN' && process.platform === 'win32') {
        check(value === false && !(key in shown), 'settings: Windows must not offer the Linux start-at-login control');
        continue;
      }
      const expected = typeof value === 'boolean' ? value
        : key === 'ALLOWED_IPS' ? String(value).split(',').map((entry) => entry.trim()).filter(Boolean).join('\n')
          : String(value);
      check(shown[key] === expected, `settings: ${key} shows ${JSON.stringify(shown[key])}, view has ${JSON.stringify(value)}`);
    }
    check(shown.SERVER_PORT === '25150' && shown.DASHBOARD_PORT === '25151' && shown.ENABLE_AUTOUPDATE === false
      && shown.ALLOWED_IPS === '127.0.0.1\nlocalhost\nabsolute', `settings: values follow vosu.toml ${JSON.stringify(shown)}`);
    check(!('ENABLE_DASHBOARD' in shown) && !('ENABLE_DASHBOARD' in (view.values ?? {})), 'settings: the dashboard switch must have no control or value');
    for (const moved of ['CALCULATE_PP', 'POLL_RATE', 'PRECISE_DATA_POLL_RATE', 'SHOW_MP_COMMANDS', 'READ_MANIA_SCROLL_SPEED', 'STATIC_FOLDER_PATH']) {
      check(!(moved in shown) && !(moved in (view.values ?? {})), `settings: moved key ${moved} must not appear on Settings`);
    }
    await page.waitForTimeout(500);
    check(saves.length === 0, `settings: filling must save nothing ${JSON.stringify(saves)}`);
    await context.close();
  }
  {
    // A partial view leaves missing keys empty rather than inventing them;
    // externally managed updates lock the auto-update control.
    const { context, page } = await openRouted('/settings', [recordSaves, [`${DASHBOARD}/api/settings`, (route) => route.fulfill({
      json: { values: { ENABLE_AUTOUPDATE: true, DEBUG_LOG: true, SERVER_PORT: 25150 }, updates_externally_managed: true },
    })]]);
    check(await filled(page), 'settings: a partial view must still enable the form');
    const shown = await controls(page);
    const managed = await page.evaluate(() => ({
      disabled: document.querySelector('[data-id="ENABLE_AUTOUPDATE"]').disabled,
      description: document.querySelector('#setting-ENABLE_AUTOUPDATE-description')?.textContent,
    }));
    report.settings.partial = { shown, managed };
    check(shown.DEBUG_LOG === true && shown.SERVER_PORT === '25150' && shown.API_V1_SUBSCRIPTION_DEBOUNCE === '', `settings: a partial view fills only its keys ${JSON.stringify(shown)}`);
    check(managed.disabled && managed.description?.includes('managed by your Linux package manager'), `settings: managed updates lock the control ${JSON.stringify(managed)}`);
    await page.waitForTimeout(500);
    check(saves.length === 0, `settings: a partial fill must save nothing ${JSON.stringify(saves)}`);
    await context.close();
  }
  for (const [label, handler] of [
    ['500', (route) => route.fulfill({ status: 500, json: { error: 'settings store unavailable' } })],
    ['network', (route) => route.abort('connectionrefused')],
  ]) {
    const { context, page } = await openRouted('/settings', [recordSaves, [`${DASHBOARD}/api/settings`, handler]]);
    const alerted = await page.waitForFunction(() => [...document.querySelectorAll('[role="alert"] h2')]
      .some((heading) => heading.textContent === "Couldn't load settings"), null, { timeout: 6000 }).then(() => true, () => false);
    const failure = await page.evaluate(() => {
      const alert = [...document.querySelectorAll('.empty-state[role="alert"]')].find((element) => element.querySelector('h2')?.textContent === "Couldn't load settings");
      return {
        form: Boolean(document.querySelector('#runtime-settings')),
        message: alert?.querySelector('p')?.textContent ?? null,
        retry: alert?.querySelector('a')?.getAttribute('href') ?? null,
      };
    });
    report.settings[`failure ${label}`] = failure;
    check(alerted && !failure.form && failure.retry === '/settings', `settings (${label}): a failed read must replace the form with a retry alert ${JSON.stringify(failure)}`);
    if (label === '500') check(failure.message === 'settings store unavailable', `settings: the alert names the error ${JSON.stringify(failure)}`);
    check(saves.length === 0, `settings (${label}): a failed read must save nothing ${JSON.stringify(saves)}`);
    if (label === '500') await page.screenshot({ path: `${evidence}/settings-load-failure.png` });
    await context.close();
  }
}

await browser.close();
report.failures = failures;
fs.writeFileSync(`${evidence}/dashboard.json`, `${JSON.stringify(report, null, 2)}\n`);
if (failures.length) {
  console.error(failures.join('\n'));
  process.exit(1);
}
browserReceipt('dashboard_browser.mjs', Object.keys(report).length - 1, diagnostics);
