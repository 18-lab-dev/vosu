// A major update installs only after the notice's consent checkbox is ticked;
// the server refuses a major update request that lacks `consent: true`.
export function createAppUpdater({ pending, requestJson, notify, errorText, setBusy }) {
  return async function runAppUpdate(button) {
    if (pending.has('app-update')) return;
    const notice = document.querySelector('#update-notice');
    const isMajor = notice?.dataset.major === 'true';
    const consented = notice?.querySelector('[data-role="major-update-consent"]')?.checked ?? false;
    if (isMajor && !consented) {
      notify('Confirm you have read the release notes before installing a major update.', 'error', 5000);
      return;
    }
    pending.add('app-update');
    setBusy(button, true, 'Updating…');
    try {
      const payload = await requestJson('/api/runUpdates', {
        method: 'POST',
        body: JSON.stringify({ consent: isMajor }),
      });
      if (payload.status === 'majorAvailable') {
        notify('vosu requires confirmation before installing a major update.', 'error', 5000);
        setBusy(button, false);
        return;
      }
      notify('vosu finished updating.', 'success');
      notice?.remove();
      document.querySelector('.status-update')?.remove();
    } catch (error) {
      notify(`vosu could not update: ${errorText(error)}`, 'error', 5000);
      setBusy(button, false);
    } finally {
      pending.delete('app-update');
    }
  };
}
