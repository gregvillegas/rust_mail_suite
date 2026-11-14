# Phase 5 — MTA Pipeline (Postfix + Amavis + SpamAssassin + ClamAV): complete

## What was built

**Filtering (`deploy/`, `scripts/setup-mta.sh`)**
- Postfix sends every message through Amavis (`content_filter`, port 10024) and takes it back on a re-entry listener (10025) that has no filter, so nothing is scanned twice. The listener only answers the clients you list in `MTA_REINJECT_CLIENTS`.
- `deploy/amavis/60-rustmail.j2` configures Amavis: ClamAV through its socket, SpamAssassin in library mode, silent discard of infected and very spammy mail (no bounce to forged senders), flat quarantine directory, and a per-recipient policy lookup in PostgreSQL.
- `filter_policies` (migration `0010`) holds thresholds per server (`@.`), domain (`@example.com`) or recipient. The most specific row wins and a NULL setting inherits. Defaults: tag 2, spam 6.2, quarantine 12. Amavis reads it through its own database role, which can read nothing else.
- Mail between the "spam" and "quarantine" scores is delivered with `X-Spam-Flag: YES`, and Dovecot's Sieve files it in Junk.
- Learning: moving a message into Junk, or out of it (except to Trash), runs `sa-learn` through Dovecot's imapsieve. Amavis and the mail user share the Bayes directory through the vmail group.
- ClamAV ships with a built-in test signature for EICAR, so the stack works offline. The real signature database comes from `freshclam` (enabled on systemd hosts; opt in for the dev container with `CLAMAV_UPDATE=1`).
- `setup-mta.sh` now also installs the packages, creates the quarantine and Bayes directories, groups and `/etc/mailname`, creates the third database role, compiles the Sieve scripts, and enables and restarts Amavis and ClamAV on systemd hosts.

**Observability (`rustmail-core::pipeline`, `rustmail-cli pipeline`)**
- A parser (written against real Postfix 3.7 and Amavis 2.13 output) recognises Amavis verdicts, Postfix delivery results and SMTP refusals. It stores parsed fields only: no raw lines, no client addresses, no message content.
- The follower reads logs in bounded batches, survives restarts and rotation, and commits events together with its new position in one transaction. Re-reading never duplicates.
- `pipeline ingest | follow | recent | prune`, `quarantine list | release | delete | purge`, `policy list | set | remove`.

**Quarantine UI (`/admin/quarantine`, super admin only)**
- List with counts, filters (status, type, sender/recipient text) and paging; detail page with headers and a short plain-text excerpt; Release and Delete with a confirmation (the release warning is stronger for viruses). Everything is escaped, nothing inline, translated en/de.
- Release sends the original to the one recipient it was held for, through the re-entry port, after removing the headers Amavis added. If the mail server refuses, the message stays held. Both actions go to the audit log.
- The quarantine file is deleted only when no other recipient still needs it. File names from the database are treated as bare names and symlinks are refused.

## Verify it yourself

```bash
make dev && make migrate
make mta-up                       # rebuilds the image (new packages), starts Postfix, Dovecot, Amavis, ClamAV
make mta-check                    # template render test, no containers needed
make mta-smoke                    # 67 checks, about 7 minutes

# By hand:
export VMAIL_BASE=$PWD/var/vhosts QUARANTINE_DIR=$PWD/var/quarantine
cargo run -q -p rustmail-cli -- domain create example.test
cargo run -q -p rustmail-cli -- user create alice@example.test --password 'a-long-password-1'
# EICAR
docker exec rustmail-mta swaks --server 127.0.0.1 --from x@example.org --to alice@example.test \
  --body 'X5O!P%@AP[4\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*'
cargo run -q -p rustmail-cli -- pipeline ingest --log var/log/amavis.log,var/log/postfix.log --log-timezone utc
cargo run -q -p rustmail-cli -- quarantine list
# Web UI: make run-web, sign in as a super admin, Administration → Quarantine
```

## What was verified

- **End to end against Postfix 3.7.11, Dovecot 2.3.19.1, Amavis 2.13.0, SpamAssassin 4.0.1, ClamAV 1.4.6 (Debian 12)** with `scripts/mta-smoke.sh`:
  - EICAR is not delivered and is indexed as a virus with its name.
  - A GTUBE spam sample under the default policy is not delivered, not in Junk, and is indexed as spam.
  - The same GTUBE for a recipient whose policy raises the quarantine level is delivered, tagged, and filed in Junk. This also proves the policy lookup in PostgreSQL works per recipient.
  - A legitimate message is delivered.
  - Events are stored for passed, blocked, SMTP-refused and delivered messages; a second ingest pass stores nothing.
  - Release from the CLI delivers the original through the re-entry port; delete removes the file; both are in the audit log; a second release and a non-admin are refused.
  - Moving a message to Junk raised SpamAssassin's spam count.
  - The earlier Phase 4 checks still pass (messages now pass through the filters, so the quota check waits for delivery).
- **Full gate** (`RUSTFLAGS=-D warnings`, offline query cache): fmt, build, clippy and 280 tests pass (171 before Phase 4, 208 after it), plus the template render test and the query-cache check. New tests: parser (real log lines), follower (partial lines, rotation, endless lines), store (transactions, duplicates, unsafe names), quarantine service (release, refusal, symlinks, multi-recipient files, retention, audit), SMTP client (including command-injection refusals), policy repository, and 12 HTTP tests for the pages (access control, CSRF, escaping, paging, language, actions).
- **Real browser** (`make browser-test`): 97 checks pass (81 existing plus 16 in the new scenario E): list, filter, detail, cancelled and confirmed release, delete, and refusal for domain admins and users. This found a real bug the Rust tests could not: htmx tries to copy `style` attributes when it swaps content, which our CSP blocks and logs. Fixed by excluding `style` from htmx's `attributesToSettle`, with a regression test.

## Things you should know

- **ClamAV runs in the same container or host as Amavis, not as a separate service.** I proposed a separate compose service. It does not work well here: Amavis hands clamd a file path, so both must see the same files (shared volumes, matching user ids). Set `AMAVIS_BYPASS_VIRUS_CHECKS=1` on machines too small for ClamAV.
- **Only a built-in EICAR signature is loaded in the dev container by default.** The full signature database (~300 MB, needs internet) was **not** downloaded or tested. Set `CLAMAV_UPDATE=1` to try it; ClamAV then needs about 1 GB of RAM.
- **"Spam sample → tagged".** The standard GTUBE test string scores 1000, far above the quarantine level, so by default it is held, not tagged. The tagged path (score between 6.2 and 12) was verified with GTUBE under a raised quarantine level, not with a naturally mid-scored message.
- **Network spam tests are off in the dev container** (`SPAMASSASSIN_SKIP_NETWORK=1`), so DNS blocklists were not exercised. Rule updates (`sa-update`) are not scheduled; Debian ships rules with the package and they age.
- **Released spam goes to Junk**, because it still carries `X-Spam-Flag`. A released virus is delivered as is, after a stronger confirmation.
- **Fail closed:** when Amavis cannot reach the database or Postfix's re-entry port, Postfix defers the mail (451) and retries instead of delivering it unchecked. I saw this happen twice while configuring.
- **The systemd path of `setup-mta.sh` is untested** (enable and restart of Amavis and ClamAV on a real host). The package list and configuration were verified in the Debian 12 container only.
- **Log handling limits:** syslog lines carry no year or time zone, so you tell the follower the zone (`--log-timezone`, default local) and it assumes the most recent past year. After a rotation, lines written to the old file but not yet read are lost; keep `pipeline follow` running. Unix only.
- **No scheduler yet:** `pipeline prune` and `quarantine purge` are manual. Run them from cron until a later phase.
- **Super admin only.** Domain admins get "Access denied" until Phase 7 adds per-domain scoping.
- The German translation is a draft, as before.
- New dependency: `regex` (already in the lock file via other crates, resolving to 1.13.1). Core also enables tokio's `fs`, `io-util`, `net` and `time` features. The SMTP client for release is hand-written (about 150 lines) to avoid adding a mail crate.
- New Debian packages: `amavisd-new spamassassin clamav-daemon clamav-freshclam libdbd-pg-perl file unzip bzip2`. Archive unpackers beyond zip, gzip and bzip2 are not installed, so Amavis logs "No decoder" lines at start-up for formats such as rar and 7z. Amavis then cannot look inside those archives itself (this matters for banned-file-type checks); ClamAV still unpacks the formats it supports natively.
- Nothing is committed. Phases 1 to 5 are uncommitted in the working tree.

## Next

Phase 6 — Webmail client (inbox, compose, folders).

Ready for Phase 6? Say "Start Phase 6".
