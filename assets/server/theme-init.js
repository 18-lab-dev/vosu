// Classic, parser-blocking script loaded in <head> before the stylesheets, so
// the first paint already uses the stored colour mode. homepage.js (a deferred
// module) owns the toggle; this only applies the saved or system choice.
(function () {
  'use strict';
  var theme = null;
  try {
    theme = window.localStorage.getItem('vosu.theme.v1');
  } catch (_error) {
    // Blocked storage falls back to the system preference.
  }
  if (theme !== 'dark' && theme !== 'light') {
    theme = window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  document.documentElement.setAttribute('data-theme', theme);
}());
