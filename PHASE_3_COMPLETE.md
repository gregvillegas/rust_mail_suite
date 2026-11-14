# Phase 3: Web UI Foundation (Bootstrap 5 + HTMX): COMPLETE

## Summary

`rustmail-web` now serves a server-rendered UI: sign-in, a user dashboard, an
administration area, localised error pages, and the static assets behind them.
Askama templates, Bootstrap 5.3 (CSS only), htmx and Alpine.js, light/dark/auto
theming, English and German via Fluent. The pages sit on the Phase 2 auth layer
and add a strict Content-Security-Policy with **no inline scripts or styles and no
`eval`**. No Phase 4 work (mailboxes, Dovecot) was started.

## Deliverables

| Brief item | Delivered |
|------------|-----------|
| axum + askama templates | `templates/`: `base.html`, `login.html`, `dashboard.html`, `admin/layout.html`, `admin/index.html`, `error.html`, `renew.html`, and `partials/` |
| Bootstrap 5.3 via CDN + local fallback | `ASSET_SOURCE=cdn` (default): jsDelivr with Subresource Integrity, falling back to the vendored copy if a file is missing, blocked or fails its hash. `local` skips the CDN entirely |
| Dark/light toggle | Auto / Light / Dark menu, remembered in `localStorage`, applied before first paint (no flash), follows the OS live while on Auto |
| HTMX partials | Sign-in form errors swap inline; dashboard activity table and admin overview load as partials (overview refreshes every 30 s); `hx-boost` navigation without full reloads |
| Alpine.js | Dropdown menus, mobile nav collapse, password reveal, confirmation dialog, theme menu |
| Static assets with cache headers | `tower-http::ServeDir`; content-hashed `?v=` URLs are `immutable` for a year, everything else revalidates |
| i18n scaffolding | Fluent, `en` (source) + `de`; plural selectors, fallback to English, language menu, `Accept-Language` negotiation |

### Routes

| Route | Method | Auth | Notes |
|-------|--------|------|-------|
| `/` | GET | user | dashboard |
| `/login` | GET / POST | none | the POST shares its rate-limit bucket with `/auth/login` |
| `/logout`, `/logout-all` | POST | user | CSRF-protected |
| `/lang` | POST | none | sets the `rm_lang` cookie |
| `/session/renew` | GET | none | silent refresh when the 15-minute access token has lapsed |
| `/partials/activity` | GET | user | the user's own recent sign-ins |
| `/admin`, `/admin/partials/overview` | GET | domain admin+ | server-wide domain count for super admins only |
| `/static/*` | GET | none | |

## Design decisions worth knowing

- **Fluent over `rust-i18n`.** The brief allowed either. Fluent has real plural
  rules, which a mail client needs ("1 message" / "5 messages", and languages with
  more than two plural forms). It costs a heavier dependency tree.
- **Alpine's CSP build, no Bootstrap JavaScript.** The standard Alpine build calls
  `eval`, which would force `'unsafe-eval'` into the CSP. The CSP build forbids
  inline expressions, so all behaviour lives in named components in `app.js`.
  Bootstrap's JS is not loaded at all.
- **CDN is the default, as the brief says, but `local` is the better choice for
  air-gapped or privacy-sensitive installs.** A CDN request tells a third party who
  is opening your admin login. Local mode also removes the CDN origin from the CSP.
- **Silent renewal.** The refresh cookie is scoped to `/auth`, so page requests never
  carry it. When the access token lapses, the server redirects to `/session/renew`,
  a tiny page whose script refreshes and returns the user to where they were (with a
  loop breaker). Otherwise users would be bounced to sign-in every 15 minutes.
- **One rate-limit bucket for both logins.** The HTML form and the JSON endpoint
  are merged into the same limiter; otherwise alternating between them would double
  an attacker's budget.
- **Errors for htmx requests become toasts** (`HX-Retarget`/`HX-Reswap`) instead of a
  JSON blob or a whole error page swapped into the middle of a page. Non-htmx clients
  still get JSON for API errors and a full localised page for browsers.
- **Open-redirect protection** on `next=`: only same-site absolute paths are
  accepted; `//host`, `/\host`, other schemes, control characters and redirect loops
  fall back to `/`.
- **htmx hardened:** `allowEval:false`, `allowScriptTags:false`, no inline indicator
  styles, and **`historyCacheSize:0`**, because htmx otherwise snapshots pages into
  `localStorage`, where they would outlive logout.

## Verification

| Check | Result |
|-------|--------|
| `cargo build --workspace`, `RUSTFLAGS=-D warnings`, `SQLX_OFFLINE=true` | pass |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass, 0 warnings |
| `cargo test --workspace --all-targets` | **171 passed, 0 failed** |
| Real-browser smoke test (`scripts/browser-smoke`) | **81 checks passed** |

Rust tests added this phase: 43 unit tests in `rustmail-web` (locale parity, template
key usage, `Accept-Language`, `safe_next`, asset hashing and SRI constants, error
mapping) and 30 HTTP tests in `tests/pages.rs` (markup, headers, redirects, the htmx
login flow, escaping, shared rate limiting, error pages, static serving, path
traversal).

### The browser test found three real bugs the Rust tests could not

1. After a form swap, focus never moved to the error message. htmx reports the
   *replaced* element for `outerHTML` swaps, and it is detached. Fixed in `app.js`.
2. **Toast close buttons were unclickable.** Bootstrap's `.toast-container` sets
   `pointer-events: none` and relies on `.toast` to opt back in; mine are `.alert`s.
   Fixed in `app.css`.
3. One test race worth knowing for future Alpine tests: `x-show` reveals content on a
   `setTimeout(0)` so the opening click cannot also count as a click-outside. Not a
   product bug, but tests must wait for visibility before the next click.

What the browser test covers: theme (persist, reload, live OS changes, Escape/click-away),
language switch, password reveal, inline sign-in errors without a reload, dashboard and
htmx-loaded activity, the dialog (open, Escape, backdrop, Cancel, focus handling),
sign-out and sign-out-everywhere (including a second device), 403/404 pages, boosted
navigation (document not reloaded, title updated, Back works), role differences,
CSRF/network/rate-limit toasts and dismissal, silent session renewal and its failure
path, the mobile layout, **zero console errors or CSP violations**, and the CDN chain:
real jsDelivr with real integrity hashes, CDN blocked, and tampered CDN files rejected
and replaced by the local copies.

### Reproduce
```bash
make dev && make migrate && make seed SEED_DOMAIN=example.com
make lint && make test
make run-web                    # http://127.0.0.1:8080
make browser-test               # optional; needs Chrome, internet, built binaries
```

## Things to know

1. **New dependencies:** `askama` 0.16, `fluent-templates` 0.15 (with `macros` and
   `walkdir`), `tower-http` 0.7 (`fs`, `set-header`), `tower` 0.5, and `base64` as a dev
   dependency. The browser test adds `playwright-core` under `scripts/browser-smoke/`
   only; it is not a Rust dependency and CI does not run it.
2. **Vendored files** (Bootstrap 5.3.9, htmx 2.0.11, Alpine CSP 3.17.5) are unmodified.
   Each was downloaded from jsDelivr and its SHA-384 matched against unpkg. A unit test
   checks the integrity constants against the files. Details and the upgrade procedure
   are in `static/vendor/README.md`.
3. **Static files must be deployed with the binary.** `STATIC_DIR` defaults to the
   crate's source directory, which only exists in a checkout. The server refuses to start
   if required assets are missing. Phase 11 packaging must ship the directory.
4. **German is a first draft** and needs a native speaker's review. Dates are shown as
   `YYYY-MM-DD HH:MM:SS UTC` in every language; locale-aware date formatting is not done.
5. **The language choice is a cookie.** A user's stored profile locale is not used until
   user settings exist (Phase 7), because the default stored value (`en`) is not a real
   choice and would override the browser's language.
6. **Content-Security-Policy and headers are set now** (CSP, `nosniff`, `X-Frame-Options`,
   `Referrer-Policy`, `Permissions-Policy`, COOP). **HSTS is not**; it needs TLS, which is
   Phase 10. CDN mode allows `https://cdn.jsdelivr.net` for scripts and styles.
7. **Admin scoping is still absent.** Domain admins can open `/admin`, but see no
   server-wide figures. Real scoping needs the Phase 7 assignment table.
8. **Not verified:** I did not test Safari or Firefox, only Chrome 157. Accessibility was
   checked structurally (labels, roles, focus handling, skip link, reduced motion) but not
   with a screen reader or an automated audit; full WCAG validation needs manual testing
   with assistive technologies. The CDN scenarios depend on jsDelivr being reachable.
9. **Not included:** the webmail UI (Phase 6), user/domain management screens (Phase 7),
   password change or reset, and a "remember me" option.

---

**Phase 3 complete. Say "Start Phase 4" to proceed.**
