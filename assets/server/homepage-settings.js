let actionId = 0;

// A modal opened over another one keeps a clear backdrop, so stacked dialogs
// do not darken the page further with every level.
export function showModal(dialog) {
  if (document.querySelector('dialog[open]')) dialog.dataset.stacked = '';
  else delete dialog.dataset.stacked;
  dialog.showModal();
}

// Esc, a backdrop click and Cancel/✕ all go through requestClose(). A dialog
// holding unsaved input asks before discarding it; an `explicit` dialog (a
// key shown once) closes only through its own buttons.
export function guardDialog(dialog, { isDirty = () => false, explicit = false } = {}) {
  let bar = null;
  function hideBar() {
    bar?.remove();
    bar = null;
  }
  function askDiscard() {
    if (bar) {
      bar.querySelector('[data-keep]')?.focus();
      return;
    }
    bar = document.createElement('div');
    bar.className = 'discard-bar';
    const message = document.createElement('p');
    message.setAttribute('role', 'alert');
    message.textContent = 'Discard unsaved changes?';
    const keep = document.createElement('button');
    keep.type = 'button';
    keep.className = 'button';
    keep.dataset.keep = '';
    keep.textContent = 'Keep editing';
    keep.addEventListener('click', hideBar);
    const discard = document.createElement('button');
    discard.type = 'button';
    discard.className = 'button button-danger';
    discard.textContent = 'Discard';
    discard.addEventListener('click', () => {
      hideBar();
      dialog.close();
    });
    bar.append(message, keep, discard);
    (dialog.querySelector('.dialog-frame') ?? dialog).append(bar);
    keep.focus();
  }
  function requestClose() {
    if (explicit) return;
    if (isDirty()) askDiscard();
    else dialog.close();
  }
  dialog.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape' || event.defaultPrevented) return;
    // An open menu inside the dialog closes first, natively.
    if (dialog.querySelector(':popover-open')) return;
    event.preventDefault();
    if (bar) hideBar();
    else requestClose();
  });
  // Fallback for close requests that are not an Escape keydown.
  dialog.addEventListener('cancel', (event) => {
    event.preventDefault();
    requestClose();
  });
  dialog.addEventListener('click', (event) => {
    if (event.target !== dialog) return;
    const bounds = dialog.getBoundingClientRect();
    if (event.clientX < bounds.left || event.clientX > bounds.right
      || event.clientY < bounds.top || event.clientY > bounds.bottom) requestClose();
  });
  dialog.addEventListener('close', hideBar);
  return requestClose;
}

function closeButton(label, run) {
  const close = document.createElement('button');
  close.type = 'button';
  close.className = 'v-btn v-btn--ghost v-btn--icon';
  close.textContent = '✕';
  close.setAttribute('aria-label', label);
  close.addEventListener('click', run);
  return close;
}

// `dismiss`: 'light' closes freely, 'guarded' asks before discarding when
// `isDirty()` is true, 'explicit' closes only through the dialog's buttons.
export function createActionDialog(title, { dismiss = 'light', isDirty } = {}) {
  const dialog = document.createElement('dialog');
  dialog.className = 'dashboard-dialog action-dialog';
  const frame = document.createElement('div');
  frame.className = 'dialog-frame';
  const header = document.createElement('header');
  header.className = 'dialog-header';
  const heading = document.createElement('h2');
  heading.id = `action-title-${++actionId}`;
  heading.textContent = title;
  dialog.setAttribute('aria-labelledby', heading.id);
  header.append(heading);
  const body = document.createElement('div');
  body.className = 'dialog-body';
  frame.append(header, body);
  dialog.append(frame);
  const requestClose = guardDialog(dialog, {
    explicit: dismiss === 'explicit',
    isDirty: dismiss === 'guarded' ? isDirty : undefined,
  });
  if (dismiss !== 'explicit') header.append(closeButton('Close', requestClose));
  dialog.addEventListener('close', () => dialog.remove(), { once: true });
  document.body.append(dialog);
  showModal(dialog);
  return { dialog, body, requestClose };
}

export function createOverflow(label, text = '⋯') {
  const wrapper = document.createElement('div');
  wrapper.className = 'settings-overflow';
  const toggle = document.createElement('button');
  toggle.type = 'button';
  toggle.className = 'icon-button card-menu-toggle';
  toggle.textContent = text;
  toggle.setAttribute('aria-label', label);
  const menu = document.createElement('div');
  menu.className = 'card-menu';
  menu.id = `action-menu-${++actionId}`;
  menu.setAttribute('popover', 'auto');
  menu.setAttribute('role', 'menu');
  toggle.setAttribute('popovertarget', menu.id);
  wrapper.append(toggle, menu);
  return { wrapper, menu };
}

// Above this many settings a search field helps; below it is noise.
const SEARCH_THRESHOLD = 8;

export function toolAutostartControl(view, { requestJson, notify, errorText, onChange = () => {} }) {
  if (view.service_role !== 'background') return null;
  const row = document.createElement('div');
  row.className = 'v-row';
  const label = document.createElement('label');
  label.className = 'v-check';
  const toggle = document.createElement('input');
  toggle.type = 'checkbox';
  toggle.className = 'v-toggle';
  toggle.setAttribute('role', 'switch');
  toggle.checked = Boolean(view.starts_with_vosu);
  label.append(toggle, 'Start with vosu');
  const reset = document.createElement('button');
  reset.type = 'button';
  reset.className = 'v-btn v-btn--ghost v-btn--sm';
  reset.textContent = 'Use default';
  reset.disabled = view.autostart === null;
  async function save(autostart) {
    toggle.disabled = reset.disabled = true;
    try {
      view = await requestJson(`/api/tools/${encodeURIComponent(view.id)}/autostart`, {
        method: 'PUT', body: JSON.stringify({ autostart }),
      });
      onChange(view);
    } catch (error) {
      notify(`Could not save startup: ${errorText(error)}`, 'error');
    } finally {
      toggle.checked = Boolean(view.starts_with_vosu);
      toggle.disabled = false;
      reset.disabled = view.autostart === null;
    }
  }
  toggle.addEventListener('change', () => save(toggle.checked));
  reset.addEventListener('click', () => save(null));
  row.append(label, reset);
  return row;
}

export function createSettingsEditor({
  results, requestJson, notify, setBusy, errorText, absoluteHttpUrl,
}) {
  const state = { dialogRequest: 0, navigationObserver: null, dirty: false, fieldId: 0 };

  function choiceControl(values, value, labels = values) {
    const select = document.createElement('select');
    select.className = 'setting-value';
    values.forEach((item, index) => select.add(new Option(labels[index], item)));
    select.value = String(value);
    return select;
  }

  const dialog = document.querySelector('#dashboard-dialog');
  const dialogTitle = document.querySelector('#dialog-title');
  const dialogBody = document.querySelector('#dialog-body');
  const requestDialogClose = dialog ? guardDialog(dialog, { isDirty: () => state.dirty }) : () => {};
  dialog?.addEventListener('close', () => {
    state.navigationObserver?.disconnect();
    state.dirty = false;
    dialog.querySelector('[data-tool-startup]')?.remove();
  });
  // An inline settings page with unsaved edits asks before navigating away.
  window.addEventListener('beforeunload', (event) => {
    if (state.dirty && !dialog?.open) event.preventDefault();
  });

  function cloneValue(value) {
    if (typeof structuredClone === 'function') return structuredClone(value);
    return JSON.parse(JSON.stringify(value));
  }

  function openDialog(title) {
    if (!dialog || !dialogBody) return null;
    const requestId = ++state.dialogRequest;
    state.dirty = false;
    dialogTitle.textContent = title;
    dialog.querySelector('[data-tool-startup]')?.remove();
    dialogBody.replaceChildren();
    if (!dialog.open) showModal(dialog);
    return { container: dialogBody, requestId };
  }

  function isCurrentDialogRequest(requestId) {
    return requestId === null || (state.dialogRequest === requestId && Boolean(dialog?.open));
  }

  // Closes without asking: used after a successful save.
  function forceCloseDialog() {
    state.dialogRequest += 1;
    state.dirty = false;
    if (dialog?.open) dialog.close();
  }

  function fieldLabel(text, control) {
    const label = document.createElement('label');
    label.className = 'field-label';
    const name = document.createElement('span');
    name.textContent = text;
    label.append(name, control);
    return label;
  }

  function makeInput(type, value) {
    const input = document.createElement('input');
    input.type = type;
    input.className = 'setting-value';
    if (type === 'checkbox') {
      input.checked = Boolean(value);
      input.classList.add('v-toggle');
      input.setAttribute('role', 'switch');
    } else if (value !== undefined && value !== null) input.value = String(value);
    return input;
  }

  function mergeCommandValues(setting, configured) {
    const defaults = Array.isArray(setting.value) ? cloneValue(setting.value) : [];
    if (!Array.isArray(configured)) return defaults;
    const uniqueKey = setting.uniqueCheck;
    if (!uniqueKey) return cloneValue(configured);
    const merged = defaults.map((entry) => {
      const replacement = configured.find((candidate) => candidate?.[uniqueKey] === entry?.[uniqueKey]);
      return replacement ? { ...entry, ...replacement } : entry;
    });
    configured.forEach((entry) => {
      if (!merged.some((candidate) => candidate?.[uniqueKey] === entry?.[uniqueKey])) {
        merged.push(cloneValue(entry));
      }
    });
    return merged;
  }

  function commandControl(option, value) {
    const type = String(option.type ?? 'text').toLowerCase();
    let control;
    if (type === 'checkbox') {
      control = makeInput('checkbox', value ?? option.value ?? false);
    } else if (type === 'number') {
      control = makeInput('number', value ?? option.value ?? 0);
      control.step = 'any';
    } else if (type === 'options') {
      const values = Array.isArray(option.values) ? option.values.filter((item) => item !== '').map(String) : [];
      control = choiceControl(values, value ?? option.value ?? values[0] ?? '');
    } else {
      control = makeInput('text', value ?? option.value ?? '');
    }
    control.dataset.commandName = String(option.name ?? '');
    return control;
  }

  function appendCommandRow(list, setting, command = {}) {
    const row = document.createElement('div');
    row.className = 'command-row';
    const fields = document.createElement('div');
    fields.className = 'command-fields';
    const options = Array.isArray(setting.options) ? setting.options : [];
    options.filter((option) => option && option.name).forEach((option) => {
      const wrapper = document.createElement('label');
      wrapper.className = 'command-field';
      const title = document.createElement('span');
      title.textContent = String(option.title || option.name);
      const control = commandControl(option, command[option.name]);
      wrapper.append(title, control);
      fields.appendChild(wrapper);
    });
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.className = 'button button-danger';
    remove.textContent = 'Remove command';
    remove.addEventListener('click', () => {
      // Keep focus in the list instead of dropping it to the page.
      const next = row.nextElementSibling ?? row.previousElementSibling;
      row.remove();
      state.dirty = true;
      (next?.querySelector('input, select, button') ?? list.parentElement?.querySelector('.command-add'))?.focus();
    });
    row.append(fields, remove);
    list.appendChild(row);
  }

  // One row per setting, like the runtime Settings page: title and
  // description on the left, the control on the right. Wide controls
  // (text areas, command lists) stack under their title.
  // `secret` is defined only for hosts that withhold password values (tools):
  // true when one is in effect, false when none is.
  function renderSettingField(setting, configuredValue, secret) {
    const kind = String(setting.type ?? 'text').toLowerCase();
    const id = String(setting.uniqueID ?? '');
    if (kind === 'header') {
      const heading = document.createElement('h3');
      heading.className = 'counter-settings-heading';
      heading.textContent = String(setting.title || id);
      return heading;
    }

    const fieldId = `setting-field-${++state.fieldId}`;
    const row = document.createElement('div');
    row.className = 'counter-setting settings-item';
    row.dataset.settingId = id;
    row.dataset.settingType = kind;
    const copy = document.createElement('span');
    copy.className = 'settings-item__copy';
    const grouped = kind === 'commands' || kind === 'button';
    const name = document.createElement(grouped ? 'span' : 'label');
    name.className = 'settings-item__name';
    name.id = `${fieldId}-name`;
    name.textContent = String(setting.title || id);
    if (!grouped) name.htmlFor = fieldId;
    copy.append(name);
    let describedBy = '';
    if (setting.description) {
      const description = document.createElement('span');
      description.className = 'settings-item__description setting-description';
      description.id = `${fieldId}-description`;
      description.textContent = String(setting.description);
      copy.append(description);
      describedBy = description.id;
    }
    const slot = document.createElement('span');
    slot.className = 'settings-item__control';
    row.append(copy, slot);
    const finish = (control) => {
      control.id = fieldId;
      if (describedBy) control.setAttribute('aria-describedby', describedBy);
      slot.append(control);
      return row;
    };

    const value = configuredValue === undefined ? setting.value : configuredValue;
    if (kind === 'commands') {
      row.classList.add('settings-item--stacked');
      row.setAttribute('role', 'group');
      row.setAttribute('aria-labelledby', name.id);
      const list = document.createElement('div');
      list.className = 'command-list';
      mergeCommandValues(setting, value).forEach((command) => appendCommandRow(list, setting, command));
      const add = document.createElement('button');
      add.type = 'button';
      add.className = 'button command-add';
      add.textContent = 'Add command';
      add.addEventListener('click', () => {
        const seed = {};
        (Array.isArray(setting.options) ? setting.options : []).forEach((option) => {
          if (option?.name) seed[option.name] = cloneValue(option.value ?? '');
        });
        appendCommandRow(list, setting, seed);
        state.dirty = true;
        list.lastElementChild?.querySelector('input, select')?.focus();
      });
      slot.append(list, add);
      return row;
    }

    if (kind === 'checkbox') return finish(makeInput('checkbox', value));
    if (kind === 'number') {
      const control = makeInput('number', value);
      control.step = 'any';
      return finish(control);
    }
    if (kind === 'color') return finish(makeInput('color', value || '#f45cad'));
    if (kind === 'password') {
      const control = makeInput('password', secret === undefined ? value : '');
      control.autocomplete = 'off';
      if (secret !== true) return finish(control);
      // The stored secret is never sent here: blank keeps it, Clear removes it.
      control.placeholder = 'Saved — leave blank to keep';
      const clear = document.createElement('button');
      clear.type = 'button';
      clear.className = 'button setting-secret-clear';
      clear.textContent = 'Clear';
      clear.addEventListener('click', () => {
        const clearing = row.dataset.clearSecret !== 'true';
        row.dataset.clearSecret = String(clearing);
        control.value = '';
        control.placeholder = clearing ? 'Will be cleared on save' : 'Saved — leave blank to keep';
        clear.textContent = clearing ? 'Keep' : 'Clear';
        clear.setAttribute('aria-pressed', String(clearing));
        state.dirty = true;
      });
      control.addEventListener('input', () => {
        if (control.value && row.dataset.clearSecret === 'true') clear.click();
      });
      const secretRow = document.createElement('span');
      secretRow.className = 'setting-secret';
      finish(control);
      secretRow.append(control, clear);
      slot.append(secretRow);
      return row;
    }
    if (kind === 'textarea') {
      row.classList.add('settings-item--stacked');
      const control = document.createElement('textarea');
      control.className = 'setting-value';
      control.value = value === undefined || value === null ? '' : String(value);
      return finish(control);
    }
    if (kind === 'options') {
      const options = Array.isArray(setting.options) ? setting.options.filter((item) => item !== '').map(String) : [];
      return finish(choiceControl(options, value ?? options[0] ?? ''));
    }
    if (kind === 'button') {
      const href = absoluteHttpUrl(value);
      const link = document.createElement(href ? 'a' : 'span');
      link.className = 'button';
      link.textContent = 'Open';
      link.setAttribute('aria-labelledby', name.id);
      if (href) {
        link.href = href;
        link.target = '_blank';
        link.rel = 'noopener noreferrer';
      } else {
        link.setAttribute('aria-disabled', 'true');
      }
      row._settingValue = cloneValue(value);
      slot.append(link);
      return row;
    }
    return finish(makeInput('text', value));
  }

  function collectCommandRows(field) {
    return Array.from(field.querySelectorAll('.command-row')).map((row) => {
      const command = {};
      row.querySelectorAll('[data-command-name]').forEach((control) => {
        const name = control.dataset.commandName;
        if (!name) return;
        if (control.type === 'checkbox') command[name] = control.checked;
        else if (control.type === 'number' && control.value !== '') command[name] = Number(control.value);
        else command[name] = control.value;
      });
      return command;
    });
  }

  function collectCounterSettings(form, schema) {
    const payload = [];
    form.querySelectorAll('.counter-setting[data-setting-id]').forEach((field) => {
      const id = field.dataset.settingId;
      const kind = field.dataset.settingType;
      let value;
      if (kind === 'commands') {
        value = collectCommandRows(field);
        const setting = schema.find((candidate) => candidate.uniqueID === id);
        const key = setting?.uniqueCheck;
        if (key) {
          const seen = new Set();
          value.forEach((command) => {
            const identity = command[key];
            if (identity === undefined || identity === '') throw new Error(`Every ${id} command needs ${key}.`);
            if (seen.has(String(identity))) throw new Error(`${id} contains a duplicate ${key}.`);
            seen.add(String(identity));
          });
        }
      } else if (kind === 'button') {
        value = cloneValue(field._settingValue);
      } else {
        const control = field.querySelector('.setting-value');
        if (!control) return;
        if (kind === 'checkbox') value = control.checked;
        else if (kind === 'number' && control.value !== '') value = Number(control.value);
        else if (kind === 'password' && field.dataset.clearSecret === 'true' && !control.value) value = null;
        else value = control.value;
      }
      payload.push({ uniqueID: id, value });
    });
    return payload;
  }

  function profilePrompt(action, profile) {
    return new Promise((resolve) => {
      const deleting = action === 'delete';
      const titles = { create: 'Create empty profile', copy: 'Copy profile', rename: 'Rename profile', delete: 'Delete profile' };
      const input = makeInput('text', action === 'rename' ? profile : '');
      const initial = input.value;
      const { dialog: prompt, body } = createActionDialog(titles[action], {
        dismiss: deleting ? 'light' : 'guarded',
        isDirty: () => input.value !== initial,
      });
      const form = document.createElement('form');
      form.className = 'dialog-form';
      input.required = true;
      input.maxLength = 48;
      input.pattern = '[A-Za-z0-9_-]+';
      input.title = 'Letters, numbers, hyphens and underscores';
      if (deleting) {
        const text = document.createElement('p');
        text.textContent = `Delete profile “${profile}”? This cannot be undone.`;
        form.append(text);
      } else form.append(fieldLabel('Profile name', input));
      const actions = document.createElement('div');
      actions.className = 'dialog-actions';
      const cancel = document.createElement('button');
      cancel.type = 'button'; cancel.className = 'button'; cancel.textContent = 'Cancel';
      cancel.addEventListener('click', () => prompt.close());
      const submit = document.createElement('button');
      submit.type = 'submit'; submit.className = deleting ? 'button button-danger' : 'button button-primary';
      submit.textContent = deleting ? 'Delete profile' : 'Save profile';
      actions.append(cancel, submit); form.append(actions); body.append(form);
      prompt.addEventListener('close', () => resolve(null), { once: true });
      form.addEventListener('submit', (event) => {
        event.preventDefault();
        resolve(deleting ? profile : input.value.trim());
        prompt.close();
      });
      (deleting ? cancel : input).focus();
    });
  }

  // Actions that reload the editor (switching or managing profiles) ask
  // first when it holds unsaved input, in the dashboard's own dialog.
  function confirmDiscard() {
    if (!state.dirty) return Promise.resolve(true);
    return new Promise((resolve) => {
      const { dialog: prompt, body } = createActionDialog('Discard unsaved changes?');
      let discard = false;
      const actions = document.createElement('div');
      actions.className = 'dialog-actions';
      const keep = document.createElement('button');
      keep.type = 'button'; keep.className = 'button'; keep.textContent = 'Keep editing';
      keep.addEventListener('click', () => prompt.close());
      const confirm = document.createElement('button');
      confirm.type = 'button'; confirm.className = 'button button-danger button-danger--solid'; confirm.textContent = 'Discard';
      confirm.addEventListener('click', () => { discard = true; prompt.close(); });
      actions.append(keep, confirm);
      body.append(actions);
      prompt.addEventListener('close', () => resolve(discard), { once: true });
      keep.focus();
    });
  }

  function profileBar(folder, inline, profile, registry) {
    const profiles = document.createElement('div');
    profiles.className = 'settings-profiles';
    const selector = choiceControl(registry.profiles, profile,
      // The reserved profile is literally named `default`; suffixing it again would read "default (default)".
      registry.profiles.map((name) => name === registry.active && name !== 'default' ? `${name} (default)` : name));
    selector.className = 'settings-control';
    selector.id = `settings-profile-${++state.fieldId}`;
    const label = document.createElement('label');
    label.htmlFor = selector.id;
    label.textContent = 'Profile';
    profiles.append(label, selector);
    selector.addEventListener('change', async () => {
      if (!await confirmDiscard()) {
        selector.value = profile;
        return;
      }
      loadCounterSettings(folder, { inline, profile: selector.value });
    });
    const { wrapper: management, menu } = createOverflow('Profile actions');
    for (const [label, action] of [['Create empty', 'create'], ['Copy profile', 'copy'], ['Rename', 'rename'], ['Set as default', 'activate'], ['Delete', 'delete']]) {
      const button = document.createElement('button');
      button.type = 'button';
      button.setAttribute('role', 'menuitem');
      if (action === 'delete') button.className = 'card-menu__danger';
      button.textContent = label;
      button.disabled = (profile === 'default' && ['rename', 'delete'].includes(action)) || (action === 'activate' && profile === registry.active);
      button.addEventListener('click', async () => {
        menu.hidePopover();
        // Every profile action reloads the editor.
        if (!await confirmDiscard()) return;
        const name = action === 'activate' ? profile : await profilePrompt(action, profile);
        if (name === null) return;
        setBusy(button, true);
        try {
          const body = { action, name: profile };
          let selected = profile;
          if (action === 'create' || action === 'copy') {
            body.action = 'create';
            body.name = name;
            if (action === 'copy') body.copyFrom = profile;
            selected = body.name;
          } else if (action === 'rename') {
            body.target = name;
            selected = body.target;
          } else if (action === 'delete') selected = 'default';
          await requestJson('/api/counters/profiles', { method: 'POST', body: JSON.stringify(body) });
          await loadCounterSettings(folder, { inline, profile: selected });
        } catch (error) {
          notify(`Could not manage profile: ${errorText(error)}`, 'error');
        } finally { setBusy(button, false); }
      });
      menu.appendChild(button);
    }
    profiles.appendChild(management);
    return profiles;
  }

  // `target` is an overlay ({ kind: 'overlay', id: folder, name }) or a tool
  // ({ kind: 'tool', id, name }). Tools share the schema and value shapes but
  // have no profiles (docs/tools/protocol.md).
  function renderCounterSettings(
    container,
    target,
    schema,
    values,
    { inline = false, dialogRequestId = null, profile, registry, secrets } = {},
  ) {
    const folder = target.id;
    const isTool = target.kind === 'tool';
    const noun = isTool ? 'tool' : 'overlay';
    const title = target.name || folder;
    container.replaceChildren();
    state.navigationObserver?.disconnect();
    state.dirty = false;
    if (inline) {
      container.classList.add('inline-counter-settings');
      // The page heading names the overlay or tool; a back link replaces
      // the second heading and introduction this page used to carry.
      const heading = document.querySelector('#page-title');
      if (heading) heading.textContent = `${title} settings`;
      document.title = `${title} settings · vosu`;
      const back = document.createElement('a');
      back.className = 'settings-back';
      back.href = isTool ? '/tools' : '/';
      back.textContent = isTool ? '← Tools' : '← Overlays';
      container.appendChild(back);
    }

    if (!isTool) container.appendChild(profileBar(folder, inline, profile, registry));

    const form = document.createElement('form');
    form.className = 'dialog-form settings-editor';
    form.addEventListener('input', () => { state.dirty = true; });
    form.addEventListener('change', () => { state.dirty = true; });
    const list = document.createElement('div');
    list.className = 'counter-settings-list';
    const search = makeInput('search', '');
    search.placeholder = 'Search settings';
    search.setAttribute('aria-label', 'Search settings');
    search.className = 'settings-control';
    search.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') event.preventDefault();
    });
    // Searching is not an edit.
    search.addEventListener('input', (event) => event.stopPropagation());
    const navigation = document.createElement('nav');
    navigation.className = 'settings-navigation';
    navigation.setAttribute('aria-label', 'Settings sections');
    const groups = [];
    let group = { heading: null, link: null, fields: [] };
    groups.push(group);
    schema.forEach((setting) => {
      const configured = Object.prototype.hasOwnProperty.call(values, setting.uniqueID)
        ? values[setting.uniqueID]
        : undefined;
      const field = renderSettingField(setting, configured, secrets?.[setting.uniqueID]);
      list.appendChild(field);
      if (String(setting.type).toLowerCase() === 'header') {
        field.id = `settings-section-${groups.length}-${state.fieldId}`;
        const link = document.createElement('a');
        link.href = `#${field.id}`;
        link.textContent = setting.title || setting.uniqueID;
        link.title = link.textContent;
        link.addEventListener('click', (event) => {
          event.preventDefault();
          field.scrollIntoView({ behavior: 'smooth', block: 'start' });
          field.tabIndex = -1;
          field.focus({ preventScroll: true });
        });
        navigation.appendChild(link);
        group = { heading: field, link, fields: [] };
        groups.push(group);
      } else {
        group.fields.push({ field, text: `${setting.title} ${setting.description} ${setting.uniqueID}`.toLocaleLowerCase() });
      }
    });
    navigation.hidden = navigation.childElementCount === 0;
    const settingCount = schema.filter((setting) => String(setting.type).toLowerCase() !== 'header').length;
    search.addEventListener('input', () => {
      const words = search.value.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
      for (const section of groups) {
        let visible = words.length === 0;
        for (const entry of section.fields) {
          entry.field.hidden = !words.every((word) => entry.text.includes(word));
          visible ||= !entry.field.hidden;
        }
        if (section.heading) section.heading.hidden = !visible;
        if (section.link) section.link.hidden = !visible;
      }
    });
    if (!schema.length) {
      const empty = document.createElement('div');
      empty.className = 'empty-state';
      const heading = document.createElement('h2');
      heading.textContent = 'No settings';
      const text = document.createElement('p');
      text.textContent = `This ${noun} has no editable settings.`;
      empty.append(heading, text);
      list.appendChild(empty);
    }

    const actions = document.createElement('div');
    actions.className = 'dialog-actions';
    if (!inline) {
      const cancel = document.createElement('button');
      cancel.type = 'button';
      cancel.className = 'button';
      cancel.textContent = 'Cancel';
      cancel.addEventListener('click', requestDialogClose);
      actions.appendChild(cancel);
    }
    const save = document.createElement('button');
    save.type = 'submit';
    save.className = 'button button-primary';
    save.textContent = 'Save';
    save.disabled = settingCount === 0;
    actions.appendChild(save);
    const layout = document.createElement('div');
    layout.className = 'settings-layout';
    layout.append(navigation, list);
    if (settingCount > SEARCH_THRESHOLD) form.append(search);
    form.append(layout, actions);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (!isCurrentDialogRequest(dialogRequestId)) return;
      setBusy(save, true, 'Saving…');
      try {
        // Collect inside the try: invalid command rows throw a user-facing error.
        const payload = collectCounterSettings(form, schema);
        const url = isTool
          ? `/api/tools/${encodeURIComponent(folder)}/settings`
          : `/api/counters/settings/${encodeURIComponent(folder)}?profile=${encodeURIComponent(profile)}`;
        await requestJson(url, {
          method: 'POST',
          body: JSON.stringify(payload),
        });
        if (!isCurrentDialogRequest(dialogRequestId)) return;
        state.dirty = false;
        notify(`Saved ${title} settings.`, 'success');
        if (!inline) forceCloseDialog();
      } catch (error) {
        if (!isCurrentDialogRequest(dialogRequestId)) return;
        notify(`Could not save ${noun} settings: ${errorText(error)}`, 'error');
      } finally {
        setBusy(save, false);
      }
    });
    container.appendChild(form);
    const sections = groups.filter((section) => section.heading);
    if (sections.length) {
      const root = inline ? null : dialogBody;
      const updateActiveSection = () => {
        const visible = sections.filter((section) => !section.heading.hidden);
        // Inline pages scroll under the pinned site header, not the viewport top.
        const header = root ? null : document.querySelector('.site-header');
        const pinned = header && getComputedStyle(header).position === 'sticky' ? header.getBoundingClientRect().bottom : 0;
        const top = (root?.getBoundingClientRect().top ?? pinned) + 16;
        const current = visible.findLast((section) => section.heading.getBoundingClientRect().top <= top)
          ?? visible[0];
        for (const section of sections) {
          if (section === current) section.link.setAttribute('aria-current', 'location');
          else section.link.removeAttribute('aria-current');
        }
      };
      state.navigationObserver = new IntersectionObserver(updateActiveSection, {
        root, rootMargin: '0px 0px -75% 0px', threshold: [0, 1],
      });
      sections.forEach((section) => state.navigationObserver.observe(section.heading));
      search.addEventListener('input', updateActiveSection);
      updateActiveSection();
    }
    if (!inline) (form.querySelector('input, select, textarea, button') ?? save).focus();
  }

  function settingsPayload(payload) {
    const schema = Array.isArray(payload.settings) ? payload.settings : [];
    const object = (value) => value && typeof value === 'object' && !Array.isArray(value) ? value : null;
    // Tools report `secrets` instead of password values; overlays have none.
    return { schema, values: object(payload.values) ?? {}, secrets: object(payload.secrets) ?? undefined };
  }

  // Shown once, where the settings would be: inline pages keep their way
  // back, dialogs keep their close button.
  function showLoadError(container, requestId, error, inline) {
    if (!isCurrentDialogRequest(requestId)) return;
    container.replaceChildren();
    const message = document.createElement('div');
    message.className = 'empty-state';
    message.setAttribute('role', 'alert');
    const heading = document.createElement('h2');
    heading.textContent = "Couldn't load these settings";
    const text = document.createElement('p');
    text.textContent = errorText(error);
    message.append(heading, text);
    if (inline) {
      const back = document.createElement('a');
      back.className = 'button';
      back.href = window.location.search.includes('tool=') ? '/tools' : '/';
      back.textContent = window.location.search.includes('tool=') ? 'Back to Tools' : 'Back to Overlays';
      message.append(back);
    }
    container.appendChild(message);
  }

  function loadingTarget(inline, title, text) {
    const target = inline
      ? (results ? { container: results, requestId: null } : null)
      : openDialog(title);
    if (!target) return null;
    const loading = document.createElement('p');
    loading.className = 'tools-loading';
    loading.textContent = text;
    target.container.replaceChildren(loading);
    return target;
  }

  async function loadCounterSettings(folder, { inline = false, profile = null, name = '' } = {}) {
    const target = loadingTarget(inline, `${name || folder} settings`, 'Loading settings…');
    if (!target) return;
    const { container, requestId } = target;
    try {
      const registry = await requestJson('/api/counters/profiles');
      const requested = profile ?? (inline ? new URLSearchParams(location.search).get('profile') : null);
      const selected = requested ?? registry.active;
      const payload = await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}?profile=${encodeURIComponent(selected)}`);
      if (!isCurrentDialogRequest(requestId)) return;
      const { schema, values } = settingsPayload(payload);
      renderCounterSettings(container, { kind: 'overlay', id: folder, name: name || folder }, schema, values, { inline, dialogRequestId: requestId, profile: selected, registry });
    } catch (error) {
      showLoadError(container, requestId, error, inline);
    }
  }

  async function loadToolSettings(id, { inline = false, name = '' } = {}) {
    const target = loadingTarget(inline, `${name || id} settings`, 'Loading settings…');
    if (!target) return;
    const { container, requestId } = target;
    const base = `/api/tools/${encodeURIComponent(id)}`;
    try {
      // The display name is decoration; a failed lookup must not block editing.
      const [payload, view] = await Promise.all([
        requestJson(`${base}/settings`),
        requestJson(base).catch(() => ({ name })),
      ]);
      if (!isCurrentDialogRequest(requestId)) return;
      const { schema, values, secrets } = settingsPayload(payload);
      const title = typeof view.name === 'string' && view.name ? view.name : id;
      renderCounterSettings(container, { kind: 'tool', id, name: title }, schema, values, { inline, dialogRequestId: requestId, secrets });
      const startup = toolAutostartControl(view, { requestJson, notify, errorText });
      if (startup) {
        startup.dataset.toolStartup = '';
        if (inline) container.prepend(startup);
        else dialogTitle.after(startup);
      }
    } catch (error) {
      showLoadError(container, requestId, error, inline);
    }
  }

  return { closeDialog: requestDialogClose, loadCounterSettings, loadToolSettings };
}
