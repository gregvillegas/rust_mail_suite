// Runs synchronously in <head>, before first paint, so the page never flashes
// the wrong theme. Kept as an external file so the CSP can forbid inline scripts.
(function () {
  var stored = null;
  try {
    stored = window.localStorage.getItem('rm-theme');
  } catch (e) {
    /* storage blocked: fall back to the system preference */
  }
  var preference = stored === 'light' || stored === 'dark' ? stored : 'auto';
  var dark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
  var root = document.documentElement;
  root.setAttribute('data-bs-theme', preference === 'auto' ? (dark ? 'dark' : 'light') : preference);
  root.setAttribute('data-rm-theme-pref', preference);
})();
