import { createSettingsEditor } from './homepage-settings.js';

(() => {
  'use strict';
  const context = JSON.parse(document.querySelector('#vosu-context')?.textContent ?? '{}');
  const csrf = context.csrf ?? '';

  const embedParameters = new URLSearchParams(window.location.search);
  if (window.location.pathname === '/settings' && embedParameters.get('overlay')
      && embedParameters.get('embed') === '1') {
    document.body.classList.add('settings-embed');
    document.querySelector('.site-header')?.remove();
    document.querySelector('#attachment-status')?.remove();
    document.querySelector('#update-notice')?.remove();
  }

  const results = document.querySelector('#counter-results');
  const searchInput = document.querySelector('.search-bar');
  const searchForm = document.querySelector('.search-form');
  const toast = document.querySelector('#toast');
  const toastMessage = toast?.querySelector('.toast__message');
  const toastAction = toast?.querySelector('.toast__action');

  const PINNED_KEY = 'vosu.pinned-overlays.v1';
  const INSTALLED_COUNT_KEY = 'total-installed-overlays';
  const AVAILABLE_COUNT_KEY = 'total-available-overlays';

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
  }

  function controlValue(control) {
    if (control.dataset.id === 'ALLOWED_IPS') return control.value.split(/[\n,]+/).map((entry) => entry.trim()).filter(Boolean).join(',');
    return control.type === 'checkbox' ? control.checked : control.value;
  }

  async function initializeApiKeys() {
    const runtime = document.querySelector('#runtime-settings');
    if (!runtime || new URLSearchParams(location.search).get('embed') === '1') return;
    const section = document.createElement('section');
    section.className = 'api-keys';
    section.setAttribute('aria-labelledby', 'api-keys-title');
    section.innerHTML = `<h2 id="api-keys-title">API keys</h2>
      <p>Give trusted software only the access it needs. Remote connections still require ALLOWED_IPS. Use TLS outside loopback.</p>
      <form class="api-key-create"><label>Name <input name="name" maxlength="64" required autocomplete="off"></label>
      <fieldset><legend>Permissions (fixed when created)</legend>
      <label><input type="checkbox" name="scope" value="settings:write"> Write overlay settings and command-socket settings</label>
      <label><input type="checkbox" name="scope" value="profiles:manage"> Create, rename, activate, copy and delete profiles</label></fieldset>
      <button class="button button-cyan" type="submit">Create API key</button></form>
      <div class="api-key-secret" hidden></div><p class="api-key-status" role="status"></p><div class="api-key-list"></div>`;
    runtime.after(section);
    const status = section.querySelector('.api-key-status');
    const list = section.querySelector('.api-key-list');
    const secret = section.querySelector('.api-key-secret');
    const form = section.querySelector('form');
    const post = (payload) => requestJson('/api/keys', { method: 'POST', body: JSON.stringify(payload) });
    const button = (text, run) => {
      const control = document.createElement('button');
      control.type = 'button'; control.className = 'button'; control.textContent = text;
      control.addEventListener('click', async () => {
        control.disabled = true;
        try { await run(); } catch (error) { status.textContent = errorText(error); }
        finally { control.disabled = false; }
      });
      return control;
    };
    async function refresh() {
      const { keys } = await requestJson('/api/keys');
      list.replaceChildren();
      if (!keys.length) list.textContent = 'No API keys.';
      for (const entry of keys) {
        const row = document.createElement('article');
        row.className = 'api-key-entry';
        const title = document.createElement('h3'); title.textContent = entry.name;
        const details = document.createElement('p');
        details.textContent = `${entry.scopes.join(', ')} · ${entry.display}… · Created ${entry.created} · Last used ${entry.last_used || 'Never'}`;
        const name = document.createElement('input'); name.value = entry.name; name.maxLength = 64;
        name.setAttribute('aria-label', `Rename ${entry.name}`);
        const rename = button('Rename', async () => {
          await post({ action: 'rename', id: entry.id, name: name.value }); await refresh();
          status.textContent = 'Key renamed.';
        });
        const confirmation = document.createElement('div'); confirmation.hidden = true;
        confirmation.setAttribute('role', 'group'); confirmation.setAttribute('aria-label', `Revoke ${entry.name}?`);
        const warning = document.createElement('p');
        warning.textContent = `Revoke ${entry.name}? Software using this key will immediately lose write access, including open sockets.`;
        confirmation.append(warning, button('Confirm revoke', async () => {
          await post({ action: 'revoke', id: entry.id }); await refresh();
          secret.replaceChildren(); secret.hidden = true; status.textContent = 'Key revoked.';
        }), button('Cancel', () => { confirmation.hidden = true; }));
        row.append(title, details, name, rename, button('Revoke', () => {
          confirmation.hidden = false; confirmation.querySelector('button').focus();
        }), confirmation);
        list.append(row);
      }
    }
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      const submit = form.querySelector('button'); submit.disabled = true;
      secret.replaceChildren(); secret.hidden = true;
      try {
        const scopes = [...form.querySelectorAll('[name="scope"]:checked')].map((input) => input.value);
        if (!scopes.length) throw new Error('Select at least one permission.');
        const result = await post({ action: 'create', name: form.elements.name.value, scopes });
        const warning = document.createElement('p'); warning.textContent = "Copy this key now. It won't be shown again. Keep it secret.";
        const value = document.createElement('input'); value.readOnly = true; value.value = result.key;
        value.setAttribute('aria-label', 'New API key (shown once)');
        secret.append(warning, value, button('Copy key', async () => {
          await navigator.clipboard.writeText(value.value); status.textContent = 'Key copied.';
        }), button('Dismiss key', () => { secret.replaceChildren(); secret.hidden = true; }));
        secret.hidden = false; value.focus(); value.select();
        form.reset(); await refresh(); status.textContent = 'API key created.';
      } catch (error) { status.textContent = errorText(error); }
      finally { submit.disabled = false; }
    });
    try { await refresh(); } catch (error) { status.textContent = errorText(error); }
  }

  function initializeDashboardStatus() {
    const header = document.querySelector('#attachment-status');
    if (!header) return;
    async function refresh() {
      try {
        const status = await requestJson('/api/dashboard/status');
        if (header) {
          const instances = status.instances ?? [];
          header.textContent = status.available === false ? 'osu! attachment status unavailable'
            : instances.length === 0 ? 'osu! not detected · waiting for a game'
              : instances.map((instance) => `osu! ${instance.client} · PID ${instance.pid} · ${instance.version || 'version unknown'} · ${instance.state} · polling ${instance.pollIntervalMs} ms / precise ${instance.precisePollIntervalMs} ms`).join(' | ');
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
        if (header) header.textContent = 'osu! attachment status unavailable';
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
      const key = 'vosu.catalog-notice-seen.v1';
      banner.open = storageGet(key) !== '1';
      storageSet(key, '1');
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
    form.append(apply, keep, networkStatus);
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
  initializeApiKeys();
  initializePresentation();
  initializeDashboardStatus();

  const requestedOverlay = new URLSearchParams(window.location.search).get('overlay');
  if (window.location.pathname === '/settings' && requestedOverlay) {
    const title = document.querySelector('#page-title');
    if (title) title.textContent = 'Overlay settings';
    loadCounterSettings(requestedOverlay, { inline: true });
  }
})();
