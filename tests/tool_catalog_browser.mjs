// Required headless companion to tool_catalog_smoke.py (isolated daemon on
// 25151, test-harness catalog listener). Drives catalog v2 management in
// Chromium through the dashboard itself and saves screenshots of each state.
import { mkdir } from 'node:fs/promises';

const [modulePath, evidence, first, second, username, password] = process.argv.slice(2);
if (!modulePath || !evidence || !first || !second || !username || !password) {
  throw new Error('Usage: node tool_catalog_browser.mjs <playwright index.mjs> <evidence> <first source> <second source> <username> <password>');
}
const DASHBOARD = 'http://127.0.0.1:25151';
const WARNING = 'Plugins from this source can run in-process, with the same memory access as vosu itself. Only trust sources you control or know well.';
const { chromium } = await import(modulePath);
await mkdir(evidence, { recursive: true });
const browser = await chromium.launch({ headless: true, executablePath: process.env.VOSU_CHROMIUM || undefined });
const check = (condition, message) => { if (!condition) throw new Error(message); };
const sources = (page) => page.evaluate(() => fetch('/api/catalog/sources').then((r) => r.json()));
const record = async (page, source) => (await sources(page)).sources.find((entry) => entry.source === source);

async function themed(theme) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 1100 }, colorScheme: theme });
  await context.addInitScript((value) => localStorage.setItem('vosu.theme.v1', value), theme);
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  return { context, page, errors };
}

async function trustToggle(page, source) {
  await page.goto(`${DASHBOARD}/settings`);
  const row = page.locator(`#catalog-sources .catalog-source-row[data-source="${source}"]`);
  await row.waitFor({ timeout: 15000 });
  return { row, toggle: row.getByRole('switch', { name: `Trust ${source}`, exact: true }) };
}

async function grantWarning(page, toggle, shot) {
  await toggle.click();
  const dialog = page.getByRole('dialog', { name: 'Trust this catalog source?' });
  await dialog.waitFor();
  check(await dialog.locator('.catalog-warning').textContent() === WARNING, 'trust warning text is not the exact normative text');
  if (shot) await dialog.screenshot({ path: `${evidence}/${shot}.png` });
  return dialog;
}

async function planDialog(page, install, shot) {
  await install.click();
  const dialog = page.locator('dialog.catalog-plan[open]');
  await dialog.waitFor({ timeout: 15000 });
  if (shot) await dialog.screenshot({ path: `${evidence}/${shot}.png` });
  return dialog;
}

async function confirmPlan(page, dialog, expected, shot) {
  await dialog.getByRole('button', { name: /^Install/ }).click();
  const outcome = dialog.locator('.catalog-outcome, .catalog-failures:not(.catalog-plan__errors)');
  await outcome.first().waitFor({ timeout: 45000 });
  const text = await outcome.first().textContent();
  check(text.includes(expected), `outcome lacks "${expected}": ${text}`);
  if (shot) await dialog.screenshot({ path: `${evidence}/${shot}.png` });
  await dialog.getByRole('button', { name: 'Close', exact: true }).click();
}

try {
  const { context, page, errors } = await themed('dark');

  // 1. Trust: the exact warning before EVERY false→true grant.
  let { row, toggle } = await trustToggle(page, first);
  let dialog = await grantWarning(page, toggle, 'trust-warning');
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  check(!(await record(page, first)).trusted && !await toggle.isChecked(), 'cancelled warning granted trust');
  dialog = await grantWarning(page, toggle);
  await dialog.getByRole('button', { name: 'Trust source', exact: true }).click();
  await page.waitForFunction(async (source) => (await (await fetch('/api/catalog/sources')).json()).sources.find((s) => s.source === source).trusted, first);
  await toggle.click(); // untrust: no warning
  await page.waitForFunction(async (source) => !(await (await fetch('/api/catalog/sources')).json()).sources.find((s) => s.source === source).trusted, first);
  check(await page.getByRole('dialog', { name: 'Trust this catalog source?' }).count() === 0, 'untrust showed the grant warning');
  dialog = await grantWarning(page, toggle); // shown again: nothing remembered
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  console.log('trust: exact warning before each grant, cancel keeps untrusted, no remembered bypass');

  // 2. Write-only credential and the private-network flag.
  await row.getByRole('button', { name: `Credential for ${first}`, exact: true }).click();
  dialog = page.getByRole('dialog', { name: `Credential for ${first}` });
  await dialog.waitFor();
  await dialog.screenshot({ path: `${evidence}/credential-dialog.png` });
  await dialog.getByLabel('Username').fill(username);
  await dialog.getByLabel('Password').fill(password);
  await dialog.getByRole('button', { name: 'Save credential' }).click();
  await row.getByText('Credential saved', { exact: true }).waitFor();
  const html = await page.content();
  const storage = await page.evaluate(() => JSON.stringify([{ ...localStorage }, { ...sessionStorage }]));
  for (const secret of [username, password]) {
    check(!html.includes(secret) && !storage.includes(secret), 'credential readable in the page or browser storage');
  }
  check((await record(page, first)).has_credential, 'credential not saved');
  await row.getByRole('switch', { name: `Allow private network for ${first}`, exact: true }).click();
  await page.waitForFunction(async (source) => (await (await fetch('/api/catalog/sources')).json()).sources.find((s) => s.source === source).private_network, first);
  await page.locator('#catalog-sources').screenshot({ path: `${evidence}/sources-dark.png` });
  await row.getByRole('switch', { name: `Allow private network for ${first}`, exact: true }).click();
  await row.getByRole('button', { name: `Credential for ${first}`, exact: true }).click();
  await page.getByRole('dialog', { name: `Credential for ${first}` }).getByRole('button', { name: 'Delete credential' }).click();
  await row.getByText('No credential', { exact: true }).waitFor();
  console.log('credential: write-only save/delete, never in DOM or storage; private toggle round-trips');

  // 3. Plan review: process plugin with approval and the D8 disclosure.
  await page.goto(`${DASHBOARD}/tools`);
  const paths = page.locator('.tools-catalog [data-catalog-entry="acme.paths"]');
  dialog = await planDialog(page, paths.getByRole('button', { name: `Install acme.paths from ${first}` }), null);
  const review = await dialog.textContent();
  for (const fact of ['Native code', 'Runs as a separate process', 'Untrusted source', 'install paths', 'Approve running acme.paths']) {
    check(review.includes(fact), `plan review lacks "${fact}"`);
  }
  const install = dialog.getByRole('button', { name: /^Install/ });
  check(await install.isDisabled(), 'unapproved native plan can be confirmed');
  await dialog.screenshot({ path: `${evidence}/plan-review-d8.png` });
  await dialog.getByRole('checkbox').check();
  await confirmPlan(page, dialog, 'Installed, not running', 'plan-outcome-process');

  // 4. Untrusted in-process: an error plan that cannot be confirmed.
  const echo = page.locator('.tools-catalog [data-catalog-entry="acme.echo"]');
  dialog = await planDialog(page, echo.getByRole('button', { name: `Install acme.echo from ${first}` }), 'plan-error');
  check(await dialog.locator('.catalog-plan__errors').count() === 1, 'in-process plugin from an untrusted source was not an error plan');
  check(await dialog.getByRole('button', { name: /^Install/ }).isDisabled(), 'error plan can be confirmed');
  await dialog.getByRole('button', { name: 'Close', exact: true }).click();

  // 5. Trust the source (warning again), then the in-process plugin loads.
  ({ row, toggle } = await trustToggle(page, first));
  dialog = await grantWarning(page, toggle);
  await dialog.getByRole('button', { name: 'Trust source', exact: true }).click();
  await page.waitForFunction(async (source) => (await (await fetch('/api/catalog/sources')).json()).sources.find((s) => s.source === source).trusted, first);
  await page.goto(`${DASHBOARD}/tools`);
  dialog = await planDialog(page, echo.getByRole('button', { name: `Install acme.echo from ${first}` }), null);
  check((await dialog.textContent()).includes('Trusted source'), 'trusted plan not labelled');
  await confirmPlan(page, dialog, 'Loaded', 'plan-outcome-loaded');

  // 6. A tool: install, then uninstall through the preview.
  const tool = page.locator('.tools-catalog [data-catalog-entry="catalog-tool"]');
  dialog = await planDialog(page, tool.getByRole('button', { name: `Install Catalog tool fixture from ${first}` }), null);
  await confirmPlan(page, dialog, 'Ready', null);
  const card = page.locator('#tools-root .tool-card[data-tool-id="catalog-tool"]');
  await card.waitFor({ timeout: 15000 });
  await page.locator('.catalog-installed [data-installed="catalog-tool"]').waitFor({ timeout: 15000 });
  await card.getByRole('button', { name: 'Details for Catalog tool fixture', exact: true }).click();
  await page.getByRole('button', { name: 'Uninstall Catalog tool fixture', exact: true }).click({ timeout: 15000 });
  dialog = page.locator('dialog.catalog-uninstall[open]');
  await dialog.locator('[data-removal="catalog-tool"]').waitFor();
  await dialog.screenshot({ path: `${evidence}/uninstall-preview.png` });
  await dialog.getByRole('button', { name: 'Uninstall, keep data', exact: true }).click();
  await card.waitFor({ state: 'detached', timeout: 20000 });
  console.log('plans: approval + D8 review, one confirmation, load reports (process, loaded), error plan, uninstall preview');

  // 7. Game offer for the running matched process: review only.
  const offer = page.locator('.catalog-offers [data-offer="acme.game"]');
  for (let attempt = 0; attempt < 20 && !await offer.count(); attempt += 1) {
    await page.waitForTimeout(1500);
    await page.reload();
  }
  await offer.waitFor({ timeout: 5000 });
  await page.locator('.catalog-offers').screenshot({ path: `${evidence}/game-offer.png` });
  dialog = await planDialog(page, offer.getByRole('button', { name: 'Review adding support for acme.game' }), 'game-offer-plan');
  check((await dialog.textContent()).includes('Requested'), 'offer plan is not a requested add');
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();

  // 8. An overlay from the Available page.
  await page.goto(`${DASHBOARD}/available`);
  const overlay = page.locator('.catalog-card--source[data-source-overlay="catalog-overlay"]');
  dialog = await planDialog(page, overlay.getByRole('button', { name: 'Install Catalog overlay fixture' }), null);
  await confirmPlan(page, dialog, 'Ready', null);
  await overlay.getByText('Installed', { exact: true }).waitFor({ timeout: 15000 });
  await overlay.screenshot({ path: `${evidence}/overlay-installed.png` });

  // 9. Untrust while acme.echo runs in-process: revocation pending restart.
  ({ row, toggle } = await trustToggle(page, first));
  await toggle.click();
  const banner = page.locator('#catalog-restart');
  await page.reload();
  await banner.waitFor({ state: 'visible', timeout: 15000 });
  check((await banner.textContent()).includes('acme.echo'), 'pending-restart banner does not name acme.echo');
  await page.screenshot({ path: `${evidence}/pending-restart.png` });
  console.log('offers, overlay install and the pending-restart banner shown');
  if (errors.length) throw new Error(errors.join('\n'));
  await context.close();

  // Light theme: the same management surfaces.
  const light = await themed('light');
  await light.page.goto(`${DASHBOARD}/settings`);
  await light.page.locator('#catalog-sources .catalog-source-row').first().waitFor();
  await light.page.locator('#catalog-sources').screenshot({ path: `${evidence}/sources-light.png` });
  await light.page.goto(`${DASHBOARD}/tools`);
  await light.page.locator('.tools-catalog [data-catalog-entry="acme.paths"]').waitFor({ timeout: 15000 });
  await light.page.screenshot({ path: `${evidence}/tools-light.png`, fullPage: true });
  if (light.errors.length) throw new Error(light.errors.join('\n'));
  await light.context.close();
} finally { await browser.close(); }
