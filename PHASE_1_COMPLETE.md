# Phase 1 — Database Schema & Domain Model — COMPLETE

## Summary

PostgreSQL schema (7 migrations, 15 tables), the Rust domain model, repository
traits with compile-time checked PostgreSQL implementations, and an idempotent
first-run seed (`rustmail-cli seed`). No Phase 2+ code was scaffolded (no JWT,
sessions logic, HTTP, templates).

## Deliverables

### Migrations (`migrations/`)

| File | Contents |
|------|----------|
| `0001_types_and_functions.sql` | enums `user_role`, `account_status`, `audit_outcome`; `set_updated_at()` trigger fn |
| `0002_domains_users_mailboxes.sql` | `domains`, `users`, `mailboxes` |
| `0003_aliases_groups.sql` | `aliases`, `groups`, `group_members` |
| `0004_sessions_audit_quota.sql` | `sessions`, `audit_log`, `quota_usage` |
| `0005_messages_meta.sql` | `messages_meta` with a generated `tsvector` + GIN index (Phase 6 search) |
| `0006_sieve_autoresponders_filters.sql` | `sieve_scripts` (one active per user), `autoresponders`, `mail_filters` |
| `0007_calendar_contacts_stubs.sql` | `calendar_events`, `contacts` (stubs for Phase 9) |

### Rust (`crates/rustmail-core`)

- `domain/`: models for every table (`serde` + `sqlx::FromRow`), enums, and
  input validation (`normalize_domain_name`, `normalize_local_part`, `split_email`).
- `repository/`: 8 object-safe async traits (`Domain`, `User`, `Mailbox`,
  `Alias`, `Group`, `Session`, `AuditLog`, `Quota`) and their `Pg*` implementations
  using `sqlx::query!`/`query_as!`.
- `db.rs`: `connect()` and an embedded `MIGRATOR` (so the CLI can migrate without `sqlx-cli`).
- `password.rs`: Argon2id `hash_password` / `verify_password`, `generate_password`.
- `seed.rs`: `seed()`; `error.rs`: `Error` / `Result` (constraint violations map to `Conflict` / `Validation`).
- `rustmail-cli`: `migrate` and `seed` subcommands.
- `.sqlx/`: committed offline query cache (45 entries).

### Design decisions

- **UUID primary keys**, `TIMESTAMPTZ` everywhere; `BIGINT IDENTITY` for the high-volume `messages_meta` and `audit_log`.
- **User vs mailbox**: a user is a login identity; a mailbox is its storage (one per user, shared mailboxes deferred). `users.email` is denormalised for login lookups and built in SQL from the domain name; domains are never renamed, so it cannot drift.
- **Addresses and domains are stored lower-case** (app normalises, CHECK constraints back it up).
- **Secrets**: `User` and `Session` never serialise their hashes; `User`/`NewUser`/`SeedRequest` redact them from `Debug`. Refresh tokens are stored hashed only.
- **Audit log** keeps `actor_email` as a snapshot, so history survives user deletion (verified by a test).
- **Seed is idempotent** and never resets an existing password.

## Verification

| Check | Result |
|-------|--------|
| `sqlx migrate run` (sqlx-cli 0.8.6) on the dev DB | 7/7 migrations applied |
| `rustmail-cli migrate` run twice on a fresh DB | up to date both times |
| `rustmail-cli seed` (real binary) | created domain, `super_admin` with `$argon2id$` hash, mailbox row, audit entry |
| `seed` re-run | domain/admin/mailbox all "already existed"; password untouched |
| `seed` with a 5-char password / with no `DATABASE_URL` | rejected with a clear error |
| `cargo build --workspace` | pass |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass, 0 warnings |
| `cargo test --workspace --all-targets` (with `RUSTFLAGS=-D warnings`) | 47 passed, 0 failed |
| Offline build (`SQLX_OFFLINE=true`, `DATABASE_URL` pointing at a dead port) | pass, proving `.sqlx/` is used |

Tests (all against real PostgreSQL; each test gets its own freshly migrated DB via `#[sqlx::test]`):
17 unit + 15 repository + 8 schema + 6 seed in `rustmail-core`, plus 1 in `rustmail-imap`.

### Reproduce
```bash
cp .env.example .env
make dev && make migrate
make seed SEED_DOMAIN=example.com
make lint && make test
```

## Things to know

1. **Postgres host port is now 5433.** A Postgres already running on the Mac held
   `127.0.0.1:5432` and shadowed the container, so host clients (sqlx, psql) reached
   the wrong server. My Phase 0 checks ran `pg_isready` *inside* the container and did
   not catch it. `.env.example`, `docker-compose.dev.yml` and the Makefile now default to 5433.
2. **Dependencies added beyond the approved list** (`sqlx`, `uuid`, `chrono`, `async-trait`):
   `serde_json` (JSONB columns), `argon2` (so seeding never stores a plaintext password;
   Phase 2 builds the auth layer on it), `clap` (CLI subcommands), `dotenvy` (`.env` loading).
3. **sqlx is pinned to 0.8**, as approved. 0.9.0 is now current; the library and `sqlx-cli`
   must stay on the same minor version or the `.sqlx/` cache format can differ.
4. **No `config`/`figment` crate yet.** Phase 1 only needs `DATABASE_URL`, read via
   clap/`.env`. A config crate fits better when Phase 2/3 add JWT and bind-address settings.
5. **Migrations are forward-only** (no `.down.sql`).
6. Repositories exist only for aggregates used through Phase 2. Models and tables for
   `messages_meta`, Sieve, autoresponders, filters, calendar and contacts exist; their
   repositories arrive with Phases 6, 8 and 9.
7. The super-admin created by `seed` has a mailbox *row* only; the Maildir directory
   itself is created in Phase 4.

---

**Phase 1 complete. Say "Start Phase 2" to proceed.**
