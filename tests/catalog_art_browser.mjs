// Headless companion to tests/catalog_art_smoke.py (isolated daemon on 25151):
//   node tests/catalog_art_browser.mjs <playwright index.mjs> <evidence dir> <before|after> <icon URL>
// before: catalog art renders on the Tools and Available pages, and the
//   hostile SVG icon opened directly on the dashboard origin is sandboxed:
//   opaque origin, no script, no onload handler, no request beyond itself.
// after:  the installed tool card shows its own art and the installed
//   catalog overlay keeps its proxied preview.
import fs from 'node:fs';

const [modulePath, evidence, phase, iconUrl] = process.argv.slice(2);
if (!modulePath || !evidence || !['before', 'after'].includes(phase) || !iconUrl) {
  throw new Error('Usage: node catalog_art_browser.mjs <playwright index.mjs> <evidence dir> <before|after> <icon URL>');
}
const { chromium } = await import(modulePath);
fs.mkdirSync(evidence, { recursive: true });
const DASHBOARD = 'http://127.0.0.1:25151';
const report = {};
const failures = [];
const check = (condition, message) => { if (!condition) failures.push(message); };
const browser = await chromium.launch({ headless: true, executablePath: process.env.VOSU_CHROMIUM || undefined });

// Every image matched by `selector` loaded and decoded with real pixels.
async function rendered(page, selector, name) {
  const locator = page.locator(selector);
  await locator.first().waitFor({ timeout: 15000 }).catch(() => {});
  await page.waitForFunction((s) => [...document.querySelectorAll(s)].every((img) => img.complete), selector, { timeout: 15000 }).catch(() => {});
  const images = await locator.evaluateAll((list) => list.map((img) => ({
    src: img.getAttribute('src'), complete: img.complete, width: img.naturalWidth, height: img.naturalHeight,
  })));
  report[name] = images;
  check(images.length > 0 && images.every((img) => img.complete && img.width > 0 && img.height > 0),
    `${name}: art did not render ${JSON.stringify(images)}`);
  return images;
}

try {
  for (const theme of ['dark', 'light']) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
    await context.addInitScript((value) => localStorage.setItem('vosu.theme.v1', value), theme);
    const page = await context.newPage();
    page.on('pageerror', (error) => failures.push(`page error: ${error.message}`));
    await page.goto(`${DASHBOARD}/tools`, { waitUntil: 'networkidle' });
    if (phase === 'before') {
      const card = '.tools-catalog [data-catalog-tool="artful"]';
      const icons = await rendered(page, `${card} .tool-card__icon img`, `tools available icon ${theme}`);
      await rendered(page, `${card} .tool-card__banner img`, `tools available banner ${theme}`);
      check(icons.every((img) => img.src.startsWith('/api/tools/catalog/art?url=')), `${theme}: catalog art is not proxied`);
      check(await page.locator(`${card} .tool-card__icon`).innerText() === '', `${theme}: icon fell back to initials`);
    } else {
      const card = '#tools-root .tool-card[data-tool-id="artful"]';
      const icons = await rendered(page, `${card} .tool-card__icon img`, `tools installed icon ${theme}`);
      await rendered(page, `${card} .tool-card__banner img`, `tools installed banner ${theme}`);
      check(icons.every((img) => img.src.includes('/tools/artful/_icon')), `${theme}: installed icon is not the tool's own`);
      check(await page.locator('.tools-catalog [data-catalog-tool="artful"]').count() === 0, `${theme}: installed tool still offered`);
    }
    await page.screenshot({ path: `${evidence}/tools-${phase}-${theme}.png`, fullPage: true });
    await page.goto(`${DASHBOARD}/available`, { waitUntil: 'networkidle' });
    const overlay = '.catalog-card--source[data-source-overlay="artlay"]';
    await rendered(page, `${overlay} img.gallery__slide`, `available overlay preview ${phase} ${theme}`);
    if (phase === 'after') {
      check(await page.locator(overlay).getByText('Installed', { exact: true }).count() === 1, `${theme}: overlay not marked installed`);
    }
    await page.locator(overlay).screenshot({ path: `${evidence}/available-${phase}-${theme}.png` });
    await context.close();
  }

  if (phase === 'before') {
    // The proxied hostile SVG opened as a page on the dashboard origin.
    const context = await browser.newContext();
    const requests = [];
    // The browser's own favicon probe is not the document's doing.
    context.on('request', (request) => { if (!request.url().endsWith('/favicon.ico')) requests.push(request.url()); });
    const page = await context.newPage();
    const target = `${DASHBOARD}/api/tools/catalog/art?url=${encodeURIComponent(iconUrl)}`;
    const response = await page.goto(target, { waitUntil: 'load' });
    await page.waitForTimeout(1500);
    const state = await page.evaluate(() => ({
      script: globalThis.__svgScript ?? null,
      onload: globalThis.__svgOnload ?? null,
      origin: self.origin,
      root: document.documentElement?.localName ?? null,
    }));
    report.direct = { status: response.status(), headers: await response.allHeaders(), state, requests };
    check(response.status() === 200 && response.headers()['content-type'] === 'image/svg+xml', 'direct: not served as SVG');
    check(state.root === 'svg', `direct: the document is not the SVG ${JSON.stringify(state)}`);
    check(state.script === null && state.onload === null, `direct: SVG script ran ${JSON.stringify(state)}`);
    check(state.origin === 'null', `direct: document is not sandboxed (origin ${state.origin})`);
    check(requests.length === 1 && requests[0] === target, `direct: the SVG made requests ${JSON.stringify(requests)}`);
    await page.screenshot({ path: `${evidence}/direct-svg.png` });
    await context.close();
  }
} finally {
  await browser.close();
}
fs.writeFileSync(`${evidence}/browser-${phase}.json`, `${JSON.stringify({ report, failures }, null, 2)}\n`);
if (failures.length) {
  console.error(failures.join('\n'));
  process.exit(1);
}
console.log(`${phase}: ${Object.keys(report).length} checks passed`);
