// The status strip: core's host diagnostics (`GET /api/dashboard/status`,
// this origin) joined by PID with API v1's session summaries
// (`GET /api/v1/sessions`, the overlay origin). Both are one-shot reads: the
// page never subscribes, so it creates no sampling demand, and polling stops
// while the page is hidden or closed. The summaries are an ordinary public
// cross-origin read without credentials, cookies or the CSRF header.
const POLL_MS = 2000;

function text(value) {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

function interval(value) {
  return Number.isFinite(value) && value > 0 ? value : null;
}

// A summary is used only when every field the strip shows has its declared
// type; anything else is treated as absent, never as zero or an empty game.
export function sessionSummaries(payload) {
  const sessions = Array.isArray(payload?.sessions) ? payload.sessions : [];
  return sessions.filter((session) => session && typeof session === 'object'
    && Number.isInteger(session.pid) && session.pid > 0 && text(session.game) !== null);
}

// One line per diagnostic instance. A session's labels come only from the
// summary with that instance's own PID, read in the same refresh, so two
// sessions never borrow each other's labels or intervals when focus moves
// between the two reads. Poll intervals are osu! settings and are shown only
// for sessions that identify as osu.
export function describeInstances(instances, sessions) {
  const byPid = new Map(sessions.map((session) => [session.pid, session]));
  return instances.map((instance) => {
    const pid = Number.isInteger(instance?.pid) ? instance.pid : null;
    const session = pid === null ? undefined : byPid.get(pid);
    const version = text(session?.version) ?? text(instance?.version) ?? 'version unknown';
    const label = session
      ? [text(session.name) ?? text(session.game), text(session.variant)].filter(Boolean).join(' ')
      : text(instance?.client) ?? 'Unidentified client';
    const osu = session?.game === 'osu';
    return {
      pid,
      state: text(instance?.state) ?? 'unknown',
      focused: session?.focused === true,
      client: `${label} ${version}${pid === null ? '' : ` · PID ${pid}`}`,
      poll: osu ? interval(instance?.pollIntervalMs) : null,
      precise: osu ? interval(instance?.precisePollIntervalMs) : null,
    };
  });
}

async function readSessions(overlayOrigin) {
  if (!overlayOrigin) return [];
  const response = await fetch(new URL('/api/v1/sessions', overlayOrigin), {
    credentials: 'omit',
    cache: 'no-store',
    referrerPolicy: 'no-referrer',
    headers: { Accept: 'application/json' },
  });
  if (!response.ok) throw new Error(`sessions returned ${response.status}`);
  return sessionSummaries(await response.json());
}

export function mountDashboardStatus({ requestJson, overlayOrigin, installedCards, onStatus }) {
  const header = document.querySelector('#attachment-status');
  if (!header) return;
  const clientCell = document.querySelector('[data-status-cell="client"]');
  const pollCell = document.querySelector('[data-status-cell="poll"]');
  let timer = 0;
  let inFlight = false;
  // Rewriting identical text would make some screen readers re-announce the
  // live region every two seconds.
  function setText(element, value, title = value) {
    if (!element) return;
    if (element.textContent !== value) element.textContent = value;
    if (element.title !== title) element.title = title;
  }
  function setCell(cell, value) {
    if (!cell) return;
    cell.hidden = !value;
    setText(cell.querySelector('.status-value'), value);
  }
  function showAttachment(state, value, detail = value) {
    header.dataset.state = state;
    setText(header, value, detail);
  }
  function showUnavailable() {
    showAttachment('error', 'Attachment status unavailable');
    setCell(clientCell, '');
    setCell(pollCell, '');
  }
  function render(status, sessions) {
    const instances = Array.isArray(status.instances) ? status.instances : [];
    if (status.available === false) {
      showUnavailable();
      return;
    }
    if (instances.length === 0) {
      // A detached session leaves nothing behind from the previous poll.
      showAttachment('idle', 'Not detected · waiting for a game');
      setCell(clientCell, '');
      setCell(pollCell, '');
      return;
    }
    const rows = describeInstances(instances, sessions);
    const reading = rows.filter((row) => row.state === 'reading').length;
    const detail = rows.map((row) => `${row.client} · ${row.state}${row.poll === null ? '' : ` · polling ${row.poll} ms${row.precise === null ? '' : ` / precise ${row.precise} ms`}`}`).join(' | ');
    showAttachment(
      reading > 0 ? 'reading' : 'starting',
      rows.length > 1 ? `${rows.length} instances · ${reading} reading` : rows[0].state,
      detail,
    );
    setCell(clientCell, rows.map((row) => (rows.length > 1 && row.focused ? `${row.client} (focused)` : row.client)).join(' | '));
    const lead = rows.find((row) => row.focused && row.poll !== null) ?? rows.find((row) => row.poll !== null);
    setCell(pollCell, lead ? `${lead.poll} ms${lead.precise === null ? '' : ` / precise ${lead.precise} ms`}` : '');
  }
  function renderConnections(connections) {
    for (const card of installedCards()) {
      const chip = card.querySelector('.connected-chip');
      if (!chip) continue;
      // The server leaves out this dashboard's own preview frames.
      const count = connections?.[card.dataset.overlayId];
      const hidden = !Number.isInteger(count) || count < 1;
      const value = hidden ? '' : `${count} connected`;
      if (chip.hidden !== hidden) chip.hidden = hidden;
      if (chip.textContent !== value) chip.textContent = value;
      chip.title = 'Identified WebSocket connections, not unique users';
    }
  }
  async function refresh() {
    timer = 0;
    inFlight = true;
    const [status, sessions] = await Promise.allSettled([requestJson('/api/dashboard/status'), readSessions(overlayOrigin)]);
    if (status.status === 'fulfilled') {
      // Without summaries the strip keeps core's own client text and shows
      // no game-specific interval rather than guessing a game.
      render(status.value, sessions.status === 'fulfilled' ? sessions.value : []);
      renderConnections(status.value.connections);
      onStatus?.(status.value);
    } else {
      showUnavailable();
      document.querySelectorAll('.connected-chip').forEach((chip) => { chip.hidden = true; });
    }
    inFlight = false;
    if (!document.hidden && !timer) timer = window.setTimeout(refresh, POLL_MS);
  }
  document.addEventListener('visibilitychange', () => {
    window.clearTimeout(timer);
    timer = 0;
    if (!document.hidden && !inFlight) refresh();
  });
  refresh();
}
