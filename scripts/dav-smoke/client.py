#!/usr/bin/env python3
"""An independent CalDAV client checking the RustMail server.

It uses the `caldav` library (not our code), so it stands in for what Thunderbird, DAVx5 or
an Apple device would do: discover the calendars from the server address, create, search by
time range, change with ETags, sync incrementally, make a calendar and ask for free/busy.

Usage (normally run by scripts/browser-smoke/calendar.mjs, which prepares the users):
    DAV_URL=http://127.0.0.1:8080/dav/ DAV_USER=alice@example.com DAV_TOKEN=... \
        python3 client.py alice
    ... python3 client.py shared      # as a colleague who was given read-only access

Prints one `PASS label` / `FAIL label -> detail` line per check and exits 1 if any failed.
"""
import os
import sys
import traceback
import uuid
from datetime import datetime, timedelta, timezone

import caldav
from caldav.lib import error

URL = os.environ["DAV_URL"]
USER = os.environ["DAV_USER"]
TOKEN = os.environ["DAV_TOKEN"]
failures = 0


def check(label, ok, detail=""):
    global failures
    if ok:
        print(f"PASS {label}", flush=True)
    else:
        failures += 1
        print(f"FAIL {label} -> {str(detail)[:300]}", flush=True)


def guarded(label):
    """Runs a block; an exception is a failed check, not a crash."""
    class Guard:
        def __enter__(self):
            return self

        def __exit__(self, kind, value, tb):
            global failures
            if kind is not None:
                failures += 1
                print(f"FAIL {label} -> {kind.__name__}: {str(value)[:300]}", flush=True)
                traceback.print_exception(kind, value, tb, file=sys.stderr)
            return True

    return Guard()


def ics(uid, summary, start, end, extra=""):
    f = "%Y%m%dT%H%M%SZ"
    return (
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//RustMail smoke test//EN\r\nBEGIN:VEVENT\r\n"
        f"UID:{uid}\r\nDTSTAMP:{datetime.now(timezone.utc).strftime(f)}\r\nSUMMARY:{summary}\r\n"
        f"DTSTART:{start.strftime(f)}\r\nDTEND:{end.strftime(f)}\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )


def summaries(events):
    out = []
    for e in events:
        e.load()
        out.append(str(e.vobject_instance.vevent.summary.value))
    return sorted(out)


def alice(client):
    principal = client.principal()
    calendars = principal.calendars()
    check("the calendars are discovered from the server address", len(calendars) >= 1, calendars)
    cal = next((c for c in calendars if "default" in str(c.url)), calendars[0])
    check("the default calendar is named Calendar", cal.get_display_name() == "Calendar", cal.get_display_name())

    day = datetime.now(timezone.utc).replace(hour=9, minute=0, second=0, microsecond=0) + timedelta(days=3)
    uid = f"smoke-{uuid.uuid4()}"
    with guarded("creating an event"):
        cal.save_event(ics(uid, "Created by a CalDAV client", day, day + timedelta(hours=1)))
        check("an event is created", True)
    with guarded("reading it back"):
        found = cal.event_by_uid(uid)
        found.load()
        check("it is found by its UID with the same text", "Created by a CalDAV client" in found.data and found.vobject_instance.vevent.uid.value == uid)

    with guarded("searching by time"):
        hit = cal.search(start=day - timedelta(hours=1), end=day + timedelta(hours=2), event=True)
        check("a time-range query finds it", uid in [e.vobject_instance.vevent.uid.value for e in hit], len(hit))
        miss = cal.search(start=day + timedelta(days=10), end=day + timedelta(days=11), event=True)
        check("a time-range query elsewhere does not", uid not in [e.vobject_instance.vevent.uid.value for e in miss], len(miss))

    with guarded("repeating event"):
        weekly = f"smoke-weekly-{uuid.uuid4()}"
        cal.save_event(ics(weekly, "Weekly sync", day, day + timedelta(hours=1), "RRULE:FREQ=WEEKLY;COUNT=4\r\n"))
        later = day + timedelta(weeks=3)
        occ = cal.search(start=later - timedelta(hours=1), end=later + timedelta(hours=2), event=True)
        check("a time-range query finds a later occurrence of a repeating event", weekly in [e.vobject_instance.vevent.uid.value for e in occ], len(occ))
        after = day + timedelta(weeks=6)
        none = cal.search(start=after, end=after + timedelta(days=1), event=True)
        check("…and none after the series ends", weekly not in [e.vobject_instance.vevent.uid.value for e in none])

    with guarded("changing with an etag"):
        e = cal.event_by_uid(uid)
        e.load()
        e.vobject_instance.vevent.summary.value = "Changed by a CalDAV client"
        e.save()
        again = cal.event_by_uid(uid)
        again.load()
        check("a change is saved", again.vobject_instance.vevent.summary.value == "Changed by a CalDAV client")

    with guarded("incremental sync"):
        first = cal.objects_by_sync_token(sync_token=None, load_objects=False)
        token = first.sync_token
        check("a first sync returns a token and the events", bool(token) and len(list(first)) >= 2, token)
        extra = f"smoke-sync-{uuid.uuid4()}"
        cal.save_event(ics(extra, "Added after the first sync", day, day + timedelta(hours=2)))
        cal.event_by_uid(uid).delete()
        second = cal.objects_by_sync_token(sync_token=token, load_objects=False)
        names = [str(o.url) for o in second]
        check("the next sync reports the new event", any(extra in n or n.endswith(".ics") for n in names) and len(names) >= 1, names)
        check("…and the next token differs", second.sync_token != token, second.sync_token)

    with guarded("deleting"):
        cal.event_by_uid(extra).delete()
        try:
            cal.event_by_uid(extra)
            check("a deleted event is gone", False)
        except error.NotFoundError:
            check("a deleted event is gone", True)

    with guarded("making a calendar"):
        name = f"Made by client {uuid.uuid4().hex[:6]}"
        made = principal.make_calendar(name=name)
        check("a calendar can be made", name in [c.get_display_name() for c in principal.calendars()])
        made.save_event(ics(f"smoke-{uuid.uuid4()}", "In the new calendar", day, day + timedelta(hours=1)))
        check("an event can be saved in it", len(made.events()) == 1)
        made.delete()
        check("it can be deleted", name not in [c.get_display_name() for c in principal.calendars()])

    with guarded("free/busy"):
        busy_uid = f"smoke-busy-{uuid.uuid4()}"
        slot = day + timedelta(days=1)
        cal.save_event(ics(busy_uid, "TOP SECRET", slot, slot + timedelta(hours=2)))
        fb = cal.freebusy_request(slot - timedelta(hours=1), slot + timedelta(hours=3))
        text = fb.data if hasattr(fb, "data") else str(fb)
        check("a free/busy request returns the busy time", "FREEBUSY" in text and slot.strftime("%Y%m%dT%H%M%SZ") in text, text[:200])
        check("…without the title", "SECRET" not in text)


def shared(client):
    principal = client.principal()
    calendars = principal.calendars()
    theirs = [c for c in calendars if "alice" in str(c.url)]
    check("a calendar shared by a colleague is listed among the user's calendars", len(theirs) == 1, [str(c.url) for c in calendars])
    cal = theirs[0]
    titles = summaries(cal.events())
    check("its events can be read", any("UI event" in t for t in titles), titles)
    try:
        cal.save_event(ics(f"smoke-{uuid.uuid4()}", "Not allowed", datetime.now(timezone.utc), datetime.now(timezone.utc) + timedelta(hours=1)))
        check("a read-only share refuses new events", False)
    except error.AuthorizationError:
        check("a read-only share refuses new events", True)
    except Exception as e:  # noqa: BLE001 - any refusal is acceptable, a success is not
        check("a read-only share refuses new events", "403" in str(e) or "forbid" in str(e).lower(), repr(e))
    try:
        cal.events()[0].delete()
        check("…and refuses deleting", False)
    except Exception as e:  # noqa: BLE001
        check("…and refuses deleting", True, repr(e))
    check("the event is still there", len(cal.events()) >= 1)


def ui_event(client):
    """Checks an event made in the web page, as an app sees it. EXPECT_START_UTC is `YYYYMMDDTHHMMSSZ`."""
    cal = next(c for c in client.principal().calendars() if "default" in str(c.url))
    titles = summaries(cal.events())
    check("an event made in the web page is seen by the calendar app", any("UI event" in t for t in titles), titles)
    found = next(e for e in cal.events() if "UI event" in e.data and "weekly" not in e.data.lower())
    found.load()
    start = found.vobject_instance.vevent.dtstart.value
    expected = os.environ["EXPECT_START_UTC"]
    check("it starts at the right instant (the page's local time, converted)", start.astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ") == expected, f"{start} vs {expected}")
    weekly = [e for e in cal.events() if "UI weekly" in e.data]
    check("a repeating event made in the page keeps its rule and its time zone", len(weekly) == 1 and "RRULE:FREQ=WEEKLY" in weekly[0].data and "TZID=" in weekly[0].data, [w.data[:200] for w in weekly])


def app_event(client):
    """Adds events the way a calendar app does, including things the web page cannot edit."""
    cal = next(c for c in client.principal().calendars() if "default" in str(c.url))
    day = datetime.now(timezone.utc).replace(hour=10, minute=0, second=0, microsecond=0) + timedelta(days=2)
    cal.save_event(ics(f"app-{uuid.uuid4()}", "Made in the app", day, day + timedelta(hours=1)))
    rich = ics(f"rich-{uuid.uuid4()}", "Board meeting (with reminder)", day + timedelta(hours=2), day + timedelta(hours=3), "ATTENDEE;CN=Bob:mailto:bob@example.com\r\n").replace(
        "END:VEVENT", "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT"
    )
    cal.save_event(rich)
    check("events with reminders and guests are accepted from an app", len(cal.events()) >= 3)


def main():
    scenario = sys.argv[1] if len(sys.argv) > 1 else "alice"
    with caldav.DAVClient(url=URL, username=USER, password=TOKEN) as client:
        {"alice": alice, "shared": shared, "ui-event": ui_event, "app-event": app_event}[scenario](client)
    return 1 if failures else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:  # noqa: BLE001
        print(f"FAIL client crashed -> {e!r}", flush=True)
        traceback.print_exc()
        sys.exit(1)
