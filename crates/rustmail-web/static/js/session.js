// Silent session renewal. The short-lived access cookie has lapsed but the
// refresh cookie (scoped to /auth, so page requests never carry it) may still
// be valid. Try to refresh, then go back where the user was; otherwise sign in.
(function () {
  var NEXT_KEY = 'rm-renew-at';

  function safeNext(value) {
    // Same-origin paths only: "/x" yes; "//evil", "/\evil", "http://evil" no.
    if (value && value.charAt(0) === '/' && value.charAt(1) !== '/' && value.charAt(1) !== '\\') {
      return value;
    }
    return '/';
  }

  function toLogin(next) {
    window.location.replace('/login?next=' + encodeURIComponent(next));
  }

  var params = new URLSearchParams(window.location.search);
  var next = safeNext(params.get('next'));

  // Loop breaker: if we renewed moments ago and still ended up here, give up.
  var last = 0;
  try {
    last = Number(window.sessionStorage.getItem(NEXT_KEY)) || 0;
    window.sessionStorage.setItem(NEXT_KEY, String(Date.now()));
  } catch (e) {
    /* storage blocked: no loop protection, but the server still bounds retries */
  }
  if (Date.now() - last < 10000) {
    toLogin(next);
    return;
  }

  function failed() {
    var notice = document.getElementById('renew-failed');
    var spinner = document.getElementById('renew-working');
    if (spinner) spinner.hidden = true;
    if (notice) notice.hidden = false;
    window.setTimeout(function () {
      toLogin(next);
    }, 1500);
  }

  window
    .fetch('/auth/csrf', { credentials: 'same-origin' })
    .then(function (r) {
      return r.ok ? r.json() : Promise.reject(new Error('csrf'));
    })
    .then(function (body) {
      return window.fetch('/auth/refresh', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'X-CSRF-Token': body.csrf_token },
      });
    })
    .then(function (r) {
      if (r.ok) window.location.replace(next);
      else failed();
    })
    .catch(failed);
})();
