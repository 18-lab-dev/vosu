(() => {
  'use strict';

  const results = document.querySelector('#counter-results');
  const searchInput = document.querySelector('.search-bar');
  const searchForm = document.querySelector('.search-form');
  const dialog = document.querySelector('#dashboard-dialog');
  const dialogTitle = document.querySelector('#dialog-title');
  const dialogDescription = document.querySelector('#dialog-description');
  const dialogBody = document.querySelector('#dialog-body');
  const toast = document.querySelector('#toast');
  const developerMode = document.querySelector('#developer-mode');

  const PINNED_KEY = 'vosu.pinned-overlays.v1';
  const INSTALLED_COUNT_KEY = 'total-installed-overlays';
  const AVAILABLE_COUNT_KEY = 'total-available-overlays';
  const DEVELOPER_MODE_KEY = 'vosu.dashboard.schema-builder';
  const PREVIEW_IFRAME_SELECTOR = '.counter-card__preview iframe';
  const PREVIEW_CANVAS = window.getComputedStyle(document.documentElement)
    .getPropertyValue('--ink')
    .trim();
  const SRGB_COMPONENT = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)%?$/;

  const state = {
    originalResults: results?.innerHTML ?? '',
    searchTimer: 0,
    searchController: null,
    searchRequest: 0,
    pinnedIds: [],
    installedIds: new Set(),
    draggedId: null,
    pending: new Set(),
    toastTimer: 0,
    dialogRequest: 0,
  };

  function isFullyTransparentSrgb(color) {
    if (typeof color !== 'string') return false;

    const normalized = color.trim().toLowerCase();
    if (normalized === 'transparent') return true;

    const functionMatch = /^(rgb|rgba)\((.*)\)$/.exec(normalized);
    if (!functionMatch) return false;

    const contents = functionMatch[2].trim();
    let channels;
    let alpha;

    if (contents.includes(',')) {
      if (functionMatch[1] !== 'rgba') return false;
      const components = contents.split(',').map((component) => component.trim());
      if (components.length !== 4) return false;
      channels = components.slice(0, 3);
      alpha = components[3];
    } else {
      const components = contents.split('/');
      if (components.length !== 2) return false;
      channels = components[0].trim().split(/\s+/);
      alpha = components[1].trim();
    }

    if (channels.length !== 3 || !channels.every((channel) => SRGB_COMPONENT.test(channel))) return false;
    return SRGB_COMPONENT.test(alpha) && Number.parseFloat(alpha) === 0;
  }

  function applyPreviewCanvas(element) {
    if (!(element instanceof HTMLIFrameElement) || !element.matches(PREVIEW_IFRAME_SELECTOR)) return;
    try {
      const root = element.contentDocument?.documentElement;
      const previewWindow = root?.ownerDocument.defaultView;
      if (!root || !previewWindow || !PREVIEW_CANVAS) return;

      const backgroundColor = previewWindow.getComputedStyle(root).backgroundColor;
      if (isFullyTransparentSrgb(backgroundColor)) root.style.backgroundColor = PREVIEW_CANVAS;
    } catch (_error) {
      // Cross-origin preview documents are intentionally inaccessible.
    }
  }

  document.addEventListener('load', (event) => applyPreviewCanvas(event.target), true);
  document.querySelectorAll(PREVIEW_IFRAME_SELECTOR).forEach(applyPreviewCanvas);

  function storageGet(key) {
    try {
      return window.localStorage.getItem(key);
    } catch (_error) {
      return null;
    }
  }

  function storageSet(key, value) {
    try {
      window.localStorage.setItem(key, String(value));
    } catch (_error) {
      // A blocked storage backend must not disable dashboard controls.
    }
  }

  function cloneValue(value) {
    if (typeof structuredClone === 'function') return structuredClone(value);
    return JSON.parse(JSON.stringify(value));
  }

  function errorText(value) {
    if (value instanceof Error) return value.message;
    if (typeof value === 'string') return value;
    try {
      return JSON.stringify(value);
    } catch (_error) {
      return String(value);
    }
  }

  function absoluteHttpUrl(value) {
    try {
      const url = new URL(String(value));
      return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null;
    } catch (_error) {
      return null;
    }
  }

  async function requestJson(url, options = {}) {
    const response = await fetch(url, {
      ...options,
      headers: {
        Accept: 'application/json',
        ...(options.body ? { 'Content-Type': 'application/json' } : {}),
        ...(options.headers ?? {}),
      },
    });
    const text = await response.text();
    let payload = {};
    if (text) {
      try {
        payload = JSON.parse(text);
      } catch (_error) {
        throw new Error(`Unexpected response from Vosu (${response.status})`);
      }
    }
    if (!response.ok) throw new Error(errorText(payload.error ?? response.statusText));
    if (payload.error !== undefined && payload.error !== null) {
      throw new Error(errorText(payload.error));
    }
    return payload;
  }

  function notify(message, tone = 'info', delay = 3200) {
    if (!toast) return;
    window.clearTimeout(state.toastTimer);
    toast.textContent = message;
    toast.dataset.tone = tone;
    toast.hidden = false;
    state.toastTimer = window.setTimeout(() => {
      toast.hidden = true;
    }, delay);
  }

  function setBusy(control, busy, busyLabel = 'Working…') {
    if (!(control instanceof HTMLElement)) return;
    if (busy) {
      control.dataset.idleLabel = control.textContent ?? '';
      control.setAttribute('aria-busy', 'true');
      if ('disabled' in control) control.disabled = true;
      control.textContent = busyLabel;
    } else {
      control.removeAttribute('aria-busy');
      if ('disabled' in control) control.disabled = false;
      if (control.dataset.idleLabel !== undefined) {
        control.textContent = control.dataset.idleLabel;
        delete control.dataset.idleLabel;
      }
    }
  }

  function pageKey() {
    if (window.location.pathname === '/available') return 'available';
    if (window.location.pathname === '/settings') return 'settings';
    if (window.location.pathname === '/local-overlays') return 'local';
    return 'installed';
  }

  function configurePageChrome() {
    const pages = {
      installed: ['Installed overlays', 'Open, configure, pin, or remove overlays served by this Vosu instance.'],
      available: ['Available overlays', 'Search the Vosu catalog, install new overlays, and apply catalog updates.'],
      settings: ['Vosu settings', 'Change daemon, polling, and listener settings for this Vosu instance.'],
      local: ['Add a local overlay', 'Place an overlay directly in the configured overlays directory.'],
    };
    const current = pageKey();
    document.querySelectorAll('.nav-link[data-page]').forEach((link) => {
      if (link.dataset.page === current) link.setAttribute('aria-current', 'page');
      else link.removeAttribute('aria-current');
    });
    const title = document.querySelector('#page-title');
    const summary = document.querySelector('#page-summary');
    if (title) title.textContent = pages[current][0];
    if (summary) summary.textContent = pages[current][1];
  }

  function countFromBadge(kind) {
    const badge = document.querySelector(`[data-count="${kind}"]`);
    const match = badge?.textContent?.match(/\d+/);
    return match ? Number(match[0]) : null;
  }

  function setCount(kind, value) {
    if (!Number.isInteger(value) || value < 0) return;
    const badge = document.querySelector(`[data-count="${kind}"]`);
    if (badge) badge.textContent = `(${value})`;
    storageSet(kind === 'installed' ? INSTALLED_COUNT_KEY : AVAILABLE_COUNT_KEY, value);
  }

  function initializeCounts() {
    for (const kind of ['installed', 'available']) {
      const serverCount = countFromBadge(kind);
      if (serverCount !== null) {
        setCount(kind, serverCount);
        continue;
      }
      const cached = Number(storageGet(kind === 'installed' ? INSTALLED_COUNT_KEY : AVAILABLE_COUNT_KEY));
      if (Number.isInteger(cached) && cached >= 0) setCount(kind, cached);
    }
  }

  function readPinnedIds() {
    let parsed = [];
    try {
      const value = JSON.parse(storageGet(PINNED_KEY) ?? '[]');
      if (Array.isArray(value)) parsed = value;
    } catch (_error) {
      parsed = [];
    }
    const seen = new Set();
    state.pinnedIds = parsed.filter((id) => {
      if (typeof id !== 'string' || seen.has(id)) return false;
      seen.add(id);
      return true;
    });
  }

  function storePinnedIds() {
    storageSet(PINNED_KEY, JSON.stringify(state.pinnedIds));
  }

  function installedCards() {
    return Array.from(results?.querySelectorAll('.counter-card[data-overlay-id]') ?? []);
  }

  function createInstalledEmptyState() {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    const heading = document.createElement('h2');
    heading.textContent = 'No local overlays yet';
    const link = document.createElement('a');
    link.className = 'button button-primary';
    link.href = '/available';
    link.textContent = 'Browse available overlays';
    empty.append(heading, link);
    return empty;
  }

  function removeCanonicalInstalledResult(id) {
    const template = document.createElement('template');
    template.innerHTML = state.originalResults;
    const card = Array.from(template.content.querySelectorAll('.counter-card[data-overlay-id]'))
      .find((candidate) => candidate.dataset.overlayId === id);
    card?.remove();
    if (!template.content.querySelector('.counter-card[data-overlay-id]')) {
      template.content.replaceChildren(createInstalledEmptyState());
    }
    state.originalResults = template.innerHTML;
  }

  function initializePins({ sanitize = false } = {}) {
    if (pageKey() !== 'installed' || !results) return;
    const cards = installedCards();
    state.installedIds = new Set(cards.map((card) => card.dataset.overlayId).filter(Boolean));
    readPinnedIds();
    if (sanitize) {
      state.pinnedIds = state.pinnedIds.filter((id) => state.installedIds.has(id));
      storePinnedIds();
    }
    renderPins();
  }

  function renderPins() {
    if (pageKey() !== 'installed' || !results) return;
    const cards = installedCards();
    const byId = new Map(cards.map((card) => [card.dataset.overlayId, card]));
    const pinnedSet = new Set(state.pinnedIds);
    const ordered = [
      ...state.pinnedIds.map((id) => byId.get(id)).filter(Boolean),
      ...cards.filter((card) => !pinnedSet.has(card.dataset.overlayId)),
    ];
    ordered.forEach((card) => results.appendChild(card));

    cards.forEach((card) => {
      const id = card.dataset.overlayId;
      const isPinned = pinnedSet.has(id);
      card.classList.toggle('is-pinned', isPinned);
      card.draggable = isPinned;
      const pin = card.querySelector('[data-action="toggle-pin"]');
      if (pin) {
        pin.setAttribute('aria-pressed', String(isPinned));
        pin.textContent = isPinned ? 'Unpin' : 'Pin';
      }
      const index = state.pinnedIds.indexOf(id);
      const earlier = card.querySelector('[data-action="pin-earlier"]');
      const later = card.querySelector('[data-action="pin-later"]');
      if (earlier) earlier.disabled = !isPinned || index <= 0;
      if (later) later.disabled = !isPinned || index < 0 || index >= state.pinnedIds.length - 1;
    });
  }

  function togglePin(card) {
    const id = card?.dataset.overlayId;
    if (!id) return;
    const index = state.pinnedIds.indexOf(id);
    if (index === -1) state.pinnedIds.push(id);
    else state.pinnedIds.splice(index, 1);
    storePinnedIds();
    renderPins();
  }

  function movePin(card, direction) {
    const id = card?.dataset.overlayId;
    const index = state.pinnedIds.indexOf(id);
    const destination = index + direction;
    if (index < 0 || destination < 0 || destination >= state.pinnedIds.length) return;
    [state.pinnedIds[index], state.pinnedIds[destination]] = [
      state.pinnedIds[destination],
      state.pinnedIds[index],
    ];
    storePinnedIds();
    renderPins();
    card.focus({ preventScroll: true });
  }

  function removePin(id) {
    state.pinnedIds = state.pinnedIds.filter((candidate) => candidate !== id);
    state.installedIds.delete(id);
    storePinnedIds();
  }

  async function downloadCounter(button, update) {
    const downloadUrl = button.dataset.downloadUrl;
    const installName = button.dataset.installName;
    if (!downloadUrl || !installName || state.pending.has(installName)) return;
    if (update && !window.confirm(`Update ${installName}?`)) return;

    state.pending.add(installName);
    setBusy(button, true, update ? 'Updating…' : 'Installing…');
    try {
      const endpoint = new URL(`/api/counters/download/${encodeURIComponent(downloadUrl)}`, window.location.origin);
      endpoint.searchParams.set('name', installName);
      if (update) endpoint.searchParams.set('update', 'true');
      await requestJson(endpoint);
      if (!update) setCount('installed', (countFromBadge('installed') ?? 0) + 1);
      notify(update ? `${installName} was updated.` : `${installName} was installed.`, 'success');
      window.setTimeout(() => window.location.reload(), 650);
    } catch (error) {
      notify(`Could not ${update ? 'update' : 'install'} ${installName}: ${errorText(error)}`, 'error', 5000);
      setBusy(button, false);
    } finally {
      state.pending.delete(installName);
    }
  }

  async function deleteCounter(button) {
    const card = button.closest('.counter-card');
    const folder = button.dataset.folder;
    if (!folder || !window.confirm(`Delete the local overlay “${folder}”?`)) return;
    setBusy(button, true, 'Deleting…');
    try {
      await requestJson(`/api/counters/delete/${encodeURIComponent(folder)}`);
      removePin(folder);
      removeCanonicalInstalledResult(folder);
      card?.remove();
      setCount('installed', Math.max(0, (countFromBadge('installed') ?? 1) - 1));
      if (results && !results.querySelector('.counter-card')) {
        results.appendChild(createInstalledEmptyState());
      }
      notify(`${folder} was deleted.`, 'success');
    } catch (error) {
      notify(`Could not delete ${folder}: ${errorText(error)}`, 'error', 5000);
      setBusy(button, false);
    }
  }

  async function openFolder(button) {
    const folder = button.dataset.folder;
    if (!folder) return;
    setBusy(button, true, 'Opening…');
    try {
      await requestJson(`/api/counters/open/${encodeURIComponent(folder)}`);
      notify(`Opened ${folder}.`, 'success');
    } catch (error) {
      notify(`Could not open ${folder}: ${errorText(error)}`, 'error', 5000);
    } finally {
      setBusy(button, false);
    }
  }

  async function openRoot(button) {
    setBusy(button, true, 'Opening…');
    try {
      await requestJson('/api/counters/open-root');
      notify('Opened the overlays folder.', 'success');
    } catch (error) {
      notify(`Could not open the overlays folder: ${errorText(error)}`, 'error', 5000);
    } finally {
      setBusy(button, false);
    }
  }

  async function runAppUpdate(button) {
    if (state.pending.has('app-update')) return;
    state.pending.add('app-update');
    setBusy(button, true, 'Updating…');
    try {
      await requestJson('/api/runUpdates');
      notify('Vosu finished updating.', 'success');
      document.querySelector('#update-notice')?.remove();
    } catch (error) {
      notify(`Vosu could not update: ${errorText(error)}`, 'error', 5000);
      setBusy(button, false);
    } finally {
      state.pending.delete('app-update');
    }
  }

  async function copyMetadata(button) {
    const value = button.dataset.copyValue ?? '';
    const label = button.dataset.copyLabel ?? 'Value';
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(value);
      } else {
        const temporary = document.createElement('textarea');
        temporary.value = value;
        temporary.setAttribute('readonly', '');
        temporary.style.position = 'fixed';
        temporary.style.left = '-10000px';
        document.body.appendChild(temporary);
        temporary.select();
        document.execCommand('copy');
        temporary.remove();
      }
      notify(`${label} copied.`, 'success', 1800);
    } catch (error) {
      notify(`Could not copy ${label.toLowerCase()}: ${errorText(error)}`, 'error');
    }
  }

  function activateMetadata(button, event) {
    if (event.shiftKey && button.dataset.copyLabel?.toLowerCase() === 'url') {
      const url = absoluteHttpUrl(button.dataset.copyValue);
      if (url) {
        window.open(url, '_blank', 'noopener,noreferrer');
        return;
      }
    }
    copyMetadata(button);
  }

  function restoreSearchResults() {
    if (!results) return;
    window.clearTimeout(state.searchTimer);
    state.searchTimer = 0;
    state.searchRequest += 1;
    state.searchController?.abort();
    state.searchController = null;
    results.innerHTML = state.originalResults;
    results.setAttribute('aria-busy', 'false');
    searchInput?.removeAttribute('aria-busy');
    initializePins();
  }

  async function runSearch(query, requestId) {
    if (!results || !searchInput || requestId !== state.searchRequest || searchInput.value.trim() !== query) return;
    state.searchController?.abort();
    const controller = new AbortController();
    state.searchController = controller;
    results.setAttribute('aria-busy', 'true');
    searchInput.setAttribute('aria-busy', 'true');
    try {
      const response = await fetch(`/api/counters/search/${encodeURIComponent(query)}`, {
        signal: controller.signal,
        headers: { Accept: 'text/html' },
      });
      if (!response.ok) throw new Error(`search returned ${response.status}`);
      const html = await response.text();
      if (
        state.searchRequest !== requestId
        || state.searchController !== controller
        || searchInput.value.trim() !== query
      ) return;
      results.innerHTML = html;
      initializePins();
    } catch (error) {
      if (
        state.searchRequest === requestId
        && state.searchController === controller
        && error.name !== 'AbortError'
      ) notify(`Search failed: ${errorText(error)}`, 'error');
    } finally {
      if (state.searchController === controller) {
        state.searchController = null;
        results.setAttribute('aria-busy', 'false');
        searchInput.removeAttribute('aria-busy');
      }
    }
  }

  function scheduleSearch() {
    window.clearTimeout(state.searchTimer);
    state.searchTimer = 0;
    const query = searchInput?.value.trim() ?? '';
    if (!query) {
      restoreSearchResults();
      return;
    }
    state.searchRequest += 1;
    const requestId = state.searchRequest;
    state.searchController?.abort();
    state.searchController = null;
    results?.setAttribute('aria-busy', 'false');
    searchInput?.removeAttribute('aria-busy');
    state.searchTimer = window.setTimeout(() => {
      state.searchTimer = 0;
      runSearch(query, requestId);
    }, 240);
  }

  function openDialog(title, description = '') {
    if (!dialog || !dialogBody) return null;
    const requestId = ++state.dialogRequest;
    dialogTitle.textContent = title;
    dialogDescription.textContent = description;
    dialogDescription.hidden = !description;
    dialogBody.replaceChildren();
    if (!dialog.open) dialog.showModal();
    return { container: dialogBody, requestId };
  }

  function isCurrentDialogRequest(requestId) {
    return requestId === null || (state.dialogRequest === requestId && Boolean(dialog?.open));
  }

  function closeDialog() {
    state.dialogRequest += 1;
    if (dialog?.open) dialog.close();
  }

  function fieldLabel(text, control, description = '') {
    const label = document.createElement('label');
    label.className = 'field-label';
    const name = document.createElement('span');
    name.textContent = text;
    label.appendChild(name);
    if (description) {
      const help = document.createElement('span');
      help.className = 'setting-description';
      help.textContent = description;
      label.appendChild(help);
    }
    label.appendChild(control);
    return label;
  }

  function makeInput(type, value) {
    const input = document.createElement('input');
    input.type = type;
    input.className = 'setting-value';
    if (type === 'checkbox') input.checked = Boolean(value);
    else if (value !== undefined && value !== null) input.value = String(value);
    return input;
  }

  function mergeCommandValues(setting, configured) {
    const defaults = Array.isArray(setting.value) ? cloneValue(setting.value) : [];
    if (!Array.isArray(configured)) return defaults;
    const uniqueKey = setting.uniqueCheck;
    if (!uniqueKey) return cloneValue(configured);
    const merged = defaults.map((entry) => {
      const replacement = configured.find((candidate) => candidate?.[uniqueKey] === entry?.[uniqueKey]);
      return replacement ? { ...entry, ...replacement } : entry;
    });
    configured.forEach((entry) => {
      if (!merged.some((candidate) => candidate?.[uniqueKey] === entry?.[uniqueKey])) {
        merged.push(cloneValue(entry));
      }
    });
    return merged;
  }

  function commandControl(option, value) {
    const type = String(option.type ?? 'text').toLowerCase();
    let control;
    if (type === 'checkbox') {
      control = makeInput('checkbox', value ?? option.value ?? false);
    } else if (type === 'number') {
      control = makeInput('number', value ?? option.value ?? 0);
      control.step = 'any';
    } else if (type === 'options') {
      control = document.createElement('select');
      control.className = 'setting-value';
      const values = Array.isArray(option.values) ? option.values.filter((item) => item !== '') : [];
      values.forEach((item) => {
        const choice = document.createElement('option');
        choice.value = String(item);
        choice.textContent = String(item);
        control.appendChild(choice);
      });
      control.value = String(value ?? option.value ?? values[0] ?? '');
    } else {
      control = makeInput('text', value ?? option.value ?? '');
    }
    control.dataset.commandName = String(option.name ?? '');
    return control;
  }

  function appendCommandRow(list, setting, command = {}) {
    const row = document.createElement('div');
    row.className = 'command-row';
    const fields = document.createElement('div');
    fields.className = 'command-fields';
    const options = Array.isArray(setting.options) ? setting.options : [];
    options.filter((option) => option && option.name).forEach((option) => {
      const wrapper = document.createElement('label');
      wrapper.className = 'command-field';
      const title = document.createElement('span');
      title.textContent = String(option.title || option.name);
      const control = commandControl(option, command[option.name]);
      wrapper.append(title, control);
      fields.appendChild(wrapper);
    });
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.className = 'button button-danger';
    remove.textContent = 'Remove command';
    remove.addEventListener('click', () => row.remove());
    row.append(fields, remove);
    list.appendChild(row);
  }

  function renderSettingField(setting, configuredValue) {
    const kind = String(setting.type ?? 'text').toLowerCase();
    const id = String(setting.uniqueID ?? '');
    if (kind === 'header') {
      const heading = document.createElement('h3');
      heading.className = 'counter-settings-heading';
      heading.textContent = String(setting.title || id);
      return heading;
    }

    const fieldset = document.createElement('fieldset');
    fieldset.className = 'counter-setting';
    fieldset.dataset.settingId = id;
    fieldset.dataset.settingType = kind;
    const legend = document.createElement('legend');
    legend.textContent = String(setting.title || id);
    fieldset.appendChild(legend);
    if (setting.description) {
      const description = document.createElement('p');
      description.className = 'setting-description';
      description.textContent = String(setting.description);
      fieldset.appendChild(description);
    }

    const value = configuredValue === undefined ? setting.value : configuredValue;
    if (kind === 'commands') {
      const list = document.createElement('div');
      list.className = 'command-list';
      mergeCommandValues(setting, value).forEach((command) => appendCommandRow(list, setting, command));
      const add = document.createElement('button');
      add.type = 'button';
      add.className = 'button';
      add.textContent = 'Add command';
      add.addEventListener('click', () => {
        const seed = {};
        (Array.isArray(setting.options) ? setting.options : []).forEach((option) => {
          if (option?.name) seed[option.name] = cloneValue(option.value ?? '');
        });
        appendCommandRow(list, setting, seed);
        list.lastElementChild?.querySelector('input, select')?.focus();
      });
      fieldset.append(list, add);
      return fieldset;
    }

    let control;
    if (kind === 'checkbox') {
      control = makeInput('checkbox', value);
    } else if (kind === 'number') {
      control = makeInput('number', value);
      control.step = 'any';
    } else if (kind === 'color') {
      control = makeInput('color', value || '#f45cad');
    } else if (kind === 'password') {
      control = makeInput('password', value);
      control.autocomplete = 'off';
    } else if (kind === 'textarea') {
      control = document.createElement('textarea');
      control.className = 'setting-value';
      control.value = value === undefined || value === null ? '' : String(value);
    } else if (kind === 'options') {
      control = document.createElement('select');
      control.className = 'setting-value';
      const options = Array.isArray(setting.options) ? setting.options.filter((item) => item !== '') : [];
      options.forEach((item) => {
        const option = document.createElement('option');
        option.value = String(item);
        option.textContent = String(item);
        control.appendChild(option);
      });
      control.value = String(value ?? options[0] ?? '');
    } else if (kind === 'button') {
      const href = absoluteHttpUrl(value);
      const link = document.createElement(href ? 'a' : 'span');
      link.className = 'button button-cyan';
      link.textContent = String(setting.title || 'Open link');
      if (href) {
        link.href = href;
        link.target = '_blank';
        link.rel = 'noopener noreferrer';
      } else {
        link.setAttribute('aria-disabled', 'true');
      }
      fieldset._settingValue = cloneValue(value);
      fieldset.appendChild(link);
      return fieldset;
    } else {
      control = makeInput('text', value);
    }
    control.setAttribute('aria-label', String(setting.title || id));
    fieldset.appendChild(control);
    return fieldset;
  }

  function collectCommandRows(fieldset) {
    return Array.from(fieldset.querySelectorAll('.command-row')).map((row) => {
      const command = {};
      row.querySelectorAll('[data-command-name]').forEach((control) => {
        const name = control.dataset.commandName;
        if (!name) return;
        if (control.type === 'checkbox') command[name] = control.checked;
        else if (control.type === 'number' && control.value !== '') command[name] = Number(control.value);
        else command[name] = control.value;
      });
      return command;
    });
  }

  function collectCounterSettings(form, schema) {
    const payload = [];
    form.querySelectorAll('.counter-setting[data-setting-id]').forEach((fieldset) => {
      const id = fieldset.dataset.settingId;
      const kind = fieldset.dataset.settingType;
      let value;
      if (kind === 'commands') {
        value = collectCommandRows(fieldset);
        const setting = schema.find((candidate) => candidate.uniqueID === id);
        const key = setting?.uniqueCheck;
        if (key) {
          const seen = new Set();
          value.forEach((command) => {
            const identity = command[key];
            if (identity === undefined || identity === '') throw new Error(`Every ${id} command needs ${key}.`);
            if (seen.has(String(identity))) throw new Error(`${id} contains a duplicate ${key}.`);
            seen.add(String(identity));
          });
        }
      } else if (kind === 'button') {
        value = cloneValue(fieldset._settingValue);
      } else {
        const control = fieldset.querySelector('.setting-value');
        if (!control) return;
        if (kind === 'checkbox') value = control.checked;
        else if (kind === 'number' && control.value !== '') value = Number(control.value);
        else value = control.value;
      }
      payload.push({ uniqueID: id, value });
    });
    return payload;
  }

  function renderCounterSettings(
    container,
    folder,
    schema,
    values,
    { inline = false, dialogRequestId = null } = {},
  ) {
    container.replaceChildren();
    if (inline) {
      container.classList.add('inline-counter-settings');
      const header = document.createElement('header');
      const title = document.createElement('h2');
      title.textContent = `Overlay settings: ${folder}`;
      const text = document.createElement('p');
      text.textContent = 'These values are stored by Vosu for this overlay.';
      header.append(title, text);
      container.appendChild(header);
    }

    const form = document.createElement('form');
    form.className = 'dialog-form';
    const list = document.createElement('div');
    list.className = 'counter-settings-list';
    schema.forEach((setting) => {
      const configured = Object.prototype.hasOwnProperty.call(values, setting.uniqueID)
        ? values[setting.uniqueID]
        : undefined;
      list.appendChild(renderSettingField(setting, configured));
    });
    if (!schema.length) {
      const empty = document.createElement('div');
      empty.className = 'empty-state';
      const title = document.createElement('h2');
      title.textContent = 'No settings exposed';
      const text = document.createElement('p');
      text.textContent = 'This overlay does not define user-editable settings.';
      empty.append(title, text);
      list.appendChild(empty);
    }

    const actions = document.createElement('div');
    actions.className = 'dialog-actions';
    if (!inline) {
      const cancel = document.createElement('button');
      cancel.type = 'button';
      cancel.className = 'button';
      cancel.textContent = 'Cancel';
      cancel.addEventListener('click', closeDialog);
      actions.appendChild(cancel);
    }
    const save = document.createElement('button');
    save.type = 'submit';
    save.className = 'button button-primary';
    save.textContent = 'Save overlay settings';
    save.disabled = !schema.some((setting) => setting.type !== 'header');
    actions.appendChild(save);
    form.append(list, actions);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (!isCurrentDialogRequest(dialogRequestId)) return;
      setBusy(save, true, 'Saving…');
      try {
        const payload = collectCounterSettings(form, schema);
        await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}`, {
          method: 'POST',
          body: JSON.stringify(payload),
        });
        if (!isCurrentDialogRequest(dialogRequestId)) return;
        notify(`Saved settings for ${folder}.`, 'success');
        if (!inline) closeDialog();
      } catch (error) {
        if (!isCurrentDialogRequest(dialogRequestId)) return;
        notify(`Could not save overlay settings: ${errorText(error)}`, 'error', 5000);
      } finally {
        setBusy(save, false);
      }
    });
    container.appendChild(form);
  }

  async function loadCounterSettings(folder, { inline = false } = {}) {
    const target = inline
      ? (results ? { container: results, requestId: null } : null)
      : openDialog(`Settings: ${folder}`, 'Edit values provided by this overlay.');
    if (!target) return;
    const { container, requestId } = target;
    const loading = document.createElement('p');
    loading.textContent = 'Loading overlay settings…';
    container.replaceChildren(loading);
    try {
      const payload = await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}`);
      if (!isCurrentDialogRequest(requestId)) return;
      const schema = Array.isArray(payload.settings) ? payload.settings : [];
      const values = payload.values && typeof payload.values === 'object' && !Array.isArray(payload.values)
        ? payload.values
        : {};
      renderCounterSettings(container, folder, schema, values, { inline, dialogRequestId: requestId });
    } catch (error) {
      if (!isCurrentDialogRequest(requestId)) return;
      container.replaceChildren();
      const message = document.createElement('p');
      message.textContent = `Could not load settings: ${errorText(error)}`;
      container.appendChild(message);
      notify(message.textContent, 'error', 5000);
    }
  }

  function builderDraft(setting = {}) {
    const kind = String(setting.type ?? 'text').toLowerCase();
    return {
      uniqueID: String(setting.uniqueID ?? ''),
      type: kind,
      title: String(setting.title ?? ''),
      description: String(setting.description ?? ''),
      uniqueCheck: String(setting.uniqueCheck ?? ''),
      options: cloneValue(setting.options ?? []),
      value: cloneValue(setting.value ?? (kind === 'checkbox' ? false : '')),
      optionsText: kind === 'commands' ? JSON.stringify(setting.options ?? [], null, 2) : '',
      valueText: kind === 'commands' ? JSON.stringify(Array.isArray(setting.value) ? setting.value : [], null, 2) : '',
    };
  }

  function builderTextInput(value, onInput, type = 'text') {
    const input = document.createElement('input');
    input.type = type;
    input.value = value ?? '';
    input.addEventListener('input', () => onInput(type === 'checkbox' ? input.checked : input.value));
    return input;
  }

  function renderBuilder(container, folder, sourceSchema, dialogRequestId) {
    let draft = sourceSchema.map((setting) => builderDraft(setting));
    const form = document.createElement('form');
    form.className = 'dialog-form';
    const toolbar = document.createElement('div');
    toolbar.className = 'builder-toolbar';
    const explanation = document.createElement('span');
    explanation.textContent = 'Define controls in the order overlays should display them.';
    const add = document.createElement('button');
    add.type = 'button';
    add.className = 'button button-cyan';
    add.textContent = 'Add setting';
    toolbar.append(explanation, add);
    const list = document.createElement('div');
    list.className = 'builder-list';

    function renderList(focusItem = null, focusAction = '') {
      list.replaceChildren();
      let focusControls = null;
      draft.forEach((item, index) => {
        const article = document.createElement('article');
        article.className = 'builder-item';
        const header = document.createElement('header');
        header.className = 'builder-item__header';
        const heading = document.createElement('h3');
        heading.textContent = item.title || item.uniqueID || `Setting ${index + 1}`;
        const itemActions = document.createElement('div');
        itemActions.className = 'builder-item__actions';
        const earlier = document.createElement('button');
        earlier.type = 'button';
        earlier.className = 'pin-order';
        earlier.textContent = '↑';
        earlier.title = 'Move setting earlier';
        earlier.disabled = index === 0;
        earlier.addEventListener('click', () => {
          [draft[index - 1], draft[index]] = [draft[index], draft[index - 1]];
          renderList(item, 'earlier');
        });
        const later = document.createElement('button');
        later.type = 'button';
        later.className = 'pin-order';
        later.textContent = '↓';
        later.title = 'Move setting later';
        later.disabled = index === draft.length - 1;
        later.addEventListener('click', () => {
          [draft[index], draft[index + 1]] = [draft[index + 1], draft[index]];
          renderList(item, 'later');
        });
        const remove = document.createElement('button');
        remove.type = 'button';
        remove.className = 'button button-danger';
        remove.textContent = 'Remove';
        remove.addEventListener('click', () => {
          if (!window.confirm(`Remove ${item.title || item.uniqueID || 'this setting'}?`)) return;
          draft.splice(index, 1);
          const nextItem = draft[index] ?? draft[index - 1] ?? null;
          renderList(nextItem, nextItem ? 'first' : 'add');
        });
        itemActions.append(earlier, later, remove);
        if (item === focusItem) focusControls = { earlier, later, remove };
        header.append(heading, itemActions);

        const fields = document.createElement('div');
        fields.className = 'builder-fields';
        const id = builderTextInput(item.uniqueID, (value) => {
          item.uniqueID = value.replace(/[^a-z0-9]/gi, '');
          id.value = item.uniqueID;
          heading.textContent = item.title || item.uniqueID || `Setting ${index + 1}`;
        });
        id.required = true;
        fields.appendChild(fieldLabel('Unique ID (letters and numbers)', id));

        const type = document.createElement('select');
        ['text', 'number', 'color', 'checkbox', 'textarea', 'password', 'options', 'button', 'commands', 'header']
          .forEach((kind) => {
            const option = document.createElement('option');
            option.value = kind;
            option.textContent = kind;
            type.appendChild(option);
          });
        type.value = item.type;
        type.addEventListener('change', () => {
          item.type = type.value;
          if (item.type === 'checkbox' && typeof item.value !== 'boolean') item.value = false;
          if (item.type === 'number' && typeof item.value !== 'number') item.value = 0;
          if (item.type === 'options' && !Array.isArray(item.options)) item.options = [];
          if (item.type === 'commands') {
            item.optionsText = JSON.stringify(Array.isArray(item.options) ? item.options : [], null, 2);
            item.valueText = JSON.stringify(Array.isArray(item.value) ? item.value : [], null, 2);
          }
          renderList();
        });
        fields.appendChild(fieldLabel('Control type', type));

        const title = builderTextInput(item.title, (value) => {
          item.title = value;
          heading.textContent = item.title || item.uniqueID || `Setting ${index + 1}`;
        });
        title.required = true;
        fields.appendChild(fieldLabel('Label', title));

        const description = document.createElement('textarea');
        description.value = item.description;
        description.addEventListener('input', () => {
          item.description = description.value;
        });
        fields.appendChild(fieldLabel('Description', description));

        const specific = document.createElement('div');
        specific.className = 'builder-specific';
        if (item.type === 'checkbox') {
          const value = makeInput('checkbox', item.value);
          value.addEventListener('change', () => {
            item.value = value.checked;
          });
          specific.appendChild(fieldLabel('Default state', value));
        } else if (item.type === 'number') {
          const value = builderTextInput(item.value, (next) => {
            item.value = next === '' ? 0 : Number(next);
          }, 'number');
          value.step = 'any';
          specific.appendChild(fieldLabel('Default number', value));
        } else if (item.type === 'options') {
          const choices = document.createElement('textarea');
          choices.value = (Array.isArray(item.options) ? item.options : []).join('\n');
          choices.addEventListener('input', () => {
            item.options = choices.value.split(/\r?\n/).map((choice) => choice.trim()).filter(Boolean);
          });
          specific.appendChild(fieldLabel('Choices (one per line)', choices));
          const value = builderTextInput(item.value, (next) => {
            item.value = next;
          });
          specific.appendChild(fieldLabel('Default choice', value));
        } else if (item.type === 'commands') {
          const key = builderTextInput(item.uniqueCheck, (value) => {
            item.uniqueCheck = value.replace(/[^a-z0-9]/gi, '');
            key.value = item.uniqueCheck;
          });
          specific.appendChild(fieldLabel('Command identity field', key, 'Each command must have a unique value for this field.'));
          const options = document.createElement('textarea');
          options.value = item.optionsText;
          options.spellcheck = false;
          options.addEventListener('input', () => {
            item.optionsText = options.value;
          });
          specific.appendChild(fieldLabel('Command field definitions (JSON array)', options));
          const value = document.createElement('textarea');
          value.value = item.valueText;
          value.spellcheck = false;
          value.addEventListener('input', () => {
            item.valueText = value.value;
          });
          specific.appendChild(fieldLabel('Default commands (JSON array)', value));
        } else if (item.type !== 'header') {
          const inputType = item.type === 'color' ? 'color' : item.type === 'password' ? 'password' : 'text';
          const value = builderTextInput(item.value, (next) => {
            item.value = next;
          }, inputType);
          specific.appendChild(fieldLabel(item.type === 'button' ? 'Link URL' : 'Default value', value));
        }
        fields.appendChild(specific);
        article.append(header, fields);
        list.appendChild(article);
      });
      if (!focusAction) return;
      if (focusAction === 'add') {
        add.focus({ preventScroll: true });
        return;
      }
      let focusTarget = focusAction === 'first'
        ? [focusControls?.earlier, focusControls?.later, focusControls?.remove].find((control) => control && !control.disabled)
        : focusControls?.[focusAction];
      if (focusTarget?.disabled) {
        focusTarget = focusAction === 'earlier' ? focusControls?.later : focusControls?.earlier;
      }
      (focusTarget ?? focusControls?.remove ?? add).focus({ preventScroll: true });
    }

    function serializeDraft() {
      const ids = new Set();
      return draft.map((item, index) => {
        if (!item.uniqueID || !/^[a-z0-9]+$/i.test(item.uniqueID)) {
          throw new Error(`Setting ${index + 1} needs an alphanumeric unique ID.`);
        }
        if (ids.has(item.uniqueID)) throw new Error(`Unique ID ${item.uniqueID} is used more than once.`);
        ids.add(item.uniqueID);
        if (!item.title) throw new Error(`${item.uniqueID} needs a label.`);

        const setting = {
          uniqueID: item.uniqueID,
          type: item.type,
          title: item.title,
          description: item.description,
          value: cloneValue(item.value),
        };
        if (item.type === 'options') {
          const options = Array.isArray(item.options) ? item.options.filter(Boolean) : [];
          if (options.length < 2) throw new Error(`${item.uniqueID} needs at least two choices.`);
          if (!options.includes(String(item.value))) throw new Error(`${item.uniqueID} has a default that is not one of its choices.`);
          setting.options = options;
        } else if (item.type === 'commands') {
          if (!item.uniqueCheck) throw new Error(`${item.uniqueID} needs a command identity field.`);
          let options;
          let value;
          try {
            options = JSON.parse(item.optionsText || '[]');
            value = JSON.parse(item.valueText || '[]');
          } catch (error) {
            throw new Error(`${item.uniqueID} contains invalid JSON: ${errorText(error)}`);
          }
          if (!Array.isArray(options) || !options.length) throw new Error(`${item.uniqueID} needs at least one command field definition.`);
          if (!Array.isArray(value)) throw new Error(`${item.uniqueID} default commands must be an array.`);
          setting.uniqueCheck = item.uniqueCheck;
          setting.options = options;
          setting.value = value;
        } else if (item.type === 'button') {
          let url;
          try {
            url = new URL(String(item.value));
          } catch (_error) {
            throw new Error(`${item.uniqueID} needs a valid link URL.`);
          }
          if (!['http:', 'https:'].includes(url.protocol)) throw new Error(`${item.uniqueID} link must use HTTP or HTTPS.`);
        } else if (item.type === 'header') {
          setting.value = '';
        }
        return setting;
      });
    }

    add.addEventListener('click', () => {
      draft.push(builderDraft());
      renderList();
      list.lastElementChild?.querySelector('input')?.focus();
    });

    const actions = document.createElement('div');
    actions.className = 'dialog-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'button';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', closeDialog);
    const save = document.createElement('button');
    save.type = 'submit';
    save.className = 'button button-primary';
    save.textContent = 'Save schema';
    actions.append(cancel, save);
    form.append(toolbar, list, actions);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (!isCurrentDialogRequest(dialogRequestId)) return;
      setBusy(save, true, 'Saving…');
      try {
        const schema = serializeDraft();
        await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}?update=yes`, {
          method: 'POST',
          body: JSON.stringify(schema),
        });
        if (!isCurrentDialogRequest(dialogRequestId)) return;
        notify(`Saved the settings schema for ${folder}.`, 'success');
        closeDialog();
      } catch (error) {
        if (!isCurrentDialogRequest(dialogRequestId)) return;
        notify(`Could not save the schema: ${errorText(error)}`, 'error', 5500);
      } finally {
        setBusy(save, false);
      }
    });

    container.replaceChildren(form);
    renderList();
  }

  async function openSettingsBuilder(folder) {
    const target = openDialog(`Settings schema: ${folder}`, 'Developer mode edits the schema exposed by this overlay.');
    if (!target) return;
    const { container, requestId } = target;
    const loading = document.createElement('p');
    loading.textContent = 'Loading the settings schema…';
    container.appendChild(loading);
    try {
      const payload = await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}`);
      if (!isCurrentDialogRequest(requestId)) return;
      renderBuilder(container, folder, Array.isArray(payload.settings) ? payload.settings : [], requestId);
    } catch (error) {
      if (!isCurrentDialogRequest(requestId)) return;
      container.replaceChildren();
      const message = document.createElement('p');
      message.textContent = `Could not load the schema: ${errorText(error)}`;
      container.appendChild(message);
      notify(message.textContent, 'error', 5000);
    }
  }

  function initializeRuntimeSettings() {
    const form = document.querySelector('#runtime-settings');
    if (!form) return;
    const status = form.querySelector('.settings-status');
    const save = form.querySelector('[type="submit"]');
    const serverIp = form.querySelector('[data-id="SERVER_IP"]');
    const serverPort = form.querySelector('[data-id="SERVER_PORT"]');
    const originalHost = serverIp?.value;
    const originalPort = serverPort?.value;

    form.addEventListener('input', () => {
      form.dataset.dirty = 'true';
      if (status) status.textContent = 'Unsaved changes';
    });
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      const settings = {};
      form.querySelectorAll('[data-id]').forEach((control) => {
        settings[control.dataset.id] = control.type === 'checkbox' ? control.checked : control.value;
      });
      setBusy(save, true, 'Saving…');
      try {
        await requestJson('/api/settingsSave', {
          method: 'POST',
          body: JSON.stringify(settings),
        });
        form.dataset.dirty = 'false';
        if (status) status.textContent = 'Settings saved';
        notify('Vosu settings were saved.', 'success');
        const listenerChanged = originalHost !== settings.SERVER_IP || originalPort !== settings.SERVER_PORT;
        if (listenerChanged) {
          const host = settings.SERVER_IP === '0.0.0.0' || settings.SERVER_IP === '::' ? 'localhost' : settings.SERVER_IP;
          const displayHost = host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
          window.setTimeout(() => {
            window.location.href = `http://${displayHost}:${settings.SERVER_PORT}${window.location.pathname}${window.location.search}`;
          }, 450);
        } else {
          setBusy(save, false);
        }
      } catch (error) {
        notify(`Could not save Vosu settings: ${errorText(error)}`, 'error', 5000);
        if (status) status.textContent = 'Save failed';
        setBusy(save, false);
      }
    });
  }

  document.addEventListener('click', (event) => {
    const control = event.target.closest('[data-action]');
    if (!control) return;
    const action = control.dataset.action;
    if (action === 'close-dialog') {
      closeDialog();
    } else if (action === 'toggle-pin') {
      togglePin(control.closest('.counter-card'));
    } else if (action === 'pin-earlier') {
      movePin(control.closest('.counter-card'), -1);
    } else if (action === 'pin-later') {
      movePin(control.closest('.counter-card'), 1);
    } else if (action === 'download-counter') {
      downloadCounter(control, false);
    } else if (action === 'update-counter') {
      downloadCounter(control, true);
    } else if (action === 'delete-counter') {
      deleteCounter(control);
    } else if (action === 'open-counter') {
      openFolder(control);
    } else if (action === 'open-root') {
      openRoot(control);
    } else if (action === 'counter-settings') {
      loadCounterSettings(control.dataset.folder);
    } else if (action === 'settings-builder') {
      openSettingsBuilder(control.dataset.folder);
    } else if (action === 'app-update') {
      runAppUpdate(control);
    } else if (action === 'copy-metadata') {
      activateMetadata(control, event);
    }
  });

  searchForm?.addEventListener('submit', (event) => event.preventDefault());
  searchInput?.addEventListener('input', scheduleSearch);
  document.addEventListener('keydown', (event) => {
    if (event.key !== '/' || event.ctrlKey || event.altKey || event.metaKey || event.shiftKey || !searchInput) return;
    if (event.target.closest('input, textarea, select, [contenteditable="true"]')) return;
    event.preventDefault();
    searchInput.focus();
  });

  results?.addEventListener('dragstart', (event) => {
    const card = event.target.closest('.counter-card.is-pinned[data-overlay-id]');
    if (!card) return;
    state.draggedId = card.dataset.overlayId;
    card.classList.add('dragging');
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('text/plain', state.draggedId);
  });
  results?.addEventListener('dragover', (event) => {
    if (!state.draggedId) return;
    const card = event.target.closest('.counter-card.is-pinned[data-overlay-id]');
    if (!card || card.dataset.overlayId === state.draggedId) return;
    event.preventDefault();
    installedCards().forEach((candidate) => candidate.classList.toggle('drag-over', candidate === card));
  });
  results?.addEventListener('drop', (event) => {
    const target = event.target.closest('.counter-card.is-pinned[data-overlay-id]');
    if (!state.draggedId || !target || target.dataset.overlayId === state.draggedId) return;
    event.preventDefault();
    const reordered = state.pinnedIds.filter((id) => id !== state.draggedId);
    let index = reordered.indexOf(target.dataset.overlayId);
    if (event.clientY > target.getBoundingClientRect().top + target.offsetHeight / 2) index += 1;
    reordered.splice(Math.max(0, index), 0, state.draggedId);
    state.pinnedIds = reordered;
    storePinnedIds();
    renderPins();
  });
  results?.addEventListener('dragend', () => {
    state.draggedId = null;
    installedCards().forEach((card) => card.classList.remove('dragging', 'drag-over'));
  });

  developerMode?.addEventListener('change', () => {
    document.body.classList.toggle('developer-mode', developerMode.checked);
    storageSet(DEVELOPER_MODE_KEY, developerMode.checked ? 'true' : 'false');
  });

  window.addEventListener('storage', (event) => {
    if (event.key === PINNED_KEY) initializePins();
    if (event.key === INSTALLED_COUNT_KEY) {
      const installedCount = Number(event.newValue);
      if (event.newValue !== null && Number.isInteger(installedCount) && installedCount >= 0) {
        setCount('installed', installedCount);
        if (pageKey() === 'installed') window.location.reload();
      }
    }
    if (event.key === AVAILABLE_COUNT_KEY) setCount('available', Number(event.newValue));
    if (event.key === DEVELOPER_MODE_KEY && developerMode) {
      developerMode.checked = event.newValue === 'true';
      document.body.classList.toggle('developer-mode', developerMode.checked);
    }
  });

  configurePageChrome();
  initializeCounts();
  initializePins({ sanitize: pageKey() === 'installed' && !window.location.search });
  initializeRuntimeSettings();

  if (developerMode) {
    developerMode.checked = storageGet(DEVELOPER_MODE_KEY) === 'true';
    document.body.classList.toggle('developer-mode', developerMode.checked);
  }

  const requestedOverlay = new URLSearchParams(window.location.search).get('overlay');
  if (window.location.pathname === '/settings' && requestedOverlay) {
    const title = document.querySelector('#page-title');
    const summary = document.querySelector('#page-summary');
    if (title) title.textContent = 'Overlay settings';
    if (summary) summary.textContent = 'Edit settings for an overlay embedded in another local view.';
    loadCounterSettings(requestedOverlay, { inline: true });
  }
})();
