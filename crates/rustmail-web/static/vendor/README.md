# Vendored third-party assets

These files are served locally and are also offered from jsDelivr in CDN mode
(`ASSET_SOURCE=cdn`). They are **unmodified** copies of the published files; the
Subresource Integrity values in `src/assets.rs` are checked against them by a unit
test, so the CDN and local copies cannot drift apart unnoticed.

| Library | Version | Licence | Local path | CDN URL |
|---------|---------|---------|-----------|---------|
| Bootstrap (CSS only) | 5.3.9 | MIT | `bootstrap/5.3.9/bootstrap.min.css` | `https://cdn.jsdelivr.net/npm/bootstrap@5.3.9/dist/css/bootstrap.min.css` |
| htmx | 2.0.11 | 0BSD | `htmx/2.0.11/htmx.min.js` | `https://cdn.jsdelivr.net/npm/htmx.org@2.0.11/dist/htmx.min.js` |
| Alpine.js (CSP build) | 3.17.5 | MIT | `alpine-csp/3.17.5/alpine-csp.min.js` | `https://cdn.jsdelivr.net/npm/@alpinejs/csp@3.17.5/dist/cdn.min.js` |

Chart.js is the one exception: it is **local only** (no CDN mode, so the Content-Security-Policy
never needs a chart origin) and is loaded on demand by `static/js/admin.js` on the quota page.

| Library | Version | Licence | Local path | Source |
|---------|---------|---------|-----------|--------|
| Chart.js | 4.4.7 | MIT | `chartjs/4.4.7/chart.umd.min.js` (+ `LICENSE.md`) | `https://cdn.jsdelivr.net/npm/chart.js@4.4.7/dist/chart.umd.js` |

SHA-384 of the file (identical on jsDelivr and unpkg):
`zYPBGXwO4633CABX/5Spf6emCKUJCfoOkhOMYyxMsatqQZPnDblmmOewfjsIVWCM`.

CodeMirror 6 (the Sieve editor on the advanced filters page) is also **local only**, and is not a
published file: it is *built* from pinned npm packages into one minified script.

| Component | Version | Licence | Local path |
|-----------|---------|---------|-----------|
| `@codemirror/{state,view,commands,language,legacy-modes}` and their dependencies | see `scripts/vendor-codemirror/package-lock.json` | MIT | `codemirror/6/sieve-editor.js` (+ `LICENSES.txt`) |

Rebuild with `cd scripts/vendor-codemirror && npm ci && npm run build`. The editor mounts in a
shadow root so CodeMirror styles itself with constructable stylesheets; its default `<style>`
element would be blocked by the `style-src 'self'` policy, and the policy is not relaxed for it.

FullCalendar (the calendar view) is **local only** and, like the editor, is *built* from pinned npm
packages rather than downloaded as one published file.

| Component | Version | Licence | Local path |
|-----------|---------|---------|-----------|
| `@fullcalendar/{core,daygrid,timegrid,list,interaction}` (and Preact) | 6.1.22 (see `scripts/vendor-fullcalendar/package-lock.json`) | MIT | `fullcalendar/6/calendar.js`, `calendar.css`, `fcicons.ttf` (+ `LICENSES.txt`) |

Rebuild with `cd scripts/vendor-fullcalendar && npm ci && npm run build`. FullCalendar normally adds its
CSS at run time with a `<style>` element and embeds its icon font as a `data:` URI; the
`style-src 'self'` and `font-src 'self'` policies forbid both. The build moves the stylesheets into
`calendar.css` and the font into `fcicons.ttf` and fails if the library's layout changes. Version 6 is
used on purpose: version 7 is a different, larger product.

Bootstrap's JavaScript is intentionally **not** included: menus, the collapse and
the dialog are built with Alpine.

Each file was downloaded from jsDelivr and its SHA-384 compared with the same file
from unpkg before being committed.

## Upgrading

1. Download the new version from jsDelivr into a new versioned directory.
2. Compare its SHA-384 with the unpkg copy:
   `openssl dgst -sha384 -binary FILE | openssl base64 -A`
3. Update the `Vendor` constants (path, URL, integrity) in `src/assets.rs`.
4. `cargo test -p rustmail-web`, then delete the old directory.

Because the directory name contains the version, vendored URLs change on upgrade
and are cached as immutable.
