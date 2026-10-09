export function createSettingsEditor({
  results, requestJson, notify, setBusy, errorText, absoluteHttpUrl,
}) {
  const state = { dialogRequest: 0 };

  const dialog = document.querySelector('#dashboard-dialog');
  const dialogTitle = document.querySelector('#dialog-title');
  const dialogDescription = document.querySelector('#dialog-description');
  const dialogBody = document.querySelector('#dialog-body');

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
      control = document.createElement('select');
      control.className = 'setting-value';
      const values = Array.isArray(option.values) ? option.values.filter((item) => item !== '') : [];
      values.forEach((item) => {
        const choice = document.createElement('option');
        choice.value = String(item);
        choice.textContent = String(item);
        control.appendChild(choice);
      });
      control.value = String(value ?? option.value ?? values[0] ?? '');
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
      control = document.createElement('select');
      control.className = 'setting-value';
      const options = Array.isArray(setting.options) ? setting.options.filter((item) => item !== '') : [];
      options.forEach((item) => {
        const option = document.createElement('option');
        option.value = String(item);
        option.textContent = String(item);
        control.appendChild(option);
      });
      control.value = String(value ?? options[0] ?? '');
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

  function renderCounterSettings(
    container,
    folder,
    schema,
    values,
    { inline = false, dialogRequestId = null } = {},
  ) {
    container.replaceChildren();
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

    const form = document.createElement('form');
    form.className = 'dialog-form';
    const list = document.createElement('div');
    list.className = 'counter-settings-list';
    schema.forEach((setting) => {
      const configured = Object.prototype.hasOwnProperty.call(values, setting.uniqueID)
        ? values[setting.uniqueID]
        : undefined;
      list.appendChild(renderSettingField(setting, configured));
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
    form.append(list, actions);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (!isCurrentDialogRequest(dialogRequestId)) return;
      setBusy(save, true, 'Saving…');
      try {
        const payload = collectCounterSettings(form, schema);
        await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}`, {
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
  }

  async function loadCounterSettings(folder, { inline = false } = {}) {
    const target = inline
      ? (results ? { container: results, requestId: null } : null)
      : openDialog(`Settings: ${folder}`, 'Edit values provided by this overlay.');
    if (!target) return;
    const { container, requestId } = target;
    const loading = document.createElement('p');
    loading.textContent = 'Loading overlay settings…';
    container.replaceChildren(loading);
    try {
      const payload = await requestJson(`/api/counters/settings/${encodeURIComponent(folder)}`);
      if (!isCurrentDialogRequest(requestId)) return;
      const schema = Array.isArray(payload.settings) ? payload.settings : [];
      const values = payload.values && typeof payload.values === 'object' && !Array.isArray(payload.values)
        ? payload.values
        : {};
      renderCounterSettings(container, folder, schema, values, { inline, dialogRequestId: requestId });
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
