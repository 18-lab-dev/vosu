let actionId = 0;

export function createActionDialog(title) {
  const dialog = document.createElement('dialog');
  dialog.className = 'dashboard-dialog action-dialog';
  const header = document.createElement('header');
  header.className = 'dialog-header';
  const heading = document.createElement('h2');
  heading.id = `action-title-${++actionId}`;
  heading.textContent = title;
  dialog.setAttribute('aria-labelledby', heading.id);
  header.append(heading);
  const body = document.createElement('div');
  body.className = 'dialog-body';
  dialog.append(header, body);
  dialog.addEventListener('close', () => dialog.remove(), { once: true });
  document.body.append(dialog);
  dialog.showModal();
  return { dialog, body };
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
  toggle.setAttribute('popovertarget', menu.id);
  wrapper.append(toggle, menu);
  return { wrapper, menu };
}

export function createSettingsEditor({
  results, requestJson, notify, setBusy, errorText, absoluteHttpUrl,
}) {
  const state = { dialogRequest: 0, navigationObserver: null };

  function choiceControl(values, value, labels = values) {
    const select = document.createElement('select');
    select.className = 'setting-value';
    values.forEach((item, index) => select.add(new Option(labels[index], item)));
    select.value = String(value);
    return select;
  }

  const dialog = document.querySelector('#dashboard-dialog');
  const dialogTitle = document.querySelector('#dialog-title');
  const dialogDescription = document.querySelector('#dialog-description');
  const dialogBody = document.querySelector('#dialog-body');
  dialog?.addEventListener('close', () => state.navigationObserver?.disconnect());

  function cloneValue(value) {
    if (typeof structuredClone === 'function') return structuredClone(value);
    return JSON.parse(JSON.stringify(value));
  }

  function openDialog(title, description = '') {
    if (!dialog || !dialogBody) return null;
    const requestId = ++state.dialogRequest;
    dialogTitle.textContent = title;
    dialogDescription.textContent = description;
    dialogDescription.hidden = !description;
    dialogBody.replaceChildren();
    if (!dialog.open) dialog.showModal();
    return { container: dialogBody, requestId };
  }

  function isCurrentDialogRequest(requestId) {
    return requestId === null || (state.dialogRequest === requestId && Boolean(dialog?.open));
  }

  function closeDialog() {
    state.dialogRequest += 1;
    if (dialog?.open) dialog.close();
  }

  function fieldLabel(text, control, description = '') {
    const label = document.createElement('label');
    label.className = 'field-label';
    const name = document.createElement('span');
    name.textContent = text;
    label.appendChild(name);
    if (description) {
      const help = document.createElement('span');
      help.className = 'setting-description';
      help.textContent = description;
      label.appendChild(help);
    }
    label.appendChild(control);
    return label;
  }

  function makeInput(type, value) {
    const input = document.createElement('input');
    input.type = type;
    input.className = 'setting-value';
    if (type === 'checkbox') input.checked = Boolean(value);
    else if (value !== undefined && value !== null) input.value = String(value);
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
      control.setAttribute('aria-label', String(option.title || option.name));
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
    remove.addEventListener('click', () => row.remove());
    row.append(fields, remove);
    list.appendChild(row);
  }

  function renderSettingField(setting, configuredValue) {
    const kind = String(setting.type ?? 'text').toLowerCase();
    const id = String(setting.uniqueID ?? '');
    if (kind === 'header') {
      const heading = document.createElement('h3');
      heading.className = 'counter-settings-heading';
      heading.textContent = String(setting.title || id);
      return heading;
    }

    const fieldset = document.createElement('fieldset');
    fieldset.className = 'counter-setting';
    fieldset.dataset.settingId = id;
    fieldset.dataset.settingType = kind;
    const legend = document.createElement('legend');
    legend.textContent = String(setting.title || id);
    fieldset.appendChild(legend);
    if (setting.description) {
      const description = document.createElement('p');
      description.className = 'setting-description';
      description.textContent = String(setting.description);
      fieldset.appendChild(description);
    }

    const value = configuredValue === undefined ? setting.value : configuredValue;
    if (kind === 'commands') {
      const list = document.createElement('div');
      list.className = 'command-list';
      mergeCommandValues(setting, value).forEach((command) => appendCommandRow(list, setting, command));
      const add = document.createElement('button');
      add.type = 'button';
      add.className = 'button';
      add.textContent = 'Add command';
      add.addEventListener('click', () => {
        const seed = {};
        (Array.isArray(setting.options) ? setting.options : []).forEach((option) => {
          if (option?.name) seed[option.name] = cloneValue(option.value ?? '');
        });
        appendCommandRow(list, setting, seed);
        list.lastElementChild?.querySelector('input, select')?.focus();
      });
      fieldset.append(list, add);
      return fieldset;
    }

    let control;
    if (kind === 'checkbox') {
      control = makeInput('checkbox', value);
    } else if (kind === 'number') {
      control = makeInput('number', value);
      control.step = 'any';
    } else if (kind === 'color') {
      control = makeInput('color', value || '#f45cad');
    } else if (kind === 'password') {
      control = makeInput('password', value);
      control.autocomplete = 'off';
    } else if (kind === 'textarea') {
      control = document.createElement('textarea');
      control.className = 'setting-value';
      control.value = value === undefined || value === null ? '' : String(value);
    } else if (kind === 'options') {
      const options = Array.isArray(setting.options) ? setting.options.filter((item) => item !== '').map(String) : [];
      control = choiceControl(options, value ?? options[0] ?? '');
    } else if (kind === 'button') {
      const href = absoluteHttpUrl(value);
      const link = document.createElement(href ? 'a' : 'span');
      link.className = 'button button-cyan';
      link.textContent = String(setting.title || 'Open link');
      if (href) {
        link.href = href;
        link.target = '_blank';
        link.rel = 'noopener noreferrer';
      } else {
        link.setAttribute('aria-disabled', 'true');
      }
      fieldset._settingValue = cloneValue(value);
      fieldset.appendChild(link);
      return fieldset;
    } else {
      control = makeInput('text', value);
    }
    control.setAttribute('aria-label', String(setting.title || id));
    fieldset.appendChild(control);
    return fieldset;
  }

  function collectCommandRows(fieldset) {
    return Array.from(fieldset.querySelectorAll('.command-row')).map((row) => {
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
    form.querySelectorAll('.counter-setting[data-setting-id]').forEach((fieldset) => {
      const id = fieldset.dataset.settingId;
      const kind = fieldset.dataset.settingType;
      let value;
      if (kind === 'commands') {
        value = collectCommandRows(fieldset);
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
        value = cloneValue(fieldset._settingValue);
      } else {
        const control = fieldset.querySelector('.setting-value');
        if (!control) return;
        if (kind === 'checkbox') value = control.checked;
        else if (kind === 'number' && control.value !== '') value = Number(control.value);
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
      const { dialog: prompt, body } = createActionDialog(titles[action]);
      const form = document.createElement('form');
      form.className = 'dialog-form';
      const input = makeInput('text', action === 'rename' ? profile : '');
      input.required = true;
      input.maxLength = 48;
      input.pattern = '[A-Za-z0-9_-]+';
      if (deleting) {
        const text = document.createElement('p');
        text.textContent = `Delete profile “${profile}”? This cannot be undone.`;
        form.append(text);
      } else form.append(fieldLabel('Profile name', input, 'Letters, numbers, hyphens and underscores; up to 48 characters.'));
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

  function renderCounterSettings(
    container,
    folder,
    schema,
    values,
    { inline = false, dialogRequestId = null, profile, registry } = {},
  ) {
    container.replaceChildren();
    state.navigationObserver?.disconnect();
    if (inline) {
      container.classList.add('inline-counter-settings');
      const header = document.createElement('header');
      const title = document.createElement('h2');
      title.textContent = `Overlay settings: ${folder}`;
      const text = document.createElement('p');
      text.textContent = 'These values are stored by Vosu for this overlay.';
      header.append(title, text);
      container.appendChild(header);
    }

    const profiles = document.createElement('section');
    profiles.className = 'settings-profiles';
    const selector = choiceControl(registry.profiles, profile,
      // The reserved profile is literally named `default`; suffixing it again would read "default (default)".
      registry.profiles.map((name) => name === registry.active && name !== 'default' ? `${name} (default)` : name));
    selector.className = 'settings-control';
    selector.setAttribute('aria-label', 'Settings profile');
    selector.title = 'Settings profile; "(default)" marks the profile unpinned overlays use';
    profiles.appendChild(selector);
    selector.addEventListener('change', () => loadCounterSettings(folder, { inline, profile: selector.value }));
    const { wrapper: management, menu } = createOverflow('Profile actions', '☰');
    for (const [label, action] of [['Create empty', 'create'], ['Copy profile', 'copy'], ['Rename', 'rename'], ['Delete', 'delete'], ['Set as default', 'activate']]) {
      const button = document.createElement('button');
      button.type = 'button';
      if (action === 'delete') button.className = 'card-menu__danger';
      button.textContent = label;
      button.disabled = (profile === 'default' && ['rename', 'delete'].includes(action)) || (action === 'activate' && profile === registry.active);
      button.addEventListener('click', async () => {
        menu.hidePopover();
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
          notify(`Could not manage profile: ${errorText(error)}`, 'error', 5000);
        } finally { setBusy(button, false); }
      });
      menu.appendChild(button);
    }
    profiles.appendChild(management);
    container.appendChild(profiles);

    const form = document.createElement('form');
    form.className = 'dialog-form';
    const list = document.createElement('div');
    list.className = 'counter-settings-list';
    const search = makeInput('search', '');
    search.placeholder = 'Search title, description or ID';
    search.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') event.preventDefault();
    });
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
      const field = renderSettingField(setting, configured);
      list.appendChild(field);
      if (String(setting.type).toLowerCase() === 'header') {
        field.id = `settings-section-${groups.length}`;
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
    search.addEventListener('input', () => {
      const term = search.value.trim().toLocaleLowerCase();
      for (const section of groups) {
        let visible = !term;
        for (const entry of section.fields) {
          entry.field.hidden = !entry.text.includes(term);
          visible ||= !entry.field.hidden;
        }
        if (section.heading) section.heading.hidden = !visible;
        if (section.link) section.link.hidden = !visible;
      }
    });
    if (!schema.length) {
      const empty = document.createElement('div');
      empty.className = 'empty-state';
      const title = document.createElement('h2');
      title.textContent = 'No settings exposed';
      const text = document.createElement('p');
      text.textContent = 'This overlay does not define user-editable settings.';
      empty.append(title, text);
      list.appendChild(empty);
    }

    const actions = document.createElement('div');
    actions.className = 'dialog-actions';
    if (!inline) {
      const cancel = document.createElement('button');
      cancel.type = 'button';
      cancel.className = 'button';
      cancel.textContent = 'Cancel';
      cancel.addEventListener('click', closeDialog);
      actions.appendChild(cancel);
    }
    const save = document.createElement('button');
    save.type = 'submit';
    save.className = 'button button-primary';
    save.textContent = 'Save overlay settings';
    save.disabled = !schema.some((setting) => setting.type !== 'header');
    actions.appendChild(save);
    const layout = document.createElement('div');
    layout.className = 'settings-layout';
    layout.append(navigation, list);
    form.append(fieldLabel('Search settings', search), layout, actions);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (!isCurrentDialogRequest(dialogRequestId)) return;
      setBusy(save, true, 'Saving…');
      try {
        const payload = collectCounterSettings(form, schema);
        await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}?profile=${encodeURIComponent(profile)}`, {
          method: 'POST',
          body: JSON.stringify(payload),
        });
        if (!isCurrentDialogRequest(dialogRequestId)) return;
        notify(`Saved settings for ${folder}.`, 'success');
        if (!inline) closeDialog();
      } catch (error) {
        if (!isCurrentDialogRequest(dialogRequestId)) return;
        notify(`Could not save overlay settings: ${errorText(error)}`, 'error', 5000);
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
        const top = (root?.getBoundingClientRect().top ?? 0) + 16;
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
  }

  async function loadCounterSettings(folder, { inline = false, profile = null } = {}) {
    const target = inline
      ? (results ? { container: results, requestId: null } : null)
      : openDialog(`Settings: ${folder}`, 'Edit values provided by this overlay.');
    if (!target) return;
    const { container, requestId } = target;
    const loading = document.createElement('p');
    loading.textContent = 'Loading overlay settings…';
    container.replaceChildren(loading);
    try {
      const registry = await requestJson('/api/counters/profiles');
      const requested = profile ?? (inline ? new URLSearchParams(location.search).get('profile') : null);
      const selected = requested ?? registry.active;
      const payload = await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}?profile=${encodeURIComponent(selected)}`);
      if (!isCurrentDialogRequest(requestId)) return;
      const schema = Array.isArray(payload.settings) ? payload.settings : [];
      const values = payload.values && typeof payload.values === 'object' && !Array.isArray(payload.values)
        ? payload.values
        : {};
      renderCounterSettings(container, folder, schema, values, { inline, dialogRequestId: requestId, profile: selected, registry });
    } catch (error) {
      if (!isCurrentDialogRequest(requestId)) return;
      container.replaceChildren();
      const message = document.createElement('p');
      message.textContent = `Could not load settings: ${errorText(error)}`;
      container.appendChild(message);
      notify(message.textContent, 'error', 5000);
    }
  }

  return { closeDialog, loadCounterSettings };
}
