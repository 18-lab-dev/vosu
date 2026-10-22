// Isolated headless companion to tool_catalog_smoke.py; no desktop attachment.
import { mkdir } from 'node:fs/promises';
const [modulePath, evidence] = process.argv.slice(2);
if (!modulePath || !evidence) throw new Error('Usage: node tool_catalog_browser.mjs <playwright index.mjs> <evidence directory>');
const { chromium } = await import(modulePath);
await mkdir(evidence, { recursive: true });
const browser = await chromium.launch({ headless: true });
try {
  for (const theme of ['dark', 'light']) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1080 }, colorScheme: theme });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.addInitScript((theme) => localStorage.setItem('vosu.theme.v1', theme), theme);
    await page.goto('http://127.0.0.1:25151/tools');
    const available = page.locator('.tools-catalog');
    await available.getByRole('heading', { name: '02 / Available' }).waitFor();
    await available.getByRole('button', { name: 'Update', exact: true }).waitFor();
    await available.getByText('Update available', { exact: true }).waitFor();
    await available.getByText('Shadowed', { exact: true }).waitFor();
    await available.getByRole('button', { name: 'Uninstall…', exact: true }).first().click();
    await page.getByRole('button', { name: 'Keep data', exact: true }).waitFor();
    await page.getByRole('button', { name: 'Remove data', exact: true }).waitFor();
    await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    await available.screenshot({ path: `${evidence}/tools-${theme}.png` });

    await page.goto('http://127.0.0.1:25151/settings');
    const sources = page.locator('#catalog-sources');
    await sources.getByLabel('Catalog source 2', { exact: true }).waitFor();
    await sources.getByRole('button', { name: 'Add source', exact: true }).click();
    await sources.getByLabel('Catalog source 3', { exact: true }).fill('owner/repo@v1.0.0');
    await sources.getByLabel('Enable source 3', { exact: true }).uncheck();
    await sources.getByRole('button', { name: 'Move up', exact: true }).last().click();
    if (await sources.getByLabel('Catalog source 2', { exact: true }).inputValue() !== 'owner/repo@v1.0.0') throw new Error('Source reorder failed');
    await sources.getByRole('button', { name: 'Remove source', exact: true }).nth(1).click();
    await sources.screenshot({ path: `${evidence}/sources-${theme}.png` });

    await page.goto('http://127.0.0.1:25151/available');
    const overlays = page.locator('#source-overlays');
    await overlays.getByRole('button', { name: 'Update overlay', exact: true }).waitFor();
    await overlays.getByText('Update available', { exact: true }).waitFor();
    await overlays.screenshot({ path: `${evidence}/overlays-${theme}.png` });
    if (errors.length) throw new Error(errors.join('\n'));
    console.log(`${theme}: tool/overlay Available, update badges, shadowing, uninstall choices, source add/remove/disable/reorder`);
    await context.close();
  }
} finally { await browser.close(); }
