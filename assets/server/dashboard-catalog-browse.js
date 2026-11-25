// Catalog v2 listings: tools and plugins on the Tools page, overlays on the
// Available page, game-support offers, and the installed catalog packages
// with uninstall and keep (promote). Every install goes through a reviewed
// plan (dashboard-catalog-flow.js); nothing here installs directly.
import { button, cardBanner, cardIcon, catalogArt, image, node, sourceFailures } from './dashboard-catalog.js';
import { installFlow, promote, reviewPlan, uninstallFlow } from './dashboard-catalog-flow.js';

const KIND_LABELS = { tool: 'Tool', plugin: 'Plugin', overlay: 'Overlay' };

function installable(entry) {
  if (!entry.host_supported) return 'Needs a different vosu version';
  if (!entry.target_supported) return 'Not available for this platform';
  if (entry.too_large) return 'Too large to install; raise the package limit in Settings';
  return null;
}

function sourceBadge(entry) {
  const badge = node('span', entry.official ? 'Official' : entry.source_trusted ? 'Trusted catalog' : 'User catalog', 'badge');
  badge.dataset.source = entry.official ? 'official' : 'user';
  badge.title = entry.source;
  return badge;
}

// The action an entry offers: Install, Update (another version from the
// same source), or nothing when it is installed as listed.
function offer(entry) {
  const installed = entry.installed;
  if (!installed) return 'install';
  if (installed.source !== entry.source) return null;
  return installed.version === entry.version ? null : 'update';
}

function startInstall(entry, control, context) {
  control.disabled = true;
  installFlow({ entry, ...context })
    .catch((error) => context.notify(`${entry.name || entry.id}: ${context.errorText(error)}`, 'error'))
    .finally(() => { if (control.isConnected) control.disabled = false; });
}

function entryCard(entry, context) {
  const titleId = `catalog-${entry.kind}-${entry.id}-${entry.source}`.replace(/[^A-Za-z0-9_-]/g, '-');
  const card = node('article', undefined, 'tool-card catalog-entry');
  card.dataset.catalogEntry = entry.id;
  card.dataset.catalogKind = entry.kind;
  card.dataset.catalogSource = entry.source;
  if (entry.kind === 'tool') card.dataset.catalogTool = entry.id;
  card.setAttribute('aria-labelledby', titleId);
  const banner = cardBanner(catalogArt(entry.banner_url));
  if (banner) card.append(banner);
  const top = node('div', undefined, 'tool-card__top');
  const head = node('div', undefined, 'tool-card__head');
  const title = node('h2'); title.id = titleId; title.title = `${entry.name} · ${entry.author ?? ''} · ${entry.source}`;
  title.append(node('span', entry.name, 'tool-card__name'), node('span', `v${entry.version}`, 'tool-card__version'));
  const chips = node('p', undefined, 'tool-card__status');
  chips.append(node('span', KIND_LABELS[entry.kind] ?? entry.kind, 'badge'), sourceBadge(entry));
  if (entry.kind === 'plugin') {
    chips.append(node('span', 'Native code', 'badge badge-warning'),
      node('span', entry.runtime === 'in-process' ? 'In-process' : 'Process', 'badge'));
  }
  if (entry.installed) chips.append(node('span', entry.installed.version === entry.version ? 'Installed' : `Installed v${entry.installed.version}`, 'badge'));
  head.append(title, chips);
  top.append(cardIcon(entry.name || entry.id, catalogArt(entry.icon_url)), head);
  card.append(top);
  if (entry.description) card.append(node('p', entry.description, 'tool-card__description'));
  const actions = node('div', undefined, 'tool-card__actions');
  const action = offer(entry);
  const blocked = installable(entry);
  if (action) {
    const label = action === 'update' ? 'Update' : 'Install';
    const control = button(`${label}…`, () => startInstall(entry, control, context), {
      className: action === 'update' ? 'button button-update' : 'button button-primary',
      disabled: Boolean(blocked),
      describe: `${label} ${entry.name} from ${entry.source}`,
    });
    actions.append(control);
  }
  if (blocked) actions.append(node('span', blocked, 'field-help'));
  card.append(actions);
  return card;
}

async function fetchEntries(requestJson, refresh) {
  if (refresh) await requestJson('/api/catalog/refresh', { method: 'POST' });
  const [entries, sources] = await Promise.all([
    requestJson('/api/catalog/entries'),
    requestJson('/api/catalog/sources'),
  ]);
  if (!entries.fetched && !refresh) return fetchEntries(requestJson, true);
  return { entries: entries.entries ?? [], sources: sources.sources ?? [] };
}

// Tools page: catalog tools and plugins, game offers and installed packages.
export function mountCatalogBrowser({ anchor, requestJson, notify, errorText, setBusy, rovingGrid = () => null, matches = () => true }) {
  const context = { requestJson, notify, errorText };
  const offers = node('section', undefined, 'catalog-offers');
  offers.setAttribute('aria-labelledby', 'catalog-offers-title');
  offers.hidden = true;
  const offersTitle = node('h2', 'Game support available'); offersTitle.id = 'catalog-offers-title';
  const offerList = node('ul', undefined, 'catalog-offers__list');
  offers.append(offersTitle, offerList);

  const refresh = button('Refresh catalog', () => load(true));
  const header = node('header', undefined, 'tools-catalog__header');
  const heading = node('h2', 'Available'); heading.id = 'tools-available-title';
  header.append(heading, refresh);
  const status = node('div', 'Fetching catalog sources…', 'tools-loading');
  const cards = node('div', undefined, 'tools-root');
  cards.setAttribute('aria-labelledby', 'tools-available-title');
  const available = node('section', undefined, 'tools-catalog');
  available.setAttribute('aria-labelledby', 'tools-available-title');
  available.append(header, status, cards);

  const installedTitle = node('h2', 'Installed from catalogs'); installedTitle.id = 'catalog-installed-title';
  const installedList = node('ul', undefined, 'catalog-installed__list');
  const installed = node('section', undefined, 'catalog-installed');
  installed.setAttribute('aria-labelledby', 'catalog-installed-title');
  installed.append(installedTitle, installedList);
  anchor.after(offers, available, installed);
  const grid = rovingGrid(cards, '.tool-card');

  let entries = [];
  let records = [];
  let query = '';
  function render() {
    const shown = entries.filter((entry) => entry.kind !== 'overlay' && matches(entry, query));
    cards.replaceChildren(...shown.map((entry) => entryCard(entry, context)));
    if (!shown.length) cards.append(node('p', entries.length ? 'No matching catalog tools or plugins.' : 'No catalog tools or plugins are available from the enabled sources.', 'tools-loading'));
    grid?.refresh();
  }
  function renderInstalled() {
    installed.hidden = !records.length;
    installedList.replaceChildren(...records.map((record) => {
      const item = node('li', undefined, 'catalog-installed__item');
      item.dataset.installed = record.id;
      const label = node('p');
      label.append(node('strong', record.id), ` ${KIND_LABELS[record.kind] ?? record.kind} v${record.version} `,
        node('span', record.installed_for === 'dependency' ? 'Dependency' : 'Requested', 'badge'));
      if (record.runtime) label.append(' ', node('span', record.approved ? 'Approved' : 'Not approved', 'badge'));
      const source = node('p', undefined, 'field-help'); source.append('From ', node('code', record.source));
      const actions = node('div', undefined, 'tool-card__actions');
      if (record.installed_for === 'dependency') {
        actions.append(button('Keep', () => promote({ target: record, ...context }), {
          describe: `Keep ${record.id} even when nothing needs it`,
        }));
      }
      actions.append(button('Uninstall…', () => uninstallFlow({ target: record, name: record.id, ...context }), {
        className: 'button button-danger', describe: `Uninstall ${record.id}`,
      }));
      item.append(label, source, actions);
      return item;
    }));
  }
  async function loadOffers() {
    let payload;
    try { payload = await requestJson('/api/catalog/offers'); } catch (_error) { return; }
    const list = payload.offers ?? [];
    offers.hidden = !list.length;
    offerList.replaceChildren(...list.map((entry) => {
      const item = node('li', undefined, 'catalog-offers__item');
      item.dataset.offer = entry.id;
      const text = node('p');
      text.append(node('strong', `Add support for ${entry.name}`), ` — ${entry.process} is running.`);
      if (entry.installed_match) text.append(' An installed plugin already handles it; this is an alternative.');
      const source = node('p', undefined, 'field-help'); source.append(`v${entry.version} from `, node('code', entry.source));
      const control = button('Review…', async () => {
        control.disabled = true;
        try {
          const plan = await requestJson(`/api/catalog/offers/${encodeURIComponent(entry.offer_id)}/accept`, { method: 'POST' });
          await reviewPlan({ plan, title: `Add support for ${entry.name}?`, ...context });
        } catch (error) { notify(errorText(error), 'error'); }
        finally { control.disabled = false; }
      }, { className: 'button button-primary', describe: `Review adding support for ${entry.name}` });
      item.append(text, source, control);
      return item;
    }));
  }
  async function loadInstalled() {
    try { records = (await requestJson('/api/catalog/installed')).packages ?? []; } catch (_error) { records = []; }
    renderInstalled();
  }
  async function load(force = false) {
    setBusy(refresh, true, 'Fetching…');
    try {
      const payload = await fetchEntries(requestJson, force);
      entries = payload.entries;
      status.replaceChildren(...[sourceFailures(payload.sources, () => load(true))].filter(Boolean));
      render();
    } catch (error) {
      const failure = node('p', `Couldn't load the catalog: ${errorText(error)}. Installed tools still work. `, 'catalog-failures');
      failure.append(button('Retry', () => load(true), { className: 'link-button' }));
      status.replaceChildren(failure);
    } finally { setBusy(refresh, false); }
    await Promise.all([loadInstalled(), loadOffers()]);
  }
  window.addEventListener('vosu:catalog-changed', () => load(false));
  load(false);
  return {
    filter(next) { query = next; render(); },
    record: (kind, id) => records.find((record) => record.kind === kind && record.id === id) ?? null,
    uninstall: (record, name) => uninstallFlow({ target: record, name, ...context }),
  };
}

// Available page: catalog overlays as the same cards as the community
// catalog's, official sources first. Returns place(tokens) to re-insert the
// matching cards after a search replaced the list.
export function mountCatalogOverlays({ root, requestJson, notify, errorText, setCount, initialTokens = [] }) {
  if (!root) return () => {};
  const context = { requestJson, notify, errorText };
  const status = node('div', undefined, 'catalog-sources-status');
  root.before(status);
  const baseCount = Number(document.querySelector('[data-count="available"]')?.textContent?.match(/\d+/)?.[0] ?? 0);
  let cards = [];
  let tokens = initialTokens;

  function card(overlay) {
    const article = node('article', undefined, 'counter-card catalog-card catalog-card--source');
    article.dataset.sourceOverlay = overlay.id;
    article.dataset.counterName = overlay.name;
    article.dataset.counterAuthor = overlay.author ?? '';
    article.setAttribute('aria-label', overlay.name);
    const gallery = node('div', undefined, 'gallery');
    const art = catalogArt(overlay.preview) ?? catalogArt(overlay.banner_url);
    if (art) {
      const slide = image(art, () => { gallery.className = 'gallery gallery--empty'; gallery.replaceChildren(node('span', 'No preview provided')); });
      slide.className = 'gallery__slide';
      slide.alt = `${overlay.name} preview`;
      gallery.append(slide);
    } else {
      gallery.classList.add('gallery--empty');
      gallery.append(node('span', 'No preview provided'));
    }
    const main = node('div', undefined, 'catalog-card__main');
    const titleRow = node('div', undefined, 'counter-card__title-row');
    const title = node('h2', overlay.name); title.title = overlay.name;
    titleRow.append(title, node('span', `v${overlay.version}`, 'counter-card__version'));
    main.append(titleRow);
    if (overlay.author) {
      const meta = node('p', undefined, 'counter-card__meta');
      meta.append(node('span', `by ${overlay.author}`));
      main.append(meta);
    }
    if (overlay.description) main.append(node('p', overlay.description, 'catalog-card__description'));
    if (Array.isArray(overlay.resolution)) {
      const facts = node('p', undefined, 'catalog-card__facts');
      const size = node('span');
      size.append(node('span', 'Size', 'fact-label'), ` ${overlay.resolution[0]} × ${overlay.resolution[1]}`);
      facts.append(size);
      main.append(facts);
    }
    const footer = node('div', undefined, 'catalog-card__footer');
    const tags = node('p', undefined, 'catalog-card__tags');
    tags.append(sourceBadge(overlay));
    const action = offer(overlay);
    if (overlay.installed && action === 'update') tags.append(node('span', 'Update available', 'badge badge-warning'));
    else if (overlay.installed) tags.append(node('span', 'Installed', 'badge'));
    const actions = node('div', undefined, 'counter-actions');
    const blocked = installable(overlay);
    if (action) {
      const label = action === 'update' ? 'Update' : 'Install';
      const control = button(`${label}…`, () => startInstall(overlay, control, context), {
        className: action === 'update' ? 'button button-update' : 'button button-primary',
        disabled: Boolean(blocked), describe: `${label} ${overlay.name}`,
      });
      actions.append(control);
    } else {
      const link = node('a', 'Open in Overlays', 'button'); link.href = '/';
      actions.append(link);
    }
    if (blocked) actions.append(node('span', blocked, 'field-help'));
    footer.append(tags, actions);
    main.append(footer);
    article.append(gallery, main);
    return article;
  }
  function matches(element) {
    const text = `${element.dataset.counterName} ${element.dataset.counterAuthor}`.toLowerCase().replace(/[^\p{Alphabetic}\p{N}\s]/gu, '');
    return tokens.every((token) => text.includes(token));
  }
  function place(nextTokens = tokens) {
    tokens = nextTokens;
    root.querySelectorAll(':scope > .catalog-card--source').forEach((element) => element.remove());
    const shown = cards.filter(matches);
    root.prepend(...shown);
    if (shown.length) root.querySelectorAll(':scope > .empty-state:not([role="alert"])').forEach((element) => element.remove());
  }
  async function load(force = false) {
    try {
      const payload = await fetchEntries(requestJson, force);
      const overlays = payload.entries.filter((entry) => entry.kind === 'overlay');
      overlays.sort((a, b) => Number(b.official) - Number(a.official));
      cards = overlays.map(card);
      status.replaceChildren(...[sourceFailures(payload.sources, () => load(true))].filter(Boolean));
      setCount?.(baseCount + overlays.filter((overlay) => !overlay.installed).length);
      place();
    } catch (error) {
      const failure = node('p', `Couldn't load catalog sources: ${errorText(error)} `, 'catalog-failures');
      failure.setAttribute('role', 'status');
      failure.append(button('Retry', () => load(true), { className: 'link-button' }));
      status.replaceChildren(failure);
    }
  }
  window.addEventListener('vosu:catalog-changed', () => load(false));
  load(false);
  return place;
}
