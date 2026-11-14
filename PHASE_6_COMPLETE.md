# Phase 6 — Webmail Client (Inbox, Compose, Folders): complete

## What was built

**Mail client library (`rustmail-imap`)**
- `MailSession`: an IMAP session acting for one user: folders with counts, paged message lists, fetching, flags, move, delete (Trash first, then for good), append, folder create/rename/delete (subfolders included), IDLE. Folder names use IMAP's modified UTF-7, so `Entwürfe` and `Q&A` work.
- `message`: parsing for display, attachment extraction with safe file names, and HTML sanitising with `ammonia`.
- `compose`: building outgoing mail with `mail-builder` (reply, reply-all, forward helpers, address validation, size and count limits, no header injection).
- `submit`: authenticated submission through Postfix on port 587 (STARTTLS, then SASL).
- `sync`: fills the search index from IMAP incrementally, per folder, restarting a folder if its UIDVALIDITY changed.
- `tls`: rustls with the `ring` provider; public roots plus an optional CA file, or an explicit insecure mode for development.

**No user passwords.** The web server logs in to Dovecot and Postfix as `<user>*<master>` (Dovecot's master-user feature). Dovecot only accepts that from the networks in `MAIL_MASTER_ALLOW_NETS`. Postfix then applies the user's own rules: the sender must be their address, and outgoing mail is scanned.

**Web (`/mail`)**
- Three-pane inbox (folders, list, preview) that works on narrow screens by stacking. Open, reply, reply all, forward, delete, spam / not spam, star, mark unread, move.
- Compose with Cc/Bcc and multiple attachments (up to 25 MB total). The message is sent through Postfix and a copy is saved in Sent; replying marks the original as answered.
- Folder management page.
- Search: PostgreSQL full-text over `messages_meta` (subject, addresses, snippet). Indexing runs in the background as people use their mailbox, and `rustmail-cli mail index <user>` runs it on demand.
- New mail arrives over a WebSocket backed by IMAP IDLE; the list also refreshes every minute.
- Keyboard shortcuts (Alpine CSP build): `j k o c r a f # s ! / g i ?`.
- English and German.

**Display safety.** HTML mail is sanitised, served as its own document with `Content-Security-Policy: sandbox; default-src 'none'`, and shown in an `<iframe sandbox>` without scripts or same-origin access. Remote pictures stay hidden until the user asks. Attachments are always downloads with a sandbox CSP and a short allow-list of content types.

**Operations.** `deploy/dovecot/master-users.j2`, new settings in `deploy/mta.env.example`, `setup-mta.sh` validation, `make webmail-smoke`, `make mail-index`.

## Verify it yourself

```bash
make dev && make migrate
make mta-up                       # the mail stack (rebuild not needed; config is re-rendered at start-up)
make run-web                      # .env.example already holds the dev webmail settings
# create a user and open http://127.0.0.1:8080/mail
cargo run -q -p rustmail-cli -- domain create example.test
cargo run -q -p rustmail-cli -- user create alice@example.test --password 'a-long-password-1'

make webmail-smoke                # 79 checks in a real browser against the real mail stack, about 7 minutes
# Library tests against the real servers:
MAIL_LIVE_USER=alice@example.test MAIL_LIVE_PEER=bob@example.test IMAP_PORT=1993 SMTP_PORT=5587 \
  MAIL_TLS_INSECURE=1 MAIL_MASTER_PASSWORD=dev-webmail-master-password \
  cargo test -p rustmail-imap --test live -- --ignored --test-threads=1
```

## What was verified

- `make webmail-smoke` (real Chrome, real Dovecot/Postfix/Amavis/Mailpit): sign-in → inbox → open → reply → send → the recipient receives it (an outside address via Mailpit; a second local user in their own browser, with attachment download). Also: HTML mail with a script, an `onerror` handler, a `javascript:` link and an iframe (none of them run or survive; remote picture blocked then allowed on request), attachment download, search by word and by address (and a hostile search string), Cc/Bcc fields, validation errors that keep the form, header-injection attempts refused by the server, folders with spaces/umlauts/`&` (create, move, rename, delete, duplicates and `..` refused), spam and not-spam, delete to Trash then for good, keyboard shortcuts, live arrival of new mail over a WebSocket, German, WebSocket refused for another site's `Origin` and for none, and no console or CSP errors.
- `rustmail-imap` live tests against the dev stack: folder and message operations, master-login failures, sending with an attachment and a Sent copy, Postfix refusing a spoofed sender, IDLE waking on new mail.
- Unit and integration tests: UTF-7 (including the RFC 3501 examples), the sanitiser against about twenty hostile inputs, address and header handling, message building and parsing round trip, index and search (including that mailboxes cannot see each other's mail), WebSocket origin check, per-user socket and send limits, access control and CSRF on every route, the "not configured" page.
- Full gate (`RUSTFLAGS=-D warnings`, offline query cache): fmt, build, clippy and 351 tests pass (280 before this phase), plus the template render test and the query-cache check. The Phase 4/5 mail smoke test still passes 67/67 with the master login added, and the earlier browser suite 97/97.
- Found by the browser test and fixed: htmx's link boosting swallowed attachment downloads (`hx-boost="false"` on download links), deleting a folder left its subfolders behind, and the keyboard handler relied on a highlight class that can lag behind a swap.

## Things you should know

- **Sending goes through Postfix on 587, not a raw LMTP inject** (as agreed). Local recipients are therefore scanned like any other mail, and external recipients work.
- **No temp-upload step.** Attachments are posted together with the message and held in memory for the request (limit about 36 MB). Nothing is stored on the web server.
- **New dependencies:** `async-imap` 0.12, `mail-parser` 1.0, `mail-builder` 2.0, `ammonia` 4.2 (and its HTML parser), `tokio-rustls` 0.26 (the `ring` provider already in the tree) and `webpki-roots` (already in the tree). axum gained its `multipart` and `ws` features, which add `multer` and `tokio-tungstenite`.
- **The master password** sits in plain text in `/etc/dovecot/master-users` (mode 0640, root:dovecot) and in both env files. Use a long random value and keep `MAIL_MASTER_ALLOW_NETS` as narrow as you can. In development it allows Docker's private ranges.
- **TLS to the mail server** is verified in production (public roots, or `MAIL_TLS_CA_FILE`, with `MAIL_TLS_SERVER_NAME` if you connect by IP). `MAIL_TLS_INSECURE=1` accepts any certificate and is for the self-signed development container only.
- **Search is as good as the index.** It covers what has been indexed (snippets only for messages up to 64 KB), refreshes at most every 90 seconds per mailbox, and can return a stale hit if another client moved or deleted a message; opening it then says it is gone. A large mailbox needs a few visits, or `make mail-index`, before everything is searchable.
- **Display limits.** Dates are shown in UTC. Newsletters look plain because their styles are removed. Pictures embedded with `cid:` are not shown. Forwarding includes the text but not the original's attachments. There is no drafts autosave, signatures, contacts or message threading yet.
- **One IMAP connection per request** (no pooling), plus one per open mail tab for IDLE (at most 5 per user and 500 in total). That suits small and medium installations; a pool is the next step if it ever shows.
- **Sending is rate-limited** to 30 messages per 10 minutes per user, in memory per process, so a stolen password is not a free spam relay.
- **Validation messages for folders and addresses are in English** even in the German UI; they come from the mail library.
- **Mobile layout** stacks the panes but was only tested at desktop sizes.
- The German translation is a draft, as before.
- Nothing is committed. Phases 1 to 6 are uncommitted in the working tree.

## Next

Phase 7 — Admin console (domain, user and quota management).

Ready for Phase 7? Say "Start Phase 7".
