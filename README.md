# RustMail Suite

A modern, self-hosted **groupware / mail collaboration platform** — functionally
equivalent to Kerio Connect, built with a **Rust core**, a **Bootstrap 5**
server-rendered web UI, and battle-tested Linux mail daemons (Postfix, Dovecot,
Amavis, ClamAV, SpamAssassin).

> **Status:** Pre-implementation. The repository currently contains the project
> **structure and blueprint documentation only**. Implementation proceeds one
> phase at a time — see [`ROADMAP.md`](./ROADMAP.md).

---

## Architecture

```
┌───────────────────────────────────────────────────────────────────────────┐
│                            RustMail Suite Stack                             │
├───────────────────────────────────────────────────────────────────────────┤
│  Frontend:  Bootstrap 5.3 + HTMX + Alpine.js  (server-rendered by axum)     │
│  API:       axum REST + WebSocket (real-time mail push)                     │
│  Core:      Rust workspace (mail-core, web, admin, imap, cli)               │
├───────────────────────────────────────────────────────────────────────────┤
│  MTA:       Postfix        │  IMAP/POP3:  Dovecot                           │
│  AV:        Amavis + ClamAV│  Spam:       SpamAssassin + Rspamd             │
│  DB:        PostgreSQL 16  │  Cache:      Redis 7                           │
│  Auth:      PAM / LDAP / internal (Argon2id)                                │
│  Storage:   Maildir on XFS/ext4  │  Sieve (ManageSieve)                     │
└───────────────────────────────────────────────────────────────────────────┘
```

Full details in [`ARCHITECTURE.md`](./ARCHITECTURE.md). Directory layout in
[`PROJECT_STRUCTURE.md`](./PROJECT_STRUCTURE.md).

---

## Technology Stack

| Layer        | Technology                                             |
|--------------|--------------------------------------------------------|
| Language     | Rust (stable), async via `tokio`                       |
| HTTP         | `axum` + `tower` / `tower-http`                        |
| Templating   | `askama` (compile-time checked)                        |
| Database     | PostgreSQL 16 via `sqlx` (compile-time checked queries)|
| Cache        | Redis 7                                                |
| Auth         | Argon2id + JWT (`jsonwebtoken`)                        |
| Frontend     | Bootstrap 5.3 + HTMX + Alpine.js                       |
| Mail daemons | Postfix, Dovecot, Amavis, ClamAV, SpamAssassin/Rspamd  |

---

## Workspace Crates

| Crate            | Type | Purpose                                              |
|------------------|------|------------------------------------------------------|
| `rustmail-core`  | lib  | Domain models, repositories, auth, config, errors    |
| `rustmail-web`   | bin  | Webmail + public UI + WebSocket (axum + askama)      |
| `rustmail-admin` | bin  | Admin console (domain/user/quota/audit)              |
| `rustmail-imap`  | lib  | Async IMAP client used to read messages server-side  |
| `rustmail-sieve` | lib  | ManageSieve client: installs users' Sieve scripts    |
| `rustmail-cli`   | bin  | Operator tooling (seed, migrate, provision)          |

---

## Quick Start (dev)

```bash
# 0. Configure (the dev Postgres listens on host port 5433, not 5432)
cp .env.example .env

# 1. Bring up dev infrastructure (Postgres 16, Redis 7, Mailpit)
make dev

# 2. Apply database migrations (needs sqlx-cli 0.8.6, see `make migrate`),
#    or without sqlx-cli:  cargo run -p rustmail-cli -- migrate
make migrate

# 3. Create the first domain + super admin (prints a generated password once)
make seed SEED_DOMAIN=example.com

# 4. Run the web server. It refuses to start with the placeholder JWT_SECRET:
#    run `make secret` and paste the value into .env as JWT_SECRET=...
make run-web          # http://127.0.0.1:8080  ->  /healthz and /auth/*

# 5. Build, lint, test (tests create their own throwaway databases)
cargo build --workspace
make lint
make test

# 6. Optional: drive a real browser through the UI (needs Chrome and internet)
make browser-test
```

Then open http://127.0.0.1:8080 and sign in with the admin created by `make seed`.

### Mail server (Phase 4, optional)

A Debian 12 container runs Postfix and Dovecot against your dev database:

```bash
make mta-up        # builds the image; renders config from deploy/ and starts both daemons
rustmail-cli domain create example.test
rustmail-cli user create alice@example.test      # prints a generated password once
make mta-smoke     # create users, deliver with swaks, read via IMAP, check quota (about 2 minutes)
```

Mail relayed outwards lands in Mailpit. Supported targets: Debian 12 (Postfix 3.7, Dovecot 2.3). Dovecot 2.4 is not supported.
Incoming mail is also scanned (ClamAV, SpamAssassin). Viruses and very high spam scores are held in quarantine; run
`rustmail-cli pipeline follow` (or `make pipeline-follow`) to index the logs, then open **Administration → Quarantine** as a super admin.
Thresholds are set with `rustmail-cli policy set @example.com --quarantine-level 15`. Without a download, ClamAV only knows a built-in
EICAR test signature; set `CLAMAV_UPDATE=1` in `deploy/dev/mta.env` to fetch the real signature database (needs the internet, ~300 MB).

### Webmail (Phase 6)

With the mail stack running and `MAIL_MASTER_PASSWORD` set (the dev values are in `.env.example`), `make run-web` serves webmail at <http://127.0.0.1:8080/mail>:
three-pane inbox, reply/forward, attachments, folders, search, spam training and keyboard shortcuts (press `?`).
`make webmail-smoke` drives all of it in a real browser; `make mail-index USER=alice@example.test` builds the search index up front.

For a real host, copy `deploy/mta.env.example` to `/etc/rustmail/mta.env` and run `sudo scripts/setup-mta.sh`.
With `COOKIE_SECURE=false` in `.env` (the example default) cookies work over plain
HTTP on localhost; leave it `true` anywhere else.

### Filters and autoresponder (Phase 8)
Signed-in users open **Filters** and **Autoresponder** from the account menu (`/settings/filters`, `/settings/autoresponder`):
a rule builder ("from boss@ → move to Boss"), an advanced Sieve editor, and a vacation reply with optional dates.
Changes are installed on Dovecot over ManageSieve (`SIEVE_HOST` / `SIEVE_PORT`, default: the IMAP host, port 4190).
`make filters-smoke` drives it all in a real browser against the dev mail stack.

### Calendar and contacts (Phase 9)
**Calendar** and **Contacts** are in the top menu. To use Thunderbird or a phone, make an app password under
**App passwords** (`/settings/apps`) and give the app the server address `https://your-host/dav/` and your email address
as the user name (or the address of one calendar, shown on the same page and under **Calendars and address books**).
Calendars and address books can be shared read-only or read/write with a colleague or a group in the same domain.
`make calendar-smoke` drives it all in a real browser with an independent CalDAV client.

### Auth API (Phase 2)

| Route | Method | Notes |
|-------|--------|-------|
| `/auth/csrf` | GET | Issues a CSRF token (cookie `rm_csrf` + JSON). |
| `/auth/login` | POST | JSON `{email, password}`; needs `X-CSRF-Token`; rate limited. |
| `/auth/refresh` | POST | Rotates the session from the `rm_refresh` cookie; needs `X-CSRF-Token`; rate limited. |
| `/auth/logout` | POST | Ends the session and clears cookies; needs `X-CSRF-Token`. |
| `/auth/me` | GET | The signed-in user (needs the `rm_access` cookie). |

Tokens only ever travel in `HttpOnly` cookies. See `PHASE_2_COMPLETE.md` for the
security model.

`SQLX_OFFLINE=true` (the default in `.env.example`) compiles queries from the
committed `.sqlx/` cache, so builds work without a running database. After
changing a query or migration, run `make migrate` then `make prepare` and
commit the updated `.sqlx/`.

---

## Development Rules

- Work strictly **one phase at a time**; do not scaffold future phases.
- Production-grade code: `thiserror`/`anyhow`, `tracing`, config via environment/`.env` (a config crate may be added when settings grow).
- No `unwrap()`/`expect()` in production paths.
- `#![deny(warnings)]` on library crates; document all public items.
- Conventional Commits.
- Never invent dependencies — confirm uncertain crates first.

See [`ROADMAP.md`](./ROADMAP.md) for the full 13-phase plan.

---

## License

TBD.
