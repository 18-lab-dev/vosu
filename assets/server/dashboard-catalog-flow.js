// Catalog v2 dialogs: the trust warning, install-plan review with its one
// confirmation, job progress and outcome, and uninstall preview. Each step
// only presents what the dashboard-only `/api/catalog/*` routes return; the
// backend owns every decision (docs/v2/catalog-v2.md "Install plan", "Trust").
import { createActionDialog } from './homepage-settings.js';
import { button, node } from './dashboard-catalog.js';

// docs/v2/catalog-v2.md "Trust": shown verbatim immediately before every
// false→true grant. Nothing records that it was seen, so the next grant of
// the same source shows it again.
export const TRUST_WARNING = 'Plugins from this source can run in-process, with the same memory access as vosu itself. Only trust sources you control or know well.';
export const LINUX_USER_NAMESPACES = 'On Linux, this plugin can create user namespaces, which Chromium-style sandboxes need.';

const changed = () => window.dispatchEvent(new CustomEvent('vosu:catalog-changed'));

function actions(...children) {
  const row = node('div', undefined, 'dialog-actions');
  row.append(...children);
  return row;
}

// Resolves true only when the user presses "Trust source" in this dialog.
export function confirmTrustGrant(source) {
  return new Promise((resolve) => {
    let granted = false;
    const { dialog, body } = createActionDialog('Trust this catalog source?', { dismiss: 'explicit' });
    dialog.classList.add('catalog-dialog');
    const warning = node('p', TRUST_WARNING, 'catalog-warning');
    warning.id = `trust-warning-${Date.now()}`;
    const target = node('p', undefined, 'catalog-dialog__source');
    target.append('Source: ', node('code', source));
    const cancel = button('Cancel', () => dialog.close());
    const grant = button('Trust source', () => { granted = true; dialog.close(); }, { className: 'button button-danger button-danger--solid' });
    grant.setAttribute('aria-describedby', warning.id);
    body.append(warning, target, actions(cancel, grant));
    dialog.addEventListener('close', () => resolve(granted), { once: true });
    cancel.focus();
  });
}

export async function pollJob(requestJson, jobId, onPhase) {
  for (;;) {
    const job = await requestJson(`/api/catalog/jobs/${encodeURIComponent(jobId)}`);
    onPhase?.(job);
    if (job.status !== 'running') return job;
    await new Promise((resolve) => window.setTimeout(resolve, 300));
  }
}

const LOAD_LABELS = {
  loaded: 'Loaded',
  'web-rescanned': 'Ready',
  'requires-process-runtime': 'Installed, not running',
  refused: 'Not loaded',
  failed: 'Load failed',
};

// The job envelope says whether the commit succeeded; each package's load
// outcome comes from the status' `load_reports`.
async function outcome(requestJson, job, ids) {
  const box = node('div', undefined, 'catalog-outcome');
  box.setAttribute('role', 'status');
  if (job.status !== 'completed') {
    box.dataset.state = 'error';
    box.append(node('p', `Nothing was installed: ${job.error || 'the catalog job failed'}.`));
    return box;
  }
  box.dataset.state = 'ok';
  box.append(node('p', 'Installed.'));
  let reports = [];
  try { reports = (await requestJson('/api/catalog/status')).load_reports ?? []; } catch (_error) { /* outcome still shown */ }
  const list = node('ul', undefined, 'catalog-outcome__list');
  for (const report of reports.filter((entry) => ids.includes(entry.id))) {
    const item = node('li');
    item.dataset.load = report.status;
    item.append(node('code', report.id), ` — ${LOAD_LABELS[report.status] ?? report.status}`);
    if (report.detail) item.append(node('span', `: ${report.detail}`, 'field-help'));
    list.append(item);
  }
  if (list.childElementCount) box.append(list);
  return box;
}

function addRow(add, approvals) {
  const row = node('li', undefined, 'catalog-plan__add');
  row.dataset.planAdd = add.id;
  const head = node('p', undefined, 'catalog-plan__head');
  head.append(node('strong', add.id), ` ${add.kind} v${add.version}`);
  const reason = node('span', add.reason === 'requested' ? 'Requested' : 'Dependency', 'badge');
  head.append(' ', reason);
  row.append(head);
  const source = node('p', undefined, 'catalog-plan__source');
  source.append('From ', node('code', add.source));
  row.append(source);
  if (add.native) {
    const facts = node('p', undefined, 'catalog-plan__facts');
    const trust = { official: 'Official source', trusted: 'Trusted source', user: 'Untrusted source' }[add.trust] ?? add.trust;
    facts.append(node('span', 'Native code', 'badge badge-warning'), ' ',
      node('span', add.runtime === 'in-process' ? 'Runs inside vosu' : 'Runs as a separate process', 'badge'), ' ',
      node('span', trust, 'badge'));
    row.append(facts);
    row.append(node('p', add.runtime === 'in-process'
      ? 'This plugin runs inside the vosu process with the same memory access as vosu.'
      : 'This plugin runs as its own process with your account’s access.', 'field-help'));
  }
  if (add.requires_internal_paths) {
    row.append(node('p', 'Can read the attached osu! client’s install paths (vosu.osu internal data). '
      + 'This is a disclosure, not a sandbox.', 'catalog-warning catalog-plan__d8'));
  }
  if (add.linux_user_namespaces) {
    row.append(node('p', LINUX_USER_NAMESPACES, 'catalog-warning'));
  }
  if (add.needs_approval) {
    const label = node('label', undefined, 'catalog-plan__approve');
    const check = node('input'); check.type = 'checkbox';
    check.dataset.approve = add.id;
    label.append(check, ` Approve running ${add.id} ${add.version} from this exact source`);
    approvals.push(check);
    row.append(label);
  }
  return row;
}

// Reviews one plan and confirms it once. Resolves with the finished job, or
// null when the user closed the review without confirming.
export function reviewPlan({ plan, title, requestJson, notify, errorText }) {
  return new Promise((resolve) => {
    let result = null;
    const { dialog, body } = createActionDialog(title, { dismiss: 'explicit' });
    dialog.classList.add('catalog-dialog', 'catalog-plan');
    dialog.dataset.planId = plan.plan_id;
    const adds = plan.adds ?? [];
    const errors = plan.errors ?? [];
    const approvals = [];
    if (errors.length) {
      const failure = node('div', undefined, 'catalog-failures catalog-plan__errors');
      failure.setAttribute('role', 'alert');
      failure.append(node('p', 'This plan cannot be installed:'));
      const list = node('ul');
      for (const error of errors) list.append(node('li', error));
      failure.append(list);
      body.append(failure);
    } else {
      const native = adds.filter((add) => add.native).length;
      body.append(node('p', `${adds.length} ${adds.length === 1 ? 'package' : 'packages'} will be installed`
        + `${native ? `, ${native} with native code` : ''}. Dependencies install first; nothing is installed unless all of them are.`));
      const list = node('ol', undefined, 'catalog-plan__adds');
      list.append(...adds.map((add) => addRow(add, approvals)));
      body.append(list);
    }
    const progress = node('p', '', 'field-help catalog-plan__progress');
    progress.setAttribute('role', 'status');
    const cancel = button(errors.length ? 'Close' : 'Cancel', () => dialog.close());
    const confirm = button(adds.length === 1 ? 'Install' : `Install ${adds.length} packages`, run, {
      className: 'button button-primary', disabled: Boolean(errors.length) || !adds.length || approvals.length > 0,
    });
    const sync = () => { confirm.disabled = Boolean(errors.length) || !adds.length || approvals.some((check) => !check.checked); };
    for (const check of approvals) check.addEventListener('change', sync);
    body.append(progress, actions(cancel, confirm));
    dialog.addEventListener('close', () => resolve(result), { once: true });
    (approvals[0] ?? cancel).focus();

    async function run() {
      confirm.disabled = true; cancel.disabled = true;
      for (const check of approvals) check.disabled = true;
      progress.textContent = 'Starting…';
      try {
        const response = await requestJson(`/api/catalog/plans/${encodeURIComponent(plan.plan_id)}/confirm`, {
          method: 'POST',
          body: JSON.stringify({ approve: approvals.map((check) => check.dataset.approve) }),
        });
        const job = await pollJob(requestJson, response.job_id, (state) => { progress.textContent = `${state.phase}${state.tool ? ` ${state.tool}` : ''}…`; });
        result = job;
        progress.textContent = '';
        body.insertBefore(await outcome(requestJson, job, adds.map((add) => add.id)), progress);
        if (job.status === 'completed') notify('Catalog installation finished.', 'success');
      } catch (error) {
        progress.textContent = '';
        const failure = node('p', `Not installed: ${errorText(error)}`, 'catalog-failures');
        failure.setAttribute('role', 'alert');
        body.insertBefore(failure, progress);
      } finally {
        confirm.remove();
        cancel.disabled = false; cancel.textContent = 'Close'; cancel.focus();
        changed();
      }
    }
  });
}

// `POST /api/catalog/install` then the review above. This is the one
// install affordance other dashboard views call, the D20 `ingame-page`
// notice included: `installFlow({ entry: { kind, id, source, name },
// requestJson, notify, errorText })` resolves with the finished job, or
// null when the review was closed unconfirmed, and rejects with the
// request's error when no plan was created (a source that is missing,
// disabled or never fetched). An entry the source does not list gets the
// backend's error plan, shown but never confirmable. It never confirms,
// approves or downloads by itself; every plan goes through `reviewPlan`.
export async function installFlow({ entry, requestJson, notify, errorText }) {
  const plan = await requestJson('/api/catalog/install', {
    method: 'POST', body: JSON.stringify({ kind: entry.kind, id: entry.id, source: entry.source }),
  });
  return reviewPlan({ plan, title: `Install ${entry.name || entry.id}?`, requestJson, notify, errorText });
}

// Preview, then one explicit keep/remove-data choice, then the job.
export async function uninstallFlow({ target, name, requestJson, notify, errorText }) {
  const request = { kind: target.kind, id: target.id, source: target.source };
  const { dialog, body } = createActionDialog(`Uninstall ${name}?`, { dismiss: 'explicit' });
  dialog.classList.add('catalog-dialog', 'catalog-uninstall');
  const cancel = button('Cancel', () => dialog.close());
  let preview;
  try {
    preview = await requestJson('/api/catalog/uninstall/preview', { method: 'POST', body: JSON.stringify(request) });
  } catch (error) {
    const failure = node('div', undefined, 'catalog-failures');
    failure.setAttribute('role', 'alert');
    failure.append(node('p', `Can't uninstall ${name}: ${errorText(error)}`));
    body.append(failure, actions(cancel));
    cancel.textContent = 'Close'; cancel.focus();
    return null;
  }
  const list = node('ul', undefined, 'catalog-plan__adds');
  for (const removal of preview.removals ?? []) {
    const item = node('li', undefined, 'catalog-plan__add');
    item.dataset.removal = removal.id;
    item.append(node('strong', removal.id), ` ${removal.kind} v${removal.version} `,
      node('span', removal.reason === 'explicit' ? 'Selected' : 'No longer needed', 'badge'));
    list.append(item);
  }
  const autoremoved = (preview.removals ?? []).filter((removal) => removal.reason !== 'explicit').length;
  body.append(node('p', autoremoved
    ? `Also removes ${autoremoved} dependency ${autoremoved === 1 ? 'plugin' : 'plugins'} nothing else needs. Their data and settings stay.`
    : 'Nothing else depends on it.'), list);
  for (const dropped of preview.dropped_optional ?? []) {
    body.append(node('p', `${dropped.consumer} loses its optional ${dropped.capability} from ${dropped.provider}.`, 'field-help'));
  }
  const progress = node('p', '', 'field-help');
  progress.setAttribute('role', 'status');
  body.append(progress);
  const run = async (removeData) => {
    for (const control of body.querySelectorAll('button')) control.disabled = true;
    try {
      const response = await requestJson('/api/catalog/uninstall', { method: 'POST', body: JSON.stringify({ ...request, remove_data: removeData }) });
      const job = await pollJob(requestJson, response.job_id, (state) => { progress.textContent = `${state.phase}…`; });
      if (job.status !== 'completed') throw new Error(job.error || 'uninstall failed');
      notify(`${name} was uninstalled.`, 'success');
      dialog.close();
    } catch (error) {
      progress.textContent = `Not uninstalled: ${errorText(error)}`;
      for (const control of body.querySelectorAll('button')) control.disabled = false;
    } finally { changed(); }
  };
  body.append(actions(cancel,
    button('Uninstall, keep data', () => run(false), { className: 'button button-danger' }),
    button('Uninstall, remove data', () => run(true), { className: 'button button-danger button-danger--solid' })));
  cancel.focus();
  return preview;
}

// Records-only: an installed dependency becomes explicitly requested.
export async function promote({ target, requestJson, notify, errorText }) {
  try {
    await requestJson('/api/catalog/promote', {
      method: 'POST', body: JSON.stringify({ kind: target.kind, id: target.id, source: target.source }),
    });
    notify(`${target.id} is kept even when nothing needs it.`, 'success');
  } catch (error) { notify(`${target.id}: ${errorText(error)}`, 'error'); }
  changed();
}
