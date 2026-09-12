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
| `rustmail-cli`   | bin  | Operator tooling (seed, migrate, provision)          |

---

## Quick Start (dev) — available after Phase 0

```bash
# 1. Bring up dev infrastructure (Postgres 16, Redis 7, Mailpit)
make dev

# 2. Run database migrations
make migrate

# 3. Build the workspace
cargo build --workspace

# 4. Lint & test
make lint
make test
```

Copy `.env.example` to `.env` and adjust before running.

---

## Development Rules

- Work strictly **one phase at a time**; do not scaffold future phases.
- Production-grade code: `thiserror`/`anyhow`, `tracing`, config via `config`/`figment`.
- No `unwrap()`/`expect()` in production paths.
- `#![deny(warnings)]` on library crates; document all public items.
- Conventional Commits.
- Never invent dependencies — confirm uncertain crates first.

See [`ROADMAP.md`](./ROADMAP.md) for the full 13-phase plan.

---

## License

TBD.
