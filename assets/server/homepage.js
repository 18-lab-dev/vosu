import { createSettingsEditor, createActionDialog, createOverflow } from './homepage-settings.js';

(() => {
  'use strict';
  const context = JSON.parse(document.querySelector('#vosu-context')?.textContent ?? '{}');
  const csrf = context.csrf ?? '';


  const results = document.querySelector('#counter-results');
  const searchInput = document.querySelector('.search-bar');
  const searchForm = document.querySelector('.search-form');
  const toast = document.querySelector('#toast');
  const toastMessage = toast?.querySelector('.toast__message');
  const toastAction = toast?.querySelector('.toast__action');

  const PINNED_KEY = 'vosu.pinned-overlays.v1';
  const THEME_KEY = 'vosu.theme.v1';
  const COUNT_KEYS = {
    installed: 'total-installed-overlays',
    available: 'total-available-overlays',
    tools: 'total-tools',
  };
  const INSTALLED_COUNT_KEY = COUNT_KEYS.installed;
  const AVAILABLE_COUNT_KEY = COUNT_KEYS.available;

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

  // WORKSTATION (dark) is the default; a first visit follows the system
  // preference until the user picks a mode, which then persists. theme-init.js
  // (THEME_KEY must match it) applies the same choice before first paint;
  // this module owns the toggle and later system-preference changes.
  const lightQuery = window.matchMedia?.('(prefers-color-scheme: light)') ?? null;
  const MODE_NAMES = { dark: 'Workstation', light: 'Drafting' };

  function storedTheme() {
    const value = storageGet(THEME_KEY);
    return value === 'dark' || value === 'light' ? value : null;
  }

  function currentTheme() {
    const value = document.documentElement.dataset.theme;
    if (value === 'dark' || value === 'light') return value;
    return lightQuery?.matches ? 'light' : 'dark';
  }

  function applyTheme(theme) {
    document.documentElement.dataset.theme = theme;
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', theme === 'light' ? '#f2f1ec' : '#111214');
    const next = theme === 'light' ? 'dark' : 'light';
    document.querySelectorAll('[data-action="toggle-theme"]').forEach((toggle) => {
      toggle.setAttribute('aria-label', `Colour mode: ${MODE_NAMES[theme]}. Switch to ${MODE_NAMES[next]}`);
      toggle.title = `Switch to ${MODE_NAMES[next]} mode`;
    });
  }

  function toggleTheme() {
    const next = currentTheme() === 'light' ? 'dark' : 'light';
    storageSet(THEME_KEY, next);
    applyTheme(next);
  }

  applyTheme(storedTheme() ?? (lightQuery?.matches ? 'light' : 'dark'));
  lightQuery?.addEventListener?.('change', (event) => {
    if (!storedTheme()) applyTheme(event.matches ? 'light' : 'dark');
  });

  function errorText(value) {
    if (value instanceof Error) return value.message;
    if (typeof value === 'string') return value;
    try {
      return JSON.stringify(value);
    } catch (_error) {
      return String(value);
    }
  }

  function navigateDashboard(url) {
    // A cross-port navigation must not be rejected because the previous
    // dashboard hostname is absent from the overlay header allowlist.
    const link = document.createElement('a');
    link.href = url;
    link.rel = 'noreferrer';
    link.referrerPolicy = 'no-referrer';
    link.hidden = true;
    document.body.append(link);
    link.click();
    link.remove();
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
        'X-Vosu-CSRF': csrf,
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
        throw new Error(`Unexpected response from vosu (${response.status})`);
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

  const query = new URLSearchParams(window.location.search);
  const requestedOverlay = window.location.pathname === '/settings' ? query.get('overlay') : null;
  const requestedTool = window.location.pathname === '/settings' && !requestedOverlay ? query.get('tool') : null;

  function pageKey() {
    if (window.location.pathname === '/available') return 'available';
    if (window.location.pathname === '/tools') return 'tools';
    if (window.location.pathname === '/settings') return 'settings';
    return 'installed';
  }

  // Overlay and tool settings live under /settings but belong to the sheet
  // they were opened from.
  function sheetKey() {
    if (requestedOverlay) return 'installed';
    if (requestedTool) return 'tools';
    return pageKey();
  }

  function configurePageChrome() {
    const pages = {
      installed: ['01', 'Overlays / installed', 'Installed overlays', 'Search installed overlays'],
      available: ['02', 'Overlays / catalog', 'Available overlays', 'Search the overlay catalog'],
      tools: ['03', 'Tools', 'Tools', 'Search tools'],
      settings: ['04', 'Settings / runtime', 'Settings', 'Search settings'],
    };
    const current = sheetKey();
    document.querySelectorAll('.nav-link[data-page]').forEach((link) => {
      if (link.dataset.page === current) link.setAttribute('aria-current', 'page');
      else link.removeAttribute('aria-current');
    });
    let [number, kicker, heading, search] = pages[current];
    if (requestedOverlay) [kicker, heading] = ['Overlays / settings', 'Overlay settings'];
    if (requestedTool) [kicker, heading] = ['Tools / settings', 'Tool settings'];
    const title = document.querySelector('#page-title');
    if (title) title.textContent = heading;
    const numberLabel = document.querySelector('#page-number');
    if (numberLabel) numberLabel.textContent = number;
    const kickerLabel = document.querySelector('#page-kicker');
    if (kickerLabel) kickerLabel.textContent = kicker;
    document.title = `${heading} · vosu`;
    if (searchInput) {
      searchInput.placeholder = search;
      searchInput.setAttribute('aria-label', search);
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
    storageSet(COUNT_KEYS[kind], value);
  }

  function initializeCounts() {
    for (const kind of Object.keys(COUNT_KEYS)) {
      const serverCount = countFromBadge(kind);
      if (serverCount !== null) {
        setCount(kind, serverCount);
        continue;
      }
      const cached = storageGet(COUNT_KEYS[kind]);
      // Number(null) is 0, which would invent a count for a page never visited.
      if (cached !== null && Number.isInteger(Number(cached)) && Number(cached) >= 0) setCount(kind, Number(cached));
    }
  }

  // The Tools page keeps its own count current; elsewhere one lookup keeps
  // the badge honest. A vosu without the tool host answers 404, which leaves
  // the badge empty.
  async function refreshToolCount() {
    if (pageKey() === 'tools') return;
    try {
      const { tools } = await requestJson('/api/tools');
      if (Array.isArray(tools)) setCount('tools', tools.length);
    } catch (_error) {
      // The badge is informational; the Tools page reports real failures.
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
    // Never magnify a preview: its declared pixel dimensions remain meaningful.
    const scale = Math.min(1, availableWidth / width, availableHeight / height);
    let chip = container.querySelector('.preview-scale');
    if (!chip) {
      chip = document.createElement('span');
      chip.className = 'preview-scale';
      container.append(chip);
    }
    chip.hidden = scale >= 1;
    chip.textContent = `${Math.round(scale * 100)}%`;
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
    const text = document.createElement('p');
    text.textContent = 'Install an overlay from the catalog, or copy an overlay folder into the overlays directory on this computer; vosu picks it up without a restart.';
    const actions = document.createElement('div');
    actions.className = 'counter-actions';
    const link = document.createElement('a');
    link.className = 'button button-primary';
    link.href = '/available';
    link.textContent = 'Browse available overlays';
    const open = document.createElement('button');
    open.type = 'button';
    open.className = 'button';
    open.dataset.action = 'open-overlays-root';
    open.textContent = 'Open overlays folder';
    actions.append(link, open);
    empty.append(heading, text, actions);
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
      card.draggable = isPinned;
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

  function initializePinDrag() {
    if (pageKey() !== 'installed' || !results) return;
    let dragged = null;
    let destination = null;
    let after = false;
    function clear() {
      results.querySelectorAll('.is-dragging, .drop-before, .drop-after').forEach((card) => card.classList.remove('is-dragging', 'drop-before', 'drop-after'));
      dragged = null; destination = null;
    }
    results.addEventListener('dragstart', (event) => {
      const card = event.target.closest('.counter-card.is-pinned');
      if (!card || event.target.closest('a, button, input, iframe')) { event.preventDefault(); return; }
      dragged = card.dataset.overlayId;
      card.classList.add('is-dragging');
      event.dataTransfer.effectAllowed = 'move';
      event.dataTransfer.setData('text/plain', dragged);
    });
    results.addEventListener('dragover', (event) => {
      if (!dragged) return;
      const card = event.target.closest('.counter-card.is-pinned');
      results.querySelectorAll('.drop-before, .drop-after').forEach((item) => item.classList.remove('drop-before', 'drop-after'));
      destination = null;
      if (!card || card.dataset.overlayId === dragged) return;
      event.preventDefault(); event.dataTransfer.dropEffect = 'move';
      const bounds = card.getBoundingClientRect();
      after = event.clientX > bounds.left + bounds.width / 2;
      destination = card.dataset.overlayId;
      card.classList.add(after ? 'drop-after' : 'drop-before');
    });
    results.addEventListener('drop', (event) => {
      if (dragged && destination) {
        event.preventDefault();
        const ordered = state.pinnedIds.filter((id) => id !== dragged);
        ordered.splice(ordered.indexOf(destination) + Number(after), 0, dragged);
        state.pinnedIds = ordered; storePinnedIds(); renderPins();
      }
      clear();
    });
    results.addEventListener('dragleave', (event) => {
      if (!results.contains(event.relatedTarget)) {
        results.querySelectorAll('.drop-before, .drop-after').forEach((card) => card.classList.remove('drop-before', 'drop-after'));
        destination = null;
      }
    });
    results.addEventListener('dragend', clear);
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

  function confirmOverlay(title, description, action) {
    const dialog = document.querySelector('#confirm-dialog');
    if (!dialog || dialog.open) return Promise.resolve(false);
    dialog.querySelector('h2').textContent = title;
    dialog.querySelector('p').textContent = description;
    dialog.querySelector('[value="confirm"]').textContent = action;
    dialog.returnValue = '';
    return new Promise((resolve) => {
      dialog.addEventListener('close', () => resolve(dialog.returnValue === 'confirm'), { once: true });
      dialog.showModal();
    });
  }

  async function downloadCounter(button, update) {
    const downloadUrl = button.dataset.downloadUrl;
    const installName = button.dataset.installName;
    if (!downloadUrl || !installName || state.pending.has(installName)) return;
    if (update && !await confirmOverlay(`Update ${installName}?`, 'Replace this overlay with the latest release?', 'Update')) return;

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
    if (!folder) return;
    let description = `${folder}\nSize unavailable. This cannot be undone.`;
    try {
      const details = await requestJson(`/api/counters/details/${encodeURIComponent(folder)}`);
      const size = new Intl.NumberFormat().format(details.bytes);
      description = `${details.path}\n${details.complete ? '' : 'At least '}${size} bytes on disk${details.complete ? '' : ' (scan incomplete)'}. This cannot be undone.`;
    } catch (_error) {
      // A disappearing or unreadable file must not prevent deletion.
    }
    if (!await confirmOverlay(`Delete ${folder}?`, description, 'Delete')) return;
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

  async function openOverlaysRoot(button) {
    setBusy(button, true, 'Opening…');
    try {
      await requestJson('/api/counters/open-root', { method: 'POST' });
      notify('Opened the overlays folder.', 'success');
    } catch (error) {
      notify(`Could not open the overlays folder: ${errorText(error)}`, 'error', 5000);
    } finally {
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
      notify('vosu finished updating.', 'success');
      document.querySelector('#update-notice')?.remove();
    } catch (error) {
      notify(`vosu could not update: ${errorText(error)}`, 'error', 5000);
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
        headers: { Accept: 'text/html', 'X-Vosu-CSRF': csrf },
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
    if (pageKey() === 'tools') {
      state.filterTools?.(searchInput?.value ?? '');
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
    document.querySelectorAll('.settings-index a').forEach((link) => {
      link.hidden = Boolean(document.getElementById(link.hash.slice(1))?.hidden);
    });
  }

  // A sheet index beside the runtime form; groups are numbered by CSS
  // counters, so the index repeats the same numbers.
  function initializeSettingsIndex() {
    const form = document.querySelector('#runtime-settings');
    if (!form || !results) return;
    const index = document.createElement('nav');
    index.className = 'settings-index';
    index.setAttribute('aria-label', 'Settings sections');
    form.querySelectorAll(':scope > .settings-group').forEach((group, position) => {
      group.id ||= `settings-group-${position + 1}`;
      const link = document.createElement('a');
      link.href = `#${group.id}`;
      const number = document.createElement('span');
      number.textContent = String(position + 1).padStart(2, '0');
      link.append(number, group.querySelector('legend')?.textContent ?? '');
      index.append(link);
    });
    if (index.childElementCount) results.append(index);
  }

  function controlValue(control) {
    if (control.dataset.id === 'ALLOWED_IPS') return control.value.split(/[\n,]+/).map((entry) => entry.trim()).filter(Boolean).join(',');
    return control.type === 'checkbox' ? control.checked : control.value;
  }

  async function initializeApiKeys() {
    const runtime = document.querySelector('#runtime-settings');
    if (!runtime) return;
    const section = document.createElement('fieldset');
    section.className = 'settings-group';
    section.innerHTML = '<legend>API keys</legend>';
    const status = document.createElement('p');
    status.setAttribute('role', 'status');
    const list = document.createElement('div');
    const post = (payload) => requestJson('/api/keys', { method: 'POST', body: JSON.stringify(payload) });
    function row(title, description, control, label = false) {
      const item = document.createElement(label ? 'label' : 'div');
      item.className = 'settings-item';
      const copy = document.createElement('span'); copy.className = 'settings-item__copy';
      const name = document.createElement('span'); name.className = 'settings-item__name'; name.textContent = title;
      const detail = document.createElement('span'); detail.className = 'settings-item__description'; detail.textContent = description;
      copy.append(name, detail);
      const controls = document.createElement('span'); controls.className = 'settings-item__control'; controls.append(control);
      item.append(copy, controls);
      return item;
    }
    function button(text, run, primary = false) {
      const control = document.createElement('button');
      control.type = 'button'; control.className = primary ? 'button button-primary' : 'button'; control.textContent = text;
      control.addEventListener('click', async () => {
        setBusy(control, true);
        try { await run(); } catch (error) { status.textContent = errorText(error); }
        finally { setBusy(control, false); }
      });
      return control;
    }
    function keyDialog(title, submitText, run, { name, description } = {}) {
      const { dialog, body } = createActionDialog(title);
      const form = document.createElement('form'); form.className = 'dialog-form';
      const input = document.createElement('input');
      input.className = 'settings-control'; input.name = 'name'; input.required = true; input.maxLength = 64; input.autocomplete = 'off'; input.value = name ?? '';
      if (name !== null) form.append(row('Name', 'A label to recognize this key.', input, true));
      if (description) {
        const text = document.createElement('p'); text.textContent = description; form.append(text);
      }
      if (name === undefined) {
        for (const [scope, title, text] of [
          ['settings:write', 'Write settings', 'Save overlay settings and command-socket settings.'],
          ['profiles:manage', 'Manage profiles', 'Create, copy, rename, set the default and delete profiles.'],
        ]) {
          const toggle = document.createElement('input'); toggle.type = 'checkbox'; toggle.name = 'scope'; toggle.value = scope;
          form.append(row(title, text, toggle, true));
        }
        const note = document.createElement('p'); note.className = 'field-help'; note.textContent = 'Permissions are fixed when the key is created.'; form.append(note);
      }
      const error = document.createElement('p'); error.setAttribute('role', 'alert');
      const actions = document.createElement('div'); actions.className = 'dialog-actions';
      const cancel = button('Cancel', () => dialog.close());
      const submit = document.createElement('button'); submit.type = 'submit'; submit.className = 'button button-primary'; submit.textContent = submitText;
      actions.append(cancel, submit); form.append(error, actions); body.append(form);
      form.addEventListener('submit', async (event) => {
        event.preventDefault(); setBusy(submit, true);
        try {
          const result = await run(input.value, [...form.querySelectorAll('[name="scope"]:checked')].map((control) => control.value));
          dialog.close(); await refresh();
          if (result?.key) {
            const shown = createActionDialog('API key created');
            const warning = document.createElement('p'); warning.textContent = "Copy this key now. It won't be shown again. Keep it secret.";
            const value = document.createElement('input'); value.className = 'settings-control'; value.readOnly = true; value.value = result.key;
            value.setAttribute('aria-label', 'New API key (shown once)');
            const actions = document.createElement('div'); actions.className = 'dialog-actions';
            actions.append(button('Copy key', async () => {
              await navigator.clipboard.writeText(value.value); status.textContent = 'Key copied.';
            }), button('Done', () => shown.dialog.close(), true));
            shown.body.append(warning, value, actions); value.focus(); value.select();
          }
        } catch (failure) { error.textContent = errorText(failure); }
        finally { setBusy(submit, false); }
      });
      (name === null ? cancel : input).focus();
    }
    async function refresh() {
      const { keys } = await requestJson('/api/keys');
      list.replaceChildren();
      for (const entry of keys) {
        const { wrapper, menu } = createOverflow(`Actions for ${entry.name}`);
        menu.append(button('Rename', () => {
          menu.hidePopover();
          keyDialog('Rename API key', 'Save name', (name) => post({ action: 'rename', id: entry.id, name }), { name: entry.name });
        }), button('Revoke', () => {
          menu.hidePopover();
          keyDialog('Revoke API key', 'Revoke key', () => post({ action: 'revoke', id: entry.id }), {
            name: null, description: `Revoke “${entry.name}”? Software using this key will immediately lose write access, including open sockets.`,
          });
        }));
        menu.querySelectorAll('button').forEach((control) => { control.className = ''; });
        list.append(row(entry.name, `${entry.scopes.join(', ')} · ${entry.display}… · Created ${entry.created} · Last used ${entry.last_used || 'Never'}`, wrapper));
      }
    }
    section.append(row('API access', 'Give trusted software only the access it needs. Remote connections still require ALLOWED_IPS. Use TLS outside loopback.',
      button('Create API key', () => keyDialog('Create API key', 'Create key', (name, scopes) => {
        if (!scopes.length) throw new Error('Select at least one permission.');
        return post({ action: 'create', name, scopes });
      }), true)), list, status);
    runtime.querySelector('.settings-empty')?.before(section);
    if (!section.isConnected) runtime.append(section);
    try { await refresh(); } catch (error) { status.textContent = errorText(error); }
  }

  function initializeDashboardStatus() {
    const header = document.querySelector('#attachment-status');
    if (!header) return;
    const clientCell = document.querySelector('[data-status-cell="client"]');
    const pollCell = document.querySelector('[data-status-cell="poll"]');
    // Rewriting identical text would make some screen readers re-announce the
    // live region every two seconds.
    function setText(element, text, title = text) {
      if (!element) return;
      if (element.textContent !== text) element.textContent = text;
      if (element.title !== title) element.title = title;
    }
    function setCell(cell, text) {
      if (!cell) return;
      cell.hidden = !text;
      setText(cell.querySelector('.status-value'), text);
    }
    function showAttachment(state, text, detail = text) {
      header.dataset.state = state;
      setText(header, text, detail);
    }
    async function refresh() {
      try {
        const status = await requestJson('/api/dashboard/status');
        const instances = status.instances ?? [];
        if (status.available === false) {
          showAttachment('error', 'Attachment status unavailable');
          setCell(clientCell, '');
          setCell(pollCell, '');
        } else if (instances.length === 0) {
          showAttachment('idle', 'Not detected · waiting for a game');
          setCell(clientCell, '');
          setCell(pollCell, '');
        } else {
          const detail = instances.map((instance) => `osu! ${instance.client} · PID ${instance.pid} · ${instance.version || 'version unknown'} · ${instance.state} · polling ${instance.pollIntervalMs} ms / precise ${instance.precisePollIntervalMs} ms`).join(' | ');
          const reading = instances.filter((instance) => instance.state === 'reading').length;
          const lead = instances[0];
          showAttachment(
            reading > 0 ? 'reading' : 'starting',
            instances.length > 1 ? `${instances.length} instances · ${reading} reading` : lead.state,
            detail,
          );
          setCell(clientCell, instances.map((instance) => `${instance.client} ${instance.version || 'version unknown'} · PID ${instance.pid}`).join(' | '));
          setCell(pollCell, `${lead.pollIntervalMs} ms / precise ${lead.precisePollIntervalMs} ms`);
        }
        for (const card of installedCards()) {
          const chip = card.querySelector('.connected-chip');
          if (!chip) continue;
          const count = status.connections?.[card.dataset.overlayId];
          chip.hidden = !Number.isInteger(count) || count < 1;
          chip.textContent = chip.hidden ? '' : `● ${count} connected`;
          chip.title = 'Identified WebSocket connections, including dashboard previews; not unique users. Unidentified sockets cannot be counted.';
        }
      } catch (_error) {
        showAttachment('error', 'Attachment status unavailable');
        setCell(clientCell, '');
        setCell(pollCell, '');
        document.querySelectorAll('.connected-chip').forEach((chip) => { chip.hidden = true; });
      } finally {
        window.setTimeout(refresh, 2000);
      }
    }
    refresh();
  }

  function cidrMayIncludePublic(entry) {
    const parts = entry.split('/');
    if (parts.length !== 2 || !/^\d+$/.test(parts[1])) return false;
    let prefix = Number(parts[1]);
    let bits = 32;
    let value;
    if (parts[0].includes(':')) {
      bits = 128;
      try {
        const address = new URL(`http://[${parts[0]}]/`).hostname.slice(1, -1);
        const halves = address.split('::');
        const left = halves[0].split(':').filter(Boolean);
        const right = (halves[1] ?? '').split(':').filter(Boolean);
        const words = [...left, ...Array(8 - left.length - right.length).fill('0'), ...right];
        value = words.reduce((number, word) => (number << 16n) | BigInt(`0x${word}`), 0n);
      } catch (_error) { return false; }
      if (prefix >= 96 && value >> 32n === 0xffffn) {
        value &= 0xffffffffn;
        prefix -= 96;
        bits = 32;
      }
    } else {
      const octets = parts[0].split('.');
      if (octets.length !== 4 || octets.some((part) => !/^\d+$/.test(part) || Number(part) > 255)) return false;
      value = octets.reduce((number, part) => (number << 8n) | BigInt(part), 0n);
    }
    if (prefix < 0 || prefix > bits) return false;
    const privateRanges = bits === 32
      ? [[0x0a000000n, 8], [0xac100000n, 12], [0xc0a80000n, 16],
        [0x64400000n, 10], [0x7f000000n, 8], [0xa9fe0000n, 16]]
      : [[0xfcn << 120n, 7], [0xfe80n << 112n, 10], [1n, 128]];
    return !privateRanges.some(([network, length]) => prefix >= length
      && value >> BigInt(bits - length) === network >> BigInt(bits - length));
  }

  function initializePresentation() {
    const banner = document.querySelector('.catalog-notice');
    if (banner) {
      const key = 'vosu.catalog-notice-dismissed.v1';
      const about = document.createElement('button');
      about.type = 'button'; about.className = 'catalog-about'; about.textContent = 'About community overlays';
      const dismiss = document.createElement('button');
      dismiss.type = 'button'; dismiss.className = 'button'; dismiss.textContent = "Don't show again";
      function show(visible) { banner.hidden = !visible; banner.open = visible; about.hidden = visible; }
      dismiss.addEventListener('click', () => { storageSet(key, '1'); show(false); });
      about.addEventListener('click', () => show(true));
      // Dismissed, the notice leaves only this link, tucked under the search
      // field in the page head so the listing keeps the usual head-to-grid gap.
      banner.append(dismiss);
      (document.querySelector('.page-head') ?? banner.parentElement).append(about);
      show(storageGet(key) !== '1');
    }
    for (const id of ['POLL_RATE', 'PRECISE_DATA_POLL_RATE', 'V2_SUBSCRIPTION_DEBOUNCE']) {
      const input = document.querySelector(`[data-id="${id}"]`);
      if (!input) continue;
      const suffix = document.createElement('span');
      suffix.className = 'settings-unit';
      suffix.textContent = 'ms';
      input.after(suffix);
    }
    const allowlist = document.querySelector('[data-id="ALLOWED_IPS"]');
    const warning = document.querySelector('#allowlist-warning');
    function updateWarning() {
      if (!warning || !allowlist) return;
      const broad = allowlist.value.split(/[\n,]+/).map((entry) => entry.trim()).filter((entry) =>
        cidrMayIncludePublic(entry)
          || (entry.includes('*') && !/^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(entry)));
      warning.textContent = broad.length ? `Warning: ${broad.join(', ')} may allow public addresses. Prefer a precise CIDR range, such as 100.64.0.0/10 for Tailscale.` : '';
    }
    allowlist?.addEventListener('input', updateWarning);
    updateWarning();
  }

  function initializeRuntimeSettings() {
    const form = document.querySelector('#runtime-settings');
    if (!form) return;
    const status = form.querySelector('.settings-status');
    const networkKeys = new Set(['SERVER_IP', 'SERVER_PORT', 'DASHBOARD_IP', 'DASHBOARD_PORT', 'ALLOWED_IPS']);
    const networkControls = [...form.querySelectorAll('[data-id]')].filter((field) => networkKeys.has(field.dataset.id));
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
      const controls = [control];
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
      setSavedStatus();
    }

    const apply = document.createElement('button');
    apply.type = 'button';
    apply.className = 'button button-primary';
    apply.textContent = 'Apply network settings';
    const networkStatus = document.createElement('p');
    networkStatus.setAttribute('role', 'status');
    const keep = document.createElement('button');
    keep.type = 'button';
    keep.className = 'button button-primary';
    keep.textContent = 'Keep network settings';
    keep.hidden = true;
    // Apply sits with the fields it commits instead of after unrelated groups.
    const networkActions = document.createElement('div');
    networkActions.className = 'network-actions';
    networkActions.append(apply, keep, networkStatus);
    const networkGroup = networkControls[0]?.closest('.settings-group');
    if (networkGroup) networkGroup.append(networkActions);
    else form.append(networkActions);
    const params = new URLSearchParams(location.search);
    const requestedDeadline = Number(params.get('network-deadline'));
    const now = Date.now();
    let deadline = Number.isFinite(requestedDeadline) && requestedDeadline > 0
      ? Math.max(now, Math.min(now + 30000, requestedDeadline)) : 0;
    let oldOrigin = context.rollbackOrigin || location.origin;
    let newOrigin = location.origin;
    let navigating = false;
    let timer;
    function finishNetwork() {
      deadline = 0;
      clearInterval(timer);
      keep.hidden = true;
      apply.disabled = false;
      params.delete('network-deadline');
      history.replaceState(null, '', `${location.pathname}${params.size ? `?${params}` : ''}`);
    }
    async function tickNetwork() {
      if (!deadline || navigating) return;
      const seconds = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
      networkStatus.textContent = `Keep these network settings? Automatically reverting in ${seconds} s.`;
      if (seconds === 0) {
        navigating = true;
        // Rollback runs in vosu even when this page has been closed.
        await new Promise((resolve) => setTimeout(resolve, 1500));
        navigateDashboard(`${oldOrigin}${location.pathname}`);
      } else if (newOrigin !== location.origin) {
        try {
          // Readable, JSON discovery proves this is vosu, not an unrelated listener.
          const response = await fetch(`${newOrigin}/api/dashboard`, {
            signal: AbortSignal.timeout(1000), cache: 'no-store',
          });
          const result = await response.json();
          if (response.ok && new URL(result.origin).port === new URL(newOrigin).port) {
            navigating = true;
            const target = new URL(location.href);
            target.protocol = new URL(newOrigin).protocol;
            target.host = new URL(newOrigin).host;
            target.searchParams.set('network-deadline', String(deadline));
            navigateDashboard(target.href);
          }
        } catch (_error) { /* The server timer restores an unreachable listener. */ }
      }
    }
    function startNetworkTimer() {
      apply.disabled = true;
      keep.hidden = newOrigin !== location.origin;
      timer = setInterval(tickNetwork, 1000);
      tickNetwork();
    }
    if (deadline) startNetworkTimer();
    keep.addEventListener('click', async () => {
      keep.disabled = true;
      try {
        await requestJson('/api/settingsSave', { method: 'POST', body: JSON.stringify({ _network_action: 'confirm' }) });
        finishNetwork();
        networkStatus.textContent = 'Network settings kept.';
      } catch (error) { networkStatus.textContent = errorText(error); }
      finally { keep.disabled = false; }
    });
    apply.addEventListener('click', async () => {
      const invalid = networkControls.find((field) => !field.checkValidity() || !field.value.trim());
      if (invalid) { invalid.reportValidity(); return; }
      const values = Object.fromEntries(networkControls.map((field) => [field.dataset.id, controlValue(field)]));
      values.ALLOWED_IPS = String(values.ALLOWED_IPS).split(/[\n,]+/).map((entry) => entry.trim()).filter(Boolean).join(',');
      const host = String(values.DASHBOARD_IP).trim();
      const browserHost = host === '0.0.0.0' || host === '::' ? location.hostname : host;
      newOrigin = `http://${browserHost.includes(':') && !browserHost.startsWith('[') ? `[${browserHost}]` : browserHost}:${values.DASHBOARD_PORT}`;
      oldOrigin = location.origin;
      apply.disabled = true;
      deadline = Date.now() + 30000;
      try {
        await requestJson('/api/settingsSave', { method: 'POST', body: JSON.stringify({ ...values, _network_action: 'apply' }) });
        startNetworkTimer();
      } catch (error) {
        apply.disabled = false;
        deadline = 0;
        networkStatus.textContent = errorText(error);
      }
    });

    // `change` fires when a text field loses focus (or Enter commits it) and
    // immediately for checkboxes; saves are serialized so they apply in order.
    form.addEventListener('change', (event) => {
      const control = event.target.closest('[data-id]');
      if (!control || control.disabled) return;
      if (networkKeys.has(control.dataset.id)) { networkStatus.textContent = 'Network changes are not saved until Apply.'; return; }
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
    if (sourceFrame.dataset.colorScheme) frame.dataset.colorScheme = sourceFrame.dataset.colorScheme;
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

  const { closeDialog, loadCounterSettings, loadToolSettings } = createSettingsEditor({
    results, requestJson, notify, setBusy, errorText, absoluteHttpUrl,
  });

  async function downloadDiagnostics(button) {
    setBusy(button, true, 'Generating report…');
    try {
      const response = await fetch('/api/generateReport', {
        method: 'POST', headers: { 'X-Vosu-CSRF': csrf },
      });
      if (!response.ok) throw new Error(`Report failed (${response.status})`);
      const url = URL.createObjectURL(await response.blob());
      const link = document.createElement('a');
      link.href = url;
      link.download = `vosu-report-${Date.now()}.html`;
      document.body.append(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 60000);
    } catch (error) {
      notify(`Could not generate report: ${errorText(error)}`, 'error', 5000);
    } finally {
      setBusy(button, false);
    }
  }

  document.addEventListener('click', (event) => {
    if (event.target instanceof HTMLDialogElement) {
      const bounds = event.target.getBoundingClientRect();
      if (event.clientX < bounds.left || event.clientX > bounds.right
        || event.clientY < bounds.top || event.clientY > bounds.bottom) event.target.close();
    }
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
    } else if (action === 'toggle-theme') {
      toggleTheme();
    } else if (action === 'diagnostics-report') {
      downloadDiagnostics(control);
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
    } else if (action === 'open-overlays-root') {
      openOverlaysRoot(control);
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
    if (event.key === COUNT_KEYS.tools) setCount('tools', Number(event.newValue));
    if (event.key === THEME_KEY) applyTheme(storedTheme() ?? (lightQuery?.matches ? 'light' : 'dark'));
  });

  configurePageChrome();
  initializeCounts();
  initializePins({ sanitize: pageKey() === 'installed' && !window.location.search });
  if (pageKey() === 'installed' && searchInput?.value) filterInstalled();
  observePreviews();
  initializeRuntimeSettings();
  initializeApiKeys();
  initializeSettingsIndex();
  initializePresentation();
  initializePinDrag();
  initializeDashboardStatus();
  refreshToolCount();

  if (pageKey() === 'settings' || pageKey() === 'available') {
    import('./dashboard-catalog.js').then(({ mountCatalogSources, mountCatalogOverlays }) => {
      const options = { root: results, requestJson, notify, errorText, setBusy, confirmAction: confirmOverlay };
      if (pageKey() === 'settings' && !requestedOverlay && !requestedTool) mountCatalogSources(options);
      else if (pageKey() === 'available') mountCatalogOverlays(options);
    }).catch((error) => notify(`Could not load catalog sources: ${errorText(error)}`, 'error', 5000));
  }

  if (requestedOverlay) {
    loadCounterSettings(requestedOverlay, { inline: true });
  } else if (requestedTool) {
    loadToolSettings(requestedTool, { inline: true });
  }

  // Loaded only on the Tools page so other pages do not parse it.
  if (pageKey() === 'tools') {
    import('./dashboard-tools.js').then(({ mountTools }) => {
      state.filterTools = mountTools({
        root: document.querySelector('#tools-root'),
        results,
        requestJson,
        notify,
        setBusy,
        errorText,
        absoluteHttpUrl,
        confirmAction: confirmOverlay,
        createActionDialog,
        createOverflow,
        currentTheme,
        setCount: (value) => setCount('tools', value),
      });
      if (searchInput?.value) state.filterTools?.(searchInput.value);
    }).catch((error) => notify(`Could not load the Tools page: ${errorText(error)}`, 'error', 0));
  }
})();
