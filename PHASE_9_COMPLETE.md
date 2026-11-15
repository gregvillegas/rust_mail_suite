# Phase 9 complete: Calendar and Contacts

CalDAV and CardDAV served by the web server itself, a calendar and a contacts page, sharing
between colleagues, and free/busy. Apps such as Thunderbird sign in with an app password.

## What was built

| Area | Where |
|------|-------|
| Reading iCalendar and vCard, repeating events, building events and contacts | `rustmail_core::dav::{calendar, contact}` |
| Collections, objects, sync, sharing, free/busy, access rules | `rustmail_core::dav::store` and the SQL function `dav_access` |
| App passwords | `rustmail_core::dav::tokens` |
| The protocol (PROPFIND, REPORT, PUT, …) | `rustmail-web`: `dav_server.rs`, `dav_xml.rs` |
| Pages | `dav_pages.rs`; templates in `templates/{calendar,contacts,settings}/`; `static/js/calendar.js` |
| Calendar view | FullCalendar 6.1.22, built from pinned packages: `scripts/vendor-fullcalendar/` |
| Database | migration `0013`: `app_passwords`, `dav_collections`, `dav_objects`, `dav_tombstones`, `dav_shares`, `dav_access()` |
| Tests | core: 26 unit + 13 store; web: 14 unit + 15 protocol + 11 page tests; `scripts/browser-smoke/calendar.mjs` (69 checks) with the independent client in `scripts/dav-smoke/` |

New dependencies, all MIT or Apache and existing crates: `ical` 0.11 (iCalendar and vCard), `rrule` 0.14 (repeats),
`roxmltree` 0.21 (reads request XML), `chrono-tz` 0.10 (already in the tree through `rrule`). FullCalendar and
Preact are vendored static files, not Rust crates.

## How it works

- **Native server, not Radicale.** One login system, one permission model, one database; sharing and free/busy
  are the same code the protocol uses. The price is that the protocol is ours to get right, which is why it is
  tested against an independent client (below).
- **Addresses.** `https://host/dav/` is enough for most apps (discovery follows from there; `/.well-known/caldav`
  and `/.well-known/carddav` redirect to it). A single calendar is `/dav/calendars/<email>/<slug>/`, an address
  book `/dav/addressbooks/<email>/<slug>/`. The pages show these addresses.
- **Sign-in.** HTTP Basic with the user's email and an **app password** made on `/settings/apps`: 128 random bits,
  shown once, only a SHA-256 stored, revocable at once, audited. Failed attempts are limited per client address
  and per user (10 in 10 minutes, then HTTP 429). No cookies are accepted on `/dav`, so CSRF does not apply.
- **Stored as sent.** The iCalendar or vCard text a client uploads is stored and returned byte for byte. A few
  extracted columns exist only for queries.
- **Supported:** `OPTIONS PROPFIND REPORT GET HEAD PUT DELETE MKCALENDAR MKCOL PROPPATCH`; reports
  `calendar-query` (time range, UID), `calendar-multiget`, `addressbook-query`, `addressbook-multiget`,
  `sync-collection` (with deletions) and `free-busy-query`; ETags with `If-Match` / `If-None-Match`.
- **Sharing.** The owner shares a calendar or address book with one person or a group in the same domain,
  read-only or read/write. Shared collections appear in the colleague's page and in their app, under the owner's
  address and labelled with the owner. A group share counts for each member; the highest right wins. A
  collection a person may not see is "not found".
- **Free/busy.** Times only, never titles, for people in the asker's own domain, from the person's own calendars,
  skipping events marked free or cancelled. A web page and the CalDAV `free-busy-query`.
- **The page edits only what it can write back.** An event made by Thunderbird with a reminder, guests or a complex
  repeat is shown and can be deleted in the page, but cannot be saved from it (a forged save is refused, and a
  test checks the stored text is untouched). Contacts are edited by replacing only the lines the page owns, so
  photos and custom fields survive.
- **Times.** The edit form shows UTC without JavaScript; with it, times are shown and entered in the browser's time
  zone, which is sent along. A repeating event is written in that zone so it stays at the same wall-clock time
  across daylight saving.

## Verify it yourself

```bash
cargo test --workspace                        # needs DATABASE_URL
cd scripts/dav-smoke && python3 -m venv .venv && .venv/bin/pip install -r requirements.txt && cd ../..
make calendar-smoke                           # real browser + independent CalDAV client (~3 min)
```

**Thunderbird, by hand** (this is the roadmap's verification; I cannot drive Thunderbird from a script):

1. Run the server over HTTPS (or locally, `make run-web`, address `http://127.0.0.1:8080/dav/`).
2. In the web app open **App passwords**, create one named "Thunderbird", and copy it.
3. In Thunderbird: **File → New → Calendar → On the Network**, location `https://your-host/dav/`
   (or the calendar address shown on **Calendars and address books**), user name your email address,
   and the app password when asked. For contacts: **Address Book → New Address Book → Add CardDAV Address Book**,
   the same server address and login.
4. Create an event in the web app's **Calendar**: it appears in Thunderbird after its next refresh. Create one in
   Thunderbird: it appears in the web calendar.

## What was verified

| Check | Result |
|-------|--------|
| `calendar.mjs`: real browser, real server, and the Python `caldav` client | 69 / 69 |
| Protocol tests (`tests/dav_server.rs`) | 15 / 15 |
| Page tests (`tests/dav_pages.rs`) | 11 / 11 |
| Core tests (`tests/dav.rs`, unit) | 13 / 13, 26 / 26 |

| Gate: fmt, build, clippy `-D warnings`, template render test, `.sqlx` check | pass |
| `cargo test --workspace` | 537 passed, 0 failed (454 before this phase) |
| `npm run admin` (Phase 7), `npm run filters` (Phase 8) | 69 / 69, 46 / 46 |
| `npm test` (earlier phases), `npm run webmail` | 97 / 97, 79 / 79 |

What the independent client did against the real server: discovered the calendars from `/dav/`, created, found
and changed events with ETags, searched by time range (including a later occurrence of a weekly event and
nothing after it ends), synced incrementally, made and deleted a calendar with `MKCALENDAR`, asked for free/busy
(no titles), and as a colleague with a read-only share was refused when writing or deleting. In the browser:
an event made in the page arrived in the app at the right instant (Europe/Berlin to UTC), an event with a reminder and a
guest made by the app appeared in the page as read-only, a contact edited in the page kept its photo and
custom field, revoking an app password stopped it at once, and the German page and calendar toolbar rendered
with no console or CSP errors.

## Caveats

- **Not verified with Thunderbird itself**, only with the Python `caldav` client and raw protocol requests. The
  steps above are the manual check. Apple Calendar, DAVx5 and Outlook are untested.
- **Not supported:** scheduling (invitations and replies by email, `schedule-inbox`), reminders sent by the server,
  `COPY`, `MOVE`, `LOCK`, calendar subscriptions, and filters other than time range and UID on events (a client that
  asks for more gets everything and filters itself). A calendar-query on a task returns nothing for a time range.
- **Time zones.** A `TZID` that is not an IANA name (for example a Windows name) is read as UTC in the web view; the
  stored text is untouched, so the app still shows it correctly. Repeating events are limited to 2000 occurrences
  per event per query.
- Basic authentication sends the app password with every request: use HTTPS in production. Anyone who can read
  the traffic can read it; revoking is immediate.
- Deletions are remembered forever (`dav_tombstones`), so a very old sync token still works; there is no clean-up job yet.
- Free/busy is visible to everyone in the domain (no per-person opt-out yet).
- Vendored build steps: FullCalendar's stylesheet, icon font, its empty `<style>` element and Preact's `style.cssText`
  would each violate the strict CSP, so the build extracts or rewrites them and fails if the library changes
  (`scripts/vendor-fullcalendar/build.mjs`). The CodeMirror bundle is unchanged.
- The Python client needs `caldav` (see `scripts/dav-smoke/requirements.txt`); `make calendar-smoke` skips nothing silently
  and fails if it is missing.
- German text is a draft; validation messages from the core are English in the German UI.
- Nothing is committed (Phases 1 to 9).
- `mta-smoke.sh` was not rerun for this phase: nothing in the mail stack changed.

Phase 9 complete. Say 'Start Phase 10' to proceed.
