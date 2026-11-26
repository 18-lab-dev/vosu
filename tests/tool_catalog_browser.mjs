// Required headless companion to tool_catalog_smoke.py (isolated daemon on
// 25151, test-harness catalog listener). Drives catalog v2 management in
// Chromium through the dashboard itself and saves screenshots of each state.
import { mkdir } from 'node:fs/promises';
import {browserReceipt, watchBrowser} from './browser_diagnostics.mjs';

const [modulePath, evidence, first, second, username, password, pluginSecret] = process.argv.slice(2);
if (!modulePath || !evidence || !first || !second || !username || !password || !pluginSecret) {
  throw new Error('Usage: node tool_catalog_browser.mjs <playwright index.mjs> <evidence> <first source> <second source> <username> <password> <plugin secret>');
}
const DASHBOARD = 'http://127.0.0.1:25151';
const WARNING = 'Plugins from this source can run in-process, with the same memory access as vosu itself. Only trust sources you control or know well.';
const {pathToFileURL} = await import('node:url');
const { chromium } = await import(pathToFileURL(modulePath).href);
await mkdir(evidence, { recursive: true });
const browser = await chromium.launch({ headless: true, executablePath: process.env.VOSU_CHROMIUM || undefined });
const check = (condition, message) => { if (!condition) throw new Error(message); };
const diagnostics = [];
const cases = [];
const passed = (name) => { cases.push(name); console.log(name); };
const sources = (page) => page.evaluate(() => fetch('/api/catalog/sources').then((r) => r.json()));
const record = async (page, source) => (await sources(page)).sources.find((entry) => entry.source === source);

async function changeSource(page, source, operation, field, expected, click) {
  // Observe the production write, not a GET issued while its transaction
  // still holds the source store. The write returns the committed view.
  const endpoint = `${DASHBOARD}/api/catalog/sources/${encodeURIComponent(source)}/${operation}`;
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.url() === endpoint && r.request().method() === 'POST'),
    click(),
  ]);
  check(response.status() === 200, `${operation}: expected HTTP 200, got ${response.status()}`);
  const saved = (await response.json()).sources?.find((entry) => entry.source === source);
  check(saved?.[field] === expected, `${operation}: expected ${field}=${expected} in the committed view`);
}

async function themed(theme) {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1100 }, colorScheme: theme,
    storageState: { cookies: [], origins: [{ origin: DASHBOARD, localStorage: [{ name: 'vosu.theme.v1', value: theme }] }] },
  });
  watchBrowser(context, diagnostics);
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
  await changeSource(page, first, 'trust', 'trusted', true,
    () => dialog.getByRole('button', { name: 'Trust source', exact: true }).click());
  await changeSource(page, first, 'trust', 'trusted', false, () => toggle.click()); // no warning
  check(await page.getByRole('dialog', { name: 'Trust this catalog source?' }).count() === 0, 'untrust showed the grant warning');
  dialog = await grantWarning(page, toggle); // shown again: nothing remembered
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  passed('trust: exact warning before each grant, cancel keeps untrusted, no remembered bypass');

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
  const privateNetwork = row.getByRole('switch', { name: `Allow private network for ${first}`, exact: true });
  await changeSource(page, first, 'private-network', 'private_network', true, () => privateNetwork.click());
  await page.locator('#catalog-sources').screenshot({ path: `${evidence}/sources-dark.png` });
  await changeSource(page, first, 'private-network', 'private_network', false, () => privateNetwork.click());
  await row.getByRole('button', { name: `Credential for ${first}`, exact: true }).click();
  await page.getByRole('dialog', { name: `Credential for ${first}` }).getByRole('button', { name: 'Delete credential' }).click();
  await row.getByText('No credential', { exact: true }).waitFor();
  passed('credential: write-only save/delete, never in DOM or storage; private toggle round-trips');

  // 3. Both reviews disclose the Linux opt-in before any confirmation.
  await page.goto(`${DASHBOARD}/tools`);
  const namespaceNotice = 'On Linux, this plugin can create user namespaces, which Chromium-style sandboxes need.';
  await page.getByRole('button', { name: 'Review and approve M7 official fixture' }).click();
  const approval = page.getByRole('dialog', { name: 'Approve M7 official fixture?' });
  await approval.getByText(namespaceNotice, { exact: true }).waitFor();
  await approval.getByRole('button', { name: /^Cancel/ }).click();
  const paths = page.locator('.tools-catalog [data-catalog-entry="acme.paths"]');
  dialog = await planDialog(page, paths.getByRole('button', { name: `Install acme.paths from ${first}` }), null);
  const review = await dialog.textContent();
  await dialog.getByText(namespaceNotice, { exact: true }).waitFor();
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
  await changeSource(page, first, 'trust', 'trusted', true,
    () => dialog.getByRole('button', { name: 'Trust source', exact: true }).click());
  await page.goto(`${DASHBOARD}/tools`);
  dialog = await planDialog(page, echo.getByRole('button', { name: `Install acme.echo from ${first}` }), null);
  check((await dialog.textContent()).includes('Trusted source'), 'trusted plan not labelled');
  await confirmPlan(page, dialog, 'Loaded', 'plan-outcome-loaded');

  // 5b. D1: plugins and web tools are separate sections; plugin settings
  // come from the validated schema and a password is never shown again,
  // not even after a failed save.
  const plugins = page.locator('section.plugins');
  const echoRow = plugins.locator('[data-catalog-plugin="acme.echo"]');
  await echoRow.waitFor({ timeout: 15000 });
  const echoText = await echoRow.textContent();
  for (const fact of ['Inside vosu', 'Trusted source', 'Running']) check(echoText.includes(fact), `acme.echo row lacks "${fact}": ${echoText}`);
  check(await plugins.locator('[data-catalog-plugin="acme.paths"]').count() === 1, 'the catalog process plugin is not listed under Plugins');
  check(await plugins.locator('[data-tool-id]').count() === 0, 'a web tool is listed under Plugins');
  check(await page.getByRole('heading', { name: 'Web tools', exact: true }).count() === 1, 'no separate Web tools section');
  await echoRow.getByRole('button', { name: 'Settings for acme.echo', exact: true }).click();
  const settings = page.locator('#dashboard-dialog');
  const token = settings.getByLabel('API token', { exact: true });
  await token.waitFor({ timeout: 15000 });
  check(await settings.getByLabel('Label', { exact: true }).inputValue() === 'hello', 'schema default not applied');
  await settings.getByLabel('Label', { exact: true }).fill('from the dashboard');
  await token.fill(pluginSecret);
  // One failed save (the network refuses it): the typed secret is cleared.
  await page.route('**/api/plugins/acme.echo/settings', (route) => (route.request().method() === 'POST'
    ? route.fulfill({ status: 400, contentType: 'application/json', body: JSON.stringify({ code: 'invalid_settings', error: 'refused by the test' }) })
    : route.fallback()), { times: 1 });
  await settings.getByRole('button', { name: 'Save', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#dashboard-dialog input[type="password"]')?.value === '');
  check(await page.evaluate(() => document.querySelector('#dashboard-dialog').open), 'dialog closed after a failed save');
  check(!(await page.content()).includes(pluginSecret), 'plugin password echoed after a failed save');
  await token.fill(pluginSecret);
  await settings.getByRole('button', { name: 'Save', exact: true }).click();
  await page.waitForFunction(() => !document.querySelector('#dashboard-dialog')?.open);
  await echoRow.getByRole('button', { name: 'Settings for acme.echo', exact: true }).click();
  await token.waitFor();
  check(await token.inputValue() === '' && await token.getAttribute('placeholder') === 'Saved — leave blank to keep', 'saved plugin password not shown as write-only');
  check(await settings.getByLabel('Label', { exact: true }).inputValue() === 'from the dashboard', 'plugin setting not saved');
  await settings.screenshot({ path: `${evidence}/plugin-settings.png` });
  check(!(await page.content()).includes(pluginSecret), 'plugin password readable in the page');
  await page.keyboard.press('Escape');
  await plugins.screenshot({ path: `${evidence}/plugins-section.png` });
  passed('plugins vs web tools separated; plugin settings defaults, write-only password, blank after a failed save');

  // 6. A tool: install, then uninstall through the preview.
  const tool = page.locator('.tools-catalog [data-catalog-entry="catalog-tool"]');
  dialog = await planDialog(page, tool.getByRole('button', { name: `Install Catalog tool fixture from ${first}` }), null);
  await confirmPlan(page, dialog, 'Ready', null);
  const card = page.locator('#tools-root .tool-card[data-tool-id="catalog-tool"]');
  await card.waitFor({ timeout: 15000 });
  await page.locator('.catalog-installed [data-installed="catalog-tool"]').waitFor({ timeout: 15000 });
  await card.getByRole('button', { name: 'Details for Catalog tool fixture', exact: true }).click();
  const details = page.locator('dialog.tool-details[data-tool-id="catalog-tool"]');
  await details.getByRole('button', { name: 'Uninstall Catalog tool fixture', exact: true }).click({ timeout: 15000 });
  dialog = page.locator('dialog.catalog-uninstall[open]');
  await dialog.locator('[data-removal="catalog-tool"]').waitFor();
  await dialog.screenshot({ path: `${evidence}/uninstall-preview.png` });
  await dialog.getByRole('button', { name: 'Uninstall, keep data', exact: true }).click();
  await card.waitFor({ state: 'detached', timeout: 20000 });
  check(await page.locator('#tools-root .tool-card').count() === 0, 'uninstall fixture did not leave an empty tools inventory');
  // Before any offer refresh or reload, verify the real empty-inventory
  // render dismissed the details drawer on both Linux and Windows.
  await details.waitFor({ state: 'hidden', timeout: 5000 });
  passed('plans: approval + D8 review, one confirmation, load reports (process, loaded), error plan, uninstall preview');

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
  // The UI must say it is still running, never that it stopped.
  ({ row, toggle } = await trustToggle(page, first));
  await changeSource(page, first, 'trust', 'trusted', false, () => toggle.click());
  const toast = page.locator('#toast .toast__message');
  await page.waitForFunction(() => document.querySelector('#toast .toast__message')?.textContent.includes('restart vosu'));
  const untrusted = await toast.textContent();
  check(untrusted.includes('acme.echo is still running inside vosu'), `untrust notice hides the running plugin: ${untrusted}`);
  check(!/stopped|unloaded\./i.test(untrusted), `untrust notice claims the code stopped: ${untrusted}`);
  const banner = page.locator('#catalog-restart');
  await page.reload();
  await banner.waitFor({ state: 'visible', timeout: 15000 });
  check((await banner.textContent()).includes('acme.echo'), 'pending-restart banner does not name acme.echo');
  await page.screenshot({ path: `${evidence}/pending-restart.png` });
  await page.goto(`${DASHBOARD}/tools`);
  const pendingRow = page.locator('section.plugins [data-catalog-plugin="acme.echo"][data-state="pending_restart"]');
  await pendingRow.waitFor({ timeout: 15000 });
  const pendingText = await pendingRow.textContent();
  for (const fact of ['Revocation pending restart', 'still loaded and running inside vosu', 'vosu does not restart for you', 'Untrusted source']) {
    check(pendingText.includes(fact), `pending row lacks "${fact}": ${pendingText}`);
  }
  await pendingRow.screenshot({ path: `${evidence}/plugin-pending-restart.png` });
  passed('offers, overlay install, truthful untrust notice and the pending-restart banner and row shown');
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
  passed('light theme: source and tool management surfaces');
} finally { await browser.close(); }
for (const sentinel of [username, password, pluginSecret]) {
  check(!JSON.stringify(diagnostics).includes(sentinel), 'browser diagnostics leaked a secret');
}
browserReceipt('tool_catalog_browser.mjs', cases.length, diagnostics);
