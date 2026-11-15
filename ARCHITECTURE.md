# RustMail Suite — Architecture Blueprint

> A modern, self-hosted groupware / mail collaboration platform.
> Functional target: **Kerio Connect** parity, with a **Rust core**, **Bootstrap 5**
> server-rendered UI, and battle-tested Linux mail daemons on the backend.

---

## 1. High-Level System View

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

---

## 2. Request / Data Flow

### 2.1 Inbound mail delivery
```
Internet
   │  SMTP (25)
   ▼
Postfix (smtpd) ──► Amavis content filter ──► ClamAV (AV) + SpamAssassin (spam)
   │                        │
   │  clean                 │  infected / spam
   ▼                        ▼
Postfix (re-inject)     Quarantine (Maildir + messages_meta row)
   │  LMTP (2424)
   ▼
Dovecot LMTP ──► Sieve (filters/vacation) ──► Maildir (user mailbox)
   │
   ▼
messages_meta INSERT (index only; body stays in Maildir)
   │
   ▼
WebSocket push ──► Web UI "new mail" notification
```

### 2.2 Webmail read path
```
Browser ──HTTP/HTMX──► rustmail-web (axum)
                          │
                          ├─ auth: JWT (httpOnly cookie) ─► rustmail-core::auth
                          ├─ list/search ─► PostgreSQL (messages_meta FTS)
                          └─ body fetch ─► rustmail-imap (async-imap) ─► Dovecot IMAP
```

### 2.3 Outbound mail (compose/send)
```
Browser ──► rustmail-web (compose) ──► temp attachment store
                          │
                          ▼
                    LMTP / SMTP submission ──► Postfix ──► Internet
```

---

## 3. Rust Workspace Layout

The project is a single Cargo workspace with five member crates. `rustmail-core`
is the shared library; everything else depends on it.

```
rustmail-core   (lib)   Domain models, repositories, auth, config, errors.
                        The single source of truth for business logic.

rustmail-web    (bin)   axum HTTP server: webmail + public site + WebSocket.
                        Renders askama templates (Bootstrap 5 + HTMX + Alpine).

rustmail-admin  (bin)   axum HTTP server (or mounted routes): admin console.
                        Domain/user/quota/audit management, gated by role.

rustmail-imap   (lib)   async-imap client wrapper used by web/admin to read
                        messages server-side from Dovecot.

rustmail-cli    (bin)   Operator tooling: seed admin, run migrations wrapper,
                        provisioning, maintenance tasks.
```

### Dependency direction
```
        rustmail-core (lib, no deps on siblings)
          ▲     ▲      ▲        ▲
          │     │      │        │
   rustmail-web │  rustmail-cli │
               rustmail-admin   rustmail-imap
                    ▲                 ▲
                    └── web/admin use imap ──┘
```
No sibling crate depends on `rustmail-web` or `rustmail-admin`. `rustmail-imap`
depends only on `rustmail-core`. This keeps the core reusable and testable.

---

## 4. Component Responsibilities

| Crate / Service    | Responsibility                                                        |
|--------------------|-----------------------------------------------------------------------|
| `rustmail-core`    | Domain types, repository traits + Postgres impls, `AuthService`, config, error types |
| `rustmail-web`     | Webmail UI, public routes, WebSocket push, template rendering         |
| `rustmail-admin`   | Admin console: CRUD for domains/users/aliases/groups, quota, audit    |
| `rustmail-imap`    | Async IMAP client used to fetch message bodies from Dovecot           |
| `rustmail-cli`     | Seeding, migrations, provisioning, maintenance                        |
| Postfix            | MTA — SMTP in/out, virtual mailboxes/aliases, SASL via Dovecot        |
| Dovecot            | IMAP/POP3, LMTP delivery, ManageSieve, quota (dict → Postgres)        |
| Amavis + ClamAV    | Content filtering: virus scanning                                     |
| SpamAssassin/Rspamd| Spam scoring + Bayesian training                                      |
| PostgreSQL 16      | Metadata, users, domains, sessions, audit, quota, message index (FTS) |
| Redis 7            | Cache, rate-limit buckets, session/ephemeral state                    |

---

## 5. Data Storage Strategy

- **Message bodies** live on disk in **Maildir** format (owned by Dovecot).
- **Message metadata** is indexed in `messages_meta` in Postgres for fast
  listing, search (Postgres FTS), and threading. Bodies are fetched via IMAP
  on demand.
- **Config** is loaded via the `config`/`figment` crate from `.env` + files,
  never hardcoded.
- **Secrets** (DB creds, JWT signing keys) come from environment / secret store,
  never committed. See `.env.example`.

---

## 6. Security Model (summary — detailed in Phase 10)

- Passwords: **Argon2id** hashing.
- Sessions: **JWT access + refresh tokens** in `httpOnly`, `Secure`, `SameSite` cookies.
  Refresh tokens rotate on every use with theft detection (implemented in Phase 2;
  see `PHASE_2_COMPLETE.md` for the full model).
- Roles: `SuperAdmin`, `DomainAdmin`, `User` (enforced per-route).
- Rate limiting via `tower-governor`; CSRF protection on state-changing routes.
- Audit log on auth events and admin mutations.
- TLS everywhere (Let's Encrypt), DKIM/SPF/DMARC, Fail2ban jails.

---

## 7. Technology Decisions (locked for the project)

| Concern            | Choice                                             |
|--------------------|----------------------------------------------------|
| Async runtime      | `tokio`                                            |
| HTTP framework     | `axum` + `tower` / `tower-http`                    |
| Templating         | `askama` (compile-time checked)                    |
| DB access          | `sqlx` (compile-time checked queries) + `sqlx-cli` |
| Migrations         | `sqlx migrate`                                     |
| Errors             | `thiserror` (libs) + `anyhow` (bins)               |
| Logging/tracing    | `tracing` + `tracing-subscriber`                   |
| Config             | `config` / `figment`                               |
| Password hashing   | `argon2`                                           |
| JWT                | `jsonwebtoken`                                     |
| IMAP client        | `async-imap`                                       |
| Frontend           | Bootstrap 5.3 + HTMX + Alpine.js (no React/Vue)    |

> Any crate not listed here will be confirmed with the maintainer before use
> (per the project's "never invent dependencies" rule).

---

## 8. Environments

- **Dev:** `docker-compose.dev.yml` brings up Postgres 16, Redis 7, and a dev
  SMTP sink (Mailpit). Rust binaries run on the host via `cargo run`.
- **Prod:** Ansible playbook provisions Postfix/Dovecot/Amavis/ClamAV +
  PostgreSQL + Redis, deploys Rust binaries as systemd units. (Phase 11)

---

## 9. Where things live

See [`PROJECT_STRUCTURE.md`](./PROJECT_STRUCTURE.md) for the annotated directory
tree, and [`ROADMAP.md`](./ROADMAP.md) for the phase-by-phase execution plan.

## 10. Mail server wiring (Phase 4)

- Postfix reads routing from PostgreSQL through the `postfix` role, which has column-level SELECT and cannot read password hashes. Recipients that do not exist are refused during the SMTP conversation.
- Dovecot authenticates against `users.password_hash` (Argon2id, same hash RustMail verifies) through the `dovecot` role. Postfix delegates SASL to Dovecot over a unix socket.
- Delivery: Postfix → LMTP unix socket → Dovecot → `<VMAIL_BASE>/<domain>/<user>/Maildir`.
- Quota: Dovecot's SQL dictionary writes `quota_usage` directly (migration 0009 lets it address rows by user name), so RustMail reads live figures.
- `AccountService` creates the database rows first and the Maildir second, and removes the rows again if the disk step fails. `MailboxProvisioner` never changes ownership and refuses paths that escape `VMAIL_BASE`.
- Config files are `.j2` templates with plain `{{ var }}` substitution only, rendered by `scripts/setup-mta.sh` (and later by Ansible).

## 11. Filtering pipeline (Phase 5)

```
Postfix smtpd ──► content_filter ──► Amavis :10024 ──► ClamAV (clamd socket)
                                         │         └─► SpamAssassin (library, Bayes)
                                         │ policy: SELECT … FROM filter_policies (role `amavis`)
                                         ▼
Postfix :10025 (re-entry, no filter) ──► Dovecot LMTP ──► Sieve (X-Spam-Flag → Junk) ──► Maildir
```

- Amavis decides per recipient. Thresholds come from `filter_policies` (server `@.`, domain `@example.com`, recipient `a@example.com`; most specific wins, NULL inherits).
- Infected, banned or very spammy mail is discarded silently (no backscatter) and written to the quarantine directory by Amavis. Mail between the "spam" and "quarantine" scores is delivered tagged and filed in Junk by Sieve.
- `rustmail-cli pipeline follow` reads the Postfix and Amavis logs, stores parsed events in `pipeline_events` and indexes quarantined files in `quarantine_items`. Position and events are committed in one transaction.
- The web UI (super admin only) lists, inspects, releases and deletes quarantined messages. Release hands the original back to Postfix's re-entry port, to that one recipient, without rescanning.
- Training: moving a message into Junk (or out of it, except to Trash) runs `sa-learn` through Dovecot's imapsieve.
- ClamAV runs in the same container/host as Amavis because Amavis passes clamd a file path.

## 12. Webmail (Phase 6)

```
Browser ──htmx/Alpine──► rustmail-web ──IMAPS (master login)──► Dovecot ──► Maildir
   ▲  ▲                      │  │ └──SMTP 587 + STARTTLS (master login)──► Postfix ► Amavis ► delivery
   │  └── WebSocket "new" ◄──┘  └── search ──► PostgreSQL messages_meta (headers + snippet)
   └── sandboxed iframe: sanitised HTML, no scripts, remote images off
```

- **No user passwords.** The web server authenticates to Dovecot and to Postfix as `<user>*<master>`. Dovecot accepts the master login only from `MAIL_MASTER_ALLOW_NETS`. Postfix then applies exactly the rules it applies to that user: the sender must be their own address, and outgoing mail is scanned like any other.
- **IMAP is the source of truth.** Pages open a short-lived IMAP session per request; nothing is cached except the search index.
- **Search** is PostgreSQL full-text over `messages_meta`: headers plus a 240-character snippet, filled in the background as people use the mailbox and by `rustmail-cli mail index`. A search hit is a pointer that may be stale if another client moved the message.
- **Display safety.** Mail is untrusted. HTML goes through `ammonia` (no scripts, styles, forms, frames, handlers; links get `noopener noreferrer`; remote images only on request), is served as its own document with `Content-Security-Policy: sandbox; default-src 'none'`, and is shown in an `<iframe sandbox>` without `allow-scripts` or `allow-same-origin`. Attachments are always downloads, with a sandbox CSP and a short allow-list of content types.
- **New mail** is pushed over a WebSocket backed by IMAP IDLE. The upgrade requires the session cookie and a matching `Origin`; sockets are limited per user and in total. The list also polls every minute.

## 13. Filters and autoresponder (Phase 8)
```
Browser ──htmx──► rustmail-web ──► PostgreSQL (mail_filters, sieve_scripts, autoresponders)
                      │
                      └─ ManageSieve :4190 (STARTTLS, master login) ──► Dovecot ~/sieve/
                           rustmail            active; includes the two below
                           rustmail-filters    the rules (generated) or the user's own script
                           rustmail-vacation   the autoresponder (generated)
Postfix ──LMTP──► Dovecot ──Pigeonhole runs the active script on delivery──► folders / reply / redirect
```
- **The database holds what the user entered; Sieve is generated from it.** Rules (`rustmail_core::filters`) become a script
  in `filters::sieve`. Every user-supplied string is quoted by one function, and validation rejects control characters, so a rule
  cannot add Sieve text. Disabled rules are left out.
- **Three scripts, not one**, so the rule builder, the advanced editor and the autoresponder never overwrite each other. The active
  script only `include`s the other two (`:optional`, so installing one never fails because the other is not there yet).
- **Raw scripts are checked by Dovecot** (`CHECKSCRIPT`) before they are saved. A script it rejects is shown to the user, with its
  line numbers, and never stored. Switching back to the rule builder keeps the hand-written script for later.
- **ManageSieve client** (`rustmail-sieve`): STARTTLS is mandatory (credentials are only sent inside TLS), login is `<user>*<master>`
  like webmail, every command has a timeout, and responses are size-bounded.
- **If Dovecot cannot be reached** the change is still saved, the page says "Not active yet" with the reason and offers "Send again",
  and "Apply to inbox" runs the same rules once over the 500 newest inbox messages with `filters::engine`. The fallback only moves
  and marks messages as read; discard and forward are skipped on purpose, because deleting or re-sending old mail on a guess is worse
  than leaving it. It cannot see `List-Id`, so rules on that field do not match there.
- **Autoresponder:** Pigeonhole's `vacation` with `:days` (1 to 30), the user's own addresses (login and enabled aliases) in
  `:addresses`, and optional first and last day compared with `currentdate`. Days follow the mail server's clock and time zone.
- **Editor:** CodeMirror 6, built from pinned npm packages into one local file and mounted in a shadow root so the strict
  `style-src 'self'` policy stays as it is. Without JavaScript the page is a plain textarea.

## 14. Calendar and contacts (Phase 9)
```
Thunderbird / phone ──HTTPS, Basic + app password──► /dav/… (rustmail-web) ──┐
Browser ──htmx──► /calendar /contacts /settings/…  (rustmail-web) ───────────┼─► rustmail_core::dav::store ─► PostgreSQL
                                                                              │   dav_collections · dav_objects
                                                       rustmail_core::dav::calendar / contact (parse, expand repeats)
```
- **Native, not a sidecar.** CalDAV and CardDAV are served by the web server, so there is one login system, one permission
  model, one database, and the calendar page, sharing and free/busy use the same code the protocol does. The cost is that the
  protocol is ours to get right; it is tested against an independent client (the Python `caldav` library) and by protocol tests.
- **The text a client sent is what is stored and returned**, byte for byte. Extracted columns (UID, start, end, last end, busy,
  search text) exist only for queries. Nothing a phone wrote is reformatted or dropped.
- **Collections and objects.** A user owns calendars and address books (made on first use). Each change bumps the collection's
  `revision`; the sync token is that number, and `dav_tombstones` remembers deletions. ETags are a hash of the text.
  Writes lock the collection row, so changes are numbered one after another.
- **Access in one place.** The SQL function `dav_access(user, collection)` gives 3 owner, 2 read/write, 1 read, 0 none (a group
  share counts for each member; the highest wins). The store checks it on every call, and a collection without access is "not
  found", so its existence is not revealed. Sharing stays inside one domain.
- **Repeating events** are expanded with the `rrule` crate, with exceptions (`EXDATE`) and moved instances (`RECURRENCE-ID`) handled
  here. The time extent of a repeating event is stored so the database can skip events outside a window.
- **The page only edits what it can write back.** An event is editable in the page only if it has nothing but the properties the
  form covers and a repeat rule the form can express; otherwise it is shown, and can be deleted, but a forged save is refused.
  Contacts are edited by replacing only the lines the page owns, so photos and custom fields survive.
- **Sign-in for apps** is HTTP Basic with an app password (128 random bits, only a SHA-256 stored, revocable, audited). Failed
  attempts are limited per client address and per user. No cookies are involved, so CSRF does not apply to `/dav`.
- **Free/busy** gives times only, for people in the asker's own domain, from the person's own calendars, ignoring events marked
  free or cancelled.
- **Front end:** FullCalendar 6, built from pinned packages into a local file; its stylesheet and icon font are extracted so the
  strict CSP stays unchanged.
