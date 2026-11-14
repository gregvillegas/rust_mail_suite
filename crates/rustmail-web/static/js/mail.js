// Webmail behaviour: new-mail push, keyboard shortcuts and the compose form.
//
// Like app.js this works with Alpine's CSP build: components are named and
// registered here, templates reference only their methods. No eval, no inline code.
(function () {
  'use strict';

  // ── new mail over a WebSocket ───────────────────────────────────────────────
  // The mail page also refreshes itself every minute, so the socket only makes
  // that quicker. It is opened while a message list is on screen and closed when
  // the user navigates elsewhere (hx-boost swaps the page without a reload).
  var socket = null;
  var timer = null;
  var attempts = 0;

  function onMailPage() {
    return !!document.getElementById('mail-list');
  }
  function connect() {
    if (socket || !('WebSocket' in window) || !onMailPage()) return;
    var scheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    try {
      socket = new WebSocket(scheme + '//' + window.location.host + '/mail/ws');
    } catch (e) {
      socket = null;
      return;
    }
    socket.onopen = function () { attempts = 0; };
    socket.onmessage = function (event) {
      if (event.data === 'new' && window.htmx && onMailPage()) {
        window.htmx.trigger(document.body, 'mail-refresh');
      }
    };
    socket.onclose = function () {
      socket = null;
      if (onMailPage()) {
        attempts = Math.min(attempts + 1, 6);
        timer = window.setTimeout(connect, 1000 * Math.pow(2, attempts));
      }
    };
  }
  function disconnect() {
    window.clearTimeout(timer);
    if (socket) {
      var s = socket;
      socket = null;
      s.onclose = null;
      s.close();
    }
  }
  function syncSocket() {
    if (onMailPage()) connect(); else disconnect();
  }
  document.addEventListener('DOMContentLoaded', syncSocket);
  document.addEventListener('htmx:load', syncSocket);

  // ── keep the open message highlighted in the list ───────────────────────────
  function markActive() {
    var open = document.querySelector('#mail-preview [data-open-uid]');
    var uid = open ? open.getAttribute('data-open-uid') : null;
    var rows = document.querySelectorAll('#mail-list .rm-row');
    for (var i = 0; i < rows.length; i++) {
      rows[i].classList.toggle('active', uid !== null && rows[i].getAttribute('data-uid') === uid);
    }
  }
  document.addEventListener('htmx:afterSettle', markActive);

  // ── Alpine components ──────────────────────────────────────────────────────
  function typing(target) {
    if (!target || !target.tagName) return false;
    var tag = target.tagName;
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target.isContentEditable === true;
  }
  function click(selector) {
    var el = document.querySelector(selector);
    if (el) el.click();
    return !!el;
  }
  function step(direction) {
    var rows = document.querySelectorAll('#mail-list .rm-row');
    if (!rows.length) return;
    // The open message is the one in the preview pane; the highlight class may lag
    // a moment behind a swap, the preview does not.
    var open = document.querySelector('#mail-preview [data-open-uid]');
    var uid = open ? open.getAttribute('data-open-uid') : null;
    var current = -1;
    for (var i = 0; i < rows.length; i++) {
      if ((uid !== null && rows[i].getAttribute('data-uid') === uid) || (uid === null && rows[i].classList.contains('active'))) {
        current = i;
        break;
      }
    }
    var next = Math.max(0, Math.min(rows.length - 1, current + direction));
    if (current === -1) next = direction > 0 ? 0 : rows.length - 1;
    rows[next].click();
    if (rows[next].scrollIntoView) rows[next].scrollIntoView({ block: 'nearest' });
  }

  document.addEventListener('alpine:init', function () {
    window.Alpine.data('mailKeys', function () {
      return {
        helpOpen: false,
        waitingForSecondKey: false,
        toggleHelp: function () {
          this.helpOpen = !this.helpOpen;
          var help = document.getElementById('mail-help');
          if (help) help.hidden = !this.helpOpen;
        },
        onKey: function (event) {
          if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) return;
          if (typing(event.target)) return;
          var key = event.key;
          if (this.waitingForSecondKey) {
            this.waitingForSecondKey = false;
            if (key === 'i') { click('#mail-folders a'); event.preventDefault(); }
            return;
          }
          switch (key) {
            case 'j': step(1); break;
            case 'k': step(-1); break;
            case 'o': if (!click('#mail-list .rm-row.active')) step(1); break;
            case 'c': click('[data-action="compose"]'); break;
            case 'r': click('[data-action="reply"]'); break;
            case 'a': click('[data-action="reply-all"]'); break;
            case 'f': click('[data-action="forward"]'); break;
            case '#': click('[data-action="delete"]'); break;
            case 's': click('[data-action="star"]'); break;
            case '!': click('[data-action="spam"]'); break;
            case '/': {
              var search = document.getElementById('mail-search');
              if (search) { search.focus(); event.preventDefault(); }
              break;
            }
            case '?': this.toggleHelp(); break;
            case 'g': {
              var self = this;
              self.waitingForSecondKey = true;
              window.setTimeout(function () { self.waitingForSecondKey = false; }, 1500);
              break;
            }
            default: return;
          }
          if (key !== 'g') event.preventDefault();
        }
      };
    });

    window.Alpine.data('composeForm', function () {
      function reveal(rowId, fieldId) {
        var row = document.getElementById(rowId);
        if (row) row.hidden = false;
        var field = document.getElementById(fieldId);
        if (field) field.focus();
      }
      return {
        showCc: function () { reveal('c-cc-row', 'c-cc'); },
        showBcc: function () { reveal('c-bcc-row', 'c-bcc'); }
      };
    });
  });
})();
