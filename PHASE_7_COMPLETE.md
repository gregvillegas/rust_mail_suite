# Phase 7 complete: Admin Console

Domains, users, aliases, groups, quota dashboard, audit viewer, system health and
bulk CSV import, all under `/admin`.

## What was built

| Area | Where |
|------|-------|
| Authorisation, scoping, audit, all CRUD | `rustmail_core::admin` (`AdminService`, `Admin`, `AdminScope`) |
| Pages and handlers | `rustmail-web`: `admin_pages.rs`, `admin_reports.rs`, `admin_health.rs`, `admin_common.rs` |
| Templates | `crates/rustmail-web/templates/admin/*.html` |
| Charts | `static/js/admin.js` (Alpine component) + vendored Chart.js 4.4.7 (local only) |
| Quarantine for domain admins | `quarantine_pages.rs` (own domain only), `QuarantineFilter.domain` in core |
| Tests | `rustmail-core/tests/admin.rs` (15), `rustmail-web/tests/admin_pages.rs` (10), quarantine tests, `scripts/browser-smoke/admin.mjs` |

Behaviour:

- **Roles.** Super administrators see everything. Domain administrators manage ordinary
  users, aliases, groups and quotas in their own domain only. They cannot create or edit
  domains, create or change administrators, or open the health page. All checks are in the
  core service, not in the handlers.
- **Self-protection.** You cannot disable, delete or demote yourself, or remove the last
  active super administrator.
- **Passwords.** Generated passwords are shown once (create / reset) and never stored, logged
  or audited. Import results with generated passwords are one download, for the importing
  administrator only, valid 10 minutes, held in memory.
- **Deleting.** Disabling is the default. Hard delete needs the typed address and removes the
  Maildir. A domain cannot be deleted while it has users.
- **Import.** Columns `email,name,role,quota_mb,password`; preview first, nothing created if any
  row has a problem; 1000 rows / 1 MB; spreadsheet formulas are neutralised in CSV output.
- **Audit.** Filters (actor, action, outcome, dates, text), CSV export, scoped for domain admins.
  Every administrative action is audited.
- **Health.** Database, IMAP, SMTP, Redis, ClamAV, Amavis, Postfix queue, pipeline freshness, disk,
  systemd units. What cannot be probed from the host shows "Not checked", not "Down".

## Verify it yourself

```bash
make dev                      # Postgres, Redis, Mailpit
make mta-up                   # dev Postfix/Dovecot/Amavis
cargo test --workspace        # needs DATABASE_URL
make admin-smoke              # the phase verification, in a real browser
```

Manual: run the web server with `VMAIL_BASE=./var/vhosts` (see `.env.example`), sign in as a
super administrator, open `/admin/domains`, create a domain, then `/admin/users/new` three times.
Sign in as each. Give one a 1 MB quota, send it a message larger than 1 MB, and watch `/admin/quota`.

## What was verified

See the results block below (filled in from the final runs).

## Caveats

- Validation messages from the core are English, also in the German UI. The German translation is a draft.
- Deleting a domain leaves its (empty) directory on disk.
- Hard-deleting a user removes the Maildir irreversibly.
- Import results live in memory in one web process; with several processes behind a load
  balancer the download must reach the same process. A CSV with passwords you typed in yourself
  is echoed back in a hidden field of the preview form (not the generated ones).
- Chart.js is local-only, so the chart origin is never in the CSP. If it fails to load, the tables
  under the charts show the same figures.
- The web server now needs write access to `VMAIL_BASE` to create accounts (as the CLI does).
- Cross-domain access to a user page returns 403; to a quarantine item, 404.
- Nothing is committed yet.

Phase 7 complete. Say 'Start Phase 8' to proceed.
