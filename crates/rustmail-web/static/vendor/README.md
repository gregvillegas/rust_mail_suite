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
