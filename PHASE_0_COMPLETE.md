# Phase 0 — Project Kickoff & Environment Audit — COMPLETE

## Summary

The RustMail Suite repository skeleton is in place. The Cargo workspace builds,
formats, lints (warnings denied), and tests cleanly, and the dev infrastructure
stack starts and reports healthy. No future-phase code was scaffolded.

### Environment audit (host)
| Tool           | Version / status                     |
|----------------|--------------------------------------|
| rustc / cargo  | 1.98.0 (Homebrew; no rustup)         |
| Docker         | 29.0.2 + Compose v2.40.3             |
| Make           | GNU Make 3.81                        |
| Git            | 2.50.1                               |

> Rust is Homebrew-managed (no `rustup`), so `rust-toolchain.toml` is
> informational locally — it does not auto-install a toolchain. CI uses the
> `dtolnay/rust-toolchain@stable` action, which honors the channel.

## Deliverables

- **Cargo workspace** (`Cargo.toml`, resolver 2) with 5 members and shared
  `[workspace.dependencies]` (tokio, anyhow, thiserror 2, tracing,
  tracing-subscriber, serde):
  - `crates/rustmail-core` (lib) — `#![deny(warnings)]` + `#![forbid(unsafe_code)]`;
    exposes a documented `version()` fn; 1 unit test.
  - `crates/rustmail-imap` (lib) — depends on core; documented `version()` fn; 1 unit test.
  - `crates/rustmail-web` (bin) — async `#[tokio::main]` + tracing init (placeholder).
  - `crates/rustmail-admin` (bin) — async `#[tokio::main]` + tracing init (placeholder).
  - `crates/rustmail-cli` (bin) — async `#[tokio::main]` + tracing init (placeholder).
- **`docker-compose.dev.yml`** — Postgres 16 (`postgres:16`), Redis 7 (`redis:7`),
  **Mailpit** (`axllent/mailpit:latest`, chosen over MailHog per request), all with
  healthchecks and env-driven ports.
- **`Makefile`** — `help`, `dev`, `dev-down`, `dev-logs`, `migrate`, `build`,
  `check`, `test`, `lint`, `fmt`, `clean`.
- **`rust-toolchain.toml`** (channel `stable`, rustfmt + clippy),
  **`rustfmt.toml`** (max_width 100, edition 2021),
  **`clippy.toml`** (thresholds + `doc-valid-idents` only).
- **`README.md`** with architecture diagram (plus `ARCHITECTURE.md`,
  `ROADMAP.md`, `PROJECT_STRUCTURE.md` blueprint docs).
- **`.env.example`** with DB / Redis / Mailpit-SMTP / web+admin bind / JWT / logging vars.
- **`.github/workflows/ci.yml`** — fmt → clippy (`-D warnings`) → test, with cargo caching.
- **`.gitignore`** (ignores `.env`, keeps `.env.example`).

## Verification — results

| Check                        | Command                                                 | Result |
|------------------------------|---------------------------------------------------------|--------|
| Workspace builds (clean)     | `cargo build --workspace`                               | ✅ pass (exit 0) |
| Formatting                   | `cargo fmt --all -- --check`                            | ✅ pass |
| Lint (warnings denied)       | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ pass (no warnings) |
| Tests                        | `cargo test --workspace --all-targets`                  | ✅ 2 passed, 0 failed |
| `make dev` starts services   | `make dev` → `docker compose ps`                        | ✅ postgres **(healthy)**, redis **(healthy)**, mailpit **(healthy)** |
| Postgres reachable           | `pg_isready -U rustmail -d rustmail`                    | ✅ accepting connections |
| Redis reachable              | `redis-cli ping`                                        | ✅ PONG |
| `make dev-down` tears down   | `make dev-down`                                         | ✅ containers + network removed |

### Reproduce
```bash
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets

# Dev infra (requires the Docker daemon running):
cp .env.example .env
make dev        # start Postgres 16, Redis 7, Mailpit
make dev-down   # stop and remove
# Mailpit UI: http://localhost:8025
```

## Known caveat
- **`rust-toolchain.toml` is informational locally** because the host uses
  Homebrew-managed Rust (no `rustup`). It is honored in CI via
  `dtolnay/rust-toolchain@stable`. No action required.

## Not done (by design)
- No DB schema/migrations, domain models, auth, HTTP servers, templates, or
  daemon configs — those belong to Phases 1+.

---

**Phase 0 complete. Say "Start Phase 1" to proceed.**
