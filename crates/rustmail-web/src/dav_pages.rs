//! Calendar, contacts and app-password pages (Phase 9).
//!
//! | Route | What |
//! |-------|------|
//! | `/calendar` | month / week / day / list view (FullCalendar) and the calendars shown |
//! | `/calendar/events` | JSON feed for the view |
//! | `/calendar/event` | create, edit, delete one event |
//! | `/calendar/freebusy` | when a colleague is busy |
//! | `/contacts`, `/contacts/edit` | the address book |
//! | `/settings/collections` | make, rename, share and delete calendars and address books |
//! | `/settings/apps` | app passwords for Thunderbird, phones and other apps |
//!
//! Everything here acts as the signed-in user, who is passed to `rustmail_core::dav::store`
//! on every call; that is where access is decided.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rustmail_core::dav::calendar::{self, NewEvent, Repeat};
use rustmail_core::dav::contact::{self, NewContact};
use rustmail_core::dav::store::{
    Access, Collection, CollectionUpdate, Grantee, GranteeRef, Kind, Precondition,
};
use rustmail_core::Error;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::admin_common::{alert, form_error, redirect_to};
use crate::app::AppState;
use crate::auth::csrf::ensure_cookie;
use crate::pages::{encode_query_value, page_ctx, WebUser};
use crate::ui::{render, PageCtx, PageError};

type Blocked = Box<Response>;

fn blocked(r: Response) -> Blocked {
    Box::new(r)
}

struct Opt {
    value: String,
    label: String,
    selected: bool,
}

fn opt(value: impl Into<String>, label: impl Into<String>, current: &str) -> Opt {
    let value = value.into();
    Opt {
        selected: value == current,
        value,
        label: label.into(),
    }
}

fn dav_err(e: rustmail_core::dav::DavError) -> Response {
    match e {
        rustmail_core::dav::DavError::NotFound => PageError::NotFound.into_response(),
        rustmail_core::dav::DavError::Database(error) => {
            tracing::error!(%error, "calendar page failed");
            PageError::Internal.into_response()
        }
        other => form_error(&Error::from(other)),
    }
}

/// `https://host` for the addresses shown to people setting up an app.
fn base_url(state: &AppState, headers: &HeaderMap) -> String {
    let scheme = if state.auth.security.cookie_secure {
        "https"
    } else {
        "http"
    };
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| {
            !h.is_empty()
                && h.len() < 256
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
        })
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

fn dav_path(c: &Collection) -> String {
    let dir = match c.kind {
        Kind::Calendar => "calendars",
        Kind::AddressBook => "addressbooks",
    };
    format!(
        "/dav/{dir}/{}/{}/",
        crate::dav_xml::encode_segment(&c.owner_email),
        crate::dav_xml::encode_segment(&c.slug)
    )
}

fn access_key(a: Access) -> &'static str {
    match a {
        Access::Owner => "dav-access-owner",
        Access::ReadWrite => "dav-access-write",
        Access::Read => "dav-access-read",
    }
}

fn notice(done: Option<&str>) -> Option<(&'static str, &'static str)> {
    Some(match done? {
        "saved" => ("success", "dav-notice-saved"),
        "deleted" => ("success", "dav-notice-deleted"),
        "shared" => ("success", "dav-notice-shared"),
        "unshared" => ("success", "dav-notice-unshared"),
        "created" => ("success", "dav-notice-created"),
        "revoked" => ("success", "dav-notice-revoked"),
        _ => return None,
    })
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Done {
    done: Option<String>,
}

// ─── the calendar ───────────────────────────────────────────────────────────

struct CalendarLine {
    name: String,
    color: String,
    owner: String,
    shared: bool,
    writable: bool,
}

#[derive(Template)]
#[template(path = "calendar/page.html")]
struct CalendarPage {
    ctx: PageCtx,
    calendars: Vec<CalendarLine>,
    initial_date: String,
    script: String,
    stylesheet: String,
    can_create: bool,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct CalendarQuery {
    date: Option<String>,
}

fn day_of(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok()
}

pub(crate) async fn calendar_page(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<CalendarQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/calendar");
    let jar = ensure_cookie(&state.auth, jar);
    if let Err(e) = state.dav.ensure_defaults(web.user.id).await {
        return dav_err(e);
    }
    let calendars = match state.dav.collections(web.user.id, Kind::Calendar).await {
        Ok(c) => c,
        Err(e) => return dav_err(e),
    };
    let page = CalendarPage {
        calendars: calendars
            .iter()
            .map(|c| CalendarLine {
                name: c.display_name.clone(),
                color: c.color.clone().unwrap_or_else(|| "#3788d8".into()),
                owner: c.owner_email.clone(),
                shared: !c.access.is_owner(),
                writable: c.access.can_write(),
            })
            .collect(),
        initial_date: q
            .date
            .as_deref()
            .and_then(day_of)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        script: ctx.assets.url("vendor/fullcalendar/6/calendar.js"),
        stylesheet: ctx.assets.url("vendor/fullcalendar/6/calendar.css"),
        can_create: calendars.iter().any(|c| c.access.can_write()),
        ctx,
    };
    (jar, render(&page)).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct RangeQuery {
    start: String,
    end: String,
}

fn parse_instant(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
        .or_else(|| {
            day_of(text)
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|n| Utc.from_utc_datetime(&n))
        })
}

pub(crate) async fn calendar_events(
    State(state): State<AppState>,
    web: WebUser,
    Query(q): Query<RangeQuery>,
) -> Response {
    let (Some(from), Some(to)) = (parse_instant(&q.start), parse_instant(&q.end)) else {
        return (StatusCode::BAD_REQUEST, "bad dates").into_response();
    };
    let events = match state.dav.calendar_events(web.user.id, from, to).await {
        Ok(e) => e,
        Err(e) => return dav_err(e),
    };
    let list: Vec<_> = events
        .iter()
        .map(|e| {
            let o = &e.occurrence;
            let (start, end) = if o.all_day { (o.start.format("%Y-%m-%d").to_string(), o.end.format("%Y-%m-%d").to_string()) } else { (o.start.to_rfc3339(), o.end.to_rfc3339()) };
            json!({
                "id": format!("{}:{}:{}", e.collection_id, e.object, o.recurrence_id.map_or(0, |t| t.timestamp())),
                "title": if o.summary.is_empty() { "—" } else { o.summary.as_str() },
                "start": start,
                "end": end,
                "allDay": o.all_day,
                "color": e.color.clone().unwrap_or_else(|| "#3788d8".into()),
                "url": format!("/calendar/event?c={}&o={}", e.collection_id, encode_query_value(&e.object)),
                "extendedProps": { "location": o.location, "writable": e.writable },
            })
        })
        .collect();
    Json(list).into_response()
}

// ─── one event ──────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "calendar/event.html")]
struct EventPage {
    ctx: PageCtx,
    is_new: bool,
    calendars: Vec<Opt>,
    collection: String,
    object: String,
    etag: String,
    uid: String,
    title: String,
    location: String,
    description: String,
    all_day: bool,
    start: String,
    end: String,
    repeat_options: Vec<Opt>,
    repeat_until: String,
    editable: bool,
    can_delete: bool,
    calendar_name: String,
    readonly_when: String,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct EventQuery {
    c: Option<String>,
    o: Option<String>,
    date: Option<String>,
}

fn local_input(t: DateTime<Utc>, all_day: bool) -> String {
    if all_day {
        t.format("%Y-%m-%d").to_string()
    } else {
        t.format("%Y-%m-%dT%H:%M").to_string()
    }
}

pub(crate) async fn event_page(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<EventQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/calendar");
    let jar = ensure_cookie(&state.auth, jar);
    let all = match state.dav.collections(web.user.id, Kind::Calendar).await {
        Ok(c) => c,
        Err(e) => return dav_err(e),
    };
    let repeat_options = |current: Repeat| {
        [
            Repeat::Never,
            Repeat::Daily,
            Repeat::Weekly,
            Repeat::Monthly,
            Repeat::Yearly,
        ]
        .into_iter()
        .map(|r| {
            opt(
                r.as_str(),
                ctx.t(&format!("cal-repeat-{}", r.as_str())),
                current.as_str(),
            )
        })
        .collect::<Vec<_>>()
    };
    let mut page = EventPage {
        is_new: true,
        calendars: Vec::new(),
        collection: String::new(),
        object: String::new(),
        etag: String::new(),
        uid: Uuid::new_v4().to_string(),
        title: String::new(),
        location: String::new(),
        description: String::new(),
        all_day: false,
        start: String::new(),
        end: String::new(),
        repeat_options: repeat_options(Repeat::Never),
        repeat_until: String::new(),
        editable: true,
        can_delete: false,
        calendar_name: String::new(),
        readonly_when: String::new(),
        ctx: ctx.clone(),
    };
    let writable: Vec<&Collection> = all.iter().filter(|c| c.access.can_write()).collect();
    match (
        q.c.as_deref().and_then(|c| Uuid::parse_str(c).ok()),
        q.o.as_deref(),
    ) {
        (Some(cid), Some(object)) => {
            let Some(c) = all.iter().find(|c| c.id == cid) else {
                return PageError::NotFound.into_response();
            };
            let obj = match state.dav.get(web.user.id, cid, object).await {
                Ok(o) => o,
                Err(e) => return dav_err(e),
            };
            page.is_new = false;
            page.collection = cid.to_string();
            page.object = object.to_owned();
            page.etag = obj.meta.etag.clone();
            page.calendar_name = c.display_name.clone();
            page.can_delete = c.access.can_write();
            page.calendars = vec![opt(
                cid.to_string(),
                c.display_name.clone(),
                &cid.to_string(),
            )];
            match (calendar::read_form(&obj.data), c.access.can_write()) {
                (Some(f), true) => {
                    page.uid = f.uid;
                    page.title = f.summary;
                    page.location = f.location;
                    page.description = f.description;
                    page.all_day = f.all_day;
                    page.start = local_input(f.start, f.all_day);
                    page.end = local_input(f.end, f.all_day);
                    page.repeat_options = repeat_options(f.repeat);
                    page.repeat_until = f
                        .repeat_until
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_default();
                }
                _ => {
                    // Not editable here: show what it is, and offer to delete it.
                    page.editable = false;
                    let now = Utc::now();
                    let shown = calendar::occurrences(
                        &obj.data,
                        now - Duration::days(365),
                        now + Duration::days(365),
                    )
                    .unwrap_or_default();
                    let first = shown.first();
                    page.title = first
                        .map(|o| o.summary.clone())
                        .unwrap_or(obj.meta.summary.clone());
                    page.location = first.map(|o| o.location.clone()).unwrap_or_default();
                    page.readonly_when = first
                        .map(|o| {
                            format!(
                                "{} – {}",
                                o.start.format("%Y-%m-%d %H:%M UTC"),
                                o.end.format("%Y-%m-%d %H:%M UTC")
                            )
                        })
                        .unwrap_or_default();
                }
            }
        }
        _ => {
            if writable.is_empty() {
                return PageError::Forbidden.into_response();
            }
            let day = q
                .date
                .as_deref()
                .and_then(day_of)
                .unwrap_or_else(|| Utc::now().date_naive());
            // A click on a time slot passes the time too (`2025-03-10T09:00:00+01:00`); otherwise 09:00.
            let start = q
                .date
                .as_deref()
                .and_then(parse_instant)
                .filter(|_| q.date.as_deref().is_some_and(|d| d.len() > 10))
                .unwrap_or_else(|| {
                    Utc.from_utc_datetime(&day.and_hms_opt(9, 0, 0).unwrap_or_default())
                });
            page.start = local_input(start, false);
            page.end = local_input(start + Duration::hours(1), false);
            page.calendars = writable
                .iter()
                .map(|c| {
                    opt(
                        c.id.to_string(),
                        if c.access.is_owner() {
                            c.display_name.clone()
                        } else {
                            format!("{} ({})", c.display_name, c.owner_email)
                        },
                        q.c.as_deref().unwrap_or(""),
                    )
                })
                .collect();
        }
    }
    (jar, render(&page)).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct EventForm {
    collection: Option<String>,
    object: Option<String>,
    etag: Option<String>,
    uid: Option<String>,
    title: Option<String>,
    location: Option<String>,
    description: Option<String>,
    all_day: Option<String>,
    start: Option<String>,
    end: Option<String>,
    repeat: Option<String>,
    repeat_until: Option<String>,
    tz: Option<String>,
}

/// Reads a form time (`2025-03-10T09:00`, or just a date) as the instant it means in `tz`.
fn parse_local(
    text: &str,
    tz: Option<chrono_tz::Tz>,
    all_day: bool,
) -> Result<DateTime<Utc>, Error> {
    let t = text.trim();
    let bad = || Error::Validation("enter the date and time as the page shows them".into());
    if all_day || t.len() == 10 {
        let d = NaiveDate::parse_from_str(t.get(..10).ok_or_else(bad)?, "%Y-%m-%d")
            .map_err(|_| bad())?;
        return Ok(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).ok_or_else(bad)?));
    }
    let n = NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S"))
        .map_err(|_| bad())?;
    Ok(match tz {
        Some(tz) => tz
            .from_local_datetime(&n)
            .earliest()
            .or_else(|| tz.from_local_datetime(&n).latest())
            .map_or_else(|| Utc.from_utc_datetime(&n), |t| t.with_timezone(&Utc)),
        None => Utc.from_utc_datetime(&n),
    })
}

impl EventForm {
    fn event(&self) -> Result<NewEvent, Error> {
        let tz = self
            .tz
            .as_deref()
            .and_then(|z| z.trim().parse::<chrono_tz::Tz>().ok());
        let all_day = self.all_day.is_some();
        let start = parse_local(self.start.as_deref().unwrap_or(""), tz, all_day)?;
        let end = parse_local(self.end.as_deref().unwrap_or(""), tz, all_day)?;
        let repeat = Repeat::parse(self.repeat.as_deref().unwrap_or(""))
            .ok_or_else(|| Error::Validation("choose how often it repeats".into()))?;
        let repeat_until = match self
            .repeat_until
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => None,
            Some(d) => Some(
                NaiveDate::parse_from_str(d, "%Y-%m-%d")
                    .map_err(|_| Error::Validation("the repeat end is not a valid date".into()))?,
            ),
        };
        Ok(NewEvent {
            uid: self.uid.clone().unwrap_or_default(),
            summary: self.title.clone().unwrap_or_default(),
            location: self.location.clone().unwrap_or_default(),
            description: self
                .description
                .clone()
                .unwrap_or_default()
                .replace("\r\n", "\n"),
            start,
            end,
            all_day,
            repeat,
            repeat_until: if repeat == Repeat::Never {
                None
            } else {
                repeat_until
            },
            tz,
        })
    }
}

pub(crate) async fn event_save(
    State(state): State<AppState>,
    web: WebUser,
    Form(f): Form<EventForm>,
) -> Response {
    let Some(collection) = f
        .collection
        .as_deref()
        .and_then(|c| Uuid::parse_str(c).ok())
    else {
        return form_error(&Error::Validation("choose a calendar".into()));
    };
    let event = match f.event() {
        Ok(e) => e,
        Err(e) => return form_error(&e),
    };
    let text = match calendar::build_event(&event) {
        Ok(t) => t,
        Err(e) => return dav_err(e),
    };
    let editing = f.object.as_deref().filter(|o| !o.is_empty());
    let (name, precondition) = match editing {
        Some(object) => {
            // Only events the page can write back faithfully may be saved from it.
            match state.dav.get(web.user.id, collection, object).await {
                Ok(existing) if calendar::read_form(&existing.data).is_some() => {}
                Ok(_) => {
                    return form_error(&Error::Validation(
                        "this event has details only your calendar app can edit".into(),
                    ))
                }
                Err(e) => return dav_err(e),
            }
            (
                object.to_owned(),
                f.etag
                    .as_deref()
                    .filter(|e| !e.is_empty())
                    .map_or(Precondition::None, |e| Precondition::Etag(e.to_owned())),
            )
        }
        None => (format!("{}.ics", event.uid), Precondition::Absent),
    };
    match state
        .dav
        .put(web.user.id, collection, &name, &text, &precondition)
        .await
    {
        Ok(_) => redirect_to(&format!(
            "/calendar?date={}",
            event.start.format("%Y-%m-%d")
        )),
        Err(e) => dav_err(e),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ObjectForm {
    collection: Option<String>,
    object: Option<String>,
}

pub(crate) async fn event_delete(
    State(state): State<AppState>,
    web: WebUser,
    Form(f): Form<ObjectForm>,
) -> Response {
    let (Some(collection), Some(object)) = (
        f.collection
            .as_deref()
            .and_then(|c| Uuid::parse_str(c).ok()),
        f.object.as_deref(),
    ) else {
        return PageError::NotFound.into_response();
    };
    match state
        .dav
        .delete(web.user.id, collection, object, None)
        .await
    {
        Ok(()) => redirect_to("/calendar?done=deleted"),
        Err(e) => dav_err(e),
    }
}

// ─── free / busy ────────────────────────────────────────────────────────────

struct BusyDay {
    day: String,
    periods: Vec<String>,
}

#[derive(Template)]
#[template(path = "calendar/freebusy.html")]
struct FreeBusyPage {
    ctx: PageCtx,
    person: String,
    date: String,
    searched: bool,
    problem: Option<&'static str>,
    days: Vec<BusyDay>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct FreeBusyQuery {
    user: Option<String>,
    date: Option<String>,
}

pub(crate) async fn freebusy(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<FreeBusyQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/calendar");
    let jar = ensure_cookie(&state.auth, jar);
    let start = q
        .date
        .as_deref()
        .and_then(day_of)
        .unwrap_or_else(|| Utc::now().date_naive());
    let person = q.user.clone().unwrap_or_default().trim().to_owned();
    let mut page = FreeBusyPage {
        ctx,
        person: person.clone(),
        date: start.format("%Y-%m-%d").to_string(),
        searched: false,
        problem: None,
        days: Vec::new(),
    };
    if !person.is_empty() {
        page.searched = true;
        let from = Utc.from_utc_datetime(&start.and_hms_opt(0, 0, 0).unwrap_or_default());
        let to = from + Duration::days(7);
        match state.dav.free_busy(web.user.id, &person, from, to).await {
            Ok(periods) => {
                page.days = (0..7)
                    .map(|i| {
                        let day_start = from + Duration::days(i);
                        let day_end = day_start + Duration::days(1);
                        let periods = periods
                            .iter()
                            .filter(|p| p.start < day_end && p.end > day_start)
                            .map(|p| {
                                format!(
                                    "{} – {}",
                                    p.start.max(day_start).format("%H:%M"),
                                    if p.end >= day_end {
                                        "24:00".to_owned()
                                    } else {
                                        p.end.format("%H:%M").to_string()
                                    }
                                )
                            })
                            .collect();
                        BusyDay {
                            day: format!(
                                "{} {}",
                                day_start.format("%Y-%m-%d"),
                                day_start.weekday()
                            ),
                            periods,
                        }
                    })
                    .collect();
            }
            Err(rustmail_core::dav::DavError::NotFound) => {
                page.problem = Some("cal-freebusy-unknown")
            }
            Err(e) => return dav_err(e),
        }
    }
    (jar, render(&page)).into_response()
}

// ─── contacts ───────────────────────────────────────────────────────────────

struct ContactLine {
    name: String,
    object: String,
    emails: String,
    phones: String,
    organisation: String,
}

#[derive(Template)]
#[template(path = "contacts/list.html")]
struct ContactsPage {
    ctx: PageCtx,
    books: Vec<Opt>,
    book: String,
    book_name: String,
    query: String,
    rows: Vec<ContactLine>,
    writable: bool,
    notice: Option<(&'static str, &'static str)>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ContactsQuery {
    b: Option<String>,
    q: Option<String>,
    done: Option<String>,
}

async fn pick_book(
    state: &AppState,
    user: Uuid,
    wanted: Option<&str>,
) -> Result<(Vec<Collection>, Collection), Blocked> {
    state
        .dav
        .ensure_defaults(user)
        .await
        .map_err(|e| blocked(dav_err(e)))?;
    let books = state
        .dav
        .collections(user, Kind::AddressBook)
        .await
        .map_err(|e| blocked(dav_err(e)))?;
    let chosen = wanted
        .and_then(|w| Uuid::parse_str(w).ok())
        .and_then(|id| books.iter().find(|b| b.id == id))
        .or_else(|| books.first())
        .cloned();
    chosen
        .map(|c| (books, c))
        .ok_or_else(|| blocked(PageError::NotFound.into_response()))
}

pub(crate) async fn contacts_page(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<ContactsQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/contacts");
    let jar = ensure_cookie(&state.auth, jar);
    let (books, book) = match pick_book(&state, web.user.id, q.b.as_deref()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let query =
        q.q.clone()
            .unwrap_or_default()
            .chars()
            .take(100)
            .collect::<String>();
    let found = match state
        .dav
        .search_contacts(web.user.id, book.id, &query, 200)
        .await
    {
        Ok(f) => f,
        Err(e) => return dav_err(e),
    };
    let rows = found
        .iter()
        .map(|o| {
            let f = contact::read_fields(&o.data);
            ContactLine {
                name: o.meta.summary.clone(),
                object: o.meta.name.clone(),
                emails: f.as_ref().map(|c| c.emails.join(", ")).unwrap_or_default(),
                phones: f.as_ref().map(|c| c.phones.join(", ")).unwrap_or_default(),
                organisation: f.map(|c| c.organisation).unwrap_or_default(),
            }
        })
        .collect();
    let page = ContactsPage {
        books: books
            .iter()
            .map(|b| {
                opt(
                    b.id.to_string(),
                    if b.access.is_owner() {
                        b.display_name.clone()
                    } else {
                        format!("{} ({})", b.display_name, b.owner_email)
                    },
                    &book.id.to_string(),
                )
            })
            .collect(),
        book: book.id.to_string(),
        book_name: book.display_name.clone(),
        query,
        rows,
        writable: book.access.can_write(),
        notice: notice(q.done.as_deref()),
        ctx,
    };
    (jar, render(&page)).into_response()
}

#[derive(Template)]
#[template(path = "contacts/edit.html")]
struct ContactPage {
    ctx: PageCtx,
    is_new: bool,
    book: String,
    object: String,
    etag: String,
    uid: String,
    name: String,
    organisation: String,
    emails: String,
    phones: String,
    notes: String,
    editable: bool,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ContactQuery {
    b: Option<String>,
    o: Option<String>,
}

pub(crate) async fn contact_page(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<ContactQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/contacts");
    let jar = ensure_cookie(&state.auth, jar);
    let (_, book) = match pick_book(&state, web.user.id, q.b.as_deref()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let mut page = ContactPage {
        is_new: true,
        book: book.id.to_string(),
        object: String::new(),
        etag: String::new(),
        uid: Uuid::new_v4().to_string(),
        name: String::new(),
        organisation: String::new(),
        emails: String::new(),
        phones: String::new(),
        notes: String::new(),
        editable: book.access.can_write(),
        ctx,
    };
    if let Some(object) = q.o.as_deref() {
        let obj = match state.dav.get(web.user.id, book.id, object).await {
            Ok(o) => o,
            Err(e) => return dav_err(e),
        };
        let Some(c) = contact::read_fields(&obj.data) else {
            return PageError::NotFound.into_response();
        };
        page.is_new = false;
        page.object = object.to_owned();
        page.etag = obj.meta.etag;
        page.uid = c.uid;
        page.name = c.name;
        page.organisation = c.organisation;
        page.emails = c.emails.join("\n");
        page.phones = c.phones.join("\n");
        page.notes = c.notes;
    } else if !book.access.can_write() {
        return PageError::Forbidden.into_response();
    }
    (jar, render(&page)).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContactForm {
    book: Option<String>,
    object: Option<String>,
    etag: Option<String>,
    uid: Option<String>,
    name: Option<String>,
    organisation: Option<String>,
    emails: Option<String>,
    phones: Option<String>,
    notes: Option<String>,
}

fn lines(text: Option<&str>) -> Vec<String> {
    text.unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) async fn contact_save(
    State(state): State<AppState>,
    web: WebUser,
    Form(f): Form<ContactForm>,
) -> Response {
    let Some(book) = f.book.as_deref().and_then(|b| Uuid::parse_str(b).ok()) else {
        return PageError::NotFound.into_response();
    };
    let edited = NewContact {
        uid: f.uid.clone().unwrap_or_default(),
        name: f.name.clone().unwrap_or_default(),
        organisation: f.organisation.clone().unwrap_or_default(),
        emails: lines(f.emails.as_deref()),
        phones: lines(f.phones.as_deref()),
        notes: f.notes.clone().unwrap_or_default().replace("\r\n", "\n"),
    };
    let editing = f.object.as_deref().filter(|o| !o.is_empty());
    let (name, text, precondition) = match editing {
        Some(object) => {
            let existing = match state.dav.get(web.user.id, book, object).await {
                Ok(o) => o,
                Err(e) => return dav_err(e),
            };
            // Whatever the page does not show (photo, address, custom fields) is kept.
            let text = match contact::merge_contact(&existing.data, &edited) {
                Ok(t) => t,
                Err(e) => return dav_err(e),
            };
            (
                object.to_owned(),
                text,
                f.etag
                    .as_deref()
                    .filter(|e| !e.is_empty())
                    .map_or(Precondition::None, |e| Precondition::Etag(e.to_owned())),
            )
        }
        None => match contact::build_contact(&edited) {
            Ok(t) => (format!("{}.vcf", edited.uid), t, Precondition::Absent),
            Err(e) => return dav_err(e),
        },
    };
    match state
        .dav
        .put(web.user.id, book, &name, &text, &precondition)
        .await
    {
        Ok(_) => redirect_to(&format!("/contacts?b={book}&done=saved")),
        Err(e) => dav_err(e),
    }
}

pub(crate) async fn contact_delete(
    State(state): State<AppState>,
    web: WebUser,
    Form(f): Form<ContactForm>,
) -> Response {
    let (Some(book), Some(object)) = (
        f.book.as_deref().and_then(|b| Uuid::parse_str(b).ok()),
        f.object.as_deref(),
    ) else {
        return PageError::NotFound.into_response();
    };
    match state.dav.delete(web.user.id, book, object, None).await {
        Ok(()) => redirect_to(&format!("/contacts?b={book}&done=deleted")),
        Err(e) => dav_err(e),
    }
}

// ─── calendars and address books: settings and sharing ──────────────────────

struct ShareLine {
    id: String,
    who: String,
    group: bool,
    write: bool,
}

struct CollectionLine {
    id: String,
    name: String,
    description: String,
    color: String,
    owner: String,
    access_key: &'static str,
    is_owner: bool,
    url: String,
    shares: Vec<ShareLine>,
}

#[derive(Template)]
#[template(path = "settings/collections.html")]
struct CollectionsPage {
    ctx: PageCtx,
    calendars: Vec<CollectionLine>,
    books: Vec<CollectionLine>,
    groups: Vec<Opt>,
    notice: Option<(&'static str, &'static str)>,
}

async fn lines_for(
    state: &AppState,
    user: Uuid,
    kind: Kind,
    base: &str,
) -> Result<Vec<CollectionLine>, Blocked> {
    let mut out = Vec::new();
    for c in state
        .dav
        .collections(user, kind)
        .await
        .map_err(|e| blocked(dav_err(e)))?
    {
        let shares = if c.access.is_owner() {
            state
                .dav
                .shares(user, c.id)
                .await
                .map_err(|e| blocked(dav_err(e)))?
                .into_iter()
                .map(|s| match s.grantee {
                    Grantee::User { email, .. } => ShareLine {
                        id: s.id.to_string(),
                        who: email,
                        group: false,
                        write: s.can_write,
                    },
                    Grantee::Group { name, .. } => ShareLine {
                        id: s.id.to_string(),
                        who: name,
                        group: true,
                        write: s.can_write,
                    },
                })
                .collect()
        } else {
            Vec::new()
        };
        out.push(CollectionLine {
            id: c.id.to_string(),
            name: c.display_name.clone(),
            description: c.description.clone(),
            color: c.color.clone().unwrap_or_default(),
            owner: c.owner_email.clone(),
            access_key: access_key(c.access),
            is_owner: c.access.is_owner(),
            url: format!("{base}{}", dav_path(&c)),
            shares,
        });
    }
    Ok(out)
}

pub(crate) async fn collections_page(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<Done>,
) -> Response {
    let ctx = page_ctx(
        &state,
        &jar,
        &headers,
        Some(&web.user),
        "/settings/collections",
    );
    let jar = ensure_cookie(&state.auth, jar);
    if let Err(e) = state.dav.ensure_defaults(web.user.id).await {
        return dav_err(e);
    }
    let base = base_url(&state, &headers);
    let (calendars, books) = match (
        lines_for(&state, web.user.id, Kind::Calendar, &base).await,
        lines_for(&state, web.user.id, Kind::AddressBook, &base).await,
    ) {
        (Ok(c), Ok(b)) => (c, b),
        (Err(r), _) | (_, Err(r)) => return *r,
    };
    let groups = match state.dav.groups_for(web.user.id).await {
        Ok(g) => g
            .into_iter()
            .map(|(id, name)| opt(id.to_string(), name, ""))
            .collect(),
        Err(e) => return dav_err(e),
    };
    (
        jar,
        render(&CollectionsPage {
            calendars,
            books,
            groups,
            notice: notice(q.done.as_deref()),
            ctx,
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct CollectionForm {
    kind: Option<String>,
    name: Option<String>,
    description: Option<String>,
    color: Option<String>,
    confirm: Option<String>,
    with_user: Option<String>,
    with_group: Option<String>,
    can_write: Option<String>,
}

fn color_of(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
}

pub(crate) async fn collection_create(
    State(state): State<AppState>,
    web: WebUser,
    Form(f): Form<CollectionForm>,
) -> Response {
    let kind = if f.kind.as_deref() == Some("addressbook") {
        Kind::AddressBook
    } else {
        Kind::Calendar
    };
    match state
        .dav
        .create_collection(
            web.user.id,
            kind,
            None,
            f.name.as_deref().unwrap_or(""),
            f.description.as_deref().unwrap_or(""),
            color_of(f.color.as_deref()).as_deref(),
        )
        .await
    {
        Ok(_) => redirect_to("/settings/collections?done=created"),
        Err(e) => dav_err(e),
    }
}

pub(crate) async fn collection_update(
    State(state): State<AppState>,
    web: WebUser,
    Path(id): Path<Uuid>,
    Form(f): Form<CollectionForm>,
) -> Response {
    let update = CollectionUpdate {
        display_name: f.name.clone(),
        description: f.description.clone(),
        color: Some(color_of(f.color.as_deref())),
    };
    match state.dav.update_collection(web.user.id, id, &update).await {
        Ok(()) => redirect_to("/settings/collections?done=saved"),
        Err(e) => dav_err(e),
    }
}

pub(crate) async fn collection_delete(
    State(state): State<AppState>,
    web: WebUser,
    Path(id): Path<Uuid>,
    Form(f): Form<CollectionForm>,
) -> Response {
    // Deleting takes everything inside with it, so the name has to be typed.
    let c = match state.dav.collection(web.user.id, id).await {
        Ok(c) => c,
        Err(e) => return dav_err(e),
    };
    if f.confirm.as_deref().map(str::trim) != Some(c.display_name.as_str()) {
        return form_error(&Error::Validation(
            "type the name exactly to confirm".into(),
        ));
    }
    match state.dav.delete_collection(web.user.id, id).await {
        Ok(()) => redirect_to("/settings/collections?done=deleted"),
        Err(e) => dav_err(e),
    }
}

pub(crate) async fn collection_share(
    State(state): State<AppState>,
    web: WebUser,
    Path(id): Path<Uuid>,
    Form(f): Form<CollectionForm>,
) -> Response {
    let with = match (
        f.with_user
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        f.with_group
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|g| Uuid::parse_str(g).ok()),
    ) {
        (Some(email), None) => GranteeRef::UserEmail(email.to_owned()),
        (None, Some(group)) => GranteeRef::Group(group),
        _ => {
            return form_error(&Error::Validation(
                "enter one person's address, or choose one group".into(),
            ))
        }
    };
    match state
        .dav
        .share(web.user.id, id, &with, f.can_write.is_some())
        .await
    {
        Ok(()) => redirect_to("/settings/collections?done=shared"),
        Err(e) => dav_err(e),
    }
}

pub(crate) async fn collection_unshare(
    State(state): State<AppState>,
    web: WebUser,
    Path((id, share)): Path<(Uuid, Uuid)>,
) -> Response {
    match state.dav.unshare(web.user.id, id, share).await {
        Ok(()) => redirect_to("/settings/collections?done=unshared"),
        Err(e) => dav_err(e),
    }
}

// ─── app passwords ──────────────────────────────────────────────────────────

struct AppLine {
    id: String,
    name: String,
    created: String,
    last_used: String,
}

#[derive(Template)]
#[template(path = "settings/apps.html")]
struct AppsPage {
    ctx: PageCtx,
    apps: Vec<AppLine>,
    server: String,
    login: String,
    notice: Option<(&'static str, &'static str)>,
}

pub(crate) async fn apps_page(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<Done>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/settings/apps");
    let jar = ensure_cookie(&state.auth, jar);
    let apps = match state.app_passwords.list(web.user.id).await {
        Ok(a) => a,
        Err(e) => return dav_err(e),
    };
    let page = AppsPage {
        apps: apps
            .iter()
            .map(|a| AppLine {
                id: a.id.to_string(),
                name: a.name.clone(),
                created: a.created_at.format("%Y-%m-%d").to_string(),
                last_used: a
                    .last_used_at
                    .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_default(),
            })
            .collect(),
        server: format!("{}/dav/", base_url(&state, &headers)),
        login: web.user.email.clone(),
        notice: notice(q.done.as_deref()),
        ctx,
    };
    (jar, render(&page)).into_response()
}

#[derive(Template)]
#[template(path = "settings/_app_created.html")]
struct AppCreated {
    ctx: PageCtx,
    token: String,
    login: String,
    server: String,
    calendar_url: String,
    book_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AppForm {
    name: Option<String>,
}

pub(crate) async fn app_create(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Form(f): Form<AppForm>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/settings/apps");
    match state
        .app_passwords
        .create(web.user.id, f.name.as_deref().unwrap_or(""))
        .await
    {
        Ok(token) => {
            let base = base_url(&state, &headers);
            let _ = state.dav.ensure_defaults(web.user.id).await;
            let path = |kind: Kind, slug: &str| {
                format!(
                    "{base}/dav/{}/{}/{slug}/",
                    if kind == Kind::Calendar {
                        "calendars"
                    } else {
                        "addressbooks"
                    },
                    crate::dav_xml::encode_segment(&web.user.email)
                )
            };
            render(&AppCreated {
                token,
                login: web.user.email.clone(),
                server: format!("{base}/dav/"),
                calendar_url: path(Kind::Calendar, "default"),
                book_url: path(Kind::AddressBook, "contacts"),
                ctx,
            })
        }
        Err(e) => dav_err(e),
    }
}

pub(crate) async fn app_revoke(
    State(state): State<AppState>,
    web: WebUser,
    Path(id): Path<Uuid>,
) -> Response {
    match state.app_passwords.revoke(web.user.id, id).await {
        Ok(()) => redirect_to("/settings/apps?done=revoked"),
        Err(rustmail_core::dav::DavError::NotFound) => alert(
            StatusCode::NOT_FOUND,
            "danger",
            "That app password no longer exists.",
        ),
        Err(e) => dav_err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_times_are_read_in_the_chosen_zone() {
        let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        let utc = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        assert_eq!(
            parse_local("2025-01-15T09:00", Some(berlin), false).unwrap(),
            utc("2025-01-15T08:00:00Z")
        );
        assert_eq!(
            parse_local("2025-07-15T09:00", Some(berlin), false).unwrap(),
            utc("2025-07-15T07:00:00Z")
        );
        assert_eq!(
            parse_local("2025-01-15T09:00", None, false).unwrap(),
            utc("2025-01-15T09:00:00Z"),
            "no zone means UTC"
        );
        assert_eq!(
            parse_local("2025-01-15T09:00:30", Some(berlin), false).unwrap(),
            utc("2025-01-15T08:00:30Z")
        );
        assert_eq!(
            parse_local("2025-06-01", Some(berlin), true).unwrap(),
            utc("2025-06-01T00:00:00Z"),
            "all-day dates have no zone"
        );
        assert_eq!(
            parse_local("2025-06-01T14:30", Some(berlin), true).unwrap(),
            utc("2025-06-01T00:00:00Z"),
            "…whatever time came with them"
        );
        // 02:30 on 30 March 2025 does not exist in Berlin; it must not fail.
        assert!(parse_local("2025-03-30T02:30", Some(berlin), false).is_ok());
        for bad in ["", "tomorrow", "2025-13-01T10:00", "2025-01-15 09:00"] {
            assert!(parse_local(bad, None, false).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_form_becomes_an_event() {
        let f = EventForm {
            collection: None,
            object: None,
            etag: None,
            uid: Some("u1".into()),
            title: Some("Lunch".into()),
            location: None,
            description: Some("a\r\nb".into()),
            all_day: None,
            start: Some("2025-03-10T12:00".into()),
            end: Some("2025-03-10T13:00".into()),
            repeat: Some("weekly".into()),
            repeat_until: Some("2025-04-01".into()),
            tz: Some("Europe/Berlin".into()),
        };
        let e = f.event().unwrap();
        assert_eq!(
            (e.repeat, e.description.as_str(), e.tz.map(|t| t.name())),
            (Repeat::Weekly, "a\nb", Some("Europe/Berlin"))
        );
        assert_eq!(e.start.to_rfc3339(), "2025-03-10T11:00:00+00:00");
        let never = EventForm {
            repeat: Some("never".into()),
            ..f
        };
        assert_eq!(
            never.event().unwrap().repeat_until,
            None,
            "a stray end date is ignored when nothing repeats"
        );
        let odd = EventForm {
            repeat: Some("hourly".into()),
            tz: Some("Not/AZone".into()),
            ..never
        };
        assert!(odd.event().is_err());
    }

    #[test]
    fn days_are_read_from_dates_and_date_times() {
        assert_eq!(day_of("2025-03-10"), NaiveDate::from_ymd_opt(2025, 3, 10));
        assert_eq!(
            day_of("2025-03-10T09:00:00+01:00"),
            NaiveDate::from_ymd_opt(2025, 3, 10)
        );
        assert_eq!(day_of("nope"), None);
        assert_eq!(day_of("2025"), None);
        assert!(
            parse_instant("2025-03-10T09:00:00+01:00").is_some()
                && parse_instant("2025-03-10").is_some()
                && parse_instant("x").is_none()
        );
    }

    #[test]
    fn base_urls_come_from_a_safe_host() {
        assert_eq!(lines(Some("a\r\n\r\n b \n")), ["a", "b"]);
        assert_eq!(color_of(Some(" #fff000 ")).as_deref(), Some("#fff000"));
        assert_eq!(color_of(Some("  ")), None);
    }
}
