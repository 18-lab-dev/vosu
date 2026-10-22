function node(tag, text, className) {
  const element = document.createElement(tag);
  if (text !== undefined) element.textContent = text;
  if (className) element.className = className;
  return element;
}
function button(label, run, disabled = false) {
  const element = node('button', label, 'button'); element.type = 'button'; element.disabled = disabled;
  element.addEventListener('click', run); return element;
}
export function mountCatalogSources({ root, requestJson, notify, errorText, setBusy }) {
  const section = node('section', undefined, 'tools-catalog'); section.id = 'catalog-sources';
  const heading = node('h2', 'Catalog sources');
  const intro = node('p', 'Tools and overlays from owner/repo[@tag] or an HTTPS catalog.json URL. Earlier sources win duplicate ids. Only shiruviaa/vosu-tools grants automatic tool trust; all other tools need approval.', 'field-help');
  const rows = node('div', undefined, 'catalog-source-list');
  const status = node('p', '', 'field-help'); status.setAttribute('role', 'status');
  let sources = [];
  function render() {
    rows.replaceChildren(...sources.map((source, index) => {
      const row = node('div', undefined, 'catalog-source-row');
      const label = node('label');
      const enabled = node('input'); enabled.type = 'checkbox'; enabled.checked = source.enabled;
      enabled.setAttribute('aria-label', `Enable source ${index + 1}`);
      enabled.addEventListener('change', () => { source.enabled = enabled.checked; });
      label.append(enabled, ' Enabled');
      const input = node('input'); input.type = 'text'; input.value = source.source;
      input.className = 'setting-value'; input.setAttribute('aria-label', `Catalog source ${index + 1}`);
      input.placeholder = 'owner/repo@tag or https://…/catalog.json';
      input.addEventListener('input', () => { source.source = input.value.trim(); });
      const move = (offset) => { [sources[index], sources[index + offset]] = [sources[index + offset], sources[index]]; render(); };
      const actions = node('div', undefined, 'tool-card__actions');
      actions.append(button('Move up', () => move(-1), index === 0), button('Move down', () => move(1), index === sources.length - 1), button('Remove source', () => { sources.splice(index, 1); render(); }));
      const detail = node('p', `${source.status || 'Not fetched'}${source.fetched_at ? ` · ${source.fetched_at}` : ''}${source.last_error ? ` · ${source.last_error}` : ''}`, 'field-help');
      row.append(label, input, actions, detail); return row;
    }));
  }
  async function load() { const payload = await requestJson('/api/tools/catalog/sources'); sources = payload.sources; render(); }
  const save = button('Save catalog sources', async () => {
    setBusy(save, true, 'Saving…');
    try {
      await requestJson('/api/tools/catalog/sources', { method: 'POST', body: JSON.stringify({ sources: sources.map(({ source, enabled }) => ({ source, enabled })) }) });
      status.textContent = 'Sources saved. Fetching enabled catalogs…';
      await requestJson('/api/tools/catalog'); await load(); status.textContent = 'Saved; fetch results are shown for each source.';
      notify('Catalog sources saved.', 'success');
    } catch (error) { status.textContent = errorText(error); notify(errorText(error), 'error', 5000); }
    finally { setBusy(save, false); }
  });
  const refresh = button('Check sources', async () => {
    setBusy(refresh, true, 'Fetching…');
    try { await requestJson('/api/tools/catalog'); await load(); status.textContent = 'Source checks complete.'; }
    catch (error) { status.textContent = errorText(error); }
    finally { setBusy(refresh, false); }
  });
  const actions = node('div', undefined, 'tool-card__actions');
  actions.append(button('Add source', () => { sources.push({ source: '', enabled: true }); render(); }), save, refresh);
  section.append(heading, intro, rows, actions, status); root.append(section);
  load().catch((error) => { status.textContent = errorText(error); });
}

export function mountCatalogOverlays({ root, requestJson, notify, errorText, setBusy, confirmAction }) {
  const section = node('section', undefined, 'tools-catalog'); section.id = 'source-overlays';
  const heading = node('h2', 'Catalog source overlays');
  const disclosure = node('p', 'Third-party JavaScript runs on the overlay origin and can read the local overlay API. Install only overlays and sources you trust. The community catalog above remains independent.', 'field-help');
  const status = node('p', 'Fetching catalog sources…', 'field-help'); status.setAttribute('role', 'status');
  const cards = node('div', undefined, 'tools-root');
  async function install(overlay, action, control) {
    const requirements = overlay.requires_tools || [];
    if (requirements.length && !await confirmAction('Install required tools too?', requirements.map((r) => `${r.id} ${r.version}`).join('\n') + '\nCatalog tools run with your OS account’s access. Tools from non-official sources remain untrusted until approved.', 'Install tools and overlay')) return;
    setBusy(control, true, 'Queued…');
    try {
      const response = await requestJson(`/api/tools/catalog/overlays/${encodeURIComponent(overlay.id)}/${action}`, { method: 'POST', body: JSON.stringify({ install_tools: requirements.length > 0 }) });
      for (;;) {
        const job = await requestJson(`/api/tools/catalog/jobs/${encodeURIComponent(response.job_id)}`);
        status.textContent = `${overlay.name}: ${job.phase}`;
        if (job.status === 'failed') throw new Error(job.error || 'Overlay installation failed');
        if (job.status === 'completed') break;
        await new Promise((resolve) => window.setTimeout(resolve, 350));
      }
      notify(`${overlay.name}: ${action} completed.`, 'success'); await load();
    } catch (error) { notify(errorText(error), 'error', 5000); status.textContent = errorText(error); }
    finally { setBusy(control, false); }
  }
  async function load() {
    try {
      const payload = await requestJson('/api/tools/catalog');
      const overlays = [...payload.overlays, ...payload.shadowed.filter((entry) => entry.kind === 'overlay')];
      status.textContent = `${payload.overlays.length} overlays · ${payload.sources.map((source) => `${source.source}: ${source.status}${source.last_error ? ` (${source.last_error})` : ''}`).join(' · ')}`;
      cards.replaceChildren(...overlays.map((overlay) => {
        const card = node('article', undefined, 'tool-card');
        const monogram = node('div', overlay.name.slice(0, 2), 'tool-card__icon'); monogram.setAttribute('aria-hidden', 'true');
        const head = node('div', undefined, 'tool-card__head');
        const badges = node('p', undefined, 'tool-card__index');
        badges.append(node('span', overlay.shadowed ? 'Shadowed' : 'Catalog overlay', 'badge'));
        if (overlay.update_available) badges.append(node('span', 'Update available', 'badge'));
        const title = node('h2', overlay.name); title.append(node('span', `v${overlay.version}`, 'tool-card__version'));
        head.append(badges, title, node('p', `by ${overlay.author} · ${overlay.source}`, 'tool-card__author'));
        card.append(monogram, head, node('p', overlay.description, 'tool-card__description'));
        const actions = node('div', undefined, 'tool-card__actions');
        if (overlay.shadowed) actions.append(node('span', 'An earlier source provides this id.', 'field-help'));
        else if (!overlay.installed || overlay.update_available && overlay.managed) {
          const action = overlay.installed ? 'update' : 'install';
          const control = button(action === 'update' ? 'Update overlay' : 'Install overlay', () => install(overlay, action, control)); actions.append(control);
        } else actions.append(node('span', overlay.managed ? `Installed v${overlay.installed_version}` : 'Installed outside this source', 'field-help'));
        if (overlay.installed) { const link = node('a', 'Manage installed overlay', 'button'); link.href = '/'; actions.append(link); }
        card.append(actions); return card;
      }));
    } catch (error) { status.textContent = `Catalog sources unavailable: ${errorText(error)}`; }
  }
  const refresh = button('Refresh source overlays', load);
  section.append(heading, disclosure, refresh, status, cards); root.after(section); load();
}
