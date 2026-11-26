// Settings › Catalog sources (catalog v2). The ordered list saves exact
// source strings, enabled flags and order only (A6b.4). Trust, the private
// network exception and credentials are per-source commands with their own
// routes; credentials are write-only and never read back.
import { button, node, sourceFailures, sourcePath } from './dashboard-catalog.js';
import { confirmTrustGrant } from './dashboard-catalog-flow.js';
import { createActionDialog } from './homepage-settings.js';

const FORMS = {
  github: 'GitHub repository',
  repository: 'Repository (GitLab, Gitea, Forgejo, Codeberg)',
  static: 'Static catalog-v2.json URL',
  invalid: 'Invalid source',
};

function stateLine(source) {
  if (!source.enabled) return 'Disabled; settings are kept.';
  if (source.status === 'failed') return `Couldn't fetch: ${source.message}`;
  if (source.status === 'ok') return `${source.entries} ${source.entries === 1 ? 'entry' : 'entries'} fetched`;
  return 'Not fetched yet';
}

// A6b.7: lowering trust never claims that loaded code stopped. Revoked
// in-process plugins keep running inside vosu until the user restarts it,
// and process plugins lose their approval; both are named.
export function revocationNotice(base, revocation) {
  const pending = revocation?.pending_restart ?? [];
  const cleared = revocation?.approvals_cleared ?? [];
  const parts = [base];
  if (pending.length) {
    parts.push(`${pending.join(', ')} ${pending.length === 1 ? 'is' : 'are'} still running inside vosu: restart vosu to unload ${pending.length === 1 ? 'it' : 'them'}.`);
  }
  if (cleared.length) parts.push(`${cleared.join(', ')} ${cleared.length === 1 ? 'needs' : 'need'} your approval again.`);
  return parts.join(' ');
}

function credentialDialog(source, { requestJson, notify, errorText, reload }) {
  const basic = source.credential_kind === 'basic';
  const { dialog, body } = createActionDialog(`Credential for ${source.source}`, { dismiss: 'explicit' });
  dialog.classList.add('catalog-dialog');
  const form = node('form', undefined, 'dialog-form catalog-credential');
  form.autocomplete = 'off';
  const guidance = node('p', basic
    ? 'HTTP Basic credentials, sent only to this exact URL. Use an account that can only read the catalog.'
    : 'Use a read-only access token scoped to this repository’s releases (GitHub fine-grained “Contents: read”, GitLab “read_api”, Gitea/Forgejo “read:repository”). Package and artwork downloads never receive it.', 'field-help');
  const fields = basic
    ? [['username', 'Username', 'text'], ['password', 'Password', 'password']]
    : [['token', 'Access token', 'password']];
  const inputs = fields.map(([name, label, type]) => {
    const wrapper = node('label', undefined, 'settings-field');
    const input = node('input', undefined, 'settings-control settings-control--mono');
    input.type = type; input.name = name; input.required = true; input.autocomplete = 'off'; input.spellcheck = false;
    wrapper.append(node('span', label, 'settings-field__label'), input);
    form.append(wrapper);
    return input;
  });
  const write = node('p', source.has_credential
    ? 'A credential is saved. It is never shown; saving replaces it.'
    : 'No credential is saved.', 'field-help');
  const cancel = button('Cancel', () => dialog.close());
  const save = node('button', 'Save credential', 'button button-primary'); save.type = 'submit';
  const row = node('div', undefined, 'dialog-actions');
  row.append(cancel);
  if (source.has_credential) {
    row.append(button('Delete credential', async () => {
      try {
        await requestJson(sourcePath(source.source, 'credential'), { method: 'DELETE' });
        notify('Credential deleted.', 'success'); dialog.close(); await reload();
      } catch (error) { notify(errorText(error), 'error'); }
    }, { className: 'button button-danger' }));
  }
  row.append(save);
  form.append(guidance, write, row);
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const payload = basic
      ? { kind: 'basic', username: inputs[0].value, password: inputs[1].value }
      : { kind: 'token', token: inputs[0].value };
    save.disabled = true;
    try {
      await requestJson(sourcePath(source.source, 'credential'), { method: 'POST', body: JSON.stringify(payload) });
      notify('Credential saved.', 'success'); dialog.close(); await reload();
    } catch (error) { notify(errorText(error), 'error'); save.disabled = false; }
    finally { for (const input of inputs) input.value = ''; }
  });
  body.append(form);
  inputs[0].focus();
}

export function mountCatalogSources({ root, requestJson, notify, errorText, setBusy }) {
  if (!root) return;
  const section = node('fieldset', undefined, 'settings-group catalog-sources');
  section.id = 'catalog-sources';
  section.dataset.title = 'Catalog sources';
  section.append(node('legend', 'Catalog sources'));
  const intro = node('p', 'Sources are tried in this order. The three 18-lab-dev catalogs are official and always trusted. '
    + 'Other sources start untrusted: their plugins run only as separate processes after you approve them.', 'settings-item__description catalog-sources__intro');
  const rows = node('div', undefined, 'catalog-source-list');
  const status = node('p', '', 'field-help'); status.setAttribute('role', 'status');
  const failures = node('div', undefined, 'catalog-sources__failures');
  let sources = [];
  let saved = '[]';
  const elements = new Map();
  const snapshot = () => JSON.stringify(sources.map(({ source, enabled }) => ({ source, enabled })));
  const dirty = () => snapshot() !== saved;
  function markDirty() {
    const unsaved = dirty();
    status.textContent = unsaved ? 'Unsaved changes' : '';
    if (section.hasAttribute('data-unsaved') !== unsaved) {
      section.toggleAttribute('data-unsaved', unsaved);
      section.dispatchEvent(new CustomEvent('vosu:unsaved', { bubbles: true }));
    }
    for (const entry of elements.values()) {
      // Flags and credentials belong to saved sources only.
      for (const control of entry.saved) control.disabled = unsaved || !entry.persisted;
    }
  }
  window.addEventListener('beforeunload', (event) => { if (dirty()) event.preventDefault(); });

  async function setFlag(source, route, body, label) {
    try {
      const result = await requestJson(sourcePath(source.source, route), { method: 'POST', body: JSON.stringify(body) });
      // A pending restart stays on screen until dismissed.
      const pending = Boolean(result?.revocation?.pending_restart?.length);
      notify(revocationNotice(label, result?.revocation), pending ? 'info' : 'success', pending ? 0 : 3200);
      window.dispatchEvent(new CustomEvent('vosu:catalog-changed'));
    } catch (error) { notify(errorText(error), 'error'); }
    await load();
  }

  function rowFor(source) {
    let entry = elements.get(source);
    if (entry) return entry;
    const row = node('div', undefined, 'catalog-source-row');
    const enabled = node('input', undefined, 'v-toggle'); enabled.type = 'checkbox'; enabled.setAttribute('role', 'switch');
    enabled.addEventListener('change', () => { source.enabled = enabled.checked; markDirty(); });
    const input = node('input', undefined, 'settings-control settings-control--mono'); input.type = 'text';
    input.placeholder = 'owner/repo@tag, https://host/group/repo or https://…/catalog-v2.json'; input.spellcheck = false;
    input.addEventListener('input', () => { source.source = input.value.trim(); entry.persisted = false; markDirty(); });
    const up = button('Move up', () => move(source, -1), { className: 'button button--sm' });
    const down = button('Move down', () => move(source, 1), { className: 'button button--sm' });
    const remove = button('Remove', () => removeSource(source), { className: 'button button--sm button-danger' });
    const actions = node('div', undefined, 'catalog-source-row__actions'); actions.append(up, down, remove);
    const detail = node('p', '', 'catalog-source-row__state');
    const flags = node('div', undefined, 'catalog-source-row__flags');
    const trust = node('input', undefined, 'v-toggle'); trust.type = 'checkbox'; trust.setAttribute('role', 'switch');
    trust.addEventListener('change', async () => {
      if (trust.checked) {
        trust.checked = false;
        // Every grant shows the warning again: nothing remembers it.
        if (!await confirmTrustGrant(source.source)) return;
        await setFlag(source, 'trust', { trusted: true }, 'Source trusted.');
      } else {
        await setFlag(source, 'trust', { trusted: false }, 'Source no longer trusted.');
      }
    });
    const priv = node('input', undefined, 'v-toggle'); priv.type = 'checkbox'; priv.setAttribute('role', 'switch');
    priv.addEventListener('change', () => setFlag(source, 'private-network', { allowed: priv.checked },
      priv.checked ? 'Private network allowed for this host.' : 'Private network not allowed.'));
    const trustLabel = node('label', undefined, 'catalog-flag'); trustLabel.append(trust, node('span', 'Trusted'));
    const privLabel = node('label', undefined, 'catalog-flag'); privLabel.append(priv, node('span', 'Private network'));
    const credential = button('Credential…', () => credentialDialog(source, { requestJson, notify, errorText, reload: load }), { className: 'button button--sm' });
    const form = node('span', '', 'badge');
    const credentialState = node('span', '', 'badge');
    flags.append(form, trustLabel, privLabel, credential, credentialState);
    row.append(enabled, input, actions, flags, detail);
    entry = { row, enabled, input, up, down, remove, detail, trust, priv, credential, form, credentialState, persisted: Boolean(source.order !== undefined), saved: [trust, priv, credential] };
    elements.set(source, entry);
    return entry;
  }
  function render() {
    for (const source of [...elements.keys()]) if (!sources.includes(source)) elements.delete(source);
    sources.forEach((source, index) => {
      const entry = rowFor(source);
      const label = source.source || `source ${index + 1}`;
      entry.row.dataset.source = source.source;
      entry.enabled.checked = source.enabled;
      entry.enabled.setAttribute('aria-label', `Enable ${label}`);
      if (entry.input.value !== source.source) entry.input.value = source.source;
      entry.input.setAttribute('aria-label', `Catalog source ${index + 1}`);
      entry.up.disabled = index === 0;
      entry.down.disabled = index === sources.length - 1;
      entry.up.setAttribute('aria-label', `Move ${label} up`);
      entry.down.setAttribute('aria-label', `Move ${label} down`);
      entry.remove.setAttribute('aria-label', `Remove ${label}`);
      entry.trust.checked = Boolean(source.trusted);
      entry.trust.setAttribute('aria-label', `Trust ${label}`);
      entry.priv.checked = Boolean(source.private_network);
      entry.priv.setAttribute('aria-label', `Allow private network for ${label}`);
      entry.credential.setAttribute('aria-label', `Credential for ${label}`);
      entry.form.textContent = source.official ? 'Official' : (FORMS[source.form] ?? 'New source');
      entry.form.dataset.source = source.official ? 'official' : 'user';
      entry.credentialState.textContent = source.has_credential ? 'Credential saved' : 'No credential';
      entry.credentialState.hidden = source.official || source.order === undefined;
      // Officials are always trusted and never get a private exception.
      entry.trust.closest('label').hidden = source.official;
      entry.priv.closest('label').hidden = source.official;
      entry.credential.hidden = source.official;
      entry.detail.textContent = source.order === undefined ? 'Save to fetch this source.' : stateLine(source);
      entry.detail.dataset.state = source.status === 'failed' ? 'error' : '';
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
    const payload = await requestJson('/api/catalog/sources');
    elements.clear();
    rows.replaceChildren();
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
      const result = await requestJson('/api/catalog/sources', { method: 'POST', body: JSON.stringify({ sources: sources.map(({ source, enabled }) => ({ source, enabled })) }) });
      await load();
      notify(revocationNotice('Catalog sources saved.', result.revocation), 'success');
      window.dispatchEvent(new CustomEvent('vosu:catalog-changed'));
    } catch (error) { notify(errorText(error), 'error'); }
    finally { setBusy(save, false); }
  }, { className: 'button button-primary' });
  async function checkSources() {
    setBusy(check, true, 'Fetching…');
    try { await requestJson('/api/catalog/refresh', { method: 'POST' }); await load(); }
    catch (error) { notify(errorText(error), 'error'); }
    finally { setBusy(check, false); }
  }
  const check = button('Fetch sources', checkSources);
  const actions = node('div', undefined, 'network-actions');
  actions.append(add, check, save, status);
  section.append(intro, failures, rows, actions);
  const anchor = root.querySelector('.settings-group--about');
  if (anchor) anchor.before(section); else root.append(section);
  load().catch((error) => { status.textContent = errorText(error); });
}
