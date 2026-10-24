function node(tag, text, className) {
  const element = document.createElement(tag);
  if (text !== undefined) element.textContent = text;
  if (className) element.className = className;
  return element;
}
function button(label, run, { className = 'button', disabled = false, describe } = {}) {
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
function image(source, onError) {
  // Eager: a lazy image that failed later would drop its banner mid-scroll.
  const element = document.createElement('img');
  element.alt = ''; element.decoding = 'async'; element.referrerPolicy = 'no-referrer';
  element.addEventListener('error', onError, { once: true });
  element.src = source;
  return element;
}
// Card art shared by tool and catalog cards (manifest `icon`/`banner`, catalog
// `icon_url`/`banner_url`). A missing or broken icon falls back to the
// initials tile; a missing or broken banner leaves the card without one.
export function cardIcon(name, url) {
  const frame = node('div', undefined, 'tool-card__icon'); frame.setAttribute('aria-hidden', 'true');
  const initials = () => frame.replaceChildren(String(name || '?').trim().slice(0, 2));
  const source = httpUrl(url);
  if (source) frame.append(image(source, initials)); else initials();
  return frame;
}
export function cardBanner(url) {
  const source = httpUrl(url);
  if (!source) return null;
  const frame = node('div', undefined, 'tool-card__banner'); frame.setAttribute('aria-hidden', 'true');
  frame.append(image(source, () => frame.remove()));
  return frame;
}

const relative = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
// "2 minutes ago" for a fetch time; the exact time stays in the title.
export function timeAgo(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return '';
  const seconds = Math.round((date.getTime() - Date.now()) / 1000);
  for (const [unit, size] of [['day', 86400], ['hour', 3600], ['minute', 60]]) {
    if (Math.abs(seconds) >= size) return relative.format(Math.round(seconds / size), unit);
  }
  return relative.format(seconds, 'second');
}

// "Couldn't reach N catalog sources · Retry", with each source's raw error
// in a disclosure instead of one long run-on line.
export function sourceFailures(sources, retry) {
  const failed = sources.filter((source) => source.last_error);
  if (!failed.length) return null;
  const box = node('div', undefined, 'catalog-failures');
  box.setAttribute('role', 'status');
  const line = node('p', `Couldn't reach ${failed.length} catalog ${failed.length === 1 ? 'source' : 'sources'}.`);
  if (retry) line.append(' ', button('Retry', retry, { className: 'link-button' }));
  const details = node('details');
  details.append(node('summary', 'Details'));
  const list = node('ul');
  for (const source of failed) {
    const item = node('li');
    item.append(node('code', source.source), `: ${source.last_error}`);
    list.append(item);
  }
  details.append(list);
  box.append(line, details);
  return box;
}

// Catalog sources live in the runtime settings form as one more settings
// group, so the section index lists them and the layout never overlaps them.
export function mountCatalogSources({ root, requestJson, notify, errorText, setBusy }) {
  if (!root) return;
  const section = node('fieldset', undefined, 'settings-group catalog-sources');
  section.id = 'catalog-sources';
  section.dataset.title = 'Catalog sources';
  section.append(node('legend', 'Catalog sources'));
  const intro = node('p', 'Earlier sources win duplicate ids. Tools from 18-lab-dev/vosu-tools and 18-lab-dev/vosulay are trusted automatically; all others need approval.', 'settings-item__description catalog-sources__intro');
  const rows = node('div', undefined, 'catalog-source-list');
  const status = node('p', '', 'field-help'); status.setAttribute('role', 'status');
  const failures = node('div', undefined, 'catalog-sources__failures');
  let sources = [];
  let saved = '[]';
  const elements = new Map();
  const snapshot = () => JSON.stringify(sources.map(({ source, enabled }) => ({ source, enabled })));
  const dirty = () => snapshot() !== saved;
  // The form's save summary reads this flag, so it never says "All changes
  // saved" while sources are edited but not saved.
  function markDirty() {
    const unsaved = dirty();
    status.textContent = unsaved ? 'Unsaved changes' : '';
    if (section.hasAttribute('data-unsaved') !== unsaved) {
      section.toggleAttribute('data-unsaved', unsaved);
      section.dispatchEvent(new CustomEvent('vosu:unsaved', { bubbles: true }));
    }
  }
  // Unsaved source edits ask before the page is left.
  window.addEventListener('beforeunload', (event) => { if (dirty()) event.preventDefault(); });

  function stateLine(source) {
    if (source.last_error) return `Couldn't fetch: ${source.last_error}`;
    if (source.status === 'ok' || source.fetched_at) {
      const ago = timeAgo(source.fetched_at);
      return ago ? `Fetched ${ago}` : 'Fetched';
    }
    if (!source.enabled) return 'Disabled';
    return 'Not checked yet';
  }

  // Rows are kept per source object and moved, not rebuilt, so focus stays
  // on the control the user pressed.
  function rowFor(source) {
    let entry = elements.get(source);
    if (entry) return entry;
    const row = node('div', undefined, 'catalog-source-row');
    const enabled = node('input', undefined, 'v-toggle'); enabled.type = 'checkbox'; enabled.setAttribute('role', 'switch');
    enabled.addEventListener('change', () => { source.enabled = enabled.checked; markDirty(); });
    const input = node('input', undefined, 'settings-control settings-control--mono'); input.type = 'text';
    input.placeholder = 'owner/repo@tag or https://…/catalog.json'; input.spellcheck = false;
    input.addEventListener('input', () => { source.source = input.value.trim(); markDirty(); });
    const up = button('Move up', () => move(source, -1), { className: 'button button--sm' });
    const down = button('Move down', () => move(source, 1), { className: 'button button--sm' });
    const remove = button('Remove', () => removeSource(source), { className: 'button button--sm button-danger' });
    const actions = node('div', undefined, 'catalog-source-row__actions'); actions.append(up, down, remove);
    const detail = node('p', '', 'catalog-source-row__state');
    row.append(enabled, input, actions, detail);
    entry = { row, enabled, input, up, down, remove, detail };
    elements.set(source, entry);
    return entry;
  }
  function render() {
    for (const source of [...elements.keys()]) if (!sources.includes(source)) elements.delete(source);
    sources.forEach((source, index) => {
      const entry = rowFor(source);
      const label = source.source || `source ${index + 1}`;
      entry.enabled.checked = source.enabled;
      entry.enabled.setAttribute('aria-label', `Enable ${label}`);
      if (entry.input.value !== source.source) entry.input.value = source.source;
      entry.input.setAttribute('aria-label', `Catalog source ${index + 1}`);
      entry.up.disabled = index === 0;
      entry.down.disabled = index === sources.length - 1;
      entry.up.setAttribute('aria-label', `Move ${label} up`);
      entry.down.setAttribute('aria-label', `Move ${label} down`);
      entry.remove.setAttribute('aria-label', `Remove ${label}`);
      entry.detail.textContent = stateLine(source);
      entry.detail.title = source.fetched_at ?? '';
      entry.detail.dataset.state = source.last_error ? 'error' : '';
      if (rows.children[index] !== entry.row) rows.insertBefore(entry.row, rows.children[index] ?? null);
    });
    while (rows.children.length > sources.length) rows.lastElementChild.remove();
    failures.replaceChildren(...[sourceFailures(sources, checkSources)].filter(Boolean));
    markDirty();
  }
  function move(source, offset) {
    const index = sources.indexOf(source);
    const target = index + offset;
    if (target < 0 || target >= sources.length) return;
    [sources[index], sources[target]] = [sources[target], sources[index]];
    render();
    const entry = elements.get(source);
    // Keep focus on the pressed button, or its partner at either end.
    const pressed = offset < 0 ? entry.up : entry.down;
    (pressed.disabled ? (offset < 0 ? entry.down : entry.up) : pressed).focus();
  }
  function removeSource(source) {
    const index = sources.indexOf(source);
    sources.splice(index, 1);
    render();
    const next = sources[index] ?? sources[index - 1];
    (next ? elements.get(next).input : add).focus();
  }
  async function load() {
    const payload = await requestJson('/api/tools/catalog/sources');
    sources = payload.sources;
    saved = snapshot();
    render();
  }
  const add = button('Add source', () => {
    const source = { source: '', enabled: true };
    sources.push(source);
    render();
    elements.get(source).input.focus();
  });
  const save = button('Save', async () => {
    setBusy(save, true, 'Saving…');
    try {
      await requestJson('/api/tools/catalog/sources', { method: 'POST', body: JSON.stringify({ sources: sources.map(({ source, enabled }) => ({ source, enabled })) }) });
      await requestJson('/api/tools/catalog'); await load();
      notify('Catalog sources saved.', 'success');
    } catch (error) { notify(errorText(error), 'error'); }
    finally { setBusy(save, false); }
  }, { className: 'button button-primary' });
  async function checkSources() {
    setBusy(check, true, 'Checking…');
    try { await requestJson('/api/tools/catalog'); await load(); }
    catch (error) { notify(errorText(error), 'error'); }
    finally { setBusy(check, false); }
  }
  const check = button('Check sources', checkSources);
  const actions = node('div', undefined, 'network-actions');
  actions.append(add, check, save, status);
  section.append(intro, failures, rows, actions);
  // Before API keys and About, after the runtime groups.
  const anchor = root.querySelector('.settings-group--about');
  if (anchor) anchor.before(section); else root.append(section);
  load().catch((error) => { status.textContent = errorText(error); });
}

const OFFICIAL_SOURCES = new Set(['18-lab-dev/vosu-tools', '18-lab-dev/vosulay']);

// Catalog-source overlays join the Available list as the same card as the
// community catalog's, official sources first, above the community cards.
// Returns place(tokens): re-inserts the matching cards after the list was
// replaced by a search.
export function mountCatalogOverlays({ root, requestJson, notify, errorText, setBusy, confirmAction, setCount, initialTokens = [] }) {
  if (!root) return () => {};
  const status = node('div', undefined, 'catalog-sources-status');
  root.before(status);
  const baseCount = Number(document.querySelector('[data-count="available"]')?.textContent?.match(/\d+/)?.[0] ?? 0);
  let cards = [];
  let tokens = initialTokens;

  async function install(overlay, action, control) {
    const requirements = overlay.requires_tools || [];
    if (requirements.length && !await confirmAction('Install required tools too?', requirements.map((r) => `${r.id} ${r.version}`).join('\n') + '\nCatalog tools run with your OS account’s access. Tools from non-official sources remain untrusted until approved.', 'Install tools and overlay')) return;
    setBusy(control, true, 'Queued…');
    try {
      const response = await requestJson(`/api/tools/catalog/overlays/${encodeURIComponent(overlay.id)}/${action}`, { method: 'POST', body: JSON.stringify({ install_tools: requirements.length > 0 }) });
      for (;;) {
        const job = await requestJson(`/api/tools/catalog/jobs/${encodeURIComponent(response.job_id)}`);
        control.textContent = `${job.phase}…`;
        if (job.status === 'failed') throw new Error(job.error || 'Overlay installation failed');
        if (job.status === 'completed') break;
        await new Promise((resolve) => window.setTimeout(resolve, 350));
      }
      notify(`${overlay.name} was ${action === 'update' ? 'updated' : 'installed'}.`, 'success'); await load();
    } catch (error) { notify(`${overlay.name}: ${errorText(error)}`, 'error'); setBusy(control, false); }
  }

  function card(overlay) {
    const official = OFFICIAL_SOURCES.has(overlay.source);
    const article = node('article', undefined, 'counter-card catalog-card catalog-card--source');
    article.dataset.sourceOverlay = overlay.id;
    article.dataset.counterName = overlay.name;
    article.dataset.counterAuthor = overlay.author ?? '';
    article.setAttribute('aria-label', overlay.name);
    const gallery = node('div', undefined, 'gallery');
    const art = httpUrl(overlay.preview) ?? httpUrl(overlay.banner_url);
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
    const facts = node('p', undefined, 'catalog-card__facts');
    if (Array.isArray(overlay.resolution)) {
      const size = node('span');
      size.append(node('span', 'Size', 'fact-label'), ` ${overlay.resolution[0]} × ${overlay.resolution[1]}`);
      facts.append(size);
    }
    if (facts.childElementCount) main.append(facts);
    const footer = node('div', undefined, 'catalog-card__footer');
    const tags = node('p', undefined, 'catalog-card__tags');
    // The source string can be a long URL; the badge names its kind and the
    // full source is in the tooltip.
    const source = node('span', official ? 'Official' : 'User catalog', 'badge');
    source.dataset.source = official ? 'official' : 'user';
    source.title = overlay.source;
    tags.append(source);
    if (overlay.shadowed) tags.append(node('span', 'Shadowed', 'badge'));
    if (overlay.update_available) tags.append(node('span', 'Update available', 'badge badge-warning'));
    else if (overlay.installed) tags.append(node('span', 'Installed', 'badge'));
    const actions = node('div', undefined, 'counter-actions');
    if (overlay.shadowed) {
      actions.append(node('span', 'An earlier source provides this id.', 'field-help'));
    } else if (!overlay.installed || (overlay.update_available && overlay.managed)) {
      const action = overlay.installed ? 'update' : 'install';
      const control = button(action === 'update' ? 'Update' : 'Install', () => install(overlay, action, control), {
        className: action === 'update' ? 'button button-update' : 'button button-primary',
        describe: `${action === 'update' ? 'Update' : 'Install'} ${overlay.name}`,
      });
      actions.append(control);
    } else {
      const link = node('a', 'Open in Overlays', 'button'); link.href = '/';
      actions.append(link);
    }
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
    // A search with source matches is not "no matching overlays".
    if (shown.length) root.querySelectorAll(':scope > .empty-state:not([role="alert"])').forEach((element) => element.remove());
  }

  async function load() {
    try {
      const payload = await requestJson('/api/tools/catalog');
      const overlays = [...payload.overlays, ...payload.shadowed.filter((entry) => entry.kind === 'overlay')];
      // Official sources first; the order within a group is the catalogs' own.
      overlays.sort((a, b) => Number(OFFICIAL_SOURCES.has(b.source)) - Number(OFFICIAL_SOURCES.has(a.source)));
      cards = overlays.map(card);
      status.replaceChildren(...[sourceFailures(payload.sources ?? [], load)].filter(Boolean));
      setCount?.(baseCount + overlays.filter((overlay) => !overlay.installed && !overlay.shadowed).length);
      place();
    } catch (error) {
      const failure = node('p', `Couldn't load catalog sources: ${errorText(error)} `, 'catalog-failures');
      failure.setAttribute('role', 'status');
      failure.append(button('Retry', load, { className: 'link-button' }));
      status.replaceChildren(failure);
    }
  }
  load();
  return place;
}
