// D20 removed-route notices (docs/v2/adapter-surfaces.md A7.4, A7.5): what
// core's `DashboardStatus.removed_routes` reports, redrawn on each existing
// status refresh and never on a timer of its own. Every string from the
// server is set as text, never parsed as HTML. A known overlay is one core
// revalidated as installed at that read; an unknown bucket names nobody and
// offers no overlay action. Nothing here disables, reinstalls or retries an
// overlay or the removed request, and nothing is stored: the server's
// 24-hour expiry ends a notice, and a restart clears them all.
import { installFlow } from './dashboard-catalog-flow.js';

// Fixed at build time; built from no request data.
export const MIGRATION_GUIDE = 'https://github.com/18-lab-dev/vosu/blob/main/docs/v2/migration-from-1x.md#overlays-that-used-removed-apis';
// The `ingame-page` successor (A7.5): one exact catalog entry, installed only
// through the ordinary plan review and its approvals.
export const VOSULAY = Object.freeze({ kind: 'tool', id: 'vosulay-editor', source: '18-lab-dev/vosulay', name: 'vosulay editor' });

const FEATURES = {
  'selected-v2-feed': {
    title: 'Selected-field live feed',
    route: '/websocket/v2/subscribe',
    change: 'vosu 2.0 removed this WebSocket. Overlays now subscribe to the fields they need through API v1.',
  },
  'legacy-skin-index': {
    title: 'Skin folder listing',
    route: '/files/skin/',
    change: 'vosu 2.0 removed this listing. Overlays now read skin files through API v1 resources.',
  },
  'ingame-page': {
    title: 'In-game overlay page',
    route: '/api/ingame',
    change: 'vosu 2.0 removed this page. vosulay is the in-game overlay now.',
  },
};

function node(tag, text, className) {
  const element = document.createElement(tag);
  if (text !== undefined && text !== null) element.textContent = String(text);
  if (className) element.className = className;
  return element;
}

function button(label, run, className = 'button') {
  const element = node('button', label, className);
  element.type = 'button';
  element.addEventListener('click', run);
  return element;
}

function externalLink(label, href, className) {
  const link = node('a', label, className);
  link.href = href;
  link.target = '_blank';
  link.rel = 'noopener noreferrer';
  return link;
}

const ages = (value) => Number.isFinite(value) && value >= 0;

function setText(element, value) {
  if (element && element.textContent !== value) element.textContent = value;
}

export function formatAge(seconds) {
  if (!ages(seconds)) return 'at an unknown time';
  if (seconds < 10) return 'just now';
  if (seconds < 60) return `${Math.floor(seconds)} s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  return `${Math.floor(minutes / 60)} h ago`;
}

export function formatAttempts(count) {
  // Counts saturate on the server; past 2^53 the exact value is not shown.
  const shown = Number.isSafeInteger(count) ? count.toLocaleString('en') : 'Over 9 quadrillion';
  return `${shown} ${count === 1 ? 'request' : 'requests'}`;
}

function text(value) {
  return typeof value === 'string' && value.length > 0 ? value : null;
}

// Well-formed notices grouped by known feature, in a fixed order. Anything
// malformed is dropped rather than shown with invented values.
export function groupNotices(payload) {
  const groups = new Map(Object.keys(FEATURES).map((feature) => [feature, { feature, known: [], unknown: null }]));
  for (const notice of Array.isArray(payload) ? payload : []) {
    const group = groups.get(notice?.feature);
    if (!group || !Number.isFinite(notice.attempts) || notice.attempts < 1
      || !ages(notice.first_seen_seconds_ago) || !ages(notice.last_seen_seconds_ago)) continue;
    if (notice.overlay === null) {
      group.unknown = notice;
    } else if (text(notice.overlay?.id) && text(notice.overlay?.name)) {
      group.known.push(notice);
    }
  }
  for (const group of groups.values()) {
    group.known.sort((a, b) => a.overlay.name.localeCompare(b.overlay.name) || a.overlay.id.localeCompare(b.overlay.id));
  }
  return [...groups.values()].filter((group) => group.known.length || group.unknown);
}

function seen(notice) {
  const first = formatAge(notice.first_seen_seconds_ago);
  const last = formatAge(notice.last_seen_seconds_ago);
  return first === last ? last : `first ${first}, last ${last}`;
}

function knownRow(notice) {
  const row = node('li', undefined, 'removed-route__overlay');
  row.dataset.overlayId = notice.overlay.id;
  const link = node('a', notice.overlay.name, 'removed-route__name');
  link.href = `/settings?overlay=${encodeURIComponent(notice.overlay.id)}`;
  link.dataset.role = 'overlay-link';
  const state = node('span', 'Enabled · needs an update', 'badge badge-warning');
  const count = node('span', `${formatAttempts(notice.attempts)} · ${seen(notice)}`, 'removed-route__meta');
  count.dataset.role = 'attempts';
  row.append(link, ' ', state, ' ', count);
  return row;
}

function unknownRow(notice) {
  const row = node('p', undefined, 'removed-route__unknown');
  row.dataset.role = 'unknown';
  const count = node('span', `${formatAttempts(notice.attempts)} · ${seen(notice)}`);
  count.dataset.role = 'attempts';
  row.append(node('strong', 'Unidentified clients'), ': ', count,
    ". vosu can't tell which overlay or app sent them, so none is named here.");
  return row;
}

// What changes the notice's structure; counts and ages are updated in place
// so a refresh never moves focus or re-announces unchanged text.
function shape(groups) {
  return JSON.stringify(groups.map((group) => [group.feature, Boolean(group.unknown),
    group.known.map((notice) => [notice.overlay.id, notice.overlay.name])]));
}

// The `ingame-page` successor action. It reads the tool registry and the
// catalog only when asked (first shown, Retry, catalog changes), never on
// the status refresh.
function createSuccessorAction({ requestJson, notify, errorText, currentTheme, absoluteHttpUrl }) {
  const box = node('div', undefined, 'removed-route__action');
  box.dataset.role = 'ingame-action';
  box.setAttribute('aria-live', 'polite');
  let checking = null;

  function show(state, ...children) {
    box.dataset.state = state;
    box.replaceChildren(...children);
  }

  async function retry(control) {
    control.disabled = true;
    control.textContent = 'Checking…';
    try { await requestJson('/api/catalog/refresh', { method: 'POST' }); } catch (_error) { /* the check below reports it */ }
    await check();
  }

  function guidance(message) {
    const line = node('p', message);
    const again = button('Retry', () => retry(again), 'button');
    const settings = node('a', 'Catalog sources', 'link-button');
    settings.href = '/settings';
    const row = node('div', undefined, 'removed-route__buttons');
    row.append(again, settings);
    show('unavailable', line, row);
  }

  async function launch(control, view) {
    // Opened inside the click so popup blockers allow it; no opener access.
    const tab = window.open('', '_blank');
    if (tab) tab.opener = null;
    control.disabled = true;
    try {
      const { url } = await requestJson(`/api/tools/${encodeURIComponent(view.id)}/launch`, {
        method: 'POST', body: JSON.stringify({ theme: currentTheme() }),
      });
      const absolute = absoluteHttpUrl(url);
      if (!absolute) throw new Error('vosu returned an invalid tool address');
      if (tab) tab.location.replace(absolute);
      else window.open(absolute, '_blank', 'noopener,noreferrer');
    } catch (error) {
      tab?.close();
      notify(`Could not open ${view.name || view.id}: ${errorText(error)}`, 'error', 5000);
    } finally { control.disabled = false; }
  }

  function installed(view) {
    const manage = node('a', 'Manage', 'link-button');
    manage.href = '/tools';
    const row = node('div', undefined, 'removed-route__buttons');
    if (view.status === 'running') {
      const open = button(`Launch ${view.name || VOSULAY.name}`, () => launch(open, view), 'button button-primary');
      open.dataset.action = 'launch-vosulay';
      row.append(open, manage);
      show('installed', node('p', 'vosulay is installed. Use it for the in-game overlay.'), row);
    } else {
      row.append(manage);
      show('installed', node('p', 'vosulay is installed but not ready to launch. Check it on the Tools page.'), row);
    }
  }

  function available(entry) {
    const install = button('Install vosulay…', async () => {
      install.disabled = true;
      try {
        await installFlow({ entry: { ...VOSULAY, name: text(entry.name) ?? VOSULAY.name }, requestJson, notify, errorText });
      } catch (error) {
        notify(`Could not prepare the vosulay install: ${errorText(error)}`, 'error', 5000);
      } finally {
        install.disabled = false;
        check();
      }
    }, 'button button-primary');
    install.dataset.action = 'install-vosulay';
    const row = node('div', undefined, 'removed-route__buttons');
    row.append(install);
    show('available', node('p', `vosulay replaces the in-game page. Installing shows the plan from ${VOSULAY.source} and every approval it needs first.`), row);
  }

  async function run() {
    show('checking', node('p', 'Checking for vosulay…', 'field-help'));
    let tools;
    try { tools = await requestJson('/api/tools'); } catch (error) {
      guidance(`Couldn't check the installed tools: ${errorText(error)}.`);
      return;
    }
    const view = (Array.isArray(tools?.tools) ? tools.tools : []).find((candidate) => candidate?.id === VOSULAY.id);
    if (view) { installed(view); return; }
    let entries; let sources;
    try {
      [entries, sources] = await Promise.all([requestJson('/api/catalog/entries'), requestJson('/api/catalog/sources')]);
    } catch (error) {
      guidance(`The catalog is unavailable: ${errorText(error)}. Nothing was installed.`);
      return;
    }
    const source = (Array.isArray(sources?.sources) ? sources.sources : []).find((candidate) => candidate?.source === VOSULAY.source);
    const entry = (Array.isArray(entries?.entries) ? entries.entries : []).find((candidate) => candidate?.kind === VOSULAY.kind
      && candidate?.id === VOSULAY.id && candidate?.source === VOSULAY.source);
    if (entry) { available(entry); return; }
    if (!source) guidance(`The ${VOSULAY.source} catalog source isn't in your list. Add it under Catalog sources in Settings, then retry.`);
    else if (!source.enabled) guidance(`The ${VOSULAY.source} catalog source is turned off. Turn it on under Catalog sources in Settings, then retry.`);
    else if (source.status === 'failed') guidance(`Couldn't fetch the ${VOSULAY.source} catalog${text(source.message) ? `: ${source.message}` : ''}. Retry when you're online.`);
    else if (source.status === 'pending') guidance(`The ${VOSULAY.source} catalog hasn't been fetched yet. Retry to fetch it.`);
    else guidance(`The ${VOSULAY.source} catalog doesn't list ${VOSULAY.id} right now. Retry later.`);
  }

  function check() {
    checking ??= run().finally(() => { checking = null; });
    return checking;
  }

  return { element: box, check };
}

export function mountRemovedRouteNotices(options) {
  const region = node('section', undefined, 'removed-routes');
  region.id = 'removed-routes';
  region.setAttribute('aria-labelledby', 'removed-routes-title');
  region.hidden = true;
  const title = node('h2', 'Some overlays use features vosu 2.0 removed', 'removed-routes__title');
  title.id = 'removed-routes-title';
  const lede = node('p', 'Their requests get “Not Found”. Nothing was turned off: each overlay keeps running without the removed feature until it is updated.', 'removed-routes__lede');
  const list = node('div', undefined, 'removed-routes__list');
  region.append(title, lede, list);
  const anchor = document.querySelector('#catalog-restart') ?? document.querySelector('#update-notice');
  if (anchor) anchor.after(region); else document.querySelector('#main-content')?.before(region);

  const successor = createSuccessorAction(options);
  window.addEventListener('vosu:catalog-changed', () => { if (successor.element.isConnected) successor.check(); });

  function featureBlock(group) {
    const feature = FEATURES[group.feature];
    const block = node('article', undefined, 'removed-route');
    block.dataset.feature = group.feature;
    const heading = node('h3', feature.title, 'removed-route__title');
    heading.append(' ', node('code', feature.route));
    block.append(heading, node('p', feature.change, 'removed-route__change'));
    if (group.known.length) {
      block.append(node('p', group.known.length === 1 ? 'Overlay that still asks for it:' : 'Overlays that still ask for it:', 'removed-route__label'));
      const known = node('ul', undefined, 'removed-route__overlays');
      known.append(...group.known.map(knownRow));
      block.append(known);
    }
    if (group.unknown) block.append(unknownRow(group.unknown));
    if (group.feature === 'ingame-page') block.append(successor.element);
    const guide = externalLink('How to update an overlay that used it', MIGRATION_GUIDE, 'removed-route__guide');
    guide.dataset.role = 'guide';
    block.append(guide);
    return block;
  }

  let current = '';
  return function render(payload) {
    const groups = groupNotices(payload);
    const next = shape(groups);
    if (next === current) {
      for (const group of groups) {
        const block = list.querySelector(`[data-feature="${group.feature}"]`);
        for (const notice of group.known) {
          const row = [...block.querySelectorAll('[data-overlay-id]')].find((item) => item.dataset.overlayId === notice.overlay.id);
          setText(row?.querySelector('[data-role="attempts"]'), `${formatAttempts(notice.attempts)} · ${seen(notice)}`);
        }
        if (group.unknown) setText(block.querySelector('[data-role="unknown"] [data-role="attempts"]'), `${formatAttempts(group.unknown.attempts)} · ${seen(group.unknown)}`);
      }
      return;
    }
    current = next;
    const hadSuccessor = successor.element.isConnected;
    list.replaceChildren(...groups.map(featureBlock));
    region.hidden = groups.length === 0;
    if (successor.element.isConnected && !hadSuccessor) successor.check();
  };
}
