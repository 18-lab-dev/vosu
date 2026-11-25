// Catalog v2 dashboard helpers shared by the Overlays, Available, Tools and
// Settings pages. Every catalog request goes to the dashboard-only
// `/api/catalog/*` routes (docs/v2/catalog-v2.md "API placement"); writes
// carry the page's X-Vosu-CSRF header through `requestJson`.
export { mountCatalogSources } from './dashboard-catalog-sources.js';
export { mountCatalogOverlays } from './dashboard-catalog-browse.js';

export function node(tag, text, className) {
  const element = document.createElement(tag);
  if (text !== undefined && text !== null) element.textContent = String(text);
  if (className) element.className = className;
  return element;
}
export function button(label, run, { className = 'button', disabled = false, describe } = {}) {
  const element = node('button', label, className); element.type = 'button'; element.disabled = disabled;
  if (describe) element.setAttribute('aria-label', describe);
  element.addEventListener('click', run); return element;
}
function httpUrl(value) {
  try {
    const url = new URL(String(value));
    return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null;
  } catch (_error) { return null; }
}
export function image(source, onError) {
  // Eager: a lazy image that failed later would drop its banner mid-scroll.
  const element = document.createElement('img');
  element.alt = ''; element.decoding = 'async'; element.referrerPolicy = 'no-referrer';
  element.addEventListener('error', onError, { once: true });
  element.src = source;
  return element;
}
// Catalog art (`icon_url`, `banner_url`, `preview`) loads through vosu: release
// downloads often arrive as application/octet-stream, which browsers will not
// render as SVG. vosu fetches only URLs the current validated catalog lists,
// exactly as written there, and serves them with an image type.
export function catalogArt(value) {
  return httpUrl(value) ? `/api/catalog/art?url=${encodeURIComponent(String(value))}` : null;
}
// The proxied path or an absolute http(s) URL (an installed tool's own art).
function artSource(value) {
  return typeof value === 'string' && value.startsWith('/api/catalog/art?') ? value : httpUrl(value);
}
// Card art shared by tool and catalog cards (manifest `icon`/`banner`, catalog
// `icon_url`/`banner_url`). A missing or broken icon falls back to the
// initials tile; a missing or broken banner leaves the card without one.
export function cardIcon(name, url) {
  const frame = node('div', undefined, 'tool-card__icon'); frame.setAttribute('aria-hidden', 'true');
  const initials = () => frame.replaceChildren(String(name || '?').trim().slice(0, 2));
  const source = artSource(url);
  if (source) frame.append(image(source, initials)); else initials();
  return frame;
}
export function cardBanner(url) {
  const source = artSource(url);
  if (!source) return null;
  const frame = node('div', undefined, 'tool-card__banner'); frame.setAttribute('aria-hidden', 'true');
  frame.append(image(source, () => frame.remove()));
  return frame;
}

// A source string is its exact identity; one path segment, encoded once.
export function sourcePath(source, action) {
  return `/api/catalog/sources/${encodeURIComponent(source)}/${action}`;
}

// "Couldn't fetch N catalog sources · Retry", with each source's fixed,
// redacted reason in a disclosure instead of one long run-on line.
export function sourceFailures(sources, retry) {
  const failed = sources.filter((source) => source.status === 'failed');
  if (!failed.length) return null;
  const box = node('div', undefined, 'catalog-failures');
  box.setAttribute('role', 'status');
  const line = node('p', `Couldn't fetch ${failed.length} catalog ${failed.length === 1 ? 'source' : 'sources'}.`);
  if (retry) line.append(' ', button('Retry', retry, { className: 'link-button' }));
  const details = node('details');
  details.append(node('summary', 'Details'));
  const list = node('ul');
  for (const source of failed) {
    const item = node('li');
    item.append(node('code', source.source), `: ${source.message}`);
    list.append(item);
  }
  details.append(list);
  box.append(line, details);
  return box;
}

// The A6b.7 notice on every page: revoked in-process code and removed
// plugins keep running until vosu restarts; a stopped or failed catalog
// store is named too. Hidden while there is nothing to say.
export function mountCatalogStatus({ requestJson }) {
  const banner = node('div', undefined, 'catalog-restart');
  banner.id = 'catalog-restart';
  banner.setAttribute('role', 'status');
  banner.hidden = true;
  document.querySelector('#update-notice')?.after(banner);
  async function load() {
    let status;
    try { status = await requestJson('/api/catalog/status'); } catch (_error) { return; }
    const pending = [...new Set([...(status.pending_restart ?? []), ...(status.restart_required ?? [])])];
    const disabled = status.disabled ?? [];
    banner.replaceChildren();
    if (pending.length) {
      const line = node('p');
      line.append(node('strong', 'Restart vosu to finish: '),
        `${pending.join(', ')} ${pending.length === 1 ? 'is' : 'are'} still running in the vosu process. `
        + 'Native code cannot be unloaded safely, so its removal or revoked trust applies after a restart.');
      banner.append(line);
    }
    for (const reason of disabled) banner.append(node('p', `Catalog unavailable: ${reason}. Nothing was changed on disk.`));
    banner.hidden = !banner.childElementCount;
  }
  window.addEventListener('vosu:catalog-changed', load);
  load();
}
