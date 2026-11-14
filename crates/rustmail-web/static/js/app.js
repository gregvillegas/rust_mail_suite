// RustMail UI behaviour: Alpine components and htmx hooks.
//
// Alpine runs its CSP build, which has no eval(). Templates therefore use only
// named components (x-data="name") with plain properties, getters and methods;
// all logic lives here. Keep it that way: it is what lets the CSP omit
// 'unsafe-eval'.
(function () {
  'use strict';

  var CSRF_COOKIE = 'rm_csrf';
  var THEME_KEY = 'rm-theme';

  function readCookie(name) {
    var pairs = document.cookie ? document.cookie.split('; ') : [];
    for (var i = 0; i < pairs.length; i++) {
      var eq = pairs[i].indexOf('=');
      if (pairs[i].slice(0, eq) === name) return decodeURIComponent(pairs[i].slice(eq + 1));
    }
    return null;
  }

  // ── htmx ────────────────────────────────────────────────────────────────

  // Double-submit CSRF: echo the (script-readable) cookie in a header on every
  // state-changing request. Reading the cookie each time keeps it current after
  // a session refresh rotates it.
  document.addEventListener('htmx:configRequest', function (event) {
    var verb = String(event.detail.verb || '').toLowerCase();
    if (verb === 'get') return;
    var token = readCookie(CSRF_COOKIE);
    if (token) event.detail.headers['X-CSRF-Token'] = token;
  });

  // Toasts come from two places: the server (htmx swaps a fragment into
  // #rm-toasts) and this file (for failures that never reach the server). Both
  // use the same markup, so one delegated handler and one timer rule cover them.
  function scheduleAutoclose(toast) {
    var ms = Number(toast.getAttribute('data-rm-autoclose'));
    if (ms > 0) {
      window.setTimeout(function () {
        if (toast.parentNode) toast.parentNode.removeChild(toast);
      }, ms);
    }
  }

  document.addEventListener('click', function (event) {
    var button = event.target && event.target.closest ? event.target.closest('[data-rm-dismiss]') : null;
    var toast = button ? button.closest('.alert') : null;
    if (toast && toast.parentNode) toast.parentNode.removeChild(toast);
  });

  function toast(message) {
    var container = document.getElementById('rm-toasts');
    if (!container || !message) return;
    var box = document.createElement('div');
    box.className = 'alert alert-danger alert-dismissible shadow-sm mb-2';
    box.setAttribute('role', 'alert');
    box.setAttribute('data-rm-autoclose', '8000');
    box.textContent = message; // textContent, never innerHTML
    var close = document.createElement('button');
    close.type = 'button';
    close.className = 'btn-close';
    close.setAttribute('data-rm-dismiss', '');
    close.setAttribute('aria-label', document.body.getAttribute('data-msg-close') || 'Close');
    box.appendChild(close);
    container.appendChild(box);
    scheduleAutoclose(box);
  }

  // Network failures never reach the server, so only the client can report them.
  ['htmx:sendError', 'htmx:timeout'].forEach(function (name) {
    document.addEventListener(name, function () {
      toast(document.body.getAttribute('data-msg-network'));
    });
  });

  // After a swap: time out any toast the server just added, and after a form
  // submission move focus to the form's error so screen readers announce it.
  document.addEventListener('htmx:afterSwap', function (event) {
    var detail = event.detail || {};
    var target = detail.target;

    if (target && target.id === 'rm-toasts') {
      var toasts = target.querySelectorAll('[data-rm-autoclose]');
      for (var i = 0; i < toasts.length; i++) scheduleAutoclose(toasts[i]);
      return;
    }

    // With hx-swap="outerHTML" htmx reports the *replaced* element, which is now
    // detached, so look the form up again by id in the live document.
    var trigger = detail.elt;
    if (trigger && trigger.tagName === 'FORM' && trigger.id) {
      var form = document.getElementById(trigger.id);
      var alert = form ? form.querySelector('[role="alert"]') : null;
      if (alert) {
        alert.setAttribute('tabindex', '-1');
        alert.focus();
      }
    }
  });

  // ── Alpine components ───────────────────────────────────────────────────

  document.addEventListener('alpine:init', function () {
    var Alpine = window.Alpine;

    // A button that opens a menu: used for the account, theme and language menus.
    Alpine.data('dropdown', function () {
      return {
        open: false,
        toggle: function () {
          this.open = !this.open;
        },
        close: function () {
          this.open = false;
        },
        get expanded() {
          return this.open ? 'true' : 'false';
        },
        get showClass() {
          return this.open ? 'show' : '';
        },
      };
    });

    // The mobile navigation collapse.
    Alpine.data('navCollapse', function () {
      return {
        open: false,
        toggle: function () {
          this.open = !this.open;
        },
        get expanded() {
          return this.open ? 'true' : 'false';
        },
        get showClass() {
          return this.open ? 'show' : '';
        },
      };
    });

    // Light / dark / auto. The initial value was applied by theme-init.js.
    Alpine.data('themeMenu', function () {
      function apply(preference) {
        var dark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
        var root = document.documentElement;
        root.setAttribute('data-bs-theme', preference === 'auto' ? (dark ? 'dark' : 'light') : preference);
        root.setAttribute('data-rm-theme-pref', preference);
      }
      return {
        open: false,
        preference: document.documentElement.getAttribute('data-rm-theme-pref') || 'auto',
        init: function () {
          var self = this;
          var media = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;
          if (media && media.addEventListener) {
            // Follow the operating system while the choice is "auto".
            media.addEventListener('change', function () {
              if (self.preference === 'auto') apply('auto');
            });
          }
        },
        toggle: function () {
          this.open = !this.open;
        },
        close: function () {
          this.open = false;
        },
        choose: function (preference) {
          try {
            if (preference === 'auto') window.localStorage.removeItem(THEME_KEY);
            else window.localStorage.setItem(THEME_KEY, preference);
          } catch (e) {
            /* storage blocked: the choice lasts for this page only */
          }
          this.preference = preference;
          apply(preference);
          this.open = false;
        },
        setAuto: function () {
          this.choose('auto');
        },
        setLight: function () {
          this.choose('light');
        },
        setDark: function () {
          this.choose('dark');
        },
        get expanded() {
          return this.open ? 'true' : 'false';
        },
        get showClass() {
          return this.open ? 'show' : '';
        },
        get autoActive() {
          return this.preference === 'auto' ? 'active' : '';
        },
        get lightActive() {
          return this.preference === 'light' ? 'active' : '';
        },
        get darkActive() {
          return this.preference === 'dark' ? 'active' : '';
        },
      };
    });

    // Show / hide a password. Labels come from data attributes so they stay translated.
    Alpine.data('passwordField', function () {
      return {
        visible: false,
        showLabel: '',
        hideLabel: '',
        init: function () {
          this.showLabel = this.$el.getAttribute('data-show-label') || 'Show password';
          this.hideLabel = this.$el.getAttribute('data-hide-label') || 'Hide password';
        },
        toggle: function () {
          this.visible = !this.visible;
        },
        get inputType() {
          return this.visible ? 'text' : 'password';
        },
        get toggleLabel() {
          return this.visible ? this.hideLabel : this.showLabel;
        },
        get pressed() {
          return this.visible ? 'true' : 'false';
        },
      };
    });

    // A confirmation dialog. Esc and the backdrop cancel; focus goes to Cancel
    // on open and back to the trigger on close.
    Alpine.data('confirmModal', function () {
      return {
        open: false,
        opener: null,
        show: function (event) {
          this.opener = event && event.currentTarget ? event.currentTarget : null;
          this.open = true;
          var self = this;
          this.$nextTick(function () {
            if (self.$refs.cancel) self.$refs.cancel.focus();
          });
        },
        hide: function () {
          this.open = false;
          if (this.opener && this.opener.focus) this.opener.focus();
        },
      };
    });
  });
})();
