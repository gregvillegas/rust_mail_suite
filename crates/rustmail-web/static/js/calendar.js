// Calendar page behaviour: the month/week/day view and the event form.
//
// Like app.js this works with Alpine's CSP build: components are named and registered here,
// templates only reference them. No eval, no inline code. FullCalendar and its stylesheet are
// vendored files loaded on demand (hx-boost swaps pages without running <script> tags or
// reading <head>, so they cannot be tags in the page).
(function () {
  'use strict';

  var loading = null;

  function loadAssets(script, css) {
    if (window.RustMailCalendar) return Promise.resolve(window.RustMailCalendar);
    if (!loading) {
      loading = new Promise(function (resolve, reject) {
        if (css && !document.querySelector('link[data-rm-calendar]')) {
          var link = document.createElement('link');
          link.rel = 'stylesheet';
          link.href = css;
          link.setAttribute('data-rm-calendar', '');
          document.head.appendChild(link);
        }
        var tag = document.createElement('script');
        tag.src = script;
        tag.onload = function () { resolve(window.RustMailCalendar); };
        tag.onerror = function () { loading = null; reject(new Error('calendar did not load')); };
        document.head.appendChild(tag);
      });
    }
    return loading;
  }

  // Colours come from the database and are applied with the CSSOM, which the strict
  // Content-Security-Policy allows (an inline style attribute would not be).
  function paintSwatches(root) {
    root.querySelectorAll('.rm-swatch[data-color]').forEach(function (el) {
      var color = el.getAttribute('data-color');
      if (/^#[0-9a-fA-F]{6}$/.test(color)) el.style.backgroundColor = color;
    });
  }

  function pad(n) { return (n < 10 ? '0' : '') + n; }

  // "2025-03-10T09:00" read as UTC, shown as the browser's local time.
  function utcToLocalInput(value) {
    var d = new Date(value + ':00Z');
    if (isNaN(d.getTime())) return value;
    return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) + 'T' + pad(d.getHours()) + ':' + pad(d.getMinutes());
  }

  document.addEventListener('alpine:init', function () {
    window.Alpine.data('calendarView', function () {
      return {
        failed: false,
        calendar: null,
        init: function () {
          var self = this;
          var root = this.$el;
          paintSwatches(document);
          loadAssets(root.getAttribute('data-script'), root.getAttribute('data-css')).then(function (api) {
            var data = root.dataset;
            var newUrl = data.newUrl;
            self.calendar = new api.Calendar(self.$refs.mount, {
              plugins: api.plugins,
              locales: api.locales,
              locale: data.lang,
              initialView: 'dayGridMonth',
              initialDate: data.initialDate || undefined,
              headerToolbar: { left: 'prev,next today', center: 'title', right: 'dayGridMonth,timeGridWeek,timeGridDay,listWeek' },
              buttonText: { today: data.labelToday, month: data.labelMonth, week: data.labelWeek, day: data.labelDay, list: data.labelList },
              height: 'auto',
              nowIndicator: true,
              navLinks: true,
              dayMaxEvents: true,
              events: { url: data.eventsUrl, failure: function () { self.failed = true; } },
              dateClick: function (info) { window.location.assign(newUrl + '?date=' + encodeURIComponent(info.dateStr)); }
            });
            self.calendar.render();
            root.setAttribute('data-calendar-ready', 'true');
          }).catch(function () {
            self.failed = true;
          });
        },
        destroy: function () {
          if (this.calendar) this.calendar.destroy();
          this.calendar = null;
        }
      };
    });

    // The event form shows times in UTC when it arrives (so it works without this script) and
    // converts them to the browser's time zone, telling the server which zone that is.
    window.Alpine.data('eventForm', function () {
      return {
        init: function () {
          var form = this.$el;
          var zone = '';
          try { zone = Intl.DateTimeFormat().resolvedOptions().timeZone || ''; } catch (e) { zone = ''; }
          var tz = form.querySelector('[name=tz]');
          var start = form.querySelector('[name=start]');
          var end = form.querySelector('[name=end]');
          var allDay = form.querySelector('[name=all_day]');
          if (zone && tz) tz.value = zone;
          if (zone && form.getAttribute('data-utc') === '1' && !allDay.checked) {
            start.value = utcToLocalInput(start.value);
            end.value = utcToLocalInput(end.value);
          }
          form.setAttribute('data-form-ready', 'true');
        },
        toggleAllDay: function (event) {
          var form = this.$el;
          var start = form.querySelector('[name=start]');
          var end = form.querySelector('[name=end]');
          var on = event.target.checked;
          [start, end].forEach(function (input, index) {
            var current = input.value || '';
            if (on) {
              input.type = 'date';
              input.value = current.slice(0, 10);
            } else {
              input.type = 'datetime-local';
              input.value = current.slice(0, 10) + (index === 0 ? 'T09:00' : 'T10:00');
            }
          });
        }
      };
    });

    // Anything that only needs its swatches painted (settings pages).
    window.Alpine.data('swatches', function () {
      return { init: function () { paintSwatches(this.$el); } };
    });
  });
})();
