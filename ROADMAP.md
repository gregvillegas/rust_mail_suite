# RustMail Suite — Execution Roadmap

Development proceeds **strictly one phase at a time**. No future phase is
scaffolded until its predecessor is verified and explicitly approved.

Each phase ends with:
- a `PHASE_X_COMPLETE.md` summary,
- test/verification commands, and
- a "Ready for Phase X+1?" confirmation gate.

Legend: ☐ not started · ◐ in progress · ☑ complete

---

## Phase 0 — Project Kickoff & Environment Audit ☑
**Deliverables**
- Cargo workspace with `rustmail-core`, `rustmail-web`, `rustmail-admin`, `rustmail-imap`, `rustmail-cli`
- `docker-compose.dev.yml` (Postgres 16, Redis 7, Mailpit)
- `Makefile` targets: `dev`, `migrate`, `test`, `lint`
- `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`
- `README.md` with architecture diagram
- `.env.example`
- CI stub `.github/workflows/ci.yml` (fmt, clippy, test)

**Verification:** `cargo build --workspace` passes; `make dev` starts Postgres + Redis.

---

## Phase 1 — Database Schema & Domain Model ☑
**Deliverables**
- SQLx migrations: `users`, `domains`, `mailboxes`, `aliases`, `groups`,
  `messages_meta`, `sessions`, `audit_log`, `quota_usage`, `sieve_scripts`,
  `autoresponders`, `mail_filters`, `calendar_events` (stub), `contacts` (stub)
- Domain models in `rustmail-core/src/domain/` (serde + `sqlx::FromRow`)
- Repository traits + Postgres implementations
- Seed to bootstrap first admin + domain (delivered as `rustmail-cli seed`, logic in `rustmail_core::seed`)

**Verification:** `sqlx migrate run` succeeds; `cargo test -p rustmail-core` passes; seed creates admin.

**Status:** complete. See `PHASE_1_COMPLETE.md`.

---

## Phase 2 — Authentication & Session Layer ☑
**Deliverables**
- Argon2id password hashing
- JWT access + refresh tokens (httpOnly cookies)
- Roles: `SuperAdmin`, `DomainAdmin`, `User`
- Rate limiting (`tower-governor`), CSRF protection
- `rustmail-core/src/auth/` with `AuthService`
- Audit log writes on login/logout/failed attempts

**Verification:** unit tests for password verify + token issue/refresh; integration test via `axum-test`.

**Status:** complete. See `PHASE_2_COMPLETE.md`.

---

## Phase 3 — Web UI Foundation (Bootstrap 5 + HTMX) ☑
**Deliverables**
- `rustmail-web` with axum + askama templates
- Layouts: `base.html`, `login.html`, `dashboard.html`, `admin/*`
- Bootstrap 5.3 CDN + local fallback; dark/light toggle
- HTMX partials; Alpine.js interactivity
- Static assets with cache headers (`tower-http::ServeDir`)
- i18n scaffolding (`fluent` or `rust-i18n`)

**Verification:** `cargo run -p rustmail-web` → login page renders, login flow works E2E.

**Status:** complete. See `PHASE_3_COMPLETE.md`.

---

## Phase 4 — Mailbox Provisioning & Dovecot Integration ☐
**Deliverables**
- Maildir creation on user/domain creation
- Dovecot config templates (`deploy/dovecot/`): Postgres auth, LMTP socket, ManageSieve, quota dict → Postgres
- Rust `MailboxProvisioner` service
- Postfix integration: virtual mailboxes/aliases, SASL via Dovecot
- Templated Postfix configs (`deploy/postfix/`)
- Idempotent `scripts/setup-mta.sh`

**Verification:** create user via UI → `swaks` test mail → appears in Dovecot IMAP.

---

## Phase 5 — MTA Pipeline: Postfix + Amavis + SpamAssassin + ClamAV ☑
**Deliverables**
- Postfix `master.cf` with amavis content filter chain
- Amavis config (`deploy/amavis/`) with Postgres lookups
- SpamAssassin rules + Bayesian training hooks
- ClamAV daemon integration
- Rust pipeline observability: parse `/var/log/mail.log` → structured events → Postgres
- Quarantine table + web UI (view/release/delete)

**Verification:** EICAR → quarantined; spam sample → tagged; legit → delivered.

**Status:** complete. See `PHASE_5_COMPLETE.md`.

---

## Phase 6 — Webmail Client (Inbox, Compose, Folders) ☑
**Deliverables**
- Rust IMAP client (`async-imap`) reading messages server-side
- Routes: `/mail/inbox`, `/mail/message/:uid`, `/mail/compose`, `/mail/folders`
- Bootstrap 3-pane layout (folders | list | preview)
- Compose with attachments (temp upload → LMTP inject)
- Search: Postgres FTS on `messages_meta`
- Real-time new-mail via WebSocket
- Keyboard shortcuts (Alpine.js)

**Verification:** login → inbox → open → reply → send → recipient receives.

**Status:** complete. See `PHASE_6_COMPLETE.md`.

---

## Phase 7 — Admin Console (Domain/User/Quota Management) ☑
**Deliverables**
- `/admin` routes gated by role
- CRUD: domains, users, aliases, groups
- Quota dashboard with charts (Chart.js / ApexCharts)
- Audit log viewer with filters
- System health page (systemd status)
- Bulk CSV import for users

**Verification:** admin creates domain + 3 users → all log in; quotas enforced.
**Status:** complete. See `PHASE_7_COMPLETE.md`.

---

## Phase 8 — Filters, Autoresponders, Sieve ☑
**Deliverables**
- Sieve editor in UI (CodeMirror 6)
- ManageSieve client in Rust
- Autoresponder UI (start/end dates, subject, body)
- Vacation integration with Dovecot
- Server-side rules-engine fallback

**Verification:** user creates filter "from boss@ → Boss folder" → routed correctly.
**Status:** complete. See `PHASE_8_COMPLETE.md`.

---

## Phase 9 — Calendar & Contacts (Groupware Parity) ☑
**Deliverables**
- CalDAV/CardDAV endpoints (native Rust or Radicale sidecar — TBD)
- Web UI: calendar (FullCalendar.js) + contacts
- iCal/vCard parsing (`ical` + `vcard`)
- Sharing between users (ACLs)
- Free/busy lookup

**Verification:** create event → sync via CalDAV in Thunderbird → appears.
**Status:** complete. See `PHASE_9_COMPLETE.md`.

---

## Phase 10 — Security Hardening & Observability ☐
**Deliverables**
- TLS everywhere (Let's Encrypt via `acme-lib` or certbot hook)
- DKIM signing, SPF, DMARC setup wizard
- Fail2ban jails (Postfix/Dovecot)
- Prometheus metrics (`metrics` + `metrics-exporter-prometheus`)
- Grafana dashboards (`deploy/grafana/`)
- Optional structured logs → Loki
- Backup/restore scripts

**Verification:** SSL Labs A+; DKIM validates; metrics scrape works.

---

## Phase 11 — Packaging & Deployment ☐
**Deliverables**
- Ansible playbook (`deploy/ansible/`) for Ubuntu 22.04 / Debian 12
- Docker images: `rustmail-web`, `rustmail-admin` (multi-stage)
- Helm chart (optional)
- Systemd units for Rust binaries
- Upgrade path / migration runner
- mdBook docs site (`docs/`)

**Verification:** fresh Ubuntu VM → `ansible-playbook site.yml` → working mail server in < 15 min.

---

## Phase 12 — Load Testing, Fuzzing, GA Release ☐
**Deliverables**
- Load tests: k6/drill (HTTP) + imaptest (IMAP)
- Fuzzing: `cargo-fuzz` on parsers (MIME, iCal, vCard)
- Security audit checklist (OWASP, CIS)
- Benchmark report
- `v1.0.0` tag, CHANGELOG, release notes

**Verification:** 1000 concurrent IMAP sessions stable; fuzzers run 24h no crash.

---

## Coding Standards (all phases)
- `#![deny(warnings)]` on library crates
- All public items documented (`///`)
- No `unwrap()`/`expect()` in production paths (tests/bins only, with justification)
- Integration tests in `tests/` per crate
- Deployment configs templated with `.j2` (Ansible) where targeted
- Conventional Commits
