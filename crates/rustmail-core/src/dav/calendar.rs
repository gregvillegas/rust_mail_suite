//! Reading iCalendar (RFC 5545) text: validation, the fields used for queries, and expansion of
//! repeating events into the occurrences that fall in a time window.
//!
//! What this handles: `VEVENT` and `VTODO`; `DTSTART` / `DTEND` / `DURATION` as UTC, floating,
//! `TZID=` (IANA names) or all-day dates; `RRULE`, `RDATE`, `EXDATE`, and overriding instances
//! (`RECURRENCE-ID`). A `TZID` that is not an IANA name (for example a Windows name) is read as
//! UTC, which can shift such an event by the zone's offset in the web view; the stored text is
//! never changed, so the client still shows it correctly.

use std::collections::HashSet;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use ical::property::Property;
use ical::IcalParser;

use super::{DavError, DavResult, MAX_OBJECT_BYTES};

/// Most components (an event plus its overriding instances) in one object.
pub const MAX_COMPONENTS: usize = 500;
/// Most occurrences produced for one object in one expansion.
pub const MAX_OCCURRENCES: u16 = 2000;

/// What kind of calendar component an object holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    /// A `VEVENT`.
    Event,
    /// A `VTODO`.
    Todo,
}

impl Component {
    /// The iCalendar name, as stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "VEVENT",
            Self::Todo => "VTODO",
        }
    }
}

/// What the server needs to know about one stored object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    /// The `UID` shared by every component of the object.
    pub uid: String,
    /// Event or task.
    pub component: Component,
    /// The `SUMMARY`, or empty.
    pub summary: String,
    /// When the first occurrence starts (events only).
    pub starts_at: Option<DateTime<Utc>>,
    /// When the first occurrence ends (events only).
    pub ends_at: Option<DateTime<Utc>>,
    /// When the last occurrence ends; `None` if it repeats forever or this is not an event.
    pub last_end: Option<DateTime<Utc>>,
    /// Whether it repeats.
    pub recurring: bool,
    /// Whether it blocks time in free/busy.
    pub busy: bool,
}

/// One concrete time an event happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// The event's UID.
    pub uid: String,
    /// The summary of this occurrence.
    pub summary: String,
    /// The `LOCATION`, or empty.
    pub location: String,
    /// Start.
    pub start: DateTime<Utc>,
    /// End (equal to `start` for an event with no length).
    pub end: DateTime<Utc>,
    /// Whether this is an all-day event.
    pub all_day: bool,
    /// For a repeating event, which instance this is (its original start).
    pub recurrence_id: Option<DateTime<Utc>>,
    /// Whether it blocks time in free/busy.
    pub busy: bool,
}

// ─── reading values ──────────────────────────────────────────────────────────

fn param<'a>(p: &'a Property, name: &str) -> Option<&'a str> {
    p.params
        .as_ref()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))?
        .1
        .first()
        .map(String::as_str)
}

fn prop<'a>(props: &'a [Property], name: &str) -> Option<&'a Property> {
    props.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Undoes iCalendar text escaping (`\\,` `\\;` `\\\\` `\\n`).
fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn raw(props: &[Property], name: &str) -> String {
    prop(props, name)
        .and_then(|p| p.value.clone())
        .unwrap_or_default()
}

/// A text property, unescaped for display.
fn text(props: &[Property], name: &str) -> String {
    unescape_text(&raw(props, name))
}

/// Resolves a `TZID` to an IANA zone. Some clients write `/mozilla.org/20050126_1/Europe/Berlin`;
/// the last one or two path segments are tried too.
fn zone(tzid: &str) -> Option<chrono_tz::Tz> {
    let tzid = tzid.trim().trim_matches('"');
    if let Ok(tz) = tzid.parse::<chrono_tz::Tz>() {
        return Some(tz);
    }
    let parts: Vec<&str> = tzid.split('/').filter(|s| !s.is_empty()).collect();
    for take in [2, 1] {
        if parts.len() >= take {
            if let Ok(tz) = parts[parts.len() - take..]
                .join("/")
                .parse::<chrono_tz::Tz>()
            {
                return Some(tz);
            }
        }
    }
    None
}

fn parse_value(value: &str, tzid: Option<&str>) -> Option<(DateTime<Utc>, bool)> {
    let v = value.trim();
    if v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit()) {
        let d = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
        return Some((Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?), true));
    }
    if let Some(utc) = v.strip_suffix('Z') {
        let n = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok()?;
        return Some((Utc.from_utc_datetime(&n), false));
    }
    let n = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    let at = match tzid.and_then(zone) {
        Some(tz) => tz
            .from_local_datetime(&n)
            .earliest()
            .or_else(|| tz.from_local_datetime(&n).latest())?
            .with_timezone(&Utc),
        None => Utc.from_utc_datetime(&n),
    };
    Some((at, false))
}

fn parse_dt(p: &Property) -> Option<(DateTime<Utc>, bool)> {
    parse_value(p.value.as_deref()?, param(p, "TZID"))
}

/// Reads an ISO 8601 duration such as `PT1H30M`, `P1D`, `P2W`, `-PT15M`.
fn parse_duration(text: &str) -> Option<Duration> {
    let t = text.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let t = t.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut number = String::new();
    let mut in_time = false;
    let mut any = false;
    for c in t.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => number.push(c),
            'W' | 'D' | 'H' | 'M' | 'S' => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                any = true;
                total += match (c, in_time) {
                    ('W', false) => Duration::try_weeks(n)?,
                    ('D', false) => Duration::try_days(n)?,
                    ('H', true) => Duration::try_hours(n)?,
                    ('M', true) => Duration::try_minutes(n)?,
                    ('S', true) => Duration::try_seconds(n)?,
                    _ => return None,
                };
            }
            _ => return None,
        }
    }
    (number.is_empty() && any).then_some(if neg { -total } else { total })
}

// ─── parsing an object ───────────────────────────────────────────────────────

/// One `VEVENT`, reduced to what expansion needs.
#[derive(Debug, Clone)]
struct Event {
    uid: String,
    summary: String,
    location: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    all_day: bool,
    busy: bool,
    recurrence_id: Option<DateTime<Utc>>,
    /// The `DTSTART` line in a form the recurrence engine understands.
    dtstart_line: String,
    rrules: Vec<String>,
    exdates: HashSet<DateTime<Utc>>,
    rdates: Vec<DateTime<Utc>>,
}

fn invalid(msg: impl Into<String>) -> DavError {
    DavError::Invalid(msg.into())
}

fn event_from(e: &ical::parser::ical::component::IcalEvent) -> DavResult<Event> {
    let props = &e.properties;
    let uid = raw(props, "UID");
    let start_prop = prop(props, "DTSTART").ok_or_else(|| invalid("an event needs a DTSTART"))?;
    let (start, all_day) =
        parse_dt(start_prop).ok_or_else(|| invalid("DTSTART is not a valid date or time"))?;
    let end = if let Some(p) = prop(props, "DTEND") {
        parse_dt(p)
            .ok_or_else(|| invalid("DTEND is not a valid date or time"))?
            .0
    } else if let Some(d) = prop(props, "DURATION").and_then(|p| p.value.as_deref()) {
        start + parse_duration(d).ok_or_else(|| invalid("DURATION is not valid"))?
    } else if all_day {
        start + Duration::days(1)
    } else {
        start
    };
    let end = end.max(start);
    let tzid = param(start_prop, "TZID").and_then(zone);
    let start_text = start_prop.value.clone().unwrap_or_default();
    let dtstart_line = if all_day {
        format!("DTSTART:{}T000000Z", start_text.trim())
    } else if start_text.trim().ends_with('Z') {
        format!("DTSTART:{}", start_text.trim())
    } else if let Some(tz) = tzid {
        format!("DTSTART;TZID={}:{}", tz.name(), start_text.trim())
    } else {
        format!("DTSTART:{}Z", start_text.trim())
    };
    let status = raw(props, "STATUS");
    let transparent = raw(props, "TRANSP").eq_ignore_ascii_case("TRANSPARENT");
    let mut exdates = HashSet::new();
    let mut rdates = Vec::new();
    for p in props
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case("EXDATE") || p.name.eq_ignore_ascii_case("RDATE"))
    {
        let is_ex = p.name.eq_ignore_ascii_case("EXDATE");
        for v in p.value.as_deref().unwrap_or("").split(',') {
            // An RDATE can be a period ("start/end"); only its start is used.
            let v = v.split('/').next().unwrap_or("");
            if let Some((at, _)) = parse_value(v, param(p, "TZID")) {
                if is_ex {
                    exdates.insert(at);
                } else {
                    rdates.push(at);
                }
            }
        }
    }
    Ok(Event {
        uid,
        summary: text(props, "SUMMARY"),
        location: text(props, "LOCATION"),
        start,
        end,
        all_day,
        busy: !transparent && !status.eq_ignore_ascii_case("CANCELLED"),
        recurrence_id: prop(props, "RECURRENCE-ID")
            .and_then(parse_dt)
            .map(|(d, _)| d),
        dtstart_line,
        rrules: props
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case("RRULE"))
            .filter_map(|p| p.value.clone())
            .collect(),
        exdates,
        rdates,
    })
}

struct Parsed {
    component: Component,
    uid: String,
    summary: String,
    events: Vec<Event>,
}

fn parse(data: &str) -> DavResult<Parsed> {
    if data.len() > MAX_OBJECT_BYTES {
        return Err(invalid("the calendar object is too large"));
    }
    if data.contains('\0') {
        return Err(invalid("the calendar object contains a NUL character"));
    }
    let mut parser = IcalParser::new(data.as_bytes());
    let calendar = match parser.next() {
        Some(Ok(c)) => c,
        Some(Err(e)) => return Err(invalid(format!("not a valid iCalendar object: {e}"))),
        None => return Err(invalid("not a valid iCalendar object: no VCALENDAR")),
    };
    if parser.next().is_some() {
        return Err(invalid("send one VCALENDAR per object"));
    }
    let count = calendar.events.len() + calendar.todos.len();
    if count == 0 {
        return Err(invalid("the object has no VEVENT or VTODO"));
    }
    if count > MAX_COMPONENTS {
        return Err(invalid("the object has too many components"));
    }
    if !calendar.events.is_empty() && !calendar.todos.is_empty() {
        return Err(invalid("an object holds events or tasks, not both"));
    }
    let (component, uids, summary) = if calendar.todos.is_empty() {
        let uids: Vec<String> = calendar
            .events
            .iter()
            .map(|e| raw(&e.properties, "UID"))
            .collect();
        let master = calendar
            .events
            .iter()
            .find(|e| prop(&e.properties, "RECURRENCE-ID").is_none())
            .unwrap_or(&calendar.events[0]);
        (Component::Event, uids, text(&master.properties, "SUMMARY"))
    } else {
        let uids: Vec<String> = calendar
            .todos
            .iter()
            .map(|t| raw(&t.properties, "UID"))
            .collect();
        (
            Component::Todo,
            uids,
            text(&calendar.todos[0].properties, "SUMMARY"),
        )
    };
    let uid = uids[0].trim().to_owned();
    if uid.is_empty() || uid.len() > 255 {
        return Err(invalid("the object needs a UID of at most 255 characters"));
    }
    if uids.iter().any(|u| u.trim() != uid) {
        return Err(invalid("all components of an object must share one UID"));
    }
    let events = if component == Component::Event {
        calendar
            .events
            .iter()
            .map(event_from)
            .collect::<DavResult<Vec<_>>>()?
    } else {
        Vec::new()
    };
    Ok(Parsed {
        component,
        uid,
        summary,
        events,
    })
}

/// Checks `data` and extracts what the server stores alongside it.
///
/// # Errors
/// [`DavError::Invalid`] saying what is wrong.
pub fn parse_object(data: &str) -> DavResult<ObjectInfo> {
    let p = parse(data)?;
    if p.component == Component::Todo {
        return Ok(ObjectInfo {
            uid: p.uid,
            component: Component::Todo,
            summary: p.summary,
            starts_at: None,
            ends_at: None,
            last_end: None,
            recurring: false,
            busy: false,
        });
    }
    let master = p
        .events
        .iter()
        .find(|e| e.recurrence_id.is_none())
        .unwrap_or(&p.events[0]);
    let recurring = !master.rrules.is_empty() || !master.rdates.is_empty();
    let forever = master.rrules.iter().any(|r| {
        !r.to_ascii_uppercase().contains("COUNT=") && !r.to_ascii_uppercase().contains("UNTIL=")
    });
    let last_end = if !recurring {
        Some(p.events.iter().map(|e| e.end).max().unwrap_or(master.end))
    } else if forever {
        None
    } else {
        let all = expand(
            &p.events,
            master.start - Duration::days(1),
            master.start + Duration::days(366 * 200),
            MAX_OCCURRENCES,
        );
        if all.limited {
            None
        } else {
            all.items.iter().map(|o| o.end).max().or(Some(master.end))
        }
    };
    Ok(ObjectInfo {
        uid: p.uid,
        component: Component::Event,
        summary: p.summary,
        starts_at: Some(master.start),
        ends_at: Some(master.end),
        last_end,
        recurring,
        busy: master.busy,
    })
}

// ─── expansion ───────────────────────────────────────────────────────────────

struct Expansion {
    items: Vec<Occurrence>,
    /// The occurrence limit was reached, so there may be more.
    limited: bool,
}

fn overlaps(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> bool {
    if start == end {
        start >= from && start < to
    } else {
        start < to && end > from
    }
}

/// `UNTIL=20250131` with a date-time start is rejected by the recurrence engine; make it a time.
fn normalise_rrule(rule: &str) -> String {
    rule.split(';')
        .map(|part| {
            match part
                .strip_prefix("UNTIL=")
                .or_else(|| part.strip_prefix("until="))
            {
                Some(v) if v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit()) => {
                    format!("UNTIL={v}T235959Z")
                }
                _ => part.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn generated_starts(
    master: &Event,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: u16,
) -> (Vec<DateTime<Utc>>, bool) {
    if master.rrules.is_empty() {
        return (vec![master.start], false);
    }
    let rules: String = master
        .rrules
        .iter()
        .map(|r| format!("\nRRULE:{}", normalise_rrule(r)))
        .collect();
    let text = format!("{}{}", master.dtstart_line, rules);
    let set = match text.parse::<rrule::RRuleSet>() {
        Ok(s) => s,
        Err(error) => {
            tracing::warn!(%error, "could not read a recurrence rule; showing the first occurrence only");
            return (vec![master.start], false);
        }
    };
    let length = master.end - master.start;
    let after = (from - length - Duration::seconds(1)).with_timezone(&rrule::Tz::UTC);
    let before = to.with_timezone(&rrule::Tz::UTC);
    let result = set.after(after).before(before).all(limit);
    (
        result
            .dates
            .into_iter()
            .map(|d| d.with_timezone(&Utc))
            .collect(),
        result.limited,
    )
}

fn expand(events: &[Event], from: DateTime<Utc>, to: DateTime<Utc>, limit: u16) -> Expansion {
    let Some(master) = events.iter().find(|e| e.recurrence_id.is_none()) else {
        // Only overriding instances (a client sent an invitation for one occurrence).
        let items = events
            .iter()
            .filter(|e| overlaps(e.start, e.end, from, to))
            .map(|e| occurrence(e, e.recurrence_id))
            .collect();
        return Expansion {
            items,
            limited: false,
        };
    };
    let overrides: HashSet<DateTime<Utc>> = events.iter().filter_map(|e| e.recurrence_id).collect();
    let length = master.end - master.start;
    let (mut starts, limited) = generated_starts(master, from, to, limit);
    for r in &master.rdates {
        if !starts.contains(r) {
            starts.push(*r);
        }
    }
    let mut items: Vec<Occurrence> = starts
        .into_iter()
        .filter(|s| !master.exdates.contains(s) && !overrides.contains(s))
        .filter(|s| overlaps(*s, *s + length, from, to))
        .map(|s| {
            let mut o = occurrence(
                master,
                (!master.rrules.is_empty() || !master.rdates.is_empty()).then_some(s),
            );
            o.start = s;
            o.end = s + length;
            o
        })
        .collect();
    for e in events.iter().filter(|e| e.recurrence_id.is_some()) {
        if overlaps(e.start, e.end, from, to) {
            items.push(occurrence(e, e.recurrence_id));
        }
    }
    items.sort_by_key(|o| (o.start, o.end));
    Expansion { items, limited }
}

fn occurrence(e: &Event, recurrence_id: Option<DateTime<Utc>>) -> Occurrence {
    Occurrence {
        uid: e.uid.clone(),
        summary: e.summary.clone(),
        location: e.location.clone(),
        start: e.start,
        end: e.end,
        all_day: e.all_day,
        recurrence_id,
        busy: e.busy,
    }
}

/// The occurrences of the event in `data` that overlap `[from, to)`. Tasks have none.
///
/// # Errors
/// [`DavError::Invalid`] if `data` is not valid.
pub fn occurrences(
    data: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> DavResult<Vec<Occurrence>> {
    let p = parse(data)?;
    Ok(expand(&p.events, from, to, MAX_OCCURRENCES).items)
}

// ─── building an event from the web form ─────────────────────────────────────

/// What the calendar page sends for one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    /// The UID (a fresh one for a new event).
    pub uid: String,
    /// Title.
    pub summary: String,
    /// Place, or empty.
    pub location: String,
    /// Notes, or empty.
    pub description: String,
    /// Start. For an all-day event only the date counts.
    pub start: DateTime<Utc>,
    /// End. For an all-day event this is the last day, inclusive.
    pub end: DateTime<Utc>,
    /// Whether it lasts whole days.
    pub all_day: bool,
    /// How it repeats.
    pub repeat: Repeat,
    /// Stop repeating after this day (inclusive), if set.
    pub repeat_until: Option<NaiveDate>,
    /// The time zone the person chose the times in. A repeating event is written in this zone so
    /// it stays at the same wall-clock time across daylight saving; others are written in UTC.
    pub tz: Option<chrono_tz::Tz>,
}

/// How often an event repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// Once.
    Never,
    /// Every day.
    Daily,
    /// Every week.
    Weekly,
    /// Every month.
    Monthly,
    /// Every year.
    Yearly,
}

impl Repeat {
    /// Parses the form value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "" | "never" => Self::Never,
            "daily" => Self::Daily,
            "weekly" => Self::Weekly,
            "monthly" => Self::Monthly,
            "yearly" => Self::Yearly,
            _ => return None,
        })
    }

    /// The form value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }

    fn freq(self) -> Option<&'static str> {
        match self {
            Self::Never => None,
            Self::Daily => Some("DAILY"),
            Self::Weekly => Some("WEEKLY"),
            Self::Monthly => Some("MONTHLY"),
            Self::Yearly => Some("YEARLY"),
        }
    }
}

/// Escapes text for an iCalendar value (RFC 5545 section 3.3.11).
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Folds a content line to at most 75 octets, never inside a character.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        let w = c.len_utf8();
        if width + w > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += w;
    }
    out
}

/// Writes the iCalendar text for an event made in the web page.
///
/// # Errors
/// [`DavError::Invalid`] for an empty title, an end before the start, or text that is too long.
pub fn build_event(e: &NewEvent) -> DavResult<String> {
    let summary = e.summary.trim();
    if summary.is_empty() || summary.chars().count() > 200 {
        return Err(invalid("the title must be 1 to 200 characters"));
    }
    if e.location.chars().count() > 200 || e.description.chars().count() > 4000 {
        return Err(invalid("the place or notes are too long"));
    }
    if e.end < e.start {
        return Err(invalid("the event ends before it starts"));
    }
    if e.uid.is_empty()
        || e.uid.len() > 255
        || e.uid.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(invalid("the event id is not valid"));
    }
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        "PRODID:-//RustMail Suite//Calendar//EN".to_owned(),
        "BEGIN:VEVENT".to_owned(),
        format!("UID:{}", e.uid),
        format!("DTSTAMP:{stamp}"),
    ];
    if e.all_day {
        lines.push(format!("DTSTART;VALUE=DATE:{}", e.start.format("%Y%m%d")));
        // DTEND of an all-day event is the day after its last day.
        lines.push(format!(
            "DTEND;VALUE=DATE:{}",
            (e.end.date_naive() + Duration::days(1)).format("%Y%m%d")
        ));
    } else if let (Some(tz), true) = (e.tz, e.repeat != Repeat::Never) {
        let local = |t: DateTime<Utc>| t.with_timezone(&tz).format("%Y%m%dT%H%M%S");
        lines.push(format!("DTSTART;TZID={}:{}", tz.name(), local(e.start)));
        lines.push(format!("DTEND;TZID={}:{}", tz.name(), local(e.end)));
    } else {
        lines.push(format!("DTSTART:{}", e.start.format("%Y%m%dT%H%M%SZ")));
        lines.push(format!("DTEND:{}", e.end.format("%Y%m%dT%H%M%SZ")));
    }
    lines.push(format!("SUMMARY:{}", escape_text(summary)));
    if !e.location.trim().is_empty() {
        lines.push(format!("LOCATION:{}", escape_text(e.location.trim())));
    }
    if !e.description.trim().is_empty() {
        lines.push(format!("DESCRIPTION:{}", escape_text(e.description.trim())));
    }
    if let Some(freq) = e.repeat.freq() {
        let mut rule = format!("RRULE:FREQ={freq}");
        if let Some(until) = e.repeat_until {
            if until < e.start.date_naive() {
                return Err(invalid("the repeat ends before the event starts"));
            }
            // UNTIL is always UTC. With a time zone, "the end of that day" is the end of the local day.
            let end_of_day = until.and_hms_opt(23, 59, 59).unwrap_or_default();
            let until_utc = match (e.tz, e.all_day) {
                (Some(tz), false) => tz.from_local_datetime(&end_of_day).latest().map_or_else(
                    || Utc.from_utc_datetime(&end_of_day),
                    |t| t.with_timezone(&Utc),
                ),
                _ => Utc.from_utc_datetime(&end_of_day),
            };
            rule.push_str(&format!(";UNTIL={}", until_utc.format("%Y%m%dT%H%M%SZ")));
        }
        lines.push(rule);
    }
    lines.push("END:VEVENT".to_owned());
    lines.push("END:VCALENDAR".to_owned());
    Ok(lines
        .iter()
        .map(|l| fold(l))
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n")
}

// ─── editing in the web page ─────────────────────────────────────────────────

/// Properties the web page can write back. An event with anything else (alarms, attendees,
/// app-specific fields) is shown but not edited here, so editing cannot drop data.
const EDITABLE_PROPERTIES: [&str; 11] = [
    "UID",
    "DTSTAMP",
    "DTSTART",
    "DTEND",
    "DURATION",
    "SUMMARY",
    "LOCATION",
    "DESCRIPTION",
    "RRULE",
    "CREATED",
    "LAST-MODIFIED",
];

/// `FREQ=WEEKLY` or `FREQ=WEEKLY;UNTIL=20250331T235959Z`: the repeats the page can express.
fn simple_rule(rule: &str) -> Option<(Repeat, Option<NaiveDate>)> {
    let mut parts = rule.split(';');
    let repeat = match parts.next()?.strip_prefix("FREQ=")? {
        "DAILY" => Repeat::Daily,
        "WEEKLY" => Repeat::Weekly,
        "MONTHLY" => Repeat::Monthly,
        "YEARLY" => Repeat::Yearly,
        _ => return None,
    };
    let until = match parts.next() {
        None => None,
        Some(u) => Some(parse_value(u.strip_prefix("UNTIL=")?, None)?.0.date_naive()),
    };
    parts.next().is_none().then_some((repeat, until))
}

/// Reads an event for the edit form, or `None` if it holds more than the page can write back
/// (see [`EDITABLE_PROPERTIES`]) or is not a single plain event.
#[must_use]
pub fn read_form(data: &str) -> Option<NewEvent> {
    let calendar = IcalParser::new(data.as_bytes()).next()?.ok()?;
    if calendar.events.len() != 1
        || !calendar.todos.is_empty()
        || !calendar.events[0].alarms.is_empty()
    {
        return None;
    }
    let props = &calendar.events[0].properties;
    if props
        .iter()
        .any(|p| !EDITABLE_PROPERTIES.contains(&p.name.to_ascii_uppercase().as_str()))
    {
        return None;
    }
    let (repeat, repeat_until) = match props
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case("RRULE"))
        .count()
    {
        0 => (Repeat::Never, None),
        1 => simple_rule(&raw(props, "RRULE"))?,
        _ => return None,
    };
    let event = event_from(&calendar.events[0]).ok()?;
    // An all-day event is stored with an exclusive end; the page shows the last day.
    let end = if event.all_day {
        (event.end - Duration::days(1)).max(event.start)
    } else {
        event.end
    };
    Some(NewEvent {
        uid: event.uid,
        summary: event.summary,
        location: event.location,
        description: text(props, "DESCRIPTION"),
        start: event.start,
        end,
        all_day: event.all_day,
        repeat,
        repeat_until,
        tz: prop(props, "DTSTART")
            .and_then(|p| param(p, "TZID"))
            .and_then(zone),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn wrap(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\n{body}END:VCALENDAR\r\n")
    }

    fn event(extra: &str) -> String {
        wrap(&format!("BEGIN:VEVENT\r\nUID:u1\r\nDTSTAMP:20250101T000000Z\r\nSUMMARY:Standup\r\n{extra}END:VEVENT\r\n"))
    }

    #[test]
    fn a_plain_event_is_read() {
        let info = parse_object(&event(
            "DTSTART:20250310T090000Z\r\nDTEND:20250310T093000Z\r\n",
        ))
        .unwrap();
        assert_eq!(info.uid, "u1");
        assert_eq!(info.summary, "Standup");
        assert_eq!(
            (info.starts_at, info.ends_at, info.last_end),
            (
                Some(at("2025-03-10T09:00:00Z")),
                Some(at("2025-03-10T09:30:00Z")),
                Some(at("2025-03-10T09:30:00Z"))
            )
        );
        assert!(!info.recurring && info.busy);
    }

    #[test]
    fn time_zones_all_day_dates_and_durations() {
        // 09:00 in Berlin in January is 08:00 UTC; in July it is 07:00 UTC.
        let winter = parse_object(&event(
            "DTSTART;TZID=Europe/Berlin:20250115T090000\r\nDURATION:PT1H30M\r\n",
        ))
        .unwrap();
        assert_eq!(
            (winter.starts_at, winter.ends_at),
            (
                Some(at("2025-01-15T08:00:00Z")),
                Some(at("2025-01-15T09:30:00Z"))
            )
        );
        let summer = parse_object(&event("DTSTART;TZID=Europe/Berlin:20250715T090000\r\nDTEND;TZID=Europe/Berlin:20250715T100000\r\n")).unwrap();
        assert_eq!(summer.starts_at, Some(at("2025-07-15T07:00:00Z")));
        let day = parse_object(&event("DTSTART;VALUE=DATE:20250601\r\n")).unwrap();
        assert_eq!(
            (day.starts_at, day.ends_at),
            (
                Some(at("2025-06-01T00:00:00Z")),
                Some(at("2025-06-02T00:00:00Z"))
            ),
            "an all-day event lasts one day"
        );
        let old = parse_object(&event("DTSTART;TZID=/mozilla.org/20050126_1/Europe/Berlin:20250115T090000\r\nDURATION:PT1H\r\n")).unwrap();
        assert_eq!(
            old.starts_at,
            Some(at("2025-01-15T08:00:00Z")),
            "old Thunderbird zone ids still work"
        );
        let unknown = parse_object(&event(
            "DTSTART;TZID=W. Europe Standard Time:20250115T090000\r\n",
        ))
        .unwrap();
        assert_eq!(
            unknown.starts_at,
            Some(at("2025-01-15T09:00:00Z")),
            "an unknown zone is read as UTC"
        );
    }

    #[test]
    fn durations_are_parsed_strictly() {
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(parse_duration("P2W"), Some(Duration::weeks(2)));
        assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
        for bad in ["", "P", "PT", "1H", "P1H", "PTH", "P1.5D", "PT5X"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn bad_objects_are_refused_with_a_reason() {
        for (what, data, expect) in [
            ("not ical", "hello".to_owned(), "valid iCalendar"),
            ("no component", wrap(""), "no VEVENT"),
            ("no uid", wrap("BEGIN:VEVENT\r\nDTSTART:20250101T000000Z\r\nEND:VEVENT\r\n"), "UID"),
            ("no start", event(""), "DTSTART"),
            ("bad start", event("DTSTART:yesterday\r\n"), "DTSTART"),
            ("two uids", wrap("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20250101T000000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b\r\nDTSTART:20250101T000000Z\r\nEND:VEVENT\r\n"), "one UID"),
            ("event and task", wrap("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20250101T000000Z\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:a\r\nEND:VTODO\r\n"), "not both"),
            ("nul", event("DTSTART:20250101T000000Z\r\nX-A:\0\r\n"), "NUL"),
        ] {
            let err = parse_object(&data).unwrap_err().to_string();
            assert!(err.contains(expect), "{what}: {err}");
        }
        let huge = format!(
            "{}{}",
            event("DTSTART:20250101T000000Z\r\n"),
            "X".repeat(MAX_OBJECT_BYTES)
        );
        assert!(parse_object(&huge).is_err());
        let twice = format!("{0}{0}", event("DTSTART:20250101T000000Z\r\n"));
        assert!(parse_object(&twice)
            .unwrap_err()
            .to_string()
            .contains("one VCALENDAR"));
    }

    #[test]
    fn tasks_have_no_time_extent() {
        let t = wrap(
            "BEGIN:VTODO\r\nUID:t1\r\nSUMMARY:Buy milk\r\nDUE:20250101T000000Z\r\nEND:VTODO\r\n",
        );
        let info = parse_object(&t).unwrap();
        assert_eq!(
            (info.component, info.starts_at, info.busy),
            (Component::Todo, None, false)
        );
        assert!(
            occurrences(&t, at("2025-01-01T00:00:00Z"), at("2026-01-01T00:00:00Z"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_weekly_event_expands_inside_a_window() {
        let data = event(
            "DTSTART:20250106T090000Z\r\nDTEND:20250106T100000Z\r\nRRULE:FREQ=WEEKLY;COUNT=5\r\n",
        );
        let info = parse_object(&data).unwrap();
        assert!(info.recurring);
        assert_eq!(
            info.last_end,
            Some(at("2025-02-03T10:00:00Z")),
            "the end of the fifth occurrence"
        );
        let all = occurrences(
            &data,
            at("2025-01-01T00:00:00Z"),
            at("2025-12-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(all.len(), 5);
        let window = occurrences(
            &data,
            at("2025-01-13T00:00:00Z"),
            at("2025-01-21T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            window
                .iter()
                .map(|o| o.start.to_rfc3339())
                .collect::<Vec<_>>(),
            ["2025-01-13T09:00:00+00:00", "2025-01-20T09:00:00+00:00"]
        );
        assert!(window
            .iter()
            .all(|o| o.end - o.start == Duration::hours(1) && o.recurrence_id == Some(o.start)));
    }

    #[test]
    fn an_endless_event_has_no_last_end() {
        let data = event("DTSTART:20250106T090000Z\r\nRRULE:FREQ=DAILY\r\n");
        assert_eq!(parse_object(&data).unwrap().last_end, None);
        assert_eq!(
            occurrences(
                &data,
                at("2030-01-01T00:00:00Z"),
                at("2030-01-04T00:00:00Z")
            )
            .unwrap()
            .len(),
            3
        );
        let until = event("DTSTART:20250106T090000Z\r\nRRULE:FREQ=DAILY;UNTIL=20250108\r\n");
        assert_eq!(
            parse_object(&until).unwrap().last_end,
            Some(at("2025-01-08T09:00:00Z")),
            "a date UNTIL is accepted"
        );
    }

    #[test]
    fn repeats_follow_the_local_clock_across_daylight_saving() {
        // Every day at 09:00 Berlin time: 08:00 UTC before the change, 07:00 UTC after (30 March 2025).
        let data = event("DTSTART;TZID=Europe/Berlin:20250328T090000\r\nDTEND;TZID=Europe/Berlin:20250328T100000\r\nRRULE:FREQ=DAILY;COUNT=4\r\n");
        let all = occurrences(
            &data,
            at("2025-03-01T00:00:00Z"),
            at("2025-04-30T00:00:00Z"),
        )
        .unwrap();
        let starts: Vec<String> = all
            .iter()
            .map(|o| o.start.format("%m-%d %H:%M").to_string())
            .collect();
        assert_eq!(
            starts,
            ["03-28 08:00", "03-29 08:00", "03-30 07:00", "03-31 07:00"]
        );
    }

    #[test]
    fn exceptions_and_overriding_instances() {
        let data = wrap(
            "BEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:Standup\r\nDTSTART:20250106T090000Z\r\nDTEND:20250106T093000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nEXDATE:20250107T090000Z\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:Standup (moved)\r\nRECURRENCE-ID:20250108T090000Z\r\nDTSTART:20250108T140000Z\r\nDTEND:20250108T143000Z\r\nEND:VEVENT\r\n",
        );
        let all = occurrences(
            &data,
            at("2025-01-01T00:00:00Z"),
            at("2025-02-01T00:00:00Z"),
        )
        .unwrap();
        let shown: Vec<String> = all
            .iter()
            .map(|o| format!("{} {}", o.start.format("%d %H:%M"), o.summary))
            .collect();
        assert_eq!(
            shown,
            [
                "06 09:00 Standup",
                "08 14:00 Standup (moved)",
                "09 09:00 Standup",
                "10 09:00 Standup"
            ],
            "07 is excluded; 08 moved"
        );
        assert_eq!(
            parse_object(&data).unwrap().last_end,
            Some(at("2025-01-10T09:30:00Z"))
        );
    }

    #[test]
    fn overlap_rules_at_the_edges() {
        let (a, b) = (at("2025-01-01T10:00:00Z"), at("2025-01-01T11:00:00Z"));
        assert!(overlaps(a, b, a, b));
        assert!(
            !overlaps(a, b, b, b + Duration::hours(1)),
            "touching is not overlapping"
        );
        assert!(!overlaps(a, b, a - Duration::hours(1), a));
        assert!(
            overlaps(a, a, a, b),
            "a zero-length event at the window start counts"
        );
        assert!(!overlaps(b, b, a, b), "…but not at its end");
    }

    #[test]
    fn busy_follows_transparency_and_status() {
        assert!(
            parse_object(&event("DTSTART:20250101T000000Z\r\n"))
                .unwrap()
                .busy
        );
        assert!(
            !parse_object(&event("DTSTART:20250101T000000Z\r\nTRANSP:TRANSPARENT\r\n"))
                .unwrap()
                .busy
        );
        assert!(
            !parse_object(&event("DTSTART:20250101T000000Z\r\nSTATUS:CANCELLED\r\n"))
                .unwrap()
                .busy
        );
    }

    #[test]
    fn events_made_in_the_page_are_valid_and_read_back() {
        let e = NewEvent {
            uid: "abc-123".into(),
            summary: "Lunch; with, \"friends\"".into(),
            location: "Café".into(),
            description: "Line one\nLine two".into(),
            start: at("2025-03-10T12:00:00Z"),
            end: at("2025-03-10T13:00:00Z"),
            all_day: false,
            repeat: Repeat::Weekly,
            repeat_until: NaiveDate::from_ymd_opt(2025, 3, 31),
            tz: None,
        };
        let text = build_event(&e).unwrap();
        assert!(
            text.contains("SUMMARY:Lunch\\; with\\, \"friends\"")
                && text.contains("RRULE:FREQ=WEEKLY;UNTIL=20250331T235959Z"),
            "{text}"
        );
        assert!(text.lines().all(|l| l.len() <= 75), "lines are folded");
        let info = parse_object(&text).unwrap();
        assert_eq!(
            (info.uid.as_str(), info.last_end),
            ("abc-123", Some(at("2025-03-31T13:00:00Z")))
        );
        let all = occurrences(
            &text,
            at("2025-03-01T00:00:00Z"),
            at("2025-05-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(all.len(), 4);
        // All-day: the page gives the last day; the file stores the day after.
        let day = NewEvent {
            all_day: true,
            start: at("2025-06-01T00:00:00Z"),
            end: at("2025-06-02T00:00:00Z"),
            repeat: Repeat::Never,
            repeat_until: None,
            tz: None,
            ..e.clone()
        };
        let text = build_event(&day).unwrap();
        assert!(
            text.contains("DTSTART;VALUE=DATE:20250601")
                && text.contains("DTEND;VALUE=DATE:20250603"),
            "{text}"
        );
        let o = &occurrences(
            &text,
            at("2025-05-30T00:00:00Z"),
            at("2025-06-30T00:00:00Z"),
        )
        .unwrap()[0];
        assert!(o.all_day && o.end - o.start == Duration::days(2));
    }

    #[test]
    fn the_page_cannot_write_broken_events() {
        let ok = NewEvent {
            uid: "u".into(),
            summary: "x".into(),
            location: String::new(),
            description: String::new(),
            start: at("2025-01-01T10:00:00Z"),
            end: at("2025-01-01T11:00:00Z"),
            all_day: false,
            repeat: Repeat::Never,
            repeat_until: None,
            tz: None,
        };
        build_event(&ok).unwrap();
        assert!(build_event(&NewEvent {
            summary: "  ".into(),
            ..ok.clone()
        })
        .is_err());
        assert!(build_event(&NewEvent {
            end: at("2025-01-01T09:00:00Z"),
            ..ok.clone()
        })
        .is_err());
        assert!(build_event(&NewEvent {
            uid: "a b".into(),
            ..ok.clone()
        })
        .is_err());
        assert!(build_event(&NewEvent {
            repeat: Repeat::Daily,
            repeat_until: NaiveDate::from_ymd_opt(2024, 1, 1),
            ..ok.clone()
        })
        .is_err());
        // Control characters and a forged line cannot break out of a value.
        let sneaky = NewEvent {
            summary: "Hi\r\nEND:VEVENT\r\nBEGIN:VEVENT".into(),
            ..ok
        };
        let text = build_event(&sneaky).unwrap();
        // The injected text stays inside the SUMMARY value: there is still one real BEGIN line.
        assert_eq!(
            text.lines()
                .filter(|l| l.starts_with("BEGIN:VEVENT"))
                .count(),
            1,
            "{text}"
        );
        assert_eq!(
            text.lines().filter(|l| l.starts_with("END:VEVENT")).count(),
            1,
            "{text}"
        );
        assert_eq!(
            parse_object(&text).unwrap().summary,
            "Hi\nEND:VEVENT\nBEGIN:VEVENT",
            "{text}"
        );
    }

    #[test]
    fn text_comes_back_unescaped_for_display() {
        let e = NewEvent {
            uid: "u".into(),
            summary: "Lunch; with, \"friends\" \\ ok".into(),
            location: "Café, 2nd floor".into(),
            description: "a\nb".into(),
            start: at("2025-03-10T12:00:00Z"),
            end: at("2025-03-10T13:00:00Z"),
            all_day: false,
            repeat: Repeat::Never,
            repeat_until: None,
            tz: None,
        };
        let text = build_event(&e).unwrap();
        assert_eq!(parse_object(&text).unwrap().summary, e.summary);
        let o = &occurrences(
            &text,
            at("2025-03-01T00:00:00Z"),
            at("2025-04-01T00:00:00Z"),
        )
        .unwrap()[0];
        assert_eq!(
            (o.summary.as_str(), o.location.as_str()),
            (e.summary.as_str(), e.location.as_str())
        );
        assert_eq!(
            read_form(&text).unwrap(),
            e,
            "the edit form gets back exactly what was written"
        );
    }

    #[test]
    fn a_repeating_event_keeps_its_wall_clock_time_in_its_zone() {
        let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        // 09:00 Berlin on 28 March 2025 is 08:00 UTC; the clocks change on the 30th.
        let e = NewEvent {
            uid: "u".into(),
            summary: "Daily".into(),
            location: String::new(),
            description: String::new(),
            start: at("2025-03-28T08:00:00Z"),
            end: at("2025-03-28T09:00:00Z"),
            all_day: false,
            repeat: Repeat::Daily,
            repeat_until: NaiveDate::from_ymd_opt(2025, 3, 31),
            tz: Some(berlin),
        };
        let text = build_event(&e).unwrap();
        assert!(
            text.contains("DTSTART;TZID=Europe/Berlin:20250328T090000")
                && text.contains("UNTIL=20250331T215959Z"),
            "{text}"
        );
        let starts: Vec<String> = occurrences(
            &text,
            at("2025-03-01T00:00:00Z"),
            at("2025-04-30T00:00:00Z"),
        )
        .unwrap()
        .iter()
        .map(|o| o.start.format("%m-%d %H:%M").to_string())
        .collect();
        assert_eq!(
            starts,
            ["03-28 08:00", "03-29 08:00", "03-30 07:00", "03-31 07:00"]
        );
        assert_eq!(read_form(&text).unwrap().tz, Some(berlin));
        // Without repeating, UTC is used: nothing to keep in step.
        let once = build_event(&NewEvent {
            repeat: Repeat::Never,
            repeat_until: None,
            ..e
        })
        .unwrap();
        assert!(once.contains("DTSTART:20250328T080000Z"));
    }

    #[test]
    fn only_events_the_page_can_write_back_are_editable() {
        let simple = event("DTSTART:20250310T090000Z\r\nDTEND:20250310T100000Z\r\nLOCATION:Room 1\r\nDESCRIPTION:Bring notes\r\nRRULE:FREQ=WEEKLY;UNTIL=20250331T235959Z\r\n");
        let f = read_form(&simple).unwrap();
        assert_eq!(
            (f.summary.as_str(), f.repeat, f.repeat_until, f.all_day),
            (
                "Standup",
                Repeat::Weekly,
                NaiveDate::from_ymd_opt(2025, 3, 31),
                false
            )
        );
        for (what, extra) in [
            (
                "an attendee",
                "DTSTART:20250310T090000Z\r\nATTENDEE:mailto:a@b.de\r\n",
            ),
            (
                "an app field",
                "DTSTART:20250310T090000Z\r\nX-MOZ-GENERATION:1\r\n",
            ),
            (
                "a weekday rule",
                "DTSTART:20250310T090000Z\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE\r\n",
            ),
            (
                "a count rule",
                "DTSTART:20250310T090000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\n",
            ),
            (
                "an interval",
                "DTSTART:20250310T090000Z\r\nRRULE:FREQ=DAILY;INTERVAL=2\r\n",
            ),
            (
                "an exception date",
                "DTSTART:20250310T090000Z\r\nRRULE:FREQ=DAILY\r\nEXDATE:20250311T090000Z\r\n",
            ),
            (
                "transparency",
                "DTSTART:20250310T090000Z\r\nTRANSP:TRANSPARENT\r\n",
            ),
        ] {
            assert!(read_form(&event(extra)).is_none(), "{what}");
        }
        let alarm = wrap("BEGIN:VEVENT\r\nUID:u1\r\nDTSTART:20250310T090000Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\n");
        assert!(read_form(&alarm).is_none(), "an alarm would be lost");
        let moved = wrap("BEGIN:VEVENT\r\nUID:u1\r\nDTSTART:20250310T090000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:u1\r\nRECURRENCE-ID:20250311T090000Z\r\nDTSTART:20250311T100000Z\r\nEND:VEVENT\r\n");
        assert!(
            read_form(&moved).is_none(),
            "overriding instances would be lost"
        );
        assert!(read_form("garbage").is_none());
        // An all-day event reads back with its last day, not the exclusive end.
        let day = read_form(&event(
            "DTSTART;VALUE=DATE:20250601\r\nDTEND;VALUE=DATE:20250604\r\n",
        ))
        .unwrap();
        assert!(day.all_day && day.end == at("2025-06-03T00:00:00Z"));
    }
}
