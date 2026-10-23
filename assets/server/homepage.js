import { createSettingsEditor, createActionDialog, createOverflow, showModal } from './homepage-settings.js';
import { mountCatalogSources, mountCatalogOverlays } from './dashboard-catalog.js';

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
  const toastClose = toast?.querySelector('.toast__close');
  const pageStatus = document.querySelector('#page-status');
  const pageAlert = document.querySelector('#page-alert');
  const pageTitle = document.querySelector('#page-title');

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
    toastDelay: 0,
    announceTimer: 0,
    updates: new Map(),
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

  // Screen readers hear short summaries here, never whole lists. Clearing
  // first makes a repeated message announce again.
  function announce(message, urgent = false) {
    const region = urgent ? pageAlert : pageStatus;
    if (!region) return;
    region.textContent = '';
    window.setTimeout(() => { region.textContent = message; }, 60);
  }

  // Typing in a search field announces the result count once it settles.
  function announceSoon(message) {
    window.clearTimeout(state.announceTimer);
    state.announceTimer = window.setTimeout(() => announce(message), 700);
  }

  function hideToast() {
    window.clearTimeout(state.toastTimer);
    if (toast) toast.hidden = true;
  }

  function scheduleToastHide() {
    window.clearTimeout(state.toastTimer);
    if (state.toastDelay > 0 && toast && !toast.matches(':hover, :focus-within')) {
      state.toastTimer = window.setTimeout(hideToast, state.toastDelay);
    }
  }

  // Errors stay until dismissed; other messages fade after `delay`, paused
  // while the toast is hovered or focused. A zero delay keeps any toast.
  function notify(message, tone = 'info', delay = 3200, action = null) {
    if (!toast || !toastMessage || !toastAction) return;
    toastMessage.textContent = message;
    toast.dataset.tone = tone;
    toastAction.hidden = !action;
    toastAction.textContent = action?.label ?? '';
    toastAction.onclick = action
      ? () => {
        hideToast();
        action.run();
      }
      : null;
    toast.hidden = false;
    announce(message, tone === 'error');
    state.toastDelay = tone === 'error' ? 0 : delay;
    scheduleToastHide();
  }

  toast?.addEventListener('mouseenter', () => window.clearTimeout(state.toastTimer));
  toast?.addEventListener('mouseleave', scheduleToastHide);
  toast?.addEventListener('focusin', () => window.clearTimeout(state.toastTimer));
  toast?.addEventListener('focusout', scheduleToastHide);
  toastClose?.addEventListener('click', hideToast);

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

  // The server renders the page heading, current tab and search hint; only
  // inline overlay and tool settings refine the heading once loaded.

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
    // Cards never magnify a preview (its declared pixel size stays
    // meaningful); the enlarged view may, up to its stage's maxScale.
    const scale = Math.min(Number(stage.dataset.maxScale) || 1, availableWidth / width, availableHeight / height);
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

  // Same markup as the server's empty state (render.rs INSTALLED_EMPTY_STATE).
  function createInstalledEmptyState() {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    const heading = document.createElement('h2');
    heading.textContent = 'No overlays yet';
    const text = document.createElement('p');
    text.textContent = 'Install one, or copy an overlay folder into the overlays folder.';
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
      const name = card.dataset.counterTitle ?? card.dataset.counterName ?? id;
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

  // Every word must match (name and nickname, author, or a client), like
  // the server's catalog search and the Tools and Settings filters.
  function cardMatches(card, tokens) {
    if (tokens.length === 0) return true;
    const name = normalizeSearchText(`${card.dataset.counterName ?? ''} ${card.dataset.counterNickname ?? ''}`);
    const author = normalizeSearchText(card.dataset.counterAuthor);
    const compatible = (card.dataset.counterCompatible ?? '')
      .split(',')
      .filter(Boolean)
      .map(normalizeSearchText);
    return tokens.every((token) => name.includes(token)
      || author.includes(token)
      || (token !== 'vosu' && compatible.some((value) => value.includes(token))));
  }

  // Installed cards are filtered in place: replacing them would restart every
  // live preview iframe on each keystroke.
  function filterInstalled() {
    if (!results) return;
    const query = searchInput?.value ?? '';
    const tokens = queryTokens(query);
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
      hint.textContent = 'Try fewer words, an author, or a client such as tosu.';
      empty.append(heading, hint);
      results.append(empty);
    } else if (!showEmpty) {
      empty?.remove();
    }
    state.roving?.refresh();
    if (tokens.length) announceSoon(`${visible} of ${cards.length} overlays match`);
  }

  // `danger` marks irreversible actions with the danger button; Cancel keeps
  // the initial focus either way.
  function confirmOverlay(title, description, action, { danger = false } = {}) {
    const dialog = document.querySelector('#confirm-dialog');
    if (!dialog || dialog.open) return Promise.resolve(false);
    dialog.querySelector('h2').textContent = title;
    dialog.querySelector('p').textContent = description;
    const confirm = dialog.querySelector('[value="confirm"]');
    confirm.textContent = action;
    confirm.className = `button ${danger ? 'button-danger button-danger--solid' : 'button-primary'}`;
    dialog.returnValue = '';
    return new Promise((resolve) => {
      dialog.addEventListener('close', () => resolve(dialog.returnValue === 'confirm'), { once: true });
      showModal(dialog);
    });
  }

  async function downloadCounter(button, update) {
    const downloadUrl = button.dataset.downloadUrl;
    const installName = button.dataset.installName;
    if (!downloadUrl || !installName || state.pending.has(installName)) return;
    // Updating keeps settings, so it is not styled as destructive.
    if (update && !await confirmOverlay(`Update ${installName}?`, 'Replace this overlay’s files with the latest release? Its settings are kept.', 'Update')) return;

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
    if (!await confirmOverlay(`Delete ${card?.dataset.counterTitle ?? folder}?`, description, 'Delete', { danger: true })) return;
    setBusy(button, true, 'Deleting…');
    // Focus moves to the neighbouring card, not to the page body.
    const visible = installedCards().filter((candidate) => !candidate.hidden);
    const position = visible.indexOf(card);
    const neighbour = visible[position + 1] ?? visible[position - 1] ?? null;
    try {
      await requestJson(`/api/counters/delete/${encodeURIComponent(folder)}`, { method: 'POST' });
      removePin(folder);
      card?.remove();
      setCount('installed', Math.max(0, (countFromBadge('installed') ?? 1) - 1));
      if (results && installedCards().length === 0) {
        results.replaceChildren(createInstalledEmptyState());
        pageTitle?.focus();
      } else {
        filterInstalled();
        if (neighbour?.isConnected) {
          state.roving?.setCurrent(neighbour);
          (neighbour.querySelector('.card-menu-toggle') ?? neighbour.querySelector('button'))?.focus();
        } else pageTitle?.focus();
      }
      notify(`${card?.dataset.counterTitle ?? folder} was deleted.`, 'success');
    } catch (error) {
      notify(`Could not delete ${folder}: ${errorText(error)}`, 'error');
      setBusy(button, false);
    }
  }

  // Mirrors the server's alias rule (src/counters/names.rs `validate_alias`
  // and `RESERVED`): pattern, reserved routes, overlay folders and other
  // aliases, with the server's messages.
  const ALIAS_PATTERN = /^[a-z0-9][a-z0-9_-]{0,47}$/;
  const RESERVED_ALIASES = new Set(['api', 'assets', 'available', 'backgroundimage', 'favicon.ico', 'files', 'json', 'settings', 'songs', 'tokens', 'tools', 'websocket', 'ws']);
  function aliasProblem(value, folder) {
    if (!ALIAS_PATTERN.test(value)) return "URL alias must be 1-48 lowercase letters, digits, '-' or '_', starting with a letter or digit";
    if (RESERVED_ALIASES.has(value)) return `/${value} is reserved by vosu`;
    const cards = installedCards();
    if (cards.some((card) => (card.dataset.overlayId ?? '').split('/')[0].toLowerCase() === value)) return `/${value} is already an overlay folder`;
    if (cards.some((card) => card.dataset.overlayId !== folder && (card.dataset.counterAlias ?? '').toLowerCase() === value)) return `/${value} is already used by another overlay`;
    return '';
  }

  // Nickname (dashboard display name) and URL alias (`/acc/` redirecting to
  // the folder) for one installed overlay; the folder stays its identity.
  function renameCounter(button) {
    const card = button.closest('.counter-card');
    const folder = button.dataset.folder;
    if (!card || !folder) return;
    const nickname = document.createElement('input');
    nickname.type = 'text';
    nickname.className = 'settings-control';
    nickname.maxLength = 64;
    nickname.autocomplete = 'off';
    nickname.value = card.dataset.counterNickname ?? '';
    nickname.placeholder = card.dataset.counterName ?? folder;
    const alias = document.createElement('input');
    alias.type = 'text';
    alias.className = 'settings-control settings-control--mono';
    alias.maxLength = 48;
    alias.autocomplete = 'off';
    alias.spellcheck = false;
    alias.value = card.dataset.counterAlias ?? '';
    alias.placeholder = 'acc';
    const initial = [nickname.value, alias.value];
    const { dialog, body, requestClose } = createActionDialog(`Nickname and URL: ${card.dataset.counterTitle ?? folder}`, {
      dismiss: 'guarded',
      isDirty: () => nickname.value !== initial[0] || alias.value !== initial[1],
    });
    const form = document.createElement('form');
    form.className = 'dialog-form rename-form';
    const field = (label, input) => {
      const wrapper = document.createElement('label');
      wrapper.className = 'field-label';
      const title = document.createElement('span');
      title.textContent = label;
      wrapper.append(title, input);
      return wrapper;
    };
    const hint = document.createElement('p');
    hint.className = 'field-help';
    hint.id = `alias-hint-${Date.now()}`;
    const error = document.createElement('p');
    error.className = 'field-error';
    error.id = `${hint.id}-error`;
    error.setAttribute('role', 'alert');
    alias.setAttribute('aria-describedby', `${hint.id} ${error.id}`);
    const overlayOrigin = new URL(card.querySelector('.preview-stage iframe')?.src ?? window.location.origin).origin;
    const aliasValue = () => alias.value.trim().replace(/^\/+|\/+$/g, '').toLowerCase();
    const describeAlias = () => {
      const value = aliasValue();
      const problem = value ? aliasProblem(value, folder) : '';
      if (problem) alias.setAttribute('aria-invalid', 'true');
      else alias.removeAttribute('aria-invalid');
      error.textContent = problem;
      // Only a valid alias gets a preview URL.
      hint.textContent = value && !problem ? `${overlayOrigin}/${value}/` : `${overlayOrigin}/${folder}/`;
    };
    alias.addEventListener('input', describeAlias);
    describeAlias();
    const actions = document.createElement('div');
    actions.className = 'dialog-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'button';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', requestClose);
    const save = document.createElement('button');
    save.type = 'submit';
    save.className = 'button button-primary';
    save.textContent = 'Save';
    actions.append(cancel, save);
    form.append(field('Nickname', nickname), field('URL alias', alias), hint, error, actions);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (alias.hasAttribute('aria-invalid')) {
        alias.focus();
        return;
      }
      setBusy(save, true, 'Saving…');
      try {
        const saved = await requestJson(`/api/counters/names/${encodeURIComponent(folder)}`, {
          method: 'POST',
          body: JSON.stringify({ nickname: nickname.value, alias: alias.value }),
        });
        applyCounterNames(card, folder, saved);
        notify(`Saved the name and URL of ${card.dataset.counterTitle}.`, 'success');
        dialog.close();
      } catch (failure) {
        // Server-side checks (reserved paths, collisions) concern the alias.
        error.textContent = errorText(failure);
        alias.setAttribute('aria-invalid', 'true');
        alias.focus();
        setBusy(save, false);
      }
    });
    body.append(form);
    nickname.focus();
  }

  function applyCounterNames(card, folder, saved) {
    const original = card.dataset.counterName ?? folder;
    const title = saved.nickname || original;
    card.dataset.counterTitle = title;
    card.dataset.counterNickname = saved.nickname ?? '';
    card.dataset.counterAlias = saved.alias ?? '';
    const heading = card.querySelector('.counter-card__identity h2');
    if (heading) { heading.textContent = title; heading.title = title; }
    let line = card.querySelector('.counter-card__folder');
    if (saved.nickname) {
      if (!line) {
        line = document.createElement('p');
        line.className = 'counter-card__folder';
        card.querySelector('.counter-card__meta')?.after(line);
      }
      line.title = `Folder: ${folder}`;
      line.replaceChildren(Object.assign(document.createElement('span'), { className: 'visually-hidden', textContent: 'Overlay: ' }), original);
    } else if (line?.title) {
      line.remove();
    }
    const url = card.querySelector('.counter-card__url');
    const frame = card.querySelector('.preview-stage iframe');
    if (url && frame) {
      // The preview frame's address carries the preview marker; browser
      // sources use the plain folder URL.
      const base = new URL(frame.src);
      base.search = '';
      const value = saved.alias ? `${base.origin}/${saved.alias}/` : base.href;
      url.dataset.copyValue = value;
      url.title = value;
      url.setAttribute('aria-label', `Copy URL ${value}`);
      url.querySelector('code').textContent = saved.alias ? `/${saved.alias}/` : `/${folder}/`;
      const open = card.querySelector('[data-action="open-url"]');
      if (open) open.dataset.url = value;
    }
    card.querySelector('[data-action="toggle-pin"]')?.setAttribute('aria-label', `Pin ${title}`);
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
      document.querySelector('.status-update')?.remove();
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
      notify(`${label} copied.`, 'success', 2600);
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

  // Search replaces the server-rendered catalog cards; catalog-source cards
  // are placed back above them, filtered by the same words.
  function afterResultsChange(query) {
    state.placeSourceOverlays?.(queryTokens(query));
    state.roving?.refresh();
    if (query) {
      const count = results?.querySelectorAll(':scope > .counter-card').length ?? 0;
      announceSoon(`${count} ${count === 1 ? 'overlay matches' : 'overlays match'} “${query}”`);
    }
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
    afterResultsChange('');
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
      afterResultsChange(query);
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

  // The section index is a left rail (before the form in DOM and Tab order)
  // and marks the section being read.
  function initializeSettingsIndex() {
    const form = document.querySelector('#runtime-settings');
    if (!form || !results) return;
    const index = document.createElement('nav');
    index.className = 'settings-index';
    index.setAttribute('aria-label', 'Settings sections');
    const groups = [...form.querySelectorAll(':scope > .settings-group')];
    const links = new Map();
    groups.forEach((group, position) => {
      group.id ||= `settings-group-${position + 1}`;
      const link = document.createElement('a');
      link.href = `#${group.id}`;
      link.textContent = group.dataset.title ?? group.querySelector('legend')?.textContent ?? '';
      index.append(link);
      links.set(group, link);
    });
    if (!index.childElementCount) return;
    form.before(index);
    let frame = 0;
    const update = () => {
      frame = 0;
      const header = document.querySelector('.site-header');
      const top = (header && getComputedStyle(header).position === 'sticky' ? header.getBoundingClientRect().bottom : 0) + 32;
      const visible = groups.filter((group) => !group.hidden);
      // At the end of the page the last sections cannot reach the top.
      const atEnd = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 4;
      const current = atEnd
        ? visible.at(-1)
        : visible.filter((group) => group.getBoundingClientRect().top <= top).at(-1) ?? visible[0];
      for (const [group, link] of links) {
        if (group === current) link.setAttribute('aria-current', 'location');
        else link.removeAttribute('aria-current');
      }
    };
    window.addEventListener('scroll', () => { frame ||= window.requestAnimationFrame(update); }, { passive: true });
    update();
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
    section.dataset.title = 'API keys';
    const legend = document.createElement('legend');
    legend.textContent = 'API keys';
    section.append(legend);
    const status = document.createElement('p');
    status.className = 'field-help';
    status.setAttribute('role', 'status');
    const list = document.createElement('div');
    list.className = 'api-key-list';
    const post = (payload) => requestJson('/api/keys', { method: 'POST', body: JSON.stringify(payload) });
    const SCOPES = { 'settings:write': 'Write settings', 'profiles:manage': 'Manage profiles' };
    const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
    const when = (value) => {
      const date = new Date(value);
      return Number.isNaN(date.getTime()) ? String(value) : dateFormat.format(date);
    };
    let rowId = 0;
    function row(title, description, control) {
      const item = document.createElement('div');
      item.className = 'settings-item';
      const copy = document.createElement('span'); copy.className = 'settings-item__copy';
      const name = document.createElement('span'); name.className = 'settings-item__name'; name.textContent = title;
      copy.append(name);
      if (description) {
        const detail = document.createElement('span');
        detail.className = 'settings-item__description';
        detail.id = `api-key-detail-${++rowId}`;
        detail.textContent = description;
        copy.append(detail);
        const target = control instanceof HTMLButtonElement ? control : control.querySelector?.('button');
        target?.setAttribute('aria-describedby', detail.id);
      }
      const controls = document.createElement('span'); controls.className = 'settings-item__control'; controls.append(control);
      item.append(copy, controls);
      return item;
    }
    function button(text, run, className = 'button') {
      const control = document.createElement('button');
      control.type = 'button'; control.className = className; control.textContent = text;
      control.addEventListener('click', async () => {
        setBusy(control, true);
        try { await run(); } catch (error) { status.textContent = errorText(error); }
        finally { setBusy(control, false); }
      });
      return control;
    }
    // The key is shown once: only Done closes this dialog, never Esc or a
    // stray click outside it.
    function showKey(key) {
      const shown = createActionDialog('API key created', { dismiss: 'explicit' });
      const warning = document.createElement('p'); warning.textContent = "Copy this key now. It won't be shown again.";
      const value = document.createElement('input');
      value.className = 'settings-control settings-control--mono api-key-value'; value.readOnly = true; value.value = key;
      value.setAttribute('aria-label', 'New API key');
      const copied = document.createElement('p'); copied.className = 'field-help'; copied.setAttribute('role', 'status');
      const actions = document.createElement('div'); actions.className = 'dialog-actions';
      const copy = document.createElement('button'); copy.type = 'button'; copy.className = 'button'; copy.textContent = 'Copy key';
      copy.addEventListener('click', async () => {
        try {
          await navigator.clipboard.writeText(value.value);
          copied.textContent = 'Key copied.';
        } catch (error) {
          value.select();
          copied.textContent = `Could not copy: ${errorText(error)}. The key is selected; copy it with Ctrl+C.`;
        }
      });
      const done = document.createElement('button'); done.type = 'button'; done.className = 'button button-primary'; done.textContent = 'Done';
      done.addEventListener('click', () => shown.dialog.close());
      actions.append(copy, done);
      shown.body.append(warning, value, copied, actions);
      value.focus(); value.select();
    }
    function keyDialog(title, submitText, run, { name, description, danger = false } = {}) {
      const input = document.createElement('input');
      input.className = 'settings-control'; input.name = 'name'; input.required = true; input.maxLength = 64; input.autocomplete = 'off'; input.value = name ?? '';
      const form = document.createElement('form'); form.className = 'dialog-form';
      const isDirty = () => input.value !== (name ?? '') || form.querySelector('[name="scope"]:checked') !== null;
      const { dialog, body, requestClose } = createActionDialog(title, { dismiss: name === null ? 'light' : 'guarded', isDirty });
      if (name !== null) {
        const label = document.createElement('label'); label.className = 'field-label';
        const text = document.createElement('span'); text.textContent = 'Name';
        label.append(text, input); form.append(label);
      }
      if (description) {
        const text = document.createElement('p'); text.textContent = description; form.append(text);
      }
      if (name === undefined) {
        const scopes = document.createElement('fieldset'); scopes.className = 'scope-choices';
        const caption = document.createElement('legend'); caption.textContent = 'Permissions (fixed once created)';
        scopes.append(caption);
        for (const [scope, label] of Object.entries(SCOPES)) {
          const choice = document.createElement('label'); choice.className = 'v-check';
          const toggle = document.createElement('input'); toggle.type = 'checkbox'; toggle.name = 'scope'; toggle.value = scope;
          choice.append(toggle, label);
          scopes.append(choice);
        }
        form.append(scopes);
      }
      const error = document.createElement('p'); error.className = 'field-error'; error.setAttribute('role', 'alert');
      const actions = document.createElement('div'); actions.className = 'dialog-actions';
      const cancel = document.createElement('button'); cancel.type = 'button'; cancel.className = 'button'; cancel.textContent = 'Cancel';
      cancel.addEventListener('click', requestClose);
      const submit = document.createElement('button'); submit.type = 'submit'; submit.className = danger ? 'button button-danger button-danger--solid' : 'button button-primary'; submit.textContent = submitText;
      actions.append(cancel, submit); form.append(error, actions); body.append(form);
      form.addEventListener('submit', async (event) => {
        event.preventDefault(); setBusy(submit, true);
        try {
          const result = await run(input.value, [...form.querySelectorAll('[name="scope"]:checked')].map((control) => control.value));
          dialog.close(); await refresh();
          if (result?.key) showKey(result.key);
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
        const item = (label, run, className = '') => {
          const control = document.createElement('button');
          control.type = 'button'; control.setAttribute('role', 'menuitem'); control.textContent = label;
          if (className) control.className = className;
          control.addEventListener('click', () => { menu.hidePopover(); run(); });
          return control;
        };
        menu.append(item('Rename', () => keyDialog('Rename API key', 'Save', (name) => post({ action: 'rename', id: entry.id, name }), { name: entry.name })),
          item('Revoke', () => keyDialog('Revoke API key', 'Revoke key', () => post({ action: 'revoke', id: entry.id }), {
            name: null, danger: true, description: `Revoke “${entry.name}”? Software using this key loses write access immediately, including open sockets.`,
          }), 'card-menu__danger'));
        const scopes = entry.scopes.map((scope) => SCOPES[scope] ?? scope).join(', ');
        list.append(row(entry.name, `${scopes} · ${entry.display}… · created ${when(entry.created)} · last used ${entry.last_used ? when(entry.last_used) : 'never'}`, wrapper));
      }
    }
    section.append(row('API access', 'Give trusted software only the access it needs. Remote connections still require ALLOWED_IPS. Use TLS outside loopback.',
      button('Create API key', () => keyDialog('Create API key', 'Create key', (name, scopes) => {
        if (!scopes.length) throw new Error('Select at least one permission.');
        return post({ action: 'create', name, scopes });
      }), 'button button-primary')), list, status);
    // Before About, which stays last.
    runtime.querySelector('.settings-group--about')?.before(section);
    if (!section.isConnected) runtime.querySelector('.settings-empty')?.before(section);
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
          // The server leaves out this dashboard's own preview frames.
          const count = status.connections?.[card.dataset.overlayId];
          const hidden = !Number.isInteger(count) || count < 1;
          const text = hidden ? '' : `${count} connected`;
          if (chip.hidden !== hidden) chip.hidden = hidden;
          if (chip.textContent !== text) chip.textContent = text;
          chip.title = 'Identified WebSocket connections, not unique users';
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

  const UPDATE_DISMISSED_KEY = 'vosu.update-dismissed.v1';

  function initializePresentation() {
    const banner = document.querySelector('.catalog-notice');
    if (banner) {
      const key = 'vosu.catalog-notice-dismissed.v1';
      const about = document.createElement('button');
      about.type = 'button'; about.className = 'catalog-about'; about.textContent = 'About community overlays';
      const dismiss = document.createElement('button');
      dismiss.type = 'button'; dismiss.className = 'button'; dismiss.textContent = "Don't show again";
      function show(visible) { banner.hidden = !visible; banner.open = visible; about.hidden = visible; }
      dismiss.addEventListener('click', () => { storageSet(key, '1'); show(false); about.focus(); });
      about.addEventListener('click', () => { show(true); banner.querySelector('summary')?.focus(); });
      // Dismissed, the notice leaves only this link, tucked under the search
      // field in the page head so the listing keeps the usual head-to-grid gap.
      banner.append(dismiss);
      (document.querySelector('.page-head') ?? banner.parentElement).append(about);
      show(storageGet(key) !== '1');
    }
    // "Later" hides the update notice for this version only; the status
    // strip keeps a way back to it.
    const notice = document.querySelector('#update-notice');
    if (notice && !notice.hidden && storageGet(UPDATE_DISMISSED_KEY) === notice.dataset.version) notice.hidden = true;
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

  function showUpdateNotice() {
    const notice = document.querySelector('#update-notice');
    if (!notice) return;
    storageSet(UPDATE_DISMISSED_KEY, '');
    notice.hidden = false;
    notice.querySelector('[data-action="app-update"]')?.focus();
  }

  function dismissUpdateNotice() {
    const notice = document.querySelector('#update-notice');
    if (!notice) return;
    storageSet(UPDATE_DISMISSED_KEY, notice.dataset.version ?? '');
    notice.hidden = true;
    document.querySelector('.status-update')?.focus();
  }

  // Overlays render without waiting for the catalog; update annotations
  // arrive afterwards and are added to the matching cards.
  async function refreshCounterUpdates() {
    if (pageKey() !== 'installed' || !results || requestedOverlay) return;
    try {
      const { updates } = await requestJson('/api/counters/updates');
      for (const { folder, download_url: url } of Array.isArray(updates) ? updates : []) state.updates.set(folder, url);
      for (const card of installedCards()) {
        const url = state.updates.get(card.dataset.overlayId);
        if (!url || card.querySelector('[data-action="update-counter"]')) continue;
        card.classList.add('is-updatable');
        const update = document.createElement('button');
        update.type = 'button';
        update.className = 'button button-update';
        update.dataset.action = 'update-counter';
        update.dataset.downloadUrl = url;
        update.dataset.installName = card.dataset.overlayId;
        update.setAttribute('aria-label', `Update ${card.dataset.counterTitle ?? card.dataset.overlayId}`);
        update.textContent = 'Update';
        card.querySelector('.counter-card__chips')?.prepend(update);
      }
      state.roving?.refresh();
    } catch (_error) {
      // The catalog host is optional: installed overlays keep working.
    }
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
      } else if (form.querySelector('[data-unsaved]')) {
        // Sections saved with their own button (Catalog sources).
        setStatus('Unsaved changes below', 'warn');
      } else {
        setStatus('All changes saved', 'success');
      }
    }
    form.addEventListener('vosu:unsaved', () => {
      if (status?.textContent !== 'Saving…') setSavedStatus();
    });

    // Feedback sits beside the control that changed: a brief "Saved" mark,
    // or an error message under the field that the field describes.
    function rowFeedback(control, { error = '', saved = false } = {}) {
      const item = control.closest('.settings-item');
      if (!item) return;
      const errorId = `${control.id || control.dataset.id}-error`;
      const described = new Set((control.getAttribute('aria-describedby') ?? '').split(/\s+/).filter(Boolean));
      let message = item.querySelector('.settings-item__error');
      if (error) {
        if (!message) {
          message = document.createElement('span');
          message.className = 'settings-item__error';
          message.id = errorId;
          item.querySelector('.settings-item__copy')?.append(message);
        }
        message.textContent = error;
        described.add(errorId);
        control.setAttribute('aria-invalid', 'true');
        item.classList.add('has-error');
      } else {
        message?.remove();
        described.delete(errorId);
        control.removeAttribute('aria-invalid');
        item.classList.remove('has-error');
      }
      if (described.size) control.setAttribute('aria-describedby', [...described].join(' '));
      if (saved) {
        let mark = item.querySelector('.settings-item__saved');
        if (!mark) {
          mark = document.createElement('span');
          mark.className = 'settings-item__saved';
          mark.setAttribute('aria-hidden', 'true');
          mark.textContent = 'Saved';
          item.querySelector('.settings-item__control')?.prepend(mark);
        }
        mark.classList.remove('is-shown');
        void mark.offsetWidth;
        mark.classList.add('is-shown');
      }
    }

    async function saveSetting(control) {
      const item = control.closest('.settings-item');
      const name = item?.querySelector('.settings-item__name')?.textContent ?? control.dataset.id;
      const settings = { [control.dataset.id]: controlValue(control) };
      // Validate locally first: the server rejects these too, but a blank or
      // out-of-range field must never look saved.
      if (control.type === 'number' && (control.value.trim() === '' || !control.checkValidity())) {
        const reason = control.value.trim() === '' ? 'Enter a number.' : control.validationMessage;
        rowFeedback(control, { error: reason });
        setSavedStatus();
        announce(`${name} was not saved. ${reason}`, true);
        return false;
      }
      setStatus('Saving…');
      try {
        await requestJson('/api/settingsSave', { method: 'POST', body: JSON.stringify(settings) });
      } catch (error) {
        if (control === startAtLogin) control.checked = savedStartAtLogin;
        rowFeedback(control, { error: errorText(error) });
        setSavedStatus();
        announce(`${name} was not saved. ${errorText(error)}`, true);
        return false;
      }
      if (control === startAtLogin) savedStartAtLogin = control.checked;
      rowFeedback(control, { saved: true });
      setSavedStatus();
      announce(`${name} saved`);
      return true;
    }

    // The overlays folder changes which overlays exist, so it waits for an
    // explicit Apply instead of saving on blur.
    const folder = form.querySelector('[data-id="STATIC_FOLDER_PATH"]');
    if (folder) {
      let savedFolder = folder.value;
      const applyFolder = document.createElement('button');
      applyFolder.type = 'button';
      applyFolder.className = 'button button-primary';
      applyFolder.textContent = 'Apply';
      applyFolder.hidden = true;
      folder.after(applyFolder);
      folder.addEventListener('input', () => { applyFolder.hidden = folder.value === savedFolder; });
      folder.addEventListener('keydown', (event) => {
        if (event.key === 'Enter' && !applyFolder.hidden) { event.preventDefault(); applyFolder.click(); }
      });
      applyFolder.addEventListener('click', async () => {
        setBusy(applyFolder, true, 'Applying…');
        const saved = await saveSetting(folder);
        setBusy(applyFolder, false);
        if (!saved) return;
        savedFolder = folder.value;
        applyFolder.hidden = true;
        folder.focus();
      });
    }

    const apply = document.createElement('button');
    apply.type = 'button';
    apply.className = 'button button-primary';
    apply.textContent = 'Apply network settings';
    const networkNote = document.createElement('p');
    networkNote.className = 'field-help';
    networkNote.textContent = 'Network changes need Apply, then Keep within 30 seconds.';
    // Apply sits with the fields it commits instead of after unrelated groups.
    const networkActions = document.createElement('div');
    networkActions.className = 'network-actions';
    networkActions.append(apply, networkNote);
    const networkGroup = networkControls[0]?.closest('.settings-group');
    if (networkGroup) networkGroup.append(networkActions);
    else form.append(networkActions);

    // A pending change gets a bar pinned under the site header: visible
    // wherever the page lands, with Keep focused and the time left.
    const pending = document.createElement('div');
    pending.className = 'network-pending';
    pending.setAttribute('role', 'region');
    pending.setAttribute('aria-label', 'Pending network change');
    pending.hidden = true;
    const pendingText = document.createElement('p');
    const pendingTitle = document.createElement('strong');
    const countdown = document.createElement('span');
    countdown.className = 'network-pending__count';
    pendingText.append(pendingTitle, ' ', countdown);
    const keep = document.createElement('button');
    keep.type = 'button';
    keep.className = 'button button-primary';
    keep.textContent = 'Keep';
    const revert = document.createElement('button');
    revert.type = 'button';
    revert.className = 'button';
    revert.textContent = 'Revert now';
    const dismissPending = document.createElement('button');
    dismissPending.type = 'button';
    dismissPending.className = 'button';
    dismissPending.textContent = 'Dismiss';
    dismissPending.hidden = true;
    pending.append(pendingText, keep, revert, dismissPending);
    (document.querySelector('.site-header') ?? document.body).append(pending);

    const params = new URLSearchParams(location.search);
    const requestedDeadline = Number(params.get('network-deadline'));
    const now = Date.now();
    let deadline = Number.isFinite(requestedDeadline) && requestedDeadline > 0
      ? Math.max(now, Math.min(now + 30000, requestedDeadline)) : 0;
    let oldOrigin = context.rollbackOrigin || location.origin;
    let newOrigin = location.origin;
    let navigating = false;
    let timer;
    let announcedLate = false;

    function cleanUrl() {
      params.delete('network-deadline');
      params.delete('network-reverted');
      history.replaceState(null, '', `${location.pathname}${params.size ? `?${params}` : ''}`);
    }
    // After a rollback the old address shows why the change is gone; the
    // message stays until dismissed.
    function showReverted() {
      pending.hidden = false;
      pending.dataset.tone = 'info';
      pendingTitle.textContent = 'Network settings reverted.';
      countdown.replaceChildren('The dashboard is back on ', Object.assign(document.createElement('code'), { textContent: location.host }), '.');
      keep.hidden = true;
      revert.hidden = true;
      dismissPending.hidden = false;
      announce(`Network settings reverted. The dashboard is back on ${location.host}.`, true);
      dismissPending.focus();
    }
    dismissPending.addEventListener('click', () => {
      pending.hidden = true;
      pageTitle?.focus();
    });
    function finishNetwork() {
      deadline = 0;
      clearInterval(timer);
      pending.hidden = true;
      apply.disabled = false;
      cleanUrl();
    }
    function goToOldOrigin() {
      navigating = true;
      const target = new URL(`${oldOrigin}${location.pathname}`);
      target.searchParams.set('network-reverted', '1');
      navigateDashboard(target.href);
    }
    async function tickNetwork() {
      if (!deadline || navigating) return;
      const seconds = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
      // The countdown text is not a live region; screen readers hear the
      // start and a single 10-second warning instead of every second.
      countdown.replaceChildren('Reverting in ', Object.assign(document.createElement('span'), { className: 'network-pending__seconds', textContent: String(seconds) }), ' s.');
      if (seconds <= 10 && seconds > 0 && !announcedLate) {
        announcedLate = true;
        announce(`${seconds} seconds left to keep the network settings.`, true);
      }
      if (seconds === 0) {
        navigating = true;
        // Rollback runs in vosu even when this page has been closed.
        await new Promise((resolve) => setTimeout(resolve, 1500));
        goToOldOrigin();
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
      const here = newOrigin === location.origin;
      pending.hidden = false;
      delete pending.dataset.tone;
      keep.hidden = !here;
      revert.hidden = false;
      dismissPending.hidden = true;
      pendingTitle.textContent = here ? 'Keep the new network settings?' : `Opening ${new URL(newOrigin).host}…`;
      timer = setInterval(tickNetwork, 1000);
      tickNetwork();
      const seconds = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
      announce(here
        ? `Network settings applied. Keep them within ${seconds} seconds or they revert.`
        : 'Network settings applied. Opening the new dashboard address.', true);
      (here ? keep : revert).focus();
    }
    if (deadline) startNetworkTimer();
    else if (params.get('network-reverted') === '1') {
      cleanUrl();
      showReverted();
    }
    keep.addEventListener('click', async () => {
      keep.disabled = true;
      try {
        await requestJson('/api/settingsSave', { method: 'POST', body: JSON.stringify({ _network_action: 'confirm' }) });
        finishNetwork();
        notify('Network settings kept.', 'success');
        apply.focus();
      } catch (error) { notify(errorText(error), 'error'); }
      finally { keep.disabled = false; }
    });
    revert.addEventListener('click', async () => {
      setBusy(revert, true, 'Reverting…');
      clearInterval(timer);
      navigating = true;
      try {
        await requestJson('/api/settingsSave', { method: 'POST', body: JSON.stringify({ _network_action: 'revert' }) });
      } catch (_error) {
        // The listener serving this page may already be gone; the server
        // timer still reverts at the deadline.
      }
      await new Promise((resolve) => setTimeout(resolve, 1200));
      if (oldOrigin === location.origin) {
        finishNetwork();
        setBusy(revert, false);
        navigating = false;
        showReverted();
      } else goToOldOrigin();
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
      announcedLate = false;
      try {
        await requestJson('/api/settingsSave', { method: 'POST', body: JSON.stringify({ ...values, _network_action: 'apply' }) });
        startNetworkTimer();
      } catch (error) {
        apply.disabled = false;
        deadline = 0;
        notify(errorText(error), 'error');
      }
    });

    // `change` fires when a text field loses focus (or Enter commits it) and
    // immediately for checkboxes; saves are serialized so they apply in order.
    form.addEventListener('change', (event) => {
      const control = event.target.closest('[data-id]');
      if (!control || control.disabled || control === folder) return;
      if (networkKeys.has(control.dataset.id)) {
        networkNote.textContent = 'Not saved yet: Apply, then Keep within 30 seconds.';
        return;
      }
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

  // A scroll (including one a click causes at high zoom) moves an open menu
  // with its toggle instead of closing it.
  function positionOpenMenus() {
    document.querySelectorAll('.card-menu:popover-open').forEach(positionCardMenu);
  }

  function menuItems(menu) {
    return [...menu.querySelectorAll('button:not(:disabled), a[href]')].filter((item) => item.offsetParent !== null);
  }

  // Card grids are a single Tab stop: arrow keys move between cards,
  // Home/End jump, and Tab from a card enters that card's own controls.
  // `selector` matches the grid's direct children (e.g. `.counter-card`).
  function rovingGrid(container, selector) {
    if (!container) return null;
    let current = null;
    // Until the user moves within the grid, its Tab stop is the first card,
    // also after cards are added before it (catalog-source overlays).
    let chosen = false;
    let frame = 0;
    const all = () => [...container.children].filter((item) => item.matches(selector));
    const visible = () => all().filter((item) => !item.hidden && item.getClientRects().length > 0);
    // The grid item holding `node`: the ancestor that is a direct child of
    // the container. (`closest()` cannot express "direct child of".)
    const itemOf = (node) => {
      while (node && node.parentElement !== container) node = node.parentElement;
      return node?.matches(selector) ? node : null;
    };
    const focusables = (item) => item.querySelectorAll('a[href], button, input, select, textarea, iframe, [tabindex]');
    function apply() {
      frame = 0;
      const items = visible();
      if (!chosen || !items.includes(current)) current = items[0] ?? null;
      for (const item of all()) {
        const active = item === current;
        item.tabIndex = active ? 0 : -1;
        for (const element of focusables(item)) {
          if (element.dataset.rovingTabindex === undefined) element.dataset.rovingTabindex = element.getAttribute('tabindex') ?? '';
          const original = element.dataset.rovingTabindex;
          if (!active) element.setAttribute('tabindex', '-1');
          else if (original === '') element.removeAttribute('tabindex');
          else element.setAttribute('tabindex', original);
        }
      }
    }
    function refresh() {
      frame ||= window.requestAnimationFrame(apply);
    }
    function setCurrent(item) {
      current = item;
      chosen = true;
      apply();
    }
    container.addEventListener('focusin', (event) => {
      const item = itemOf(event.target);
      if (item && item !== current && container.contains(item)) setCurrent(item);
    });
    container.addEventListener('keydown', (event) => {
      if (!['ArrowRight', 'ArrowLeft', 'ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
      if (event.altKey || event.ctrlKey || event.metaKey) return;
      const item = itemOf(event.target);
      // Text fields and menus keep their own arrow keys.
      if (!item || event.target.closest('input, select, textarea, .card-menu')) return;
      const items = visible();
      const index = items.indexOf(item);
      let next = null;
      if (event.key === 'ArrowRight') next = items[index + 1];
      else if (event.key === 'ArrowLeft') next = items[index - 1];
      else if (event.key === 'Home') next = items[0];
      else if (event.key === 'End') next = items.at(-1);
      else {
        const box = item.getBoundingClientRect();
        const down = event.key === 'ArrowDown';
        let best = null;
        let bestScore = Infinity;
        for (const candidate of items) {
          const rect = candidate.getBoundingClientRect();
          if (down ? rect.top <= box.top + 2 : rect.top >= box.top - 2) continue;
          const score = Math.abs(rect.top - box.top) * 10000 + Math.abs(rect.left - box.left);
          if (score < bestScore) { best = candidate; bestScore = score; }
        }
        next = best;
      }
      if (!next) return;
      event.preventDefault();
      setCurrent(next);
      next.focus();
    });
    new MutationObserver(refresh).observe(container, { childList: true, subtree: true });
    apply();
    return { refresh, setCurrent };
  }

  const previewDialog = document.querySelector('#preview-dialog');
  const previewFrame = previewDialog?.querySelector('.preview-dialog__frame');

  function openPreviewDialog(title, description, content) {
    if (!previewDialog || !previewFrame) return;
    previewDialog.querySelector('#preview-dialog-title').textContent = title;
    previewDialog.querySelector('#preview-dialog-description').textContent = description;
    previewFrame.replaceChildren(content);
    if (!previewDialog.open) showModal(previewDialog);
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
    const resolution = card.querySelector('.counter-card__size')?.textContent ?? '';
    openPreviewDialog(card.dataset.counterTitle ?? card.dataset.counterName ?? '', resolution, stage);
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
    openPreviewDialog(card?.dataset.counterTitle ?? card?.dataset.counterName ?? '', shown.alt, image);
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

  // Dialogs handle their own closing (homepage-settings.js guardDialog): a
  // click outside one never closes another dialog's unsaved form.
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
    } else if (action === 'open-url') {
      const url = absoluteHttpUrl(control.dataset.url);
      if (url) window.open(url, '_blank', 'noopener,noreferrer');
    } else if (action === 'rename-counter') {
      renameCounter(control);
    } else if (action === 'open-overlays-root') {
      openOverlaysRoot(control);
    } else if (action === 'counter-settings') {
      loadCounterSettings(control.dataset.folder, { name: control.closest('.counter-card')?.dataset.counterTitle });
    } else if (action === 'app-update') {
      runAppUpdate(control);
    } else if (action === 'show-update') {
      showUpdateNotice();
    } else if (action === 'dismiss-update') {
      dismissUpdateNotice();
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
    if (!(event.target instanceof HTMLElement) || !event.target.classList.contains('card-menu')) return;
    if (event.newState === 'open') {
      positionCardMenu(event.target);
      menuItems(event.target)[0]?.focus();
    }
  }, true);
  document.addEventListener('keydown', (event) => {
    const menu = event.target instanceof Element ? event.target.closest('.card-menu') : null;
    if (!menu || !['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    const items = menuItems(menu);
    const index = items.indexOf(event.target);
    const next = event.key === 'Home' ? items[0]
      : event.key === 'End' ? items.at(-1)
        : items[(index + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length];
    event.preventDefault();
    next?.focus();
  });
  window.addEventListener('resize', positionOpenMenus);
  window.addEventListener('scroll', positionOpenMenus, { passive: true });

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

  // Sticky elements under the header (settings status, section rails) use
  // its live height, which grows when the status strip wraps or a network
  // change is pending; an unpinned header contributes nothing.
  const siteHeader = document.querySelector('.site-header');
  if (siteHeader && typeof ResizeObserver === 'function') {
    new ResizeObserver(() => {
      const pinned = getComputedStyle(siteHeader).position === 'sticky';
      document.documentElement.style.setProperty('--header-h', `${pinned ? siteHeader.offsetHeight : 0}px`);
    }).observe(siteHeader);
  }

  initializeCounts();
  initializePins({ sanitize: pageKey() === 'installed' && !window.location.search });
  if (pageKey() === 'installed' && searchInput?.value) filterInstalled();
  observePreviews();
  initializeRuntimeSettings();
  initializeApiKeys();
  if (pageKey() === 'settings' && !requestedOverlay && !requestedTool) {
    mountCatalogSources({ root: document.querySelector('#runtime-settings'), requestJson, notify, errorText, setBusy });
  }
  initializeSettingsIndex();
  initializePresentation();
  initializePinDrag();
  initializeDashboardStatus();
  refreshToolCount();
  if ((pageKey() === 'installed' || pageKey() === 'available') && !requestedOverlay) {
    state.roving = rovingGrid(results, '.counter-card');
  }
  refreshCounterUpdates();

  if (pageKey() === 'available') {
    state.placeSourceOverlays = mountCatalogOverlays({
      root: results, requestJson, notify, errorText, setBusy, confirmAction: confirmOverlay,
      setCount: (value) => setCount('available', value),
      initialTokens: queryTokens(searchInput?.value ?? ''),
    });
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
        showModal,
        currentTheme,
        announce: announceSoon,
        rovingGrid,
        setCount: (value) => setCount('tools', value),
        // Same dialog as an overlay card's Settings button.
        openToolSettings: (id, name) => loadToolSettings(id, { name }),
      });
      if (searchInput?.value) state.filterTools?.(searchInput.value);
    }).catch((error) => notify(`Could not load the Tools page: ${errorText(error)}`, 'error', 0));
  }
})();
