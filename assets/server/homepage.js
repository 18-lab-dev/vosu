import { createSettingsEditor } from './homepage-settings.js';

(() => {
  'use strict';

  const results = document.querySelector('#counter-results');
  const searchInput = document.querySelector('.search-bar');
  const searchForm = document.querySelector('.search-form');
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

  const { closeDialog, loadCounterSettings, openSettingsBuilder } = createSettingsEditor({
    results, requestJson, notify, setBusy, errorText, absoluteHttpUrl,
  });

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
