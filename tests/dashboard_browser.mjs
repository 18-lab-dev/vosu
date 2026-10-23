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
import fs from 'node:fs';

const [modulePath, evidence] = process.argv.slice(2);
if (!modulePath || !evidence) throw new Error('Usage: node dashboard_browser.mjs <playwright index.mjs> <evidence directory>');
const { chromium } = await import(modulePath);
fs.mkdirSync(evidence, { recursive: true });
const OVERLAY = 'http://127.0.0.1:25150';
const DASHBOARD = 'http://127.0.0.1:25151';
const TOLERANCE = 0.5;
const report = { baselines: [], keyboard: [], previews: {}, heads: [], saveState: [] };
const failures = [];
const check = (condition, message) => { if (!condition) failures.push(message); };

const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.VOSU_CHROMIUM || undefined,
  args: ['--host-resolver-rules=MAP tools.invalid 127.0.0.1'],
});

async function open(path, { theme = 'dark', width = 1280, height = 900, reducedMotion = 'no-preference' } = {}) {
  const context = await browser.newContext({ viewport: { width, height }, reducedMotion });
  await context.addInitScript((value) => { try { localStorage.setItem('vosu.theme.v1', value); } catch (_) { /* opaque */ } }, theme);
  const page = await context.newPage();
  page.on('pageerror', (error) => failures.push(`${path}: page error ${error.message}`));
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

await browser.close();
report.failures = failures;
fs.writeFileSync(`${evidence}/dashboard.json`, `${JSON.stringify(report, null, 2)}\n`);
if (failures.length) {
  console.error(failures.join('\n'));
  process.exit(1);
}
