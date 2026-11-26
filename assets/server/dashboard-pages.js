// Content the dashboard's page shells do not carry (A7.1): the overlay lists
// come from the overlay host's search fragment, Settings values from core's
// `GET /api/settings` (SettingsView) and the update notice from
// `DashboardStatus.update`. Every value is written through the DOM, never
// spliced into markup.

function alertState(title, message, actions) {
  const state = document.createElement('div');
  state.className = 'empty-state';
  state.setAttribute('role', 'alert');
  const heading = document.createElement('h2');
  heading.textContent = title;
  const text = document.createElement('p');
  text.textContent = message;
  const row = document.createElement('div');
  row.className = 'counter-actions';
  row.append(...actions);
  state.append(heading, text, row);
  return state;
}

function retryLink(href) {
  const link = document.createElement('a');
  link.className = 'button button-primary';
  link.href = href;
  link.textContent = 'Retry';
  return link;
}

// Card frames and copied browser-source URLs that are origin-relative point
// at the overlay listener, never at this dashboard.
function absolutizeOverlayUrls(root, overlayOrigin) {
  if (!overlayOrigin) return;
  const relative = (value) => typeof value === 'string' && value.startsWith('/') && !value.startsWith('//');
  root.querySelectorAll('iframe[src]').forEach((frame) => {
    const src = frame.getAttribute('src');
    if (relative(src)) frame.setAttribute('src', new URL(src, overlayOrigin).href);
  });
  root.querySelectorAll('[data-copy-value]').forEach((element) => {
    if (relative(element.dataset.copyValue)) element.dataset.copyValue = new URL(element.dataset.copyValue, overlayOrigin).href;
  });
  root.querySelectorAll('[data-action="open-url"][data-url]').forEach((element) => {
    if (relative(element.dataset.url)) element.dataset.url = new URL(element.dataset.url, overlayOrigin).href;
  });
}

// Resolves to the number of cards shown, or null when the list could not be read.
export async function loadOverlayList({ results, source, csrf, overlayOrigin, emptyState }) {
  if (!results) return null;
  results.setAttribute('aria-busy', 'true');
  try {
    const response = await fetch(`/api/counters/search/?source=${source === 'available' ? 'available' : 'installed'}`, {
      headers: { Accept: 'text/html', 'X-Vosu-CSRF': csrf },
    });
    const html = await response.text();
    if (!response.ok) {
      const message = new DOMParser().parseFromString(html, 'text/html').body.textContent?.trim();
      throw new Error(message || `the list returned ${response.status}`);
    }
    const template = document.createElement('template');
    template.innerHTML = html;
    const count = [...template.content.children].filter((element) => element.matches('.counter-card')).length;
    absolutizeOverlayUrls(template.content, overlayOrigin);
    if (count > 0) {
      results.replaceChildren(template.content);
    } else if (source === 'available') {
      const done = document.createElement('div');
      done.className = 'empty-state';
      done.innerHTML = '<h2>Everything is installed</h2><a class="button" href="/">View installed overlays</a>';
      results.replaceChildren(done);
    } else {
      results.replaceChildren(emptyState());
    }
    return count;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (source === 'available') {
      results.replaceChildren(alertState("Couldn't reach the community catalog", message, [retryLink('/available')]));
      results.firstElementChild.classList.add('catalog-unavailable');
    } else {
      const open = document.createElement('button');
      open.type = 'button';
      open.className = 'button';
      open.dataset.action = 'open-overlays-root';
      open.textContent = 'Open overlays folder';
      results.replaceChildren(alertState("Couldn't read the overlays folder", message, [retryLink('/'), open]));
    }
    return null;
  } finally {
    results.setAttribute('aria-busy', 'false');
  }
}

const MANAGED_UPDATES = 'Updates are managed by your Linux package manager; automatic updates are disabled.';

// Fills the server-rendered form from one SettingsView. A key the view does
// not carry keeps an empty control rather than an invented default.
export function fillSettings(form, view) {
  const values = view?.values && typeof view.values === 'object' ? view.values : {};
  form.querySelectorAll('[data-id]').forEach((control) => {
    const id = control.dataset.id;
    if (!Object.hasOwn(values, id)) return;
    const value = values[id];
    if (control.type === 'checkbox') control.checked = value === true;
    else if (id === 'ALLOWED_IPS') control.value = String(value).split(',').map((entry) => entry.trim()).filter(Boolean).join('\n');
    else control.value = String(value ?? '');
  });
  if (view?.updates_externally_managed === true) {
    const update = form.querySelector('[data-id="ENABLE_AUTOUPDATE"]');
    if (update) update.disabled = true;
    const description = form.querySelector('#setting-ENABLE_AUTOUPDATE-description');
    if (description) description.textContent = MANAGED_UPDATES;
  }
  form.inert = false;
  form.removeAttribute('aria-busy');
}

// Resolves once the form is filled, or after it was replaced by an alert
// (the form is removed then, so nothing initializes or saves stale values).
export async function loadRuntimeSettings({ form, requestJson, errorText }) {
  if (!form) return;
  try {
    fillSettings(form, await requestJson('/api/settings'));
  } catch (error) {
    form.replaceWith(alertState("Couldn't load settings", errorText(error), [retryLink('/settings')]));
  }
}

function httpUrl(value) {
  try {
    const url = new URL(String(value));
    return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null;
  } catch (_error) {
    return null;
  }
}

// Shows `DashboardStatus.update` once per version. "Later" (the stored
// dismissed version) hides the notice; the status strip keeps a way back.
export function applyUpdateNotice(update, dismissedVersion) {
  const notice = document.querySelector('#update-notice');
  const chip = document.querySelector('.status-update');
  const version = typeof update?.version === 'string' ? update.version.trim() : '';
  if (!notice || !chip || !version || notice.dataset.version === version) return;
  const major = update.major === true;
  notice.dataset.version = version;
  notice.dataset.major = String(major);
  document.querySelectorAll('[data-role="new-version"]').forEach((element) => { element.textContent = version; });
  notice.querySelector(':scope > .update-notice__copy').hidden = major;
  notice.querySelector('.update-notice__major').hidden = !major;
  const link = notice.querySelector('[data-role="release-url"]');
  const href = major ? httpUrl(update.release_url) : null;
  if (href) link.href = href;
  else link.removeAttribute('href');
  chip.hidden = false;
  notice.hidden = dismissedVersion === version;
}
