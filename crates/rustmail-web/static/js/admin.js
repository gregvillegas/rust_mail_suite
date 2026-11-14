// Admin console behaviour: the quota charts and the health auto-refresh helper.
//
// Like app.js this works with Alpine's CSP build: the component is named and
// registered here, templates only reference it. No eval, no inline code. Chart.js
// is a vendored file that is loaded on demand (hx-boost swaps pages without
// running <script> tags, so it cannot be a tag in the page body).
(function () {
  'use strict';

  var chartPromise = null;

  function loadChartLibrary(src) {
    if (window.Chart) return Promise.resolve(window.Chart);
    if (!chartPromise) {
      chartPromise = new Promise(function (resolve, reject) {
        var tag = document.createElement('script');
        tag.src = src;
        tag.onload = function () { resolve(window.Chart); };
        tag.onerror = function () { chartPromise = null; reject(new Error('chart.js did not load')); };
        document.head.appendChild(tag);
      });
    }
    return chartPromise;
  }

  function cssVar(name, fallback) {
    var value = window.getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return value || fallback;
  }

  function megabytes(bytes) {
    return Math.round((bytes / (1024 * 1024)) * 10) / 10;
  }

  document.addEventListener('alpine:init', function () {
    window.Alpine.data('quotaCharts', function () {
      return {
        failed: false,
        charts: [],
        init: function () {
          var self = this;
          var root = this.$el;
          var src = root.getAttribute('data-chart-src');
          var url = root.getAttribute('data-url');
          Promise.all([loadChartLibrary(src), window.fetch(url, { credentials: 'same-origin', headers: { Accept: 'application/json' } }).then(function (r) {
            if (!r.ok) throw new Error('quota data ' + r.status);
            return r.json();
          })]).then(function (results) {
            self.draw(results[0], results[1], root);
          }).catch(function () {
            self.failed = true;
          });
        },
        draw: function (Chart, data, root) {
          var primary = cssVar('--bs-primary', '#0d6efd');
          var danger = cssVar('--bs-danger', '#dc3545');
          var warning = cssVar('--bs-warning', '#ffc107');
          var muted = cssVar('--bs-secondary-bg', '#e9ecef');
          var domainCanvas = document.getElementById('chart-domains');
          if (domainCanvas && data.domains.length) {
            this.charts.push(new Chart(domainCanvas, {
              type: 'bar',
              data: {
                labels: data.domains.map(function (d) { return d.domain; }),
                datasets: [{
                  label: root.getAttribute('data-label-used'),
                  data: data.domains.map(function (d) { return megabytes(d.used_bytes); }),
                  backgroundColor: primary
                }, {
                  label: root.getAttribute('data-label-limit'),
                  data: data.domains.map(function (d) { return megabytes(d.limited_quota_bytes); }),
                  backgroundColor: muted
                }]
              },
              options: { responsive: true, maintainAspectRatio: false, scales: { y: { beginAtZero: true, title: { display: true, text: 'MB' } } } }
            }));
          }
          var topCanvas = document.getElementById('chart-top');
          var limited = data.top.filter(function (m) { return m.quota_bytes; });
          if (topCanvas && limited.length) {
            this.charts.push(new Chart(topCanvas, {
              type: 'bar',
              data: {
                labels: limited.map(function (m) { return m.email; }),
                datasets: [{
                  label: root.getAttribute('data-label-percent'),
                  data: limited.map(function (m) { return Math.round((m.used_bytes / m.quota_bytes) * 1000) / 10; }),
                  backgroundColor: limited.map(function (m) {
                    var p = m.used_bytes / m.quota_bytes;
                    return p >= 0.9 ? danger : (p >= 0.75 ? warning : primary);
                  })
                }]
              },
              options: { indexAxis: 'y', responsive: true, maintainAspectRatio: false, scales: { x: { beginAtZero: true, max: 100 } }, plugins: { legend: { display: false } } }
            }));
          }
          root.setAttribute('data-charts-ready', String(this.charts.length));
        },
        destroy: function () {
          this.charts.forEach(function (c) { c.destroy(); });
          this.charts = [];
        }
      };
    });
  });
})();
