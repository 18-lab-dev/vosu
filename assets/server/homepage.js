import { createSettingsEditor } from './homepage-settings.js';

(() => {
  'use strict';

  const results = document.querySelector('#counter-results');
  const searchInput = document.querySelector('.search-bar');
  const searchForm = document.querySelector('.search-form');
  const toast = document.querySelector('#toast');
  const toastMessage = toast?.querySelector('.toast__message');
  const toastAction = toast?.querySelector('.toast__action');

  const PINNED_KEY = 'vosu.pinned-overlays.v1';
  const INSTALLED_COUNT_KEY = 'total-installed-overlays';
  const AVAILABLE_COUNT_KEY = 'total-available-overlays';
  const LISTENER_POLL_INTERVAL = 500;
  const LISTENER_POLL_TIMEOUT = 15000;

  const state = {
    originalResults: results?.innerHTML ?? '',
    searchTimer: 0,
    searchController: null,
    searchRequest: 0,
    pinnedIds: [],
    pending: new Set(),
    toastTimer: 0,
  };

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

  // A zero delay keeps the toast until the user acts on or replaces it.
  function notify(message, tone = 'info', delay = 3200, action = null) {
    if (!toast || !toastMessage || !toastAction) return;
    window.clearTimeout(state.toastTimer);
    state.toastTimer = 0;
    toastMessage.textContent = message;
    toast.dataset.tone = tone;
    toastAction.hidden = !action;
    toastAction.textContent = action?.label ?? '';
    toastAction.onclick = action
      ? () => {
        toast.hidden = true;
        action.run();
      }
      : null;
    toast.hidden = false;
    if (delay > 0) {
      state.toastTimer = window.setTimeout(() => {
        toast.hidden = true;
      }, delay);
    }
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
    return 'installed';
  }

  function configurePageChrome() {
    const pages = {
      installed: ['Installed overlays', 'Search installed overlays'],
      available: ['Available overlays', 'Search the overlay catalog'],
      settings: ['Vosu settings', 'Search settings'],
    };
    const current = pageKey();
    document.querySelectorAll('.nav-link[data-page]').forEach((link) => {
      if (link.dataset.page === current) link.setAttribute('aria-current', 'page');
      else link.removeAttribute('aria-current');
    });
    const title = document.querySelector('#page-title');
    if (title) title.textContent = pages[current][0];
    if (searchInput) {
      searchInput.placeholder = pages[current][1];
      searchInput.setAttribute('aria-label', pages[current][1]);
    }
  }

  function countFromBadge(kind) {
    const badge = document.querySelector(`[data-count="${kind}"]`);
    const match = badge?.textContent?.match(/\d+/);
    return match ? Number(match[0]) : null;
  }

  function setCount(kind, value) {
    if (!Number.isInteger(value) || value < 0) return;
    const badge = document.querySelector(`[data-count="${kind}"]`);
    if (badge) badge.textContent = String(value);
    storageSet(kind === 'installed' ? INSTALLED_COUNT_KEY : AVAILABLE_COUNT_KEY, value);
  }

  function initializeCounts() {
    for (const kind of ['installed', 'available']) {
      const serverCount = countFromBadge(kind);
      if (serverCount !== null) {
        setCount(kind, serverCount);
        continue;
      }
      const cached = storageGet(kind === 'installed' ? INSTALLED_COUNT_KEY : AVAILABLE_COUNT_KEY);
      // Number(null) is 0, which would invent a count for a page never visited.
      if (cached !== null && Number.isInteger(Number(cached)) && Number(cached) >= 0) setCount(kind, Number(cached));
    }
  }

  function fitPreview(stage) {
    const width = Number(stage.dataset.previewWidth);
    const height = Number(stage.dataset.previewHeight);
    const iframe = stage.querySelector('iframe');
    const container = stage.parentElement;
    if (!iframe || !container || !(width > 0) || !(height > 0)) return;
    const style = window.getComputedStyle(container);
    const availableWidth = container.clientWidth
      - Number.parseFloat(style.paddingLeft)
      - Number.parseFloat(style.paddingRight);
    const availableHeight = container.clientHeight
      - Number.parseFloat(style.paddingTop)
      - Number.parseFloat(style.paddingBottom);
    if (!(availableWidth > 0) || !(availableHeight > 0)) return;
    // Contain the overlay in its frame. Small overlays may grow up to the
    // stage's limit so their content stays legible.
    const maxScale = Number(stage.dataset.maxScale) || 2;
    const scale = Math.min(maxScale, availableWidth / width, availableHeight / height);
    stage.style.width = `${width * scale}px`;
    stage.style.height = `${height * scale}px`;
    iframe.style.transform = `scale(${scale})`;
  }

  const previewObserver = typeof ResizeObserver === 'function'
    ? new ResizeObserver((entries) => {
      for (const entry of entries) {
        const stage = entry.target.querySelector('.preview-stage[data-preview-width]');
        if (stage) fitPreview(stage);
      }
    })
    : null;

  function observePreviews() {
    document.querySelectorAll('.preview-stage[data-preview-width]').forEach((stage) => {
      fitPreview(stage);
      if (stage.parentElement) previewObserver?.observe(stage.parentElement);
    });
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
    return Array.from(results?.querySelectorAll(':scope > .counter-card[data-overlay-id]') ?? []);
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

  // Moving a card re-parents its iframe, which restarts the live overlay.
  // Only out-of-place cards move, and moveBefore() keeps the iframe running
  // where the browser supports it.
  function arrangeCards(ordered) {
    ordered.forEach((card, index) => {
      const current = installedCards()[index];
      if (current === card) return;
      try {
        if (typeof results.moveBefore === 'function') {
          results.moveBefore(card, current);
          return;
        }
      } catch (_error) {
        // Fall through to the reloading insertion.
      }
      results.insertBefore(card, current);
    });
  }

  function initializePins({ sanitize = false } = {}) {
    if (pageKey() !== 'installed' || !results) return;
    readPinnedIds();
    if (sanitize) {
      const installed = new Set(installedCards().map((card) => card.dataset.overlayId));
      state.pinnedIds = state.pinnedIds.filter((id) => installed.has(id));
      storePinnedIds();
    }
    renderPins();
  }

  function renderPins() {
    if (pageKey() !== 'installed' || !results) return;
    const cards = installedCards();
    const byId = new Map(cards.map((card) => [card.dataset.overlayId, card]));
    const pinnedSet = new Set(state.pinnedIds);
    arrangeCards([
      ...state.pinnedIds.map((id) => byId.get(id)).filter(Boolean),
      ...cards.filter((card) => !pinnedSet.has(card.dataset.overlayId)),
    ]);

    cards.forEach((card) => {
      const id = card.dataset.overlayId;
      const isPinned = pinnedSet.has(id);
      const name = card.dataset.counterName ?? id;
      card.classList.toggle('is-pinned', isPinned);
      const pin = card.querySelector('[data-action="toggle-pin"]');
      if (pin) {
        pin.setAttribute('aria-pressed', String(isPinned));
        pin.setAttribute('aria-label', `${isPinned ? 'Unpin' : 'Pin'} ${name}`);
        pin.title = isPinned ? 'Unpin' : 'Pin';
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
  }

  function removePin(id) {
    state.pinnedIds = state.pinnedIds.filter((candidate) => candidate !== id);
    storePinnedIds();
  }

  function normalizeSearchText(value) {
    return String(value ?? '').replace(/[^\p{Alphabetic}\p{N}\s]/gu, '').toLowerCase();
  }

  // Mirrors the server's catalog query rules in src/runtime/render.rs.
  function queryTokens(query) {
    const tokens = [];
    for (const word of normalizeSearchText(query).split(/\s+/)) {
      if (!word) continue;
      const token = word === 'sc' ? 'streamcompanion' : word;
      if (!tokens.includes(token)) tokens.push(token);
    }
    return tokens;
  }

  function cardMatches(card, tokens) {
    if (tokens.length === 0) return true;
    const name = normalizeSearchText(card.dataset.counterName);
    const author = normalizeSearchText(card.dataset.counterAuthor);
    const compatible = (card.dataset.counterCompatible ?? '')
      .split(',')
      .filter(Boolean)
      .map(normalizeSearchText);
    return tokens.some((token) => name.includes(token)
      || author.includes(token)
      || (token !== 'vosu' && compatible.some((value) => value.includes(token))));
  }

  // Installed cards are filtered in place: replacing them would restart every
  // live preview iframe on each keystroke.
  function filterInstalled() {
    if (!results) return;
    const tokens = queryTokens(searchInput?.value ?? '');
    const cards = installedCards();
    let visible = 0;
    cards.forEach((card) => {
      const match = cardMatches(card, tokens);
      card.hidden = !match;
      if (match) visible += 1;
    });
    const showEmpty = cards.length > 0 && visible === 0;
    let empty = results.querySelector(':scope > .no-results');
    if (showEmpty && !empty) {
      empty = document.createElement('div');
      empty.className = 'no-results';
      const heading = document.createElement('h2');
      heading.textContent = 'No matching overlays';
      const hint = document.createElement('p');
      hint.textContent = 'Try a shorter name, a different author, or another compatible client.';
      empty.append(heading, hint);
      results.append(empty);
    } else if (!showEmpty) {
      empty?.remove();
    }
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
      await requestJson(endpoint, { method: 'POST' });
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
      await requestJson(`/api/counters/delete/${encodeURIComponent(folder)}`, { method: 'POST' });
      removePin(folder);
      card?.remove();
      setCount('installed', Math.max(0, (countFromBadge('installed') ?? 1) - 1));
      if (results && installedCards().length === 0) {
        results.replaceChildren(createInstalledEmptyState());
      } else {
        filterInstalled();
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
      await requestJson(`/api/counters/open/${encodeURIComponent(folder)}`, { method: 'POST' });
      notify(`Opened ${folder}.`, 'success');
    } catch (error) {
      notify(`Could not open ${folder}: ${errorText(error)}`, 'error', 5000);
    } finally {
      setBusy(button, false);
    }
  }

  async function runAppUpdate(button) {
    if (state.pending.has('app-update')) return;
    state.pending.add('app-update');
    setBusy(button, true, 'Updating…');
    try {
      await requestJson('/api/runUpdates', { method: 'POST' });
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
  }

  async function runSearch(query, requestId) {
    if (!results || !searchInput || requestId !== state.searchRequest || searchInput.value.trim() !== query) return;
    state.searchController?.abort();
    const controller = new AbortController();
    state.searchController = controller;
    results.setAttribute('aria-busy', 'true');
    searchInput.setAttribute('aria-busy', 'true');
    try {
      const response = await fetch(`/api/counters/search/${encodeURIComponent(query)}?source=available`, {
        signal: controller.signal,
        headers: { Accept: 'text/html' },
      });
      const html = await response.text();
      if (!response.ok) {
        const message = new DOMParser().parseFromString(html, 'text/html').body.textContent?.trim();
        throw new Error(message || `search returned ${response.status}`);
      }
      if (
        state.searchRequest !== requestId
        || state.searchController !== controller
        || searchInput.value.trim() !== query
      ) return;
      results.innerHTML = html;
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
    if (pageKey() === 'installed') {
      filterInstalled();
      return;
    }
    if (pageKey() === 'settings') {
      filterSettings();
      return;
    }
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

  async function waitForListener(origin) {
    const deadline = Date.now() + LISTENER_POLL_TIMEOUT;
    while (Date.now() < deadline) {
      try {
        // An opaque response proves only that something answers HTTP there,
        // which is all the redirect needs; it cannot read cross-origin bodies.
        await fetch(new URL('/favicon.ico', origin), {
          mode: 'no-cors',
          cache: 'no-store',
          signal: AbortSignal.timeout(2000),
        });
        return true;
      } catch (_error) {
        // Not listening yet.
      }
      await new Promise((resolve) => {
        window.setTimeout(resolve, LISTENER_POLL_INTERVAL);
      });
    }
    return false;
  }

  // Settings narrow with every word (AND), unlike overlay search, because the
  // list is short and users type a specific option name.
  function filterSettings() {
    const form = document.querySelector('#runtime-settings');
    if (!form) return;
    const words = normalizeSearchText(searchInput?.value ?? '').split(/\s+/).filter(Boolean);
    let anyVisible = false;
    form.querySelectorAll('.settings-group').forEach((group) => {
      const legend = normalizeSearchText(group.querySelector('legend')?.textContent);
      let visible = 0;
      group.querySelectorAll('.settings-item').forEach((item) => {
        const text = `${legend} ${normalizeSearchText(item.textContent)}`;
        const match = words.every((word) => text.includes(word));
        item.hidden = !match;
        if (match) visible += 1;
      });
      group.hidden = visible === 0;
      if (visible > 0) anyVisible = true;
    });
    const empty = form.querySelector('.settings-empty');
    if (empty) empty.hidden = anyVisible;
  }

  function controlValue(control) {
    return control.type === 'checkbox' ? control.checked : control.value;
  }

  function initializeRuntimeSettings() {
    const form = document.querySelector('#runtime-settings');
    if (!form) return;
    const status = form.querySelector('.settings-status');
    const serverIp = form.querySelector('[data-id="SERVER_IP"]');
    const serverPort = form.querySelector('[data-id="SERVER_PORT"]');
    const startAtLogin = form.querySelector('[data-id="START_AT_LOGIN"]');
    let savedStartAtLogin = startAtLogin?.checked;
    let queue = Promise.resolve();

    function setStatus(text, tone = '') {
      if (!status) return;
      status.textContent = text;
      status.dataset.tone = tone;
    }

    function setSavedStatus() {
      if (form.querySelector('.settings-item.has-error')) {
        setStatus('Some changes were not saved', 'error');
      } else {
        setStatus('All changes saved', 'success');
      }
    }

    async function saveSetting(control) {
      const item = control.closest('.settings-item');
      const name = item?.querySelector('.settings-item__name')?.textContent ?? control.dataset.id;
      const listener = control === serverIp || control === serverPort;
      // The listener moves as a unit, so both halves are always sent together.
      const controls = listener ? [serverIp, serverPort].filter(Boolean) : [control];
      const settings = Object.fromEntries(controls.map((field) => [field.dataset.id, controlValue(field)]));
      // Validate locally first: the server rejects these too, but a blank or
      // out-of-range field must never look saved.
      const invalid = controls.find((field) => field.type === 'number'
        && (field.value.trim() === '' || !field.checkValidity()));
      if (invalid) {
        const invalidItem = invalid.closest('.settings-item');
        const invalidName = invalidItem?.querySelector('.settings-item__name')?.textContent ?? invalid.dataset.id;
        const reason = invalid.value.trim() === '' ? 'enter a number' : invalid.validationMessage;
        invalidItem?.classList.add('has-error');
        setStatus(`${invalidName} was not saved: ${reason}`, 'error');
        return;
      }
      controls.forEach((field) => field.closest('.settings-item')?.classList.remove('has-error'));
      setStatus('Saving…');
      try {
        await requestJson('/api/settingsSave', { method: 'POST', body: JSON.stringify(settings) });
      } catch (error) {
        if (control === startAtLogin) control.checked = savedStartAtLogin;
        item?.classList.add('has-error');
        setStatus(`${name} was not saved`, 'error');
        notify(`Could not save ${name}: ${errorText(error)}`, 'error', 5000);
        return;
      }
      if (control === startAtLogin) savedStartAtLogin = control.checked;
      if (!listener) {
        setSavedStatus();
        return;
      }
      const ip = String(settings.SERVER_IP ?? '');
      const host = ip === '0.0.0.0' || ip === '::' ? 'localhost' : ip;
      const displayHost = host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
      const origin = `http://${displayHost}:${settings.SERVER_PORT}`;
      if (origin === window.location.origin) {
        setSavedStatus();
        return;
      }
      setStatus(`Waiting for Vosu at ${origin}…`);
      if (await waitForListener(origin)) {
        window.location.href = `${origin}${window.location.pathname}${window.location.search}`;
        return;
      }
      setStatus(`Vosu did not respond at ${origin}`, 'error');
      notify(`Vosu did not respond at ${origin}. Check the listener settings, then open that address manually.`, 'error', 0);
    }

    // `change` fires when a text field loses focus (or Enter commits it) and
    // immediately for checkboxes; saves are serialized so they apply in order.
    form.addEventListener('change', (event) => {
      const control = event.target.closest('[data-id]');
      if (!control || control.disabled) return;
      queue = queue.then(() => saveSetting(control));
    });
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
    });
  }

  function cardMenuToggle(menu) {
    return menu.parentElement?.querySelector('.card-menu-toggle') ?? null;
  }

  // Popovers render in the top layer, outside the card's clipping, so they
  // are anchored to their toggle here rather than with CSS positioning.
  function positionCardMenu(menu) {
    const toggle = cardMenuToggle(menu);
    if (!toggle) return;
    const anchor = toggle.getBoundingClientRect();
    const gap = 6;
    const margin = 8;
    const below = anchor.bottom + gap;
    const top = below + menu.offsetHeight > window.innerHeight - margin
      ? Math.max(margin, anchor.top - gap - menu.offsetHeight)
      : below;
    const left = Math.min(
      Math.max(margin, anchor.right - menu.offsetWidth),
      window.innerWidth - margin - menu.offsetWidth,
    );
    menu.style.top = `${top}px`;
    menu.style.left = `${left}px`;
  }

  function closeCardMenus() {
    document.querySelectorAll('.card-menu').forEach((menu) => {
      if (menu.matches(':popover-open')) menu.hidePopover();
    });
  }

  const previewDialog = document.querySelector('#preview-dialog');
  const previewFrame = previewDialog?.querySelector('.preview-dialog__frame');

  function openPreviewDialog(title, description, content) {
    if (!previewDialog || !previewFrame) return;
    previewDialog.querySelector('#preview-dialog-title').textContent = title;
    previewDialog.querySelector('#preview-dialog-description').textContent = description;
    previewFrame.replaceChildren(content);
    if (!previewDialog.open) previewDialog.showModal();
  }

  // The enlarged view is a second live instance of the overlay, scaled to fill
  // most of the window so its content is readable without leaving the page.
  function expandPreview(button) {
    const card = button.closest('.counter-card');
    const source = card?.querySelector('.preview-stage');
    const sourceFrame = source?.querySelector('iframe');
    if (!card || !source || !sourceFrame) return;
    const stage = source.cloneNode(false);
    stage.removeAttribute('style');
    stage.dataset.maxScale = '4';
    const frame = document.createElement('iframe');
    frame.src = sourceFrame.getAttribute('src');
    frame.width = sourceFrame.getAttribute('width');
    frame.height = sourceFrame.getAttribute('height');
    frame.title = sourceFrame.title;
    frame.setAttribute('scrolling', 'no');
    stage.append(frame);
    const resolution = card.querySelector('.meta-copy')?.textContent ?? '';
    openPreviewDialog(card.dataset.counterName ?? '', resolution, stage);
    fitPreview(stage);
    previewObserver?.observe(previewFrame);
  }

  function expandImage(button) {
    const gallery = button.closest('.gallery');
    const shown = gallery?.querySelector('.gallery__slide:not([hidden])');
    if (!shown) return;
    const image = document.createElement('img');
    image.src = shown.currentSrc || shown.src;
    image.alt = shown.alt;
    image.referrerPolicy = 'no-referrer';
    const card = button.closest('.counter-card');
    openPreviewDialog(card?.dataset.counterName ?? '', shown.alt, image);
  }

  function stepGallery(button, direction) {
    const gallery = button.closest('.gallery');
    const slides = Array.from(gallery?.querySelectorAll('.gallery__slide') ?? []);
    if (slides.length < 2) return;
    const index = (Number(gallery.dataset.galleryIndex) + direction + slides.length) % slides.length;
    slides.forEach((slide, position) => {
      slide.hidden = position !== index;
    });
    gallery.dataset.galleryIndex = String(index);
    const count = gallery.querySelector('.gallery__count');
    if (count) count.textContent = `${index + 1} / ${slides.length}`;
  }

  previewDialog?.addEventListener('close', () => {
    if (previewFrame) previewObserver?.unobserve(previewFrame);
    // Dropping the frame stops the enlarged overlay's sockets and timers.
    previewFrame?.replaceChildren();
  });
  previewDialog?.addEventListener('click', (event) => {
    if (event.target === previewDialog) previewDialog.close();
  });

  const { closeDialog, loadCounterSettings } = createSettingsEditor({
    results, requestJson, notify, setBusy, errorText, absoluteHttpUrl,
  });

  document.addEventListener('click', (event) => {
    const control = event.target.closest('[data-action]');
    if (!control) return;
    const action = control.dataset.action;
    const menu = control.closest('.card-menu');
    if (menu) {
      menu.hidePopover();
      cardMenuToggle(menu)?.focus({ preventScroll: true });
    }
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
    } else if (action === 'counter-settings') {
      loadCounterSettings(control.dataset.folder);
    } else if (action === 'app-update') {
      runAppUpdate(control);
    } else if (action === 'copy-metadata') {
      activateMetadata(control, event);
    } else if (action === 'expand-preview') {
      expandPreview(control);
    } else if (action === 'expand-image') {
      expandImage(control);
    } else if (action === 'gallery-prev') {
      stepGallery(control, -1);
    } else if (action === 'gallery-next') {
      stepGallery(control, 1);
    } else if (action === 'close-preview') {
      previewDialog?.close();
    }
  });

  document.addEventListener('toggle', (event) => {
    if (event.target instanceof HTMLElement
      && event.target.classList.contains('card-menu')
      && event.newState === 'open') positionCardMenu(event.target);
  }, true);
  window.addEventListener('resize', closeCardMenus);
  window.addEventListener('scroll', closeCardMenus, { passive: true });

  searchForm?.addEventListener('submit', (event) => event.preventDefault());
  searchInput?.addEventListener('input', scheduleSearch);
  document.addEventListener('keydown', (event) => {
    if (event.key !== '/' || event.ctrlKey || event.altKey || event.metaKey || event.shiftKey || !searchInput) return;
    if (event.target.closest('input, textarea, select, [contenteditable="true"]')) return;
    event.preventDefault();
    searchInput.focus();
  });

  window.addEventListener('storage', (event) => {
    if (event.key === PINNED_KEY) initializePins();
    if (event.key === INSTALLED_COUNT_KEY) {
      const installedCount = Number(event.newValue);
      if (event.newValue !== null && Number.isInteger(installedCount) && installedCount >= 0) {
        setCount('installed', installedCount);
        // Never reload unprompted: that would discard an open settings dialog
        // in this tab.
        if (pageKey() === 'installed') {
          notify('Installed overlays changed in another tab.', 'info', 0, {
            label: 'Reload',
            run: () => window.location.reload(),
          });
        }
      }
    }
    if (event.key === AVAILABLE_COUNT_KEY) setCount('available', Number(event.newValue));
  });

  configurePageChrome();
  initializeCounts();
  initializePins({ sanitize: pageKey() === 'installed' && !window.location.search });
  if (pageKey() === 'installed' && searchInput?.value) filterInstalled();
  observePreviews();
  initializeRuntimeSettings();

  const requestedOverlay = new URLSearchParams(window.location.search).get('overlay');
  if (window.location.pathname === '/settings' && requestedOverlay) {
    const title = document.querySelector('#page-title');
    if (title) title.textContent = 'Overlay settings';
    loadCounterSettings(requestedOverlay, { inline: true });
  }
})();
