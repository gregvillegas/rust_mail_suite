# Phase 0 — Project Kickoff & Environment Audit — COMPLETE

## Summary

The RustMail Suite repository skeleton is in place and the Cargo workspace
builds, lints, formats, and tests cleanly. No future-phase code was scaffolded.

### Environment audit (host)
| Tool           | Version / status                     |
|----------------|--------------------------------------|
| rustc / cargo  | 1.98.0 (Homebrew; no rustup)         |
| Docker         | 29.0.2 + Compose v2.40.3             |
| Make           | GNU Make 3.81                        |
| Git            | 2.50.1                               |

> Note: Rust is Homebrew-managed (no `rustup`), so `rust-toolchain.toml` is
> informational locally — it does not auto-install a toolchain. CI uses the
> `dtolnay/rust-toolchain@stable` action, which honors it.

## Deliverables

- **Cargo workspace** (`Cargo.toml`) with 5 members and shared `[workspace.dependencies]`:
  - `crates/rustmail-core` (lib) — `#![deny(warnings)]`, `version()` + unit test
  - `crates/rustmail-imap` (lib) — depends on core; `version()` + unit test
  - `crates/rustmail-web` (bin) — tokio + tracing entrypoint
  - `crates/rustmail-admin` (bin) — tokio + tracing entrypoint
  - `crates/rustmail-cli` (bin) — sync tracing entrypoint
- **`docker-compose.dev.yml`** — Postgres 16, Redis 7, **Mailpit** (chosen over
  MailHog as the maintained successor), all with healthchecks.
- **`Makefile`** — `dev`, `down`, `logs`, `migrate`, `build`, `test`, `lint`,
  `fmt`, `fmt-check`, `clippy`, `clean`, `help`.
- **`rust-toolchain.toml`**, **`rustfmt.toml`**, **`clippy.toml`** (MSRV 1.98).
- **`README.md`** with architecture diagram (pre-existing blueprint docs updated
  Mailpit ← MailHog).
- **`.env.example`** with DB/Redis/SMTP/logging/JWT vars.
- **`.github/workflows/ci.yml`** — fmt → clippy (`-D warnings`) → test, with
  caching and concurrency control.
- **`.gitignore`** and initialized git repo.

## Verification — results

| Check                              | Command                                          | Result |
|------------------------------------|--------------------------------------------------|--------|
| Workspace builds                   | `cargo build --workspace`                        | ✅ pass |
| Formatting                         | `cargo fmt --all -- --check`                     | ✅ pass |
| Lint (warnings denied)             | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ pass |
| Tests                              | `cargo test --workspace`                         | ✅ 2 passed |
| Compose file valid                 | `docker compose -f docker-compose.dev.yml config -q` | ✅ valid |
| `make dev` starts Postgres + Redis | `make dev`                                       | ⚠️ not run — Docker daemon (Rancher Desktop) was not running on the host |

### Reproduce
```bash
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Dev infra (requires the Docker daemon running):
make dev      # start Postgres 16, Redis 7, Mailpit
make down     # stop
# Mailpit UI: http://localhost:8025
```

## Known caveat / action for you
- **`make dev` was not executed** because the Docker daemon (Rancher Desktop)
  is installed but was not running during verification. The compose file itself
  is validated. To confirm the second verification criterion, start Rancher
  Desktop, then run `make dev` and check `docker compose -f docker-compose.dev.yml ps`.

## Not done (by design)
- No DB schema/migrations, domain models, auth, HTTP servers, templates, or
  daemon configs — those belong to Phases 1+.

---

**Phase 0 complete. Say "Start Phase 1" to proceed.**
(Optional: start Rancher Desktop and run `make dev` first to tick the last box.)
