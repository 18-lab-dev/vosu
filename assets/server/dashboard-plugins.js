// Tools page, Plugins section: renders `GET /api/plugins` and drives the
// approval, restart and stop routes of docs/v2/catalog-v2.md
// "Process-plugin approval and management" (A5.5).
//
// The approval prompt is not an installer: it shows what will run (id,
// where it came from, runtime, both digests, the capabilities it provides
// and asks for, the install-path disclosure) and that it runs with the
// user's own OS rights. Cancelling leaves the plugin unloaded. Restart and
// stop name the generation this view showed, so a stale button never acts
// on a newer generation.

const POLL_MS = 3000;

const STATES = {
  approval_required: { label: 'Needs approval', dot: 'warn' },
  starting: { label: 'Starting', dot: 'busy' },
  running: { label: 'Running', dot: 'ok' },
  backoff: { label: 'Restarting', dot: 'warn' },
  failed: { label: 'Failed', dot: 'error' },
  stopped: { label: 'Stopped', dot: 'idle' },
};

const TRUST = { bundled: 'Bundled', development: 'Developer build', user: 'User trust' };
const RUNTIME = { process: 'Separate process', 'in-process': 'Inside vosu' };

// What each `reason` means to a person (plugin-abi.md §7.6 stages and loss
// reasons); anything else is shown as its code.
const REASONS = {
  approval: 'it has not been approved',
  load: 'its package could not be read',
  create: 'it failed to start',
  crash: 'it crashed while starting',
  timeout: 'it did not finish starting in time',
  security: 'its security setup failed',
  library: 'its library could not be loaded',
  abi: 'it was built for another plugin interface',
  generation: 'vosu was updated; restart vosu',
  exited: 'it exited unexpectedly',
  protocol_violation: 'it broke the plugin protocol',
  pressure_timeout: 'it stopped responding',
  transport_timeout: 'it lost contact with vosu',
  action_grace_expired: 'it ignored a cancelled action',
  refused: 'vosu refused to start it',
};

function el(tag, { className, text, attrs } = {}, ...children) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  for (const [name, value] of Object.entries(attrs ?? {})) {
    if (value === false || value === null || value === undefined) continue;
    node.setAttribute(name, value === true ? '' : String(value));
  }
  node.append(...children.filter((child) => child !== null && child !== undefined && child !== false));
  return node;
}

function list(values) {
  return Array.isArray(values) ? values : [];
}

function nameOf(plugin) {
  return plugin.name || plugin.id || plugin.provenance?.path || 'Unnamed plugin';
}

function stateLine(plugin) {
  const reason = plugin.reason ? (REASONS[plugin.reason] ?? plugin.reason) : '';
  switch (plugin.state) {
    case 'approval_required':
      return 'It stays unloaded until you review and approve it.';
    case 'backoff': {
      const seconds = Math.max(1, Math.round((plugin.restart_delay_ms ?? 0) / 1000));
      const crashes = plugin.crash_events ?? 0;
      return `Restarting in ${seconds} s · ${crashes} crash${crashes === 1 ? '' : 'es'} in the last 5 minutes`;
    }
    case 'failed':
      return reason ? `Stopped because ${reason}.` : 'Stopped after a failure.';
    case 'stopped':
      return 'Stopped. It stays stopped until you restart it.';
    default:
      return '';
  }
}

// Insert the section's stylesheet once; the section only exists on Tools.
function ensureStyles() {
  if (document.querySelector('link[data-plugins-css]')) return;
  document.head.append(el('link', { attrs: { rel: 'stylesheet', href: '/assets/dashboard-plugins.css', 'data-plugins-css': true } }));
}

export function mountPlugins({ before, requestJson, notify, setBusy, errorText, confirmAction, createActionDialog }) {
  if (!before) return;
  ensureStyles();
  const state = { plugins: [], signature: '', pending: new Set(), timer: 0, loaded: false };
  const status = el('p', { className: 'tools-loading', text: 'Reading plugins…', attrs: { role: 'status' } });
  const rows = el('ul', { className: 'plugin-list', attrs: { 'aria-labelledby': 'plugins-title' } });
  const bundled = el('details', { className: 'plugins-bundled' });
  const section = el('section', { className: 'plugins', attrs: { 'aria-labelledby': 'plugins-title', 'aria-busy': 'true' } },
    el('header', { className: 'tools-catalog__header' }, el('h2', { text: 'Plugins', attrs: { id: 'plugins-title' } })),
    status, rows, bundled);
  before.before(section);

  const api = (id, action) => `/api/plugins/${encodeURIComponent(id)}/${action}`;

  function button(label, describe, { primary = false, danger = false } = {}) {
    return el('button', {
      className: `button${primary ? ' button-primary' : ''}${danger ? ' button-danger' : ''}`,
      text: label,
      attrs: { type: 'button', 'aria-label': describe },
    });
  }

  function facts(entries) {
    return el('dl', { className: 'v-facts' }, ...entries.flatMap(([term, value]) => [el('dt', { text: term }), el('dd', {}, value)]));
  }

  function capabilityList(items) {
    const entries = list(items);
    if (!entries.length) return document.createTextNode('None');
    return el('ul', { className: 'plugin-capabilities' },
      ...entries.map((item) => el('li', {}, el('code', { text: item.id }), item.version && item.version !== '*' ? ` ${item.version}` : '')));
  }

  // The approval prompt. Resolves true only when the user approves.
  function review(plugin) {
    const name = nameOf(plugin);
    const { dialog, body } = createActionDialog(`Approve ${name}?`);
    dialog.classList.add('plugin-approval');
    body.append(
      el('p', { className: 'plugin-approval__warning', text: `${name} runs native code in its own process with your OS account's rights: it can read and write your files and use the network. It is not sandboxed. Approve only code you trust.` }),
      facts([
        ['Plugin', el('code', { text: plugin.id })],
        ['Version', plugin.version ? `v${plugin.version}` : 'Not stated'],
        ['From', el('code', { text: plugin.provenance?.path ?? 'Unknown' })],
        ['Runtime', RUNTIME[plugin.runtime] ?? plugin.runtime],
        ['Manifest SHA-256', el('code', { className: 'plugin-digest', text: plugin.approval.manifest_sha256 })],
        ['Library SHA-256', el('code', { className: 'plugin-digest', text: plugin.approval.library_sha256 })],
        ['Provides', capabilityList(plugin.provides)],
        ['Requires', capabilityList(plugin.requires)],
        ['Optional', capabilityList(plugin.optional)],
      ]),
    );
    if (plugin.requires_internal_paths) {
      body.append(el('p', { className: 'plugin-approval__disclosure', text: 'It asks for osu! internal paths: it can see where osu! and its files are installed on this computer.' }));
    }
    body.append(el('p', { className: 'field-help', text: 'This approval covers exactly these files. If they change, vosu asks again.' }));
    const cancel = button('Cancel', `Cancel; ${name} stays unloaded`);
    const approve = button('Approve and start', `Approve and start ${name}`, { primary: true });
    body.append(el('div', { className: 'dialog-actions' }, cancel, approve));
    cancel.focus();
    return new Promise((resolve) => {
      let approved = false;
      cancel.addEventListener('click', () => dialog.close());
      approve.addEventListener('click', () => { approved = true; dialog.close(); });
      dialog.addEventListener('close', () => resolve(approved), { once: true });
    });
  }

  async function act(control, plugin, verb, request) {
    if (state.pending.has(plugin.id)) return;
    state.pending.add(plugin.id);
    setBusy(control, true, { approve: 'Starting…', revoke: 'Revoking…', restart: 'Restarting…', stop: 'Stopping…' }[verb]);
    try {
      const view = await requestJson(request.url, { method: request.method, ...(request.body ? { body: JSON.stringify(request.body) } : {}) });
      const outcome = STATES[view.state]?.label ?? view.state;
      notify(`${nameOf(plugin)}: ${outcome}.`, view.state === 'failed' ? 'error' : 'success');
    } catch (error) {
      notify(`Could not ${verb} ${nameOf(plugin)}: ${errorText(error)}`, 'error', 5000);
    } finally {
      state.pending.delete(plugin.id);
      setBusy(control, false);
      await refresh();
    }
  }

  function actions(plugin) {
    const name = nameOf(plugin);
    const wrap = el('div', { className: 'plugin-row__actions' });
    if (plugin.runtime !== 'process' || !plugin.id) return wrap;
    const generation = { generation: plugin.generation };
    if (plugin.state === 'approval_required' && plugin.approval) {
      const approve = button('Review…', `Review and approve ${name}`, { primary: true });
      approve.addEventListener('click', async () => {
        if (!await review(plugin)) return;
        act(approve, plugin, 'approve', { url: api(plugin.id, 'approval'), method: 'POST', body: plugin.approval });
      });
      wrap.append(approve);
      return wrap;
    }
    if (['failed', 'stopped', 'running', 'backoff'].includes(plugin.state)) {
      const restart = button(plugin.state === 'running' ? 'Restart' : 'Start', `${plugin.state === 'running' ? 'Restart' : 'Start'} ${name}`, { primary: plugin.state !== 'running' });
      restart.addEventListener('click', () => act(restart, plugin, 'restart', { url: api(plugin.id, 'restart'), method: 'POST', body: generation }));
      wrap.append(restart);
    }
    if (['running', 'backoff', 'starting'].includes(plugin.state)) {
      const stop = button('Stop', `Stop ${name}`);
      stop.addEventListener('click', () => act(stop, plugin, 'stop', { url: api(plugin.id, 'stop'), method: 'POST', body: generation }));
      wrap.append(stop);
    }
    if (plugin.trust === 'user' && plugin.approved) {
      const revoke = button('Revoke approval', `Revoke approval for ${name}`, { danger: true });
      revoke.addEventListener('click', async () => {
        if (!await confirmAction(`Revoke approval for ${name}?`, `${name} stops now and stays unloaded until you approve it again.`, 'Revoke', { danger: true })) return;
        act(revoke, plugin, 'revoke', { url: api(plugin.id, 'approval'), method: 'DELETE' });
      });
      wrap.append(revoke);
    }
    return wrap;
  }

  function row(plugin) {
    const shown = STATES[plugin.state] ?? { label: plugin.state, dot: 'idle' };
    const meta = [plugin.id, RUNTIME[plugin.runtime], TRUST[plugin.trust]].filter(Boolean).join(' · ');
    const line = stateLine(plugin);
    return el('li', { className: 'plugin-row', attrs: { 'data-plugin-id': plugin.id ?? '', 'data-state': plugin.state } },
      el('div', { className: 'plugin-row__main' },
        el('p', { className: 'plugin-row__title' },
          el('span', { className: 'plugin-row__name', text: nameOf(plugin) }),
          plugin.version ? el('span', { className: 'tool-card__version', text: `v${plugin.version}` }) : null,
          el('span', { className: 'tool-status', attrs: { 'data-status': plugin.state } },
            el('span', { className: 'v-dot', attrs: { 'data-state': shown.dot, 'aria-hidden': 'true' } }), shown.label)),
        meta ? el('p', { className: 'plugin-row__meta', text: meta }) : null,
        line ? el('p', { className: 'plugin-row__line', text: line }) : null,
        plugin.detail && plugin.state !== 'running' ? el('p', { className: 'tool-detail', text: plugin.detail }) : null),
      actions(plugin));
  }

  function render() {
    const external = state.plugins.filter((plugin) => plugin.trust !== 'bundled');
    const builtIn = state.plugins.filter((plugin) => plugin.trust === 'bundled');
    rows.replaceChildren(...external.map(row));
    status.textContent = external.length
      ? ''
      : 'No development plugins loaded. Start vosu with --plugin <folder> to load one.';
    const failing = builtIn.filter((plugin) => plugin.state !== 'running');
    bundled.replaceChildren(
      el('summary', { text: `${builtIn.length} bundled plugin${builtIn.length === 1 ? '' : 's'} · ${failing.length ? `${failing.length} not running` : 'all running'}` }),
      el('ul', { className: 'plugin-list' }, ...builtIn.map(row)));
    bundled.hidden = builtIn.length === 0;
  }

  async function refresh() {
    window.clearTimeout(state.timer);
    try {
      const payload = await requestJson('/api/plugins');
      state.plugins = list(payload.plugins);
      // Re-render only on change, so focus and open menus survive polling.
      const signature = JSON.stringify(state.plugins);
      if (signature !== state.signature || !state.loaded) {
        state.signature = signature;
        render();
      }
      state.loaded = true;
      section.removeAttribute('aria-busy');
    } catch (error) {
      if (!state.loaded) status.textContent = `Couldn't read plugins: ${errorText(error)}`;
    } finally {
      if (!document.hidden) state.timer = window.setTimeout(refresh, POLL_MS);
    }
  }

  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) refresh();
  });
  refresh();
}
