# Phase 8 complete: Filters, Autoresponders, Sieve

Users build mail rules and an out-of-office reply in the web UI. RustMail turns them into Sieve
scripts and installs them on Dovecot over ManageSieve, so mail is sorted and answered while it
is delivered.

## What was built

| Area | Where |
|------|-------|
| Rule model, validation, Sieve generator, fallback engine, store | `rustmail_core::filters` (`mod`, `sieve`, `engine`, `store`) |
| ManageSieve client (new crate) | `crates/rustmail-sieve` (`protocol`, `client`, `error`, `install`) |
| Pages | `rustmail-web`: `filter_pages.rs`, templates in `templates/settings/`, `static/js/filters.js` |
| Editor | CodeMirror 6 built from pinned npm packages: `scripts/vendor-codemirror/` → `static/vendor/codemirror/6/` |
| Dovecot | `deploy/dovecot/dovecot.conf.j2`: script size, quota, redirect and vacation-period limits |
| Database | migration `0012`: shapes the Phase 1 tables `mail_filters`, `sieve_scripts`, `autoresponders`; adds `sieve_sync_state` |
| Tests | core: 19 unit + 5 store; sieve: 14 unit + 2 live; web: 7 integration; `scripts/browser-smoke/filters.mjs` (46 checks) |

How it works:

- **Pages** (account menu → Filters / Autoresponder): `/settings/filters`, `/settings/filters/rule`,
  `/settings/filters/advanced`, `/settings/autoresponder`. Every signed-in user manages only their own.
- **Rule builder:** name, "all/any" of up to 3 conditions (sender, to, cc, subject, list-id; contains, is, starts with, ends with),
  then move to a folder (existing or new), mark read, send a copy, delete, stop. Rules can be disabled, reordered and deleted.
- **Three scripts per user** (`rustmail` active, `rustmail-filters`, `rustmail-vacation`), so the builder, the editor and
  the autoresponder never overwrite each other.
- **Advanced editor:** your own Sieve script, checked by Dovecot (`CHECKSCRIPT`) before it is saved; a rejected script is shown
  with the server's line-numbered explanation and is not stored. "Go back to the rule builder" keeps both.
- **Autoresponder:** subject, body, 1 to 30 days between replies to the same sender, optional first and last day. Answers only
  mail addressed to the user's login or an enabled alias.
- **Fallback:** if Dovecot cannot be reached the change is saved, the page says "Not active yet" with the reason, and offers
  "Send again". "Apply to inbox" runs the rules once over the 500 newest inbox messages (move and mark read only).

Security:

- All user text in a script goes through one quoting function; validation rejects control characters. A test tries to inject
  `"; fileinto "evil` through a rule and shows it stays inside the string.
- STARTTLS is mandatory before credentials are sent; login is `<user>*<master>`, like webmail. Script names are restricted to
  `[A-Za-z0-9._-]`, every command has a timeout, and the parser bounds line and literal sizes.
- The editor mounts in a shadow root so the strict `style-src 'self'` policy is unchanged (see Caveats for the build shim).
- Forwarding (`redirect :copy`) is limited by `sieve_max_redirects = 4`.

## Verify it yourself

```bash
make dev && make mta-up                  # Postgres, Redis, Mailpit; Postfix/Dovecot/Amavis
cargo test --workspace                   # needs DATABASE_URL
make filters-smoke                       # the phase verification, in a real browser (~5 min)
make sieve-live USER=alice@example.test  # ManageSieve against the dev Dovecot (user must exist)
```

By hand: run `make run-web` with `MAIL_MASTER_PASSWORD` set, sign in, open **Filters**, add the rule
"Sender contains boss@ → move to Boss", then send a message from `boss@…` through Postfix: it appears in the Boss folder.

## What was verified

| Check | Result |
|-------|--------|
| `npm run filters` (new, real browser, real Postfix/Dovecot/Amavis/Mailpit) | 46 / 46 |
| Gate: fmt, build, clippy `-D warnings`, template render test, `.sqlx` check | pass |
| `cargo test --workspace` | 454 passed, 0 failed |
| `npm run admin` (Phase 7) | 69 / 69 |
| `npm test` (earlier phases) | 97 / 97 |
| `npm run webmail` | 79 / 79 |
| `scripts/mta-smoke.sh` (includes the Dovecot quota refusal) | 67 / 67 |
| ManageSieve live tests against the dev Dovecot (`make sieve-live`) | 2 / 2 |

The browser test proves: the rule is installed on Dovecot; a message from `boss@` lands in the Boss folder, marked read, and
another message stays in the inbox; CodeMirror loads from the local bundle with no CSP errors; a script Dovecot rejects is
refused with its explanation; a valid script routes an invoice to its folder; going back to the builder restores the rules;
the autoresponder replies (seen in Mailpit) and does not reply before a future start date; and with ManageSieve unreachable the
page reports it, "Send again" reports the failure, and "Apply to inbox" moves the message with the fallback engine.

## Caveats

- **Vendored build shim.** CodeMirror sets some styles with `style.cssText = …`, which the CSP blocks. `build.mjs` rewrites those
  six assignments to set properties one by one and fails the build if upstream changes the count. The bundle is built, not a
  published file; rebuild with `cd scripts/vendor-codemirror && npm ci && npm run build`.
- The editor keeps a light background in dark mode (its syntax colours are chosen for light).
- Vacation dates follow the **mail server's** clock and time zone. The fallback engine cannot see `List-Id`, and skips delete and
  forward on purpose.
- Rules apply to **new** mail; "Apply to inbox" covers existing mail only through the fallback and only the newest 500.
- Forwarding to any address is allowed (up to 4 per message). An operator who wants to restrict it should lower `sieve_max_redirects`
  or remove the action; there is no per-domain policy yet.
- Validation and server messages are English in the German UI; the German text is a draft.
- **Schema change to Phase 1 tables.** Migration `0012` replaces `autoresponders.starts_at/ends_at` (timestamps) with `starts_on/ends_on`
  (dates), because Sieve compares days. The unused Phase 1 model and its schema test were updated; no data existed.
- Two Phase 7 test fragilities showed up and were fixed: `admin.mjs` read the whole `docker logs` (over `spawnSync`'s buffer once the
  container had logged a lot) and counted audit rows from earlier aborted runs. It now reads the last 10 minutes and scopes to its run.
  A Phase 7 query (`admin/quota.rs`) now states its column nullability explicitly instead of relying on inference, which changed with the
  query plan.
- Nothing is committed yet (Phases 1 to 8).

Phase 8 complete. Say 'Start Phase 9' to proceed.
