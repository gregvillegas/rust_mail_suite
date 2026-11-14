// Fallback for CDN mode. Loads the vendored copy of any library that did not
// arrive from the CDN (network failure, blocked host, or failed integrity check).
// Detection is by result, not by error events, so it covers every failure mode.
(function () {
  function addScript(src) {
    var s = document.createElement('script');
    s.src = src;
    document.head.appendChild(s);
  }

  function addStylesheet(href) {
    var l = document.createElement('link');
    l.rel = 'stylesheet';
    l.href = href;
    document.head.appendChild(l);
  }

  // Bootstrap's .visually-hidden is position:absolute only if the CSS applied.
  function bootstrapLoaded() {
    var probe = document.createElement('div');
    probe.className = 'visually-hidden';
    document.body.appendChild(probe);
    var ok = window.getComputedStyle(probe).position === 'absolute';
    document.body.removeChild(probe);
    return ok;
  }

  function fallbackFor(selector) {
    var el = document.querySelector(selector);
    return el ? el.getAttribute('data-fallback') : null;
  }

  function run() {
    var css = fallbackFor('link[data-lib="bootstrap"]');
    if (css && !bootstrapLoaded()) addStylesheet(css);

    var htmx = fallbackFor('script[data-lib="htmx"]');
    if (htmx && typeof window.htmx === 'undefined') addScript(htmx);

    var alpine = fallbackFor('script[data-lib="alpine"]');
    if (alpine && typeof window.Alpine === 'undefined') addScript(alpine);
  }

  if (document.readyState === 'complete') run();
  else window.addEventListener('load', run);
})();
