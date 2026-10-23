// Isolated headless companion to tool_catalog_smoke.py; no desktop attachment.
// Runs while `fixture` 1.0.0 is installed from the first source and 2.0.0
// is published.
import { mkdir } from 'node:fs/promises';
const [modulePath, evidence] = process.argv.slice(2);
if (!modulePath || !evidence) throw new Error('Usage: node tool_catalog_browser.mjs <playwright index.mjs> <evidence directory>');
const { chromium } = await import(modulePath);
await mkdir(evidence, { recursive: true });
const browser = await chromium.launch({ headless: true, executablePath: process.env.VOSU_CHROMIUM || undefined });
try {
  for (const theme of ['dark', 'light']) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1080 }, colorScheme: theme });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.addInitScript((theme) => localStorage.setItem('vosu.theme.v1', theme), theme);
    await page.goto('http://127.0.0.1:25151/tools');
    // The installed card offers the update itself (from /api/tools, so it
    // appears once the catalog is fetched), and Details uninstalls.
    const installed = page.locator('#tools-root .tool-card[data-tool-id="fixture"]');
    await installed.getByRole('button', { name: 'Update Tool host fixture to v2.0.0', exact: true }).waitFor({ timeout: 15000 });
    const available = page.locator('.tools-catalog');
    await available.getByRole('heading', { name: 'Available', exact: true }).waitFor();
    // An update is also offered in Available; the later source's copy of an
    // installed id is not offered at all.
    await available.getByRole('button', { name: 'Update Catalog service fixture', exact: true }).waitFor();
    if (await available.getByText('Shadowed', { exact: true }).count()) throw new Error('Shadowed copy of an installed tool is offered');
    await installed.getByRole('button', { name: 'Details for Tool host fixture', exact: true }).click();
    await page.getByRole('button', { name: 'Uninstall Tool host fixture', exact: true }).click();
    await page.getByRole('button', { name: 'Uninstall, keep data', exact: true }).waitFor();
    await page.getByRole('button', { name: 'Uninstall, remove data', exact: true }).waitFor();
    await page.screenshot({ path: `${evidence}/tools-uninstall-${theme}.png` });
    await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    await page.keyboard.press('Escape');
    await page.screenshot({ path: `${evidence}/tools-${theme}.png`, fullPage: true });

    await page.goto('http://127.0.0.1:25151/settings');
    const sources = page.locator('#catalog-sources');
    await sources.getByLabel('Catalog source 2', { exact: true }).waitFor();
    await sources.getByRole('button', { name: 'Add source', exact: true }).click();
    await sources.getByLabel('Catalog source 3', { exact: true }).fill('owner/repo@v1.0.0');
    // Labels name the source from the last render: "source 3" until moved.
    await sources.getByLabel('Enable source 3', { exact: true }).uncheck();
    await sources.getByRole('button', { name: 'Move source 3 up', exact: true }).click();
    if (await sources.getByLabel('Catalog source 2', { exact: true }).inputValue() !== 'owner/repo@v1.0.0') throw new Error('Source reorder failed');
    // Edited, unsaved sources show in the form's save summary.
    await page.locator('.page-head .settings-status', { hasText: 'Unsaved changes' }).waitFor();
    await sources.getByRole('button', { name: 'Remove owner/repo@v1.0.0', exact: true }).click();
    // Back to the saved list: the save state is empty again.
    await page.locator('.page-head .settings-status').waitFor({ state: 'hidden' });
    await sources.screenshot({ path: `${evidence}/sources-${theme}.png` });

    await page.goto('http://127.0.0.1:25151/available');
    const overlay = page.locator('.catalog-card--source', { hasText: 'Catalog overlay fixture' });
    await overlay.getByRole('button', { name: 'Update Catalog overlay fixture', exact: true }).waitFor({ timeout: 15000 });
    await overlay.getByText('Update available', { exact: true }).waitFor();
    await overlay.screenshot({ path: `${evidence}/overlays-${theme}.png` });
    if (errors.length) throw new Error(errors.join('\n'));
    console.log(`${theme}: installed-card update, Details uninstall choices, shadowing, source add/disable/reorder/remove with save state, overlay update`);
    await context.close();
  }
} finally { await browser.close(); }
