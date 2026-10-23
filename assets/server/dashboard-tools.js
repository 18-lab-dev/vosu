// Tools page: renders `GET /api/tools` and drives the lifecycle, launch,
// data, log, and trust actions of the dashboard API in docs/tools/protocol.md.

import { cardBanner, cardIcon, sourceFailures } from './dashboard-catalog.js';

const STATUS_LABELS = {
  stopped: 'Stopped',
  starting: 'Starting',
  running: 'Running',
  failed: 'Failed',
  blocked: 'Blocked',
  untrusted: 'Untrusted',
  disabled: 'Disabled',
  invalid: 'Invalid',
};
// system.css .v-dot states for each tool status.
const STATUS_DOTS = {
  running: 'ok',
  starting: 'busy',
  failed: 'error',
  invalid: 'error',
  blocked: 'warn',
  untrusted: 'warn',
};
const SOURCE_LABELS = { dev: 'Dev', official: 'Official', user: 'User' };
// One chip per interface a tool provides, in this order.
const INTERFACE_LABELS = { ui: 'UI', service: 'Service', cli: 'CLI' };
const STARTUP_LABELS = { always: 'With vosu', 'on-demand': 'On demand' };
const BUSY_LABELS = {
  start: 'Starting…',
  stop: 'Stopping…',
  restart: 'Restarting…',
  enable: 'Enabling…',
  disable: 'Disabling…',
  trust: 'Trusting…',
  'open-data': 'Opening…',
  'clear-cache': 'Clearing…',
};
const AUTHORING_DOCS = 'https://github.com/shiruviaa/vosu/blob/main/docs/tools/protocol.md';
const POLL_MS = 3000;
const LOG_POLL_MS = 2000;
// Matches the host's bounded buffer, so the drawer never holds more than vosu.
const MAX_LOG_LINES = 1000;

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

function formatBytes(value) {
  if (!Number.isFinite(value) || value < 0) return 'Unknown';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const digits = unit === 0 || size >= 100 ? 0 : 1;
  return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: digits }).format(size)} ${units[unit]}`;
}

function parseTime(value) {
  if (typeof value === 'number' && Number.isFinite(value)) {
    // Seconds and milliseconds are both plausible epoch encodings.
    return new Date(value < 1e12 ? value * 1000 : value);
  }
  if (typeof value === 'string' && value) {
    const parsed = new Date(value);
    if (!Number.isNaN(parsed.getTime())) return parsed;
  }
  return null;
}

function list(values) {
  return Array.isArray(values) ? values : [];
}

function hasService(view) {
  return view.kind === 'service' || view.kind === 'hybrid';
}

// Installed views carry `kind` and `cli`; catalog entries list `interfaces`.
function interfacesOf(view) {
  if (Array.isArray(view.interfaces)) return view.interfaces;
  return [
    (view.kind === 'ui' || view.kind === 'hybrid') && 'ui',
    hasService(view) && 'service',
    view.cli && 'cli',
  ].filter(Boolean);
}

function interfaceChips(view) {
  return Object.keys(INTERFACE_LABELS).filter((key) => interfacesOf(view).includes(key)).map((key) => el('span', {
    className: 'badge',
    text: INTERFACE_LABELS[key],
    attrs: { 'data-interface': key, title: `Provides a ${key === 'ui' ? 'browser UI' : key === 'cli' ? 'command-line interface' : 'background service'}` },
  }));
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
  // Installed and Available share one plain heading style.
  root.before(el('header', { className: 'tools-catalog__header tools-installed__header' },
    el('h2', { text: 'Installed', attrs: { id: 'tools-installed-title' } })));
  root.setAttribute('aria-labelledby', 'tools-installed-title');

  const api = (id, suffix = '') => `/api/tools/${encodeURIComponent(id)}${suffix}`;
  const availableCards = el('div', { className: 'tools-root', attrs: { 'aria-labelledby': 'tools-available-title' } });
  const catalogStatus = el('div', { className: 'tools-loading', text: 'Fetching catalog sources…' });
  const catalogRefresh = el('button', { className: 'button', text: 'Refresh catalog', attrs: { type: 'button' } });
  const available = el('section', { className: 'tools-catalog', attrs: { 'aria-labelledby': 'tools-available-title' } },
    el('header', { className: 'tools-catalog__header' },
      el('h2', { text: 'Available', attrs: { id: 'tools-available-title' } }), catalogRefresh),
    catalogStatus, availableCards);
  root.after(available);
  let catalogTools = [];
  // One Tab stop per card grid; arrows move between cards.
  const installedGrid = rovingGrid(root, ':scope > .tool-card');
  const availableGrid = rovingGrid(availableCards, ':scope > .tool-card');

  // The catalog entry of an installed tool, for Update and Uninstall.
  function catalogEntry(id) {
    return catalogTools.find((tool) => tool.id === id && !tool.shadowed) ?? null;
  }

  // Only tools that are not installed, or have an update, are offered here;
  // installed tools are managed from their own card and its Details.
  function renderCatalog() {
    const offered = catalogTools.filter((tool) => {
      const installed = state.tools.find((entry) => entry.id === tool.id);
      return !installed || (tool.managed && tool.update_available);
    });
    const shown = offered.filter((tool) => matches(tool, tokensOf(state.query)));
    availableCards.replaceChildren(...shown.map((tool) => {
      const installed = state.tools.find((entry) => entry.id === tool.id);
      const update = Boolean(installed && tool.update_available);
      const official = ['shiruviaa/vosu-tools', 'shiruviaa/vosulay'].includes(tool.source);
      const titleId = `catalog-tool-${tool.id}`;
      const card = el('article', { className: 'tool-card', attrs: { 'data-catalog-tool': tool.id, 'aria-labelledby': titleId } },
        cardBanner(tool.banner_url),
        el('div', { className: 'tool-card__top' },
          cardIcon(tool.name || tool.id, tool.icon_url),
          el('div', { className: 'tool-card__head' },
            el('h2', { attrs: { id: titleId, title: `${tool.author} · ${tool.source}` } }, tool.name, el('span', { className: 'tool-card__version', text: `v${tool.version}` })),
            el('p', { className: 'tool-card__status' },
              ...interfaceChips(tool),
              tool.shadowed || !official ? el('span', { className: 'badge', text: tool.shadowed ? 'Shadowed' : 'User catalog' }) : null,
              update ? el('span', { className: 'badge badge-warning', text: 'Update available' }) : null))),
        tool.description ? el('p', { className: 'tool-card__description', text: tool.description }) : null);
      if (list(tool.requires).length) card.append(el('p', { className: 'tool-card__description', text: `Requires ${tool.requires.map((r) => `${r.id} ${r.version}`).join(', ')}` }));
      const actions = el('div', { className: 'tool-card__actions' });
      if (tool.shadowed) actions.append(el('span', { className: 'field-help', text: 'An earlier source provides this id.' }));
      else {
        const action = update ? 'update' : 'install';
        const button = actionButton(update ? 'Update' : 'Install', action, {
          primary: true, disabled: !tool.supported, describe: `${update ? 'Update' : 'Install'} ${tool.name}`,
        });
        button.addEventListener('click', () => catalogAction(button, tool, action));
        actions.append(button);
      }
      if (!tool.supported) actions.append(el('span', { className: 'field-help', text: 'Not available for this platform' }));
      card.append(actions);
      return card;
    }));
    if (!shown.length && catalogTools.length) {
      availableCards.append(el('p', { className: 'tools-loading', text: offered.length ? 'No matching tools.' : 'Every catalog tool is installed.' }));
    }
    availableGrid?.refresh();
  }

  async function fetchCatalog() {
    setBusy(catalogRefresh, true, 'Fetching…');
    try {
      const payload = await requestJson('/api/tools/catalog');
      catalogTools = list(payload.tools);
      catalogTools.push(...list(payload.shadowed).filter((entry) => entry.kind === 'tool'));
      // Sources are only worth a line when one failed.
      catalogStatus.replaceChildren(...[sourceFailures(list(payload.sources), fetchCatalog)].filter(Boolean));
      renderCatalog();
      refreshOpenDetails();
    } catch (error) {
      const failure = el('p', { className: 'catalog-failures', text: `Couldn't load the catalog: ${errorText(error)}. Installed tools still work. ` });
      const retry = el('button', { className: 'link-button', text: 'Retry', attrs: { type: 'button' } });
      retry.addEventListener('click', fetchCatalog);
      failure.append(retry);
      catalogStatus.replaceChildren(failure);
    } finally { setBusy(catalogRefresh, false); }
  }
  catalogRefresh.addEventListener('click', fetchCatalog);

  function chooseUninstall(button, tool) {
    const { dialog, body } = createActionDialog(`Uninstall ${tool.name}?`);
    body.append(el('p', { text: 'Keep or remove its saved data and cache?' }));
    const cancel = el('button', { className: 'button', text: 'Cancel', attrs: { type: 'button' } });
    cancel.addEventListener('click', () => dialog.close());
    const actions = el('div', { className: 'dialog-actions' }, cancel);
    for (const [label, removeData] of [['Uninstall, keep data', false], ['Uninstall, remove data', true]]) {
      const choice = el('button', { className: `button ${removeData ? 'button-danger button-danger--solid' : 'button-danger'}`, text: label, attrs: { type: 'button' } });
      choice.addEventListener('click', () => { dialog.close(); catalogAction(button, tool, 'uninstall', removeData); });
      actions.append(choice);
    }
    body.append(actions);
    cancel.focus();
  }

  async function catalogAction(button, tool, action, removeData) {
    if (action === 'uninstall' && removeData === undefined) { chooseUninstall(button, tool); return; }
    setBusy(button, true, 'Queued…');
    try {
      const response = await requestJson(api(tool.id, `/${action}`), { method: 'POST', body: JSON.stringify({ remove_data: removeData }) });
      for (;;) {
        const job = await requestJson(`/api/tools/catalog/jobs/${encodeURIComponent(response.job_id)}`);
        if (button.isConnected) button.textContent = `${job.phase}…`;
        if (job.status === 'completed') break;
        if (job.status === 'failed') throw new Error(job.error || 'Installation failed');
        await new Promise((resolve) => window.setTimeout(resolve, 350));
      }
      const done = { install: 'installed', update: 'updated', uninstall: 'uninstalled' };
      notify(`${tool.name} was ${done[action] ?? 'updated'}.`, 'success');
      await refresh();
      await fetchCatalog();
    } catch (error) { notify(`${tool.name}: ${errorText(error)}`, 'error'); }
    finally { setBusy(button, false); }
  }

  function tokensOf(query) {
    return String(query).toLocaleLowerCase().split(/\s+/).filter(Boolean);
  }

  function matches(view, tokens) {
    if (tokens.length === 0) return true;
    const text = [
      view.name, view.id, view.author, view.description, view.source, view.kind, view.status,
    ].map((value) => String(value ?? '').toLocaleLowerCase()).join(' ');
    return tokens.every((token) => text.includes(token));
  }

  function idList(items, render, empty) {
    if (items.length === 0) return document.createTextNode(empty);
    return el('ul', {}, ...items.map(render));
  }

  function statusValue(view) {
    const status = String(view.status ?? 'stopped');
    const value = el('span', {}, el('span', {
      className: 'tool-status',
      text: STATUS_LABELS[status] ?? status,
      attrs: { 'data-status': status },
    }));
    const process = view.process;
    const notes = [];
    if (process?.pid) notes.push(`PID ${process.pid}`);
    const started = parseTime(process?.started_at);
    if (started) notes.push(`since ${started.toLocaleString(undefined, { dateStyle: 'short', timeStyle: 'short' })}`);
    if (Number.isInteger(process?.restarts) && process.restarts > 0) {
      notes.push(`${process.restarts} restart${process.restarts === 1 ? '' : 's'}`);
    }
    if (notes.length) value.append(el('span', { className: 'tool-detail', text: notes.join(' · ') }));
    if (view.status_detail) value.append(el('span', { className: 'tool-detail', text: view.status_detail }));
    return value;
  }

  function actionButton(label, action, { primary = false, danger = false, update = false, disabled = false, describe } = {}) {
    const button = el('button', {
      className: `button${primary ? ' button-primary' : ''}${danger ? ' button-danger' : ''}${update ? ' button-update' : ''}`,
      text: label,
      attrs: { type: 'button', 'data-tool-action': action, 'aria-label': describe },
    });
    button.disabled = disabled;
    return button;
  }

  // Cards carry identity, status and the primary actions; everything else
  // (description, facts, routes, maintenance actions) lives in Details.
  function renderCard(view) {
    const name = view.name || view.id;
    const status = String(view.status ?? 'stopped');
    const card = el('article', {
      className: 'tool-card',
      attrs: { 'data-tool-id': view.id, 'data-status': status, 'aria-labelledby': `tool-title-${view.id}` },
    });
    const title = el('h2', { attrs: { id: `tool-title-${view.id}`, title: view.description || undefined } }, name,
      view.version ? el('span', { className: 'tool-card__version', text: `v${view.version}` }) : null);
    const line = el('p', { className: 'tool-card__status' },
      el('span', { className: 'v-dot', attrs: { 'data-state': STATUS_DOTS[status] ?? 'idle' } }),
      el('span', { text: STATUS_LABELS[status] ?? status }),
      ...interfaceChips(view));
    if (view.source && view.source !== 'official') {
      line.append(el('span', { className: 'badge', text: SOURCE_LABELS[view.source] ?? view.source, attrs: { 'data-source': view.source } }));
    }
    const banner = cardBanner(view.banner_url);
    if (banner) card.append(banner);
    card.append(el('div', { className: 'tool-card__top' }, cardIcon(name, view.icon_url),
      el('div', { className: 'tool-card__head' }, title, line)));

    const actions = el('div', { className: 'tool-card__actions' });
    const usable = view.trusted && view.enabled && !['invalid', 'untrusted', 'disabled'].includes(status);
    if (!view.trusted && status !== 'invalid') {
      actions.append(actionButton('Trust…', 'trust', { primary: true, describe: `Trust ${name}` }));
    }
    // A disabled tool's next step is Enable; Open returns once it is usable.
    const enable = !view.enabled && view.trusted && status !== 'invalid';
    if (enable) actions.append(actionButton('Enable', 'enable', { primary: true, describe: `Enable ${name}` }));
    if (view.ui_url && !enable) {
      actions.append(actionButton('Open', 'launch', {
        primary: view.trusted, disabled: !usable, describe: `Open ${name} in a new tab`,
      }));
    }
    if (hasService(view) && usable) {
      const running = status === 'running' || status === 'starting';
      actions.append(running
        ? actionButton('Stop', 'stop', { describe: `Stop ${name}` })
        : actionButton('Start', 'start', { describe: `Start ${name}` }));
    }
    const entry = catalogEntry(view.id);
    if (entry?.managed && entry.update_available) {
      actions.append(actionButton('Update', 'catalog-update', { update: true, describe: `Update ${name} to v${entry.version}` }));
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
    const status = String(view.status ?? 'stopped');
    const requires = list(view.requires);
    const requiredBy = list(view.required_by);
    const permissions = list(view.permissions?.host);
    const data = view.data ?? {};
    const apiRead = list(view.api?.read);
    const apiWrite = list(view.api?.write);
    const homepage = absoluteHttpUrl(view.homepage);
    const facts = el('dl', { className: 'v-facts' });
    const add = (label, value) => { if (value !== null && value !== undefined) facts.append(el('dt', { text: label }), el('dd', {}, value)); };
    add('Status', statusValue(view));
    add('Version', view.version ? `v${view.version}` : null);
    add('Author', view.author || null);
    add('Source', SOURCE_LABELS[view.source] ?? view.source ?? null);
    add('Startup', hasService(view) ? (STARTUP_LABELS[view.startup] ?? view.startup ?? '—') : 'No service');
    add('Storage', `${formatBytes(Number(data.data_bytes))} data · ${formatBytes(Number(data.cache_bytes))} cache`);
    add('Requires', idList(requires, (dependency) => el('li', {
      className: 'tool-dependency',
      attrs: { 'data-satisfied': String(Boolean(dependency.satisfied)) },
    }, el('code', { text: dependency.id }), dependency.version ? ` ${dependency.version}` : '',
    el('span', { className: 'tool-dependency__state', text: dependency.satisfied ? 'OK' : 'Unmet' })), 'Nothing'));
    add('Required by', idList(requiredBy, (id) => el('li', {}, el('code', { text: id })), 'No tools'));
    if (homepage) add('Homepage', el('a', { text: new URL(homepage).host, attrs: { href: homepage, target: '_blank', rel: 'noopener noreferrer' } }));

    // Permission ids, routes and the command line matter to tool authors,
    // not to everyday use.
    const developer = el('dl', { className: 'v-facts' });
    const addDeveloper = (label, value) => developer.append(el('dt', { text: label }), el('dd', {}, value));
    addDeveloper('Permissions', idList(permissions, (permission) => el('li', {}, el('code', { text: permission })), 'None'));
    if (apiRead.length || apiWrite.length) {
      addDeveloper('API routes', el('ul', {},
        ...apiRead.map((route) => el('li', {}, 'read ', el('code', { text: route }))),
        ...apiWrite.map((route) => el('li', {}, 'write ', el('code', { text: route })))));
    }
    if (view.cli) addDeveloper('Command', el('code', { text: view.cli.usage }));
    if (view.path) addDeveloper('Folder', el('code', { text: view.path }));
    const developerDetails = el('details', { className: 'tool-details__developer' }, el('summary', { text: 'Developer details' }), developer);

    const actions = el('div', { className: 'v-row' });
    const usable = view.trusted && view.enabled && !['invalid', 'untrusted', 'disabled'].includes(status);
    if (hasService(view) && usable && (status === 'running' || status === 'starting')) {
      actions.append(actionButton('Restart', 'restart', { describe: `Restart ${name}` }));
    }
    actions.append(
      actionButton('Data', 'data', { describe: `Data folders for ${name}` }),
      actionButton('Logs', 'logs', { describe: `Logs for ${name}` }));
    const entry = catalogEntry(view.id);
    if (entry?.managed && entry.update_available) {
      actions.append(actionButton('Update', 'catalog-update', { update: true, describe: `Update ${name} to v${entry.version}` }));
    }
    if (status !== 'invalid' && view.trusted && view.enabled) {
      actions.append(actionButton('Disable', 'disable', { danger: true, describe: `Disable ${name}` }));
    }
    // Catalog-installed tools are uninstalled here, not from Available.
    if (entry?.managed) {
      actions.append(actionButton('Uninstall…', 'catalog-uninstall', { danger: true, describe: `Uninstall ${name}` }));
    }
    return [
      view.description ? el('p', { className: 'tool-details__description', text: view.description }) : null,
      facts,
      developerDetails,
      actions,
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
        el('p', { text: 'Try fewer words, an author, or a status such as running.' }));
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
      if (!root.querySelector(':scope > .empty-state')) root.replaceChildren(emptyState());
      return;
    }
    root.querySelector(':scope > .empty-state')?.remove();
    banner.remove();
    const seen = new Set();
    let previous = null;
    for (const view of state.tools) {
      seen.add(view.id);
      const signature = JSON.stringify(view);
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
    const name = view.name || view.id;
    if (action === 'trust') {
      const permissions = list(view.permissions?.host);
      return confirmAction(
        `Trust ${name}?`,
        [
          `${name}${view.version ? ` ${view.version}` : ''}${view.author ? ` by ${view.author}` : ''}`,
          view.path ? `Folder: ${view.path}` : '',
          '',
          `${name} runs as your OS user, with the same access to your files, network, and accounts as any program you start yourself.`,
          `Its permissions (${permissions.length ? permissions.join(', ') : 'none declared'}) only limit what vosu's host API returns to it. They are not a sandbox.`,
          '',
          'Trust tools only from authors you trust.',
        ].filter((line, index, lines) => line !== '' || (index > 0 && lines[index - 1] !== '')).join('\n'),
        'Trust tool',
      );
    }
    const dependents = list(view.required_by);
    if ((action === 'disable' || action === 'stop') && dependents.length) {
      return confirmAction(
        `${action === 'disable' ? 'Disable' : 'Stop'} ${name}?`,
        `${dependents.join(', ')} ${dependents.length === 1 ? 'depends' : 'depend'} on ${name} and will be blocked until it runs again.`,
        action === 'disable' ? 'Disable' : 'Stop',
        { danger: true },
      );
    }
    return true;
  }

  async function lifecycle(button, view, action) {
    if (state.pending.has(view.id)) return;
    if (!await confirmFor(view, action)) return;
    state.pending.add(view.id);
    setBusy(button, true, BUSY_LABELS[action] ?? 'Working…');
    const name = view.name || view.id;
    try {
      await requestJson(api(view.id, `/${action}`), { method: 'POST' });
      const done = {
        start: 'started', stop: 'stopped', restart: 'restarted', enable: 'enabled', disable: 'disabled', trust: 'trusted',
      };
      notify(`${name} ${done[action] ?? 'updated'}.`, 'success');
    } catch (error) {
      notify(`Could not ${action} ${name}: ${errorText(error)}`, 'error', 5000);
    } finally {
      state.pending.delete(view.id);
      setBusy(button, false);
      await refresh();
    }
  }

  function openData(view) {
    const name = view.name || view.id;
    const { dialog, body } = createActionDialog(`${name} data`);
    // Folder paths and sizes only; what each folder is for is in the docs.
    const rows = el('dl', { className: 'tool-data' });
    const help = el('p', { className: 'field-help' });
    const openFolder = el('button', { className: 'button', text: 'Open data folder', attrs: { type: 'button' } });
    const clear = el('button', { className: 'button button-danger', text: 'Clear cache', attrs: { type: 'button' } });
    const close = el('button', { className: 'button button-primary', text: 'Done', attrs: { type: 'button' } });
    const status = el('p', { attrs: { role: 'status' } });
    let current = view;

    function paint() {
      const data = current.data ?? {};
      rows.replaceChildren(
        el('div', {}, el('dt', { text: 'Data' }), el('dd', {}, el('code', { text: data.data_dir ?? 'Unknown' })),
          el('dd', { className: 'tool-data__size', text: formatBytes(Number(data.data_bytes)) })),
        el('div', {}, el('dt', { text: 'Cache' }), el('dd', {}, el('code', { text: data.cache_dir ?? 'Unknown' })),
          el('dd', { className: 'tool-data__size', text: formatBytes(Number(data.cache_bytes)) })),
      );
      // The protocol lets the dashboard clear a cache only while nothing uses
      // it, and an empty cache has nothing to clear.
      const busy = Boolean(current.process?.pid);
      const empty = Number(data.cache_bytes) === 0;
      clear.disabled = busy || empty;
      help.textContent = busy ? `Stop ${name} to clear its cache.` : '';
      help.hidden = !busy;
    }

    async function reload() {
      try {
        current = await requestJson(api(view.id));
        paint();
      } catch (_error) {
        // The sizes shown remain the last known ones.
      }
    }

    async function post(button, action, success) {
      setBusy(button, true, BUSY_LABELS[action]);
      try {
        await requestJson(api(view.id, `/${action}`), { method: 'POST' });
        status.textContent = success;
        await reload();
        refresh();
      } catch (error) {
        status.textContent = `${errorText(error)}`;
      } finally {
        setBusy(button, false);
        paint();
      }
    }

    openFolder.addEventListener('click', () => post(openFolder, 'open-data', 'Opened the data folder.'));
    clear.addEventListener('click', async () => {
      const size = formatBytes(Number(current.data?.cache_bytes));
      if (!await confirmAction(`Clear the ${name} cache?`, `${current.data?.cache_dir ?? ''}\n${size} will be deleted.`, 'Clear cache', { danger: true })) return;
      post(clear, 'clear-cache', 'Cache cleared.');
    });
    close.addEventListener('click', () => dialog.close());
    paint();
    body.append(rows, help, status, el('div', { className: 'dialog-actions' }, openFolder, clear, close));
    close.focus();
    reload();
  }

  function openLogs(view) {
    const name = view.name || view.id;
    const titleId = `logs-title-${view.id}`;
    const dialog = el('dialog', { className: 'dashboard-dialog logs-drawer', attrs: { 'aria-labelledby': titleId } });
    const closeButton = el('button', { className: 'icon-button', text: '✕', attrs: { type: 'button', 'aria-label': 'Close logs' } });
    const follow = el('input', { className: 'v-toggle', attrs: { type: 'checkbox', role: 'switch', checked: true } });
    const copy = el('button', { className: 'button', text: 'Copy', attrs: { type: 'button' } });
    const status = el('p', { attrs: { role: 'status' } });
    // A log region would read every new line aloud; users read it on demand.
    const output = el('div', {
      className: 'logs-output',
      attrs: { role: 'log', 'aria-live': 'off', tabindex: '0', 'aria-label': `${name} log output` },
    });
    const empty = el('p', { className: 'logs-empty', text: 'No output yet.' });
    output.append(empty);
    dialog.append(el('div', { className: 'dialog-frame' },
      el('header', { className: 'dialog-header' },
        el('h2', { text: `${name} logs`, attrs: { id: titleId } }),
        closeButton),
      el('div', { className: 'logs-toolbar' }, el('label', { className: 'v-check' }, follow, 'Follow output'), copy, status),
      output));

    let after = 0;
    let timer = 0;
    let open = true;
    let count = 0;

    function line(entry) {
      const time = parseTime(entry.ts);
      const stream = String(entry.stream ?? 'stdout');
      return el('div', { className: 'log-line', attrs: { 'data-stream': stream } },
        el('span', { className: 'log-line__time', text: time ? time.toLocaleTimeString(undefined, { hour12: false }) : String(entry.ts ?? '') }),
        el('span', { className: 'log-line__stream', text: stream }),
        el('span', { className: 'log-line__text', text: String(entry.text ?? '') }));
    }

    async function poll() {
      if (!open) return;
      try {
        const payload = await requestJson(`${api(view.id, '/logs')}?after=${encodeURIComponent(after)}`);
        const lines = list(payload.lines);
        if (lines.length) {
          empty.remove();
          const fragment = document.createDocumentFragment();
          for (const entry of lines) {
            fragment.append(line(entry));
            if (Number.isFinite(entry.seq)) after = Math.max(after, entry.seq);
          }
          output.append(fragment);
          while (output.childElementCount > MAX_LOG_LINES) output.firstElementChild.remove();
          count = output.childElementCount;
          if (follow.checked) output.scrollTop = output.scrollHeight;
        }
        status.textContent = `${follow.checked ? 'Live' : 'Paused'} · ${count} line${count === 1 ? '' : 's'}`;
      } catch (error) {
        status.textContent = `Retrying · ${errorText(error)}`;
      }
      if (open) timer = window.setTimeout(poll, LOG_POLL_MS);
    }

    follow.addEventListener('change', () => {
      status.textContent = `${follow.checked ? 'Live' : 'Paused'} · ${count} line${count === 1 ? '' : 's'}`;
      if (follow.checked) output.scrollTop = output.scrollHeight;
    });
    // Scrolling back through history pauses following until the user returns
    // to the end.
    output.addEventListener('scroll', () => {
      const atEnd = output.scrollHeight - output.scrollTop - output.clientHeight < 8;
      if (follow.checked !== atEnd && count > 0) {
        follow.checked = atEnd;
        status.textContent = `${atEnd ? 'Live' : 'Paused'} · ${count} line${count === 1 ? '' : 's'}`;
      }
    }, { passive: true });
    copy.addEventListener('click', async () => {
      const text = [...output.querySelectorAll('.log-line')]
        .map((row) => [...row.children].map((cell) => cell.textContent).join('  ')).join('\n');
      try {
        await navigator.clipboard.writeText(text);
        notify('Logs copied.', 'success', 2600);
      } catch (error) {
        notify(`Could not copy logs: ${errorText(error)}`, 'error');
      }
    });
    closeButton.addEventListener('click', () => dialog.close());
    dialog.addEventListener('close', () => {
      open = false;
      window.clearTimeout(timer);
      dialog.remove();
    }, { once: true });
    document.body.append(dialog);
    showModal(dialog);
    output.focus();
    poll();
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
    else if (action === 'data') openData(view);
    else if (action === 'settings') openToolSettings(view.id, view.name || view.id);
    else if (action === 'logs') openLogs(view);
    else if (action === 'catalog-update' || action === 'catalog-uninstall') {
      const entry = catalogEntry(view.id);
      if (entry) catalogAction(button, entry, action === 'catalog-update' ? 'update' : 'uninstall');
    } else lifecycle(button, view, action);
  }
  root.addEventListener('click', handleAction);

  refresh();
  fetchCatalog();

  return (query) => {
    state.query = query;
    applyFilter();
    renderCatalog();
  };
}
