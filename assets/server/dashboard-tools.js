// Tools page: renders the 2.0 `GET /api/tools` ToolView and drives the
// enable, disable, trust and launch actions of the dashboard API in
// docs/tools/protocol.md. Tools are web-only: there is nothing to start, stop
// or read logs from.

import { cardBanner, cardIcon } from './dashboard-catalog.js';
import { mountCatalogBrowser } from './dashboard-catalog-browse.js';
import { mountPlugins } from './dashboard-plugins.js';

// `running` means servable: valid, enabled, trusted and every `requires`
// entry resolved.
const STATUS_LABELS = {
  running: 'Ready',
  blocked: 'Blocked',
  untrusted: 'Untrusted',
  disabled: 'Disabled',
  invalid: 'Invalid',
};
// system.css .v-dot states for each tool status.
const STATUS_DOTS = {
  running: 'ok',
  invalid: 'error',
  blocked: 'warn',
  untrusted: 'warn',
};
const SOURCE_LABELS = { dev: 'Dev', official: 'Official', 'official-fixture': 'Official fixture', user: 'User' };
function sourceLabel(view) {
  return view.fixture && view.source === 'user' ? 'Official fixture (inactive)' : SOURCE_LABELS[view.source] ?? view.source;
}
const BUSY_LABELS = {
  enable: 'Enabling…',
  disable: 'Disabling…',
  trust: 'Trusting…',
};
// `status_detail` of a package with only the 1.x `tool.toml`.
const LEGACY_DETAIL = 'needs-2.0-update';
const AUTHORING_DOCS = 'https://github.com/18-lab-dev/vosu/blob/main/docs/tools/protocol.md';
const POLL_MS = 3000;

function el(tag, { className, text, attrs } = {}, ...children) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  for (const [name, value] of Object.entries(attrs ?? {})) {
    if (value === false || value === null || value === undefined) continue;
    node.setAttribute(name, value === true ? '' : String(value));
  }
  node.append(...children.filter((child) => child !== null && child !== undefined && child !== false));
  return node;
}

function list(values) {
  return Array.isArray(values) ? values : [];
}

function statusOf(view) {
  return String(view.status ?? 'invalid');
}

function cardStatus(view) {
  const status = statusOf(view);
  return { label: STATUS_LABELS[status] ?? status, dot: STATUS_DOTS[status] ?? 'idle' };
}

export function mountTools({
  root,
  results,
  requestJson,
  notify,
  setBusy,
  errorText,
  absoluteHttpUrl,
  confirmAction,
  createActionDialog,
  showModal = (dialog) => dialog.showModal(),
  currentTheme,
  announce = () => {},
  rovingGrid = () => null,
  setCount,
  openToolSettings,
  openPluginSettings,
}) {
  if (!root) return null;
  // Cards are re-rendered by polling; a live region would re-announce them.
  // Action outcomes are announced through the toast instead.
  results?.removeAttribute('aria-live');

  const state = {
    tools: [],
    cards: new Map(),
    signatures: new Map(),
    pending: new Set(),
    loaded: false,
    query: '',
    timer: 0,
  };
  const banner = el('p', { className: 'tools-loading', text: 'Reading the tool registry…', attrs: { role: 'status' } });
  root.setAttribute('aria-busy', 'true');
  root.replaceChildren(banner);
  // Web tools are pages vosu serves to the browser, never native code; the
  // Plugins section above lists what vosu loads. Each has its own heading
  // so the two are never mistaken for each other.
  const installedHeader = el('header', { className: 'tools-catalog__header tools-installed__header' },
    el('h2', { text: 'Web tools', attrs: { id: 'tools-installed-title' } }),
    el('p', { className: 'plugins__intro', text: 'Pages vosu serves to your browser. They run in the browser tab you open, not inside vosu.' }));
  root.before(installedHeader);
  root.setAttribute('aria-labelledby', 'tools-installed-title');
  // Plugins come first: one needing approval is the page's first decision.
  mountPlugins({ before: installedHeader, requestJson, notify, setBusy, errorText, confirmAction, createActionDialog, openPluginSettings });

  const api = (id, suffix = '') => `/api/tools/${encodeURIComponent(id)}${suffix}`;
  // Catalog v2: tools and plugins to install through a reviewed plan, game
  // offers and catalog-installed packages (dashboard-catalog-browse.js).
  const catalog = mountCatalogBrowser({
    anchor: root, requestJson, notify, errorText, setBusy, rovingGrid,
    matches: (entry, query) => matches(entry, tokensOf(query)),
  });
  // A catalog install/uninstall changes the installed cards and the open
  // drawer's Uninstall action.
  window.addEventListener('vosu:catalog-changed', async () => { await refresh(); refreshOpenDetails(); });
  // One Tab stop per card grid; arrows move between cards.
  rovingGrid(root, '.tool-card');

  function tokensOf(query) {
    return String(query).toLocaleLowerCase().split(/\s+/).filter(Boolean);
  }

  function matches(view, tokens) {
    if (tokens.length === 0) return true;
    const text = [
      view.name, view.id, view.author, view.description, view.source, view.status, cardStatus(view).label,
    ].map((value) => String(value ?? '').toLocaleLowerCase()).join(' ');
    return tokens.every((token) => text.includes(token));
  }

  function idList(items, render, empty) {
    if (items.length === 0) return document.createTextNode(empty);
    return el('ul', {}, ...items.map(render));
  }

  function statusValue(view) {
    const status = statusOf(view);
    const value = el('span', {}, el('span', {
      className: 'tool-status',
      text: STATUS_LABELS[status] ?? status,
      attrs: { 'data-status': status },
    }));
    if (view.status_detail) value.append(el('span', { className: 'tool-detail', text: view.status_detail }));
    return value;
  }

  function dependency(requirement) {
    return el('li', {
      className: 'tool-dependency',
      attrs: { 'data-satisfied': String(Boolean(requirement.satisfied)) },
    }, el('code', { text: requirement.id }), requirement.version ? ` ${requirement.version}` : '',
    el('span', { className: 'tool-dependency__state', text: requirement.satisfied ? 'OK' : 'Unmet' }));
  }

  function actionButton(label, action, { primary = false, danger = false, describe } = {}) {
    return el('button', {
      className: `button${primary ? ' button-primary' : ''}${danger ? ' button-danger' : ''}`,
      text: label,
      attrs: { type: 'button', 'data-tool-action': action, 'aria-label': describe },
    });
  }

  // Cards carry identity, status and the primary actions; everything else
  // (description, facts, maintenance actions) lives in Details.
  function renderCard(view) {
    const name = view.name || view.id;
    const status = statusOf(view);
    const displayStatus = cardStatus(view);
    const card = el('article', {
      className: 'tool-card',
      attrs: { 'data-tool-id': view.id, 'data-status': status, 'aria-labelledby': `tool-title-${view.id}` },
    });
    // The name truncates on its own, so the version always stays visible.
    const title = el('h2', { attrs: { id: `tool-title-${view.id}`, title: name } }, el('span', { className: 'tool-card__name', text: name }),
      view.version ? el('span', { className: 'tool-card__version', text: `v${view.version}` }) : null);
    const line = el('p', { className: 'tool-card__status' },
      el('span', { className: 'v-dot', attrs: { 'data-state': displayStatus.dot } }),
      el('span', { text: displayStatus.label }));
    if (view.source && view.source !== 'official') {
      line.append(el('span', { className: 'badge', text: sourceLabel(view), attrs: { 'data-source': view.source } }));
    }
    // Core's catalog update source (ToolView `update_available`); the
    // update itself goes through the Available card's reviewed plan.
    if (view.update_available === true) {
      line.append(el('span', { className: 'badge badge-warning', text: 'Update available', attrs: { 'data-update': 'available' } }));
    }
    const banner = cardBanner(view.banner_url);
    if (banner) card.append(banner);
    card.append(el('div', { className: 'tool-card__top' }, cardIcon(name, view.icon_url),
      el('div', { className: 'tool-card__head' }, title, line)));

    const actions = el('div', { className: 'tool-card__actions' });
    // A 1.x package can only be updated to 2.0; vosu refuses every action.
    const legacy = view.status_detail === LEGACY_DETAIL;
    if (!view.trusted && !view.fixture && status !== 'invalid' && !legacy) {
      actions.append(actionButton('Trust…', 'trust', { primary: true, describe: `Trust ${name}` }));
    }
    // A disabled tool's next step is Enable. Open appears once the tool is
    // servable; a blocked tool's Details names the unmet requirement.
    const enable = !view.enabled && view.trusted && status !== 'invalid' && !legacy;
    if (enable) actions.append(actionButton('Enable', 'enable', { primary: true, describe: `Enable ${name}` }));
    if (view.ui_url && status === 'running') {
      actions.append(actionButton('Open', 'launch', { primary: true, describe: `Open ${name} in a new tab` }));
    }
    if (view.has_settings) {
      actions.append(actionButton('Settings', 'settings', { describe: `Settings for ${name}` }));
    }
    actions.append(actionButton('Details', 'details', { describe: `Details for ${name}` }));
    card.append(actions);
    return card;
  }

  function detailsContent(view) {
    const name = view.name || view.id;
    const status = statusOf(view);
    const homepage = absoluteHttpUrl(view.homepage);
    const facts = el('dl', { className: 'v-facts' });
    const add = (label, value) => { if (value !== null && value !== undefined) facts.append(el('dt', { text: label }), el('dd', {}, value)); };
    add('Status', statusValue(view));
    add('Version', view.version ? `v${view.version}` : null);
    add('Author', view.author || null);
    add('Source', sourceLabel(view) ?? null);
    add('Requires', idList(list(view.requires), dependency, 'Nothing'));
    if (list(view.optional).length) add('Uses if present', idList(list(view.optional), dependency, 'Nothing'));
    if (homepage) add('Homepage', el('a', { text: new URL(homepage).host, attrs: { href: homepage, target: '_blank', rel: 'noopener noreferrer' } }));

    const actions = el('div', { className: 'v-row' });
    if (status !== 'invalid' && view.trusted && view.enabled) {
      actions.append(actionButton('Disable', 'disable', { danger: true, describe: `Disable ${name}` }));
    }
    // Catalog-installed tools are uninstalled here, through a preview.
    if (catalog.record('tool', view.id)) {
      actions.append(actionButton('Uninstall…', 'catalog-uninstall', { danger: true, describe: `Uninstall ${name}` }));
    }
    return [
      view.description ? el('p', { className: 'tool-details__description', text: view.description }) : null,
      facts,
      actions.childElementCount ? actions : null,
    ].filter(Boolean);
  }

  function refreshOpenDetails() {
    const open = state.details;
    const current = open && state.tools.find((view) => view.id === open.id);
    if (current && open.dialog.open) open.body.replaceChildren(...detailsContent(current));
  }

  // One drawer, refreshed by polling while it is open.
  function openDetails(view) {
    state.details?.dialog.close();
    const titleId = `tool-details-${view.id}`;
    const dialog = el('dialog', { className: 'v-drawer tool-details', attrs: { 'aria-labelledby': titleId, 'data-tool-id': view.id } });
    const close = el('button', { className: 'v-btn v-btn--ghost v-btn--icon', text: '✕', attrs: { type: 'button', 'aria-label': 'Close details' } });
    const body = el('div', { className: 'v-drawer__body' }, ...detailsContent(view));
    dialog.append(el('header', { className: 'v-drawer__head' }, el('h3', { text: view.name || view.id, attrs: { id: titleId } }), close), body);
    close.addEventListener('click', () => dialog.close());
    dialog.addEventListener('click', handleAction);
    dialog.addEventListener('close', () => {
      dialog.remove();
      if (state.details?.dialog === dialog) state.details = null;
    }, { once: true });
    document.body.append(dialog);
    state.details = { dialog, body, id: view.id, signature: state.signatures.get(view.id) };
    showModal(dialog);
    close.focus();
  }

  function emptyState() {
    return el('div', { className: 'empty-state' },
      el('h2', { text: 'No tools yet' }),
      el('p', {}, 'Install one from Available, or run ', el('code', { text: 'vosu --tool <directory>' }), '.'),
      el('a', {
        className: 'button',
        text: 'Authoring guide',
        attrs: { href: AUTHORING_DOCS, target: '_blank', rel: 'noopener noreferrer' },
      }));
  }

  function errorState(error) {
    const retry = el('button', { className: 'button button-primary', text: 'Try again', attrs: { type: 'button' } });
    retry.addEventListener('click', () => {
      root.replaceChildren(banner);
      banner.textContent = 'Reading the tool registry…';
      refresh();
    });
    return el('div', { className: 'empty-state tool-error', attrs: { role: 'alert' } },
      el('h2', { text: 'Tools are unavailable' }),
      el('p', { text: `vosu did not return its tool list: ${errorText(error)}` }),
      retry);
  }

  function applyFilter() {
    const tokens = tokensOf(state.query);
    let visible = 0;
    for (const view of state.tools) {
      const card = state.cards.get(view.id);
      if (!card) continue;
      card.hidden = !matches(view, tokens);
      if (!card.hidden) visible += 1;
    }
    let empty = root.querySelector(':scope > .no-results');
    const showEmpty = state.tools.length > 0 && visible === 0;
    if (showEmpty && !empty) {
      empty = el('div', { className: 'no-results' },
        el('h2', { text: 'No matching tools' }),
        el('p', { text: 'Try fewer words, an author, or a status such as ready.' }));
      root.append(empty);
    } else if (!showEmpty) {
      empty?.remove();
    }
    if (tokens.length) announce(`${visible} of ${state.tools.length} tools match`);
  }

  // Replaces only cards whose view changed, keeping keyboard focus on the
  // same control when its card is rebuilt.
  function render() {
    if (state.tools.length === 0) {
      state.cards.clear();
      state.signatures.clear();
      state.details?.dialog.close();
      if (!root.querySelector(':scope > .empty-state')) root.replaceChildren(emptyState());
      return;
    }
    root.querySelector(':scope > .empty-state')?.remove();
    banner.remove();
    const seen = new Set();
    let previous = null;
    for (const view of state.tools) {
      seen.add(view.id);
      const signature = JSON.stringify([view, Boolean(catalog.record('tool', view.id))]);
      let card = state.cards.get(view.id);
      if (!card || (state.signatures.get(view.id) !== signature && !state.pending.has(view.id))) {
        const next = renderCard(view);
        if (card) {
          const focused = card.contains(document.activeElement)
            ? (document.activeElement.dataset.toolAction ?? document.activeElement.getAttribute('href'))
            : null;
          card.replaceWith(next);
          if (focused) {
            next.querySelector(`[data-tool-action="${CSS.escape(focused)}"], [href="${CSS.escape(focused)}"]`)?.focus({ preventScroll: true });
          }
        }
        card = next;
        state.cards.set(view.id, card);
        state.signatures.set(view.id, signature);
      }
      const expected = previous ? previous.nextElementSibling : root.firstElementChild;
      if (expected !== card) root.insertBefore(card, expected);
      previous = card;
    }
    for (const [id, card] of state.cards) {
      if (seen.has(id)) continue;
      card.remove();
      state.cards.delete(id);
      state.signatures.delete(id);
    }
    applyFilter();
    const open = state.details;
    const current = open && state.tools.find((view) => view.id === open.id);
    if (current && open.dialog.open && state.signatures.get(open.id) !== open.signature) {
      open.signature = state.signatures.get(open.id);
      open.body.replaceChildren(...detailsContent(current));
    } else if (open && !current) open.dialog.close();
  }

  async function refresh() {
    window.clearTimeout(state.timer);
    try {
      const payload = await requestJson('/api/tools');
      state.tools = list(payload.tools).filter((view) => view && typeof view.id === 'string');
      state.loaded = true;
      setCount(state.tools.length);
      render();
    } catch (error) {
      if (!state.loaded) {
        root.replaceChildren(errorState(error));
        root.setAttribute('aria-busy', 'false');
        return;
      }
      // Keep the last known cards; the next poll retries.
      banner.textContent = `Lost contact with vosu · retrying (${errorText(error)})`;
      if (!banner.isConnected) root.prepend(banner);
    } finally {
      if (state.loaded) root.setAttribute('aria-busy', 'false');
    }
    if (document.visibilityState === 'visible') state.timer = window.setTimeout(refresh, POLL_MS);
  }

  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible' && state.loaded) refresh();
    else window.clearTimeout(state.timer);
  });

  function withTheme(url) {
    const target = new URL(url);
    const fragment = new URLSearchParams(target.hash.slice(1));
    if (!fragment.has('theme')) {
      fragment.set('theme', currentTheme());
      target.hash = fragment.toString();
    }
    return target.href;
  }

  async function launch(button, view) {
    // The tab opens inside the click so popup blockers allow it; dropping its
    // opener before it navigates keeps the tool from scripting this dashboard.
    const tab = window.open('', '_blank');
    if (tab) tab.opener = null;
    setBusy(button, true, 'Opening…');
    try {
      const { url } = await requestJson(api(view.id, '/launch'), {
        method: 'POST',
        body: JSON.stringify({ theme: currentTheme() }),
      });
      const absolute = absoluteHttpUrl(url);
      if (!absolute) throw new Error('vosu returned an invalid tool address');
      const target = withTheme(absolute);
      if (tab) {
        tab.location.replace(target);
      } else if (!window.open(target, '_blank', 'noopener,noreferrer')) {
        notify(`Your browser blocked the new tab for ${view.name || view.id}.`, 'error', 0, {
          label: 'Open here',
          run: () => window.location.assign(target),
        });
      }
    } catch (error) {
      tab?.close();
      notify(`Could not open ${view.name || view.id}: ${errorText(error)}`, 'error', 5000);
    } finally {
      setBusy(button, false);
    }
  }

  async function confirmFor(view, action) {
    if (action !== 'trust') return true;
    const name = view.name || view.id;
    const declared = [...list(view.requires), ...list(view.optional)].map((requirement) => requirement.id);
    return confirmAction(
      `Trust ${name}?`,
      [
        `${name}${view.version ? ` ${view.version}` : ''}${view.author ? ` by ${view.author}` : ''}`,
        '',
        `vosu will serve ${name}'s pages, and a tab you open from this dashboard can read and run what it declares: ${declared.length ? declared.join(', ') : 'nothing beyond its own pages'}.`,
        '',
        'Trust tools only from authors you trust.',
      ].join('\n'),
      'Trust tool',
    );
  }

  async function changeState(button, view, action) {
    if (state.pending.has(view.id)) return;
    if (!await confirmFor(view, action)) return;
    state.pending.add(view.id);
    setBusy(button, true, BUSY_LABELS[action] ?? 'Working…');
    const name = view.name || view.id;
    try {
      await requestJson(api(view.id, `/${action}`), { method: 'POST' });
      const done = { enable: 'enabled', disable: 'disabled', trust: 'trusted' };
      notify(`${name} ${done[action] ?? 'updated'}.`, 'success');
    } catch (error) {
      notify(`Could not ${action} ${name}: ${errorText(error)}`, 'error', 5000);
    } finally {
      state.pending.delete(view.id);
      setBusy(button, false);
      await refresh();
    }
  }

  function handleAction(event) {
    const button = event.target.closest('[data-tool-action]');
    if (!button || button.disabled) return;
    const id = button.closest('[data-tool-id]')?.dataset.toolId;
    const view = state.tools.find((candidate) => candidate.id === id);
    if (!view) return;
    const action = button.dataset.toolAction;
    if (action === 'launch') launch(button, view);
    else if (action === 'details') openDetails(view);
    else if (action === 'settings') openToolSettings(view.id, view.name || view.id);
    else if (action === 'catalog-uninstall') {
      const record = catalog.record('tool', view.id);
      if (record) catalog.uninstall(record, view.name || view.id);
    } else if (action in BUSY_LABELS) changeState(button, view, action);
  }
  root.addEventListener('click', handleAction);

  refresh();

  return (query) => {
    state.query = query;
    applyFilter();
    catalog.filter(query);
  };
}
