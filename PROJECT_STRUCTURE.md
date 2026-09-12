# RustMail Suite — Project Structure

This document describes the repository layout. Directories marked _(Phase N)_
are placeholders that get populated when that phase runs. Empty structural
directories currently hold a `.gitkeep` file so the layout is preserved in git.

```
rust_mail_suite/
├── ARCHITECTURE.md              # System blueprint, diagrams, tech decisions
├── ROADMAP.md                   # 13-phase execution plan + deliverables
├── README.md                    # Overview, stack, quick start
├── PROJECT_STRUCTURE.md         # This file
│
├── Cargo.toml                   # Workspace manifest            (Phase 0)
├── rust-toolchain.toml          # Pinned Rust toolchain          (Phase 0)
├── rustfmt.toml                 # Formatting rules               (Phase 0)
├── clippy.toml                  # Lint config                    (Phase 0)
├── Makefile                     # dev / migrate / test / lint    (Phase 0)
├── docker-compose.dev.yml       # Postgres 16, Redis 7, MailHog  (Phase 0)
├── .env.example                 # Required env vars              (Phase 0)
├── .gitignore                                                     # (Phase 0)
│
├── crates/                      # Cargo workspace members
│   ├── rustmail-core/           # (lib) domain, repos, auth, config, errors
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── domain/          # Entities + FromRow models       (Phase 1)
│   │   │   ├── repository/      # Repo traits + Postgres impls     (Phase 1)
│   │   │   └── auth/            # AuthService, tokens, hashing      (Phase 2)
│   │   └── tests/               # Integration tests
│   │
│   ├── rustmail-web/            # (bin) webmail + public UI + WebSocket
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   ├── templates/          # askama templates (Bootstrap/HTMX) (Phase 3)
│   │   ├── static/             # CSS/JS/assets served by tower-http (Phase 3)
│   │   └── tests/
│   │
│   ├── rustmail-admin/          # (bin) admin console
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   └── tests/
│   │
│   ├── rustmail-imap/           # (lib) async IMAP client wrapper
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   └── tests/
│   │
│   └── rustmail-cli/            # (bin) operator tooling (seed/migrate/provision)
│       ├── Cargo.toml
│       ├── src/
│       └── tests/
│
├── migrations/                  # SQLx migrations                (Phase 1)
│
├── deploy/                      # Deployment artifacts / daemon configs
│   ├── postfix/                 # Postfix templated configs      (Phase 4/5)
│   ├── dovecot/                 # Dovecot configs (SQL auth, LMTP, Sieve, quota) (Phase 4)
│   ├── amavis/                  # Amavis config                   (Phase 5)
│   ├── grafana/                 # Grafana dashboards JSON          (Phase 10)
│   └── ansible/                 # Ansible playbook (Ubuntu/Debian) (Phase 11)
│
├── scripts/                     # Ops scripts (setup-mta, backup, restore)
│
├── docs/                        # mdBook documentation site       (Phase 11)
│   └── src/
│
└── .github/
    └── workflows/               # CI: fmt, clippy, test           (Phase 0)
```

## Conventions

- **`rustmail-core` is the only shared library.** Binaries (`web`, `admin`,
  `cli`) and the `imap` lib depend on it; nothing depends on the binaries.
- **Message bodies** live in Maildir (managed by Dovecot). Postgres stores only
  metadata/index (`messages_meta`).
- **Deployment-targeted configs** are templated with `.j2` (Ansible) where the
  value depends on the target host; static dev configs stay plain.
- **`.gitkeep`** files are placeholders only; they are removed as real files are
  added to a directory during its phase.

## Cross-references

- Blueprint & data flow: [`ARCHITECTURE.md`](./ARCHITECTURE.md)
- Phase plan & deliverables: [`ROADMAP.md`](./ROADMAP.md)
