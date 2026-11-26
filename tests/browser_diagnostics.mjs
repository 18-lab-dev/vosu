// Collect Chromium failures separately from scenario assertions. Some scenarios
// deliberately receive a rejected request, abort a route, or load a sandboxed
// document; retain those diagnostics instead of treating them as silent skips.
export function watchBrowser(context, diagnostics) {
  const path = (raw) => {
    const url = new URL(raw);
    return url.origin + url.pathname; // No query or fragment secrets in receipts.
  };
  context.on('page', (page) => {
    page.on('pageerror', (error) => diagnostics.push({kind: 'page', message: error.message}));
    page.on('console', (message) => {
      if (message.type() === 'error') {
        diagnostics.push({kind: 'console', message: message.text()});
      }
    });
  });
  context.on('requestfailed', (request) => {
    diagnostics.push({kind: 'network', path: path(request.url()), message: request.failure()?.errorText});
  });
}

export function browserReceipt(name, count, diagnostics) {
  if (!Number.isInteger(count) || count <= 0) throw new Error(`${name}: no named browser scenarios ran`);
  const pageErrors = diagnostics.filter((entry) => entry.kind === 'page');
  if (pageErrors.length) throw new Error(`${name}: ${JSON.stringify(pageErrors)}`);
  console.log(`M7_BROWSER ${name} ${count} ${JSON.stringify(diagnostics)}`);
}
