# Phase 4 — Mailbox Provisioning & Dovecot Integration: complete

## What was built

**Rust (`rustmail-core::provisioning`, `rustmail-cli`)**
- `MailboxProvisioner` creates `<VMAIL_BASE>/<domain>/<user>/Maildir/{cur,new,tmp}`. It never changes ownership, sets exact modes (umask-proof), and refuses `..`, absolute paths, NUL/backslash/control characters, symlinks and files in the way.
- `AccountService` creates domains and users: database rows first, then disk. If the disk step fails the rows are removed again. It enforces `max_mailboxes`, active domain and a 12-character minimum password.
- CLI: `domain create`, `user create` (prints a generated password once if none given), `mailbox check [--repair]`; `seed` now also creates the admin's Maildir.
- Migration `0009`: `quota_usage` gets a unique lower-case `username` so Dovecot can address rows by name. A trigger fills in `mailbox_id`; clamp triggers replace the `CHECK (>= 0)` constraints because Dovecot's decrements (`INSERT ... VALUES (-n) ON CONFLICT`) would be rejected by them.

**Deployment (`deploy/`, `scripts/`)**
- Postfix: `main.cf.j2`, `master.cf.j2`, five `pgsql/*.cf.j2` lookups (domains, mailboxes, aliases, groups, sender-login). Unknown recipients get `550 5.1.1` during SMTP.
- Dovecot 2.3: Postgres auth (Argon2id), LMTP socket, SASL socket for Postfix, ManageSieve (4190), quota via SQL dict into `quota_usage`.
- Submission (587) needs STARTTLS + SASL and refuses sender addresses the login does not own. Port 25 offers no AUTH and does not relay.
- `deploy/postgres/mta-roles.sql`: two least-privilege roles. Postfix has column-level SELECT and cannot read `password_hash`. Dovecot can read what it needs and maintain `quota_usage`.
- `scripts/setup-mta.sh`: idempotent. Installs packages, creates the vmail user and base directory, creates the roles (credentials via stdin and `PG*` variables, not argv), renders templates, backs up changed files, runs `postfix check` and `doveconf -n`, restores the backup on failure, reloads only when something changed. Flags: `--env-file --packages-only --skip-packages --skip-db --no-reload --container --check --render-to`.
- `scripts/test-templates.sh` (also in CI): renders every template and fails on undefined variables or leftover syntax.
- Dev container (`make mta-up`): Debian 12, compose profile `mta`, loopback-only ports, relays outbound mail to Mailpit.

## Verify it yourself

```bash
make dev                              # Postgres, Redis, Mailpit
make migrate
make mta-up                           # builds and starts Postfix + Dovecot

make mta-check                        # template render test (no containers needed)
make mta-smoke                        # 34 end-to-end checks, about 2 minutes

# By hand:
export VMAIL_BASE=$PWD/var/vhosts
cargo run -q -p rustmail-cli -- domain create example.test
cargo run -q -p rustmail-cli -- user create alice@example.test --password 'a-long-password-1'
docker exec rustmail-mta swaks --server 127.0.0.1 --from me@example.org --to alice@example.test
python3 - <<'PY'
import imaplib, ssl
c = ssl.create_default_context(); c.check_hostname = False; c.verify_mode = ssl.CERT_NONE
m = imaplib.IMAP4_SSL("127.0.0.1", 1993, ssl_context=c)
m.login("alice@example.test", "a-long-password-1"); m.select("INBOX")
print(m.search(None, "ALL"))
PY
```

## What was verified

- Ran against the real stack (Debian 12: Postfix 3.7.11, Dovecot 2.3.19.1, PostgreSQL 16, Mailpit). `scripts/mta-smoke.sh`: 34/34 checks passed, covering:
  - domain/user creation and Maildir on disk, `mailbox check`
  - IMAP login with right, wrong and differently-cased credentials
  - swaks delivery that appears in IMAP and on disk
  - `550 5.1.1` for unknown recipients, relay refused for outside domains
  - alias and group delivery
  - suspended users: mail refused and login refused
  - submission with STARTTLS + SASL relayed to Mailpit, sender spoofing refused (`553`), bad SASL password rejected
  - quota counters written to `quota_usage`, decremented on delete, over-quota delivery refused (`552 5.2.2`)
  - ManageSieve greeting with STARTTLS
  - a second `setup-mta.sh` run changes nothing
- Full gate (`RUSTFLAGS=-D warnings`, `SQLX_OFFLINE=true`): `cargo fmt --check`, build, clippy (`-D warnings`), and 208 tests all pass, plus the template render test.
- Dovecot's SQL dictionary does use `INSERT ... ON CONFLICT (username) DO UPDATE`, confirmed in the PostgreSQL log. That is what the migration 0009 design relies on.

## Things you should know

- **User creation is CLI-only.** The brief says "create user via UI"; user management in the web UI is Phase 7. The same `AccountService` will back it.
- **Supported: Debian 12 only** (Dovecot 2.3). Dovecot 2.4 uses a different config format and is not supported. Other distributions were not tested.
- **TLS is Debian's self-signed "snakeoil" certificate.** Real certificates are Phase 10. Clients must skip verification until then.
- **Over-quota mail is bounced after acceptance** (Postfix gets the `552` from LMTP and generates a bounce) rather than refused in the SMTP session. Refusing it earlier needs recipient verification against quota; not done.
- **Dovecot errors on quota writes are silent in its log.** While debugging, a missing grant showed up only in the PostgreSQL log. `mta-roles.sql` now grants what is needed; if you change roles, check the PostgreSQL log first.
- **Dev uid mapping:** the container's `vmail` uses your host uid/gid (default 501/20) so files in `./var/vhosts` are yours. Override with `VMAIL_UID` / `VMAIL_GID` when building. Rancher Desktop only bind-mounts paths under `/Users`.
- **Production permissions:** base dir `vmail:vmail` mode `2770`, RustMail's user in group `vmail`, `VMAIL_DIR_MODE=0770`. The provisioner never chowns.
- Carried over from earlier phases: German translation is a draft; HSTS is Phase 10; DomainAdmin scoping is Phase 7; the login rate limiter is in-memory; auth auditing is best-effort.
- Packages installed by the setup script: `postfix postfix-pgsql dovecot-core dovecot-imapd dovecot-lmtpd dovecot-pgsql dovecot-sieve dovecot-managesieved ssl-cert postgresql-client perl ca-certificates`. The dev image adds `tini swaks procps`. Nothing new was added to the Rust dependency set.
- Nothing is committed. Phases 1 to 4 are uncommitted in the working tree.

## Next

Phase 5 — MTA pipeline: Postfix + Amavis + SpamAssassin + ClamAV.

Ready for Phase 5? Say "Start Phase 5".
