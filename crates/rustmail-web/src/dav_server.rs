//! The CalDAV and CardDAV server (`/dav/…`).
//!
//! | URL | What it is |
//! |-----|-----------|
//! | `/dav/` | entry point: tells a client who is signed in |
//! | `/dav/principals/{email}/` | a user: where their calendars and address books are |
//! | `/dav/calendars/{owner}/` , `/dav/addressbooks/{owner}/` | a user's home (own and shared collections) |
//! | `…/{owner}/{slug}/` | one calendar or address book |
//! | `…/{owner}/{slug}/{name}` | one event, task or contact |
//!
//! Supported: `OPTIONS PROPFIND REPORT GET HEAD PUT DELETE MKCALENDAR MKCOL PROPPATCH`, with
//! `calendar-query` (time range, UID), `calendar-multiget`, `addressbook-query`,
//! `addressbook-multiget`, `sync-collection` and `free-busy-query`. Not supported: scheduling
//! (invitations by email), `COPY`/`MOVE`/`LOCK`, and text filters other than UID on events.
//!
//! Authentication is HTTP Basic with an **app password** (never the account password), so it
//! does not use cookies at all and CSRF does not apply. Failed logins are limited per address
//! and per user. Access to a collection is decided by `rustmail_core::dav::store`, which
//! reports collections the user may not see as "not found".

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{FromRequestParts, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use rustmail_core::dav::store::{
    Access, Collection, CollectionUpdate, Kind, ObjectMeta, Precondition,
};
use rustmail_core::dav::tokens::Principal;
use rustmail_core::dav::{calendar, contact, DavError};

use crate::app::AppState;
use crate::auth::ClientMeta;
use crate::dav_xml::{
    self as x, element, esc, href, Multistatus, PropName, PropRequest, NS_CALDAV, NS_CARDDAV,
    NS_CS, NS_DAV, NS_ICAL,
};

/// Body limit for DAV requests: one object plus slack.
pub(crate) const DAV_BODY_LIMIT: usize = 1536 * 1024;
/// Most hrefs one multiget may name.
const MAX_HREFS: usize = 5000;
const SYNC_PREFIX: &str = "urn:rustmail:sync:";
const DAV_HEADER: &str = "1, 3, calendar-access, addressbook, extended-mkcol";
const ALLOW: &str =
    "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT, MKCALENDAR, MKCOL";

// ─── sign-in ────────────────────────────────────────────────────────────────

/// Failed sign-ins in the last [`FailureLimiter::WINDOW`], by key (client address or user).
#[derive(Default)]
pub struct FailureLimiter {
    failures: Mutex<HashMap<String, Vec<Instant>>>,
}

impl FailureLimiter {
    /// How long a failure counts.
    pub const WINDOW: Duration = Duration::from_secs(600);
    /// Failures allowed in the window before further attempts are refused.
    pub const LIMIT: usize = 10;

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<Instant>>> {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// How long until `key` may try again, if it is blocked now.
    pub fn blocked(&self, key: &str) -> Option<Duration> {
        let now = Instant::now();
        let mut map = self.locked();
        let list = map.get_mut(key)?;
        list.retain(|t| now.duration_since(*t) < Self::WINDOW);
        if list.len() >= Self::LIMIT {
            list.first()
                .map(|oldest| Self::WINDOW.saturating_sub(now.duration_since(*oldest)))
        } else {
            None
        }
    }

    /// Notes one failure.
    pub fn record(&self, key: &str) {
        let now = Instant::now();
        let mut map = self.locked();
        if map.len() > 10_000 {
            map.retain(|_, v| v.iter().any(|t| now.duration_since(*t) < Self::WINDOW));
        }
        map.entry(key.to_owned()).or_default().push(now);
    }

    /// Forgets a key's failures (after a good login).
    pub fn clear(&self, key: &str) {
        self.locked().remove(key);
    }
}

/// A user signed in with an app password.
pub(crate) struct DavUser(pub Principal);

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"RustMail\", charset=\"UTF-8\""),
        )],
        "Sign in with your email address and an app password.\n",
    )
        .into_response()
}

fn too_many(wait: Duration) -> Response {
    let secs = wait.as_secs().max(1).to_string();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(
            header::RETRY_AFTER,
            HeaderValue::from_str(&secs).unwrap_or(HeaderValue::from_static("60")),
        )],
        "Too many failed sign-ins. Try again later.\n",
    )
        .into_response()
}

fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    if value.len() > 1024 {
        return None;
    }
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(STANDARD.decode(encoded.trim()).ok()?).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_owned(), pass.to_owned()))
}

impl FromRequestParts<AppState> for DavUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let ClientMeta(client) = match ClientMeta::from_request_parts(parts, state).await {
            Ok(meta) => meta,
            Err(never) => match never {},
        };
        let ip_key = format!("ip:{}", client.ip_address.as_deref().unwrap_or("unknown"));
        let Some((email, token)) = basic_credentials(&parts.headers) else {
            return Err(unauthorized());
        };
        let user_key = format!("user:{}", email.to_lowercase());
        for key in [&ip_key, &user_key] {
            if let Some(wait) = state.dav_failures.blocked(key) {
                return Err(too_many(wait));
            }
        }
        match state.app_passwords.verify(&email, &token).await {
            Ok(Some(principal)) => {
                state.dav_failures.clear(&user_key);
                Ok(Self(principal))
            }
            Ok(None) => {
                state.dav_failures.record(&ip_key);
                state.dav_failures.record(&user_key);
                Err(unauthorized())
            }
            Err(error) => {
                tracing::error!(%error, "app password check failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR.into_response())
            }
        }
    }
}

// ─── URLs ───────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum Target {
    Root,
    Principals,
    Principal(String),
    Home(Kind, String),
    Collection(Kind, String, String),
    Object(Kind, String, String, String),
}

fn kind_dir(kind: Kind) -> &'static str {
    match kind {
        Kind::Calendar => "calendars",
        Kind::AddressBook => "addressbooks",
    }
}

fn parse_target(rest: &str) -> Option<Target> {
    let mut parts = Vec::new();
    for raw in rest.split('/').filter(|s| !s.is_empty()) {
        let seg = x::decode_segment(raw)?;
        if seg == "." || seg == ".." || seg.chars().any(|c| c == '/' || c == '\\' || c.is_control())
        {
            return None;
        }
        parts.push(seg);
    }
    let kind = |s: &str| match s {
        "calendars" => Some(Kind::Calendar),
        "addressbooks" => Some(Kind::AddressBook),
        _ => None,
    };
    match parts.as_slice() {
        [] => Some(Target::Root),
        [p] if p == "principals" => Some(Target::Principals),
        [p, email] if p == "principals" => Some(Target::Principal(email.clone())),
        [k, owner] => Some(Target::Home(kind(k)?, owner.clone())),
        [k, owner, slug] => Some(Target::Collection(kind(k)?, owner.clone(), slug.clone())),
        [k, owner, slug, name] => Some(Target::Object(
            kind(k)?,
            owner.clone(),
            slug.clone(),
            name.clone(),
        )),
        _ => None,
    }
}

fn principal_href(email: &str) -> String {
    format!("/dav/principals/{}/", x::encode_segment(email))
}

fn home_href(kind: Kind, email: &str) -> String {
    format!("/dav/{}/{}/", kind_dir(kind), x::encode_segment(email))
}

fn collection_href(c: &Collection) -> String {
    format!(
        "{}{}/",
        home_href(c.kind, &c.owner_email),
        x::encode_segment(&c.slug)
    )
}

fn object_href(c: &Collection, name: &str) -> String {
    format!("{}{}", collection_href(c), x::encode_segment(name))
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ─── errors and small responses ─────────────────────────────────────────────

fn plain(status: StatusCode, text: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("{text}\n"),
    )
        .into_response()
}

fn xml(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

fn dav_error(e: &DavError) -> Response {
    match e {
        DavError::NotFound => plain(StatusCode::NOT_FOUND, "Not found."),
        DavError::Forbidden => plain(StatusCode::FORBIDDEN, "You may not do that here."),
        DavError::PreconditionFailed => plain(
            StatusCode::PRECONDITION_FAILED,
            "The item was changed by someone else.",
        ),
        DavError::Conflict(m) => plain(StatusCode::CONFLICT, m),
        DavError::Invalid(m) => plain(StatusCode::BAD_REQUEST, m),
        DavError::Database(error) => {
            tracing::error!(%error, "calendar database error");
            plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong on the server.",
            )
        }
    }
}

fn quoted(etag: &str) -> String {
    format!("\"{etag}\"")
}

fn etag_header(etag: &str) -> (HeaderName, HeaderValue) {
    (
        header::ETAG,
        HeaderValue::from_str(&quoted(etag)).unwrap_or(HeaderValue::from_static("\"\"")),
    )
}

/// `If-Match` / `If-None-Match` as a [`Precondition`]. `Err` for a header we cannot honour.
fn precondition(headers: &HeaderMap) -> Result<Precondition, Box<Response>> {
    let get = |name: HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if let Some(v) = get(header::IF_NONE_MATCH) {
        return if v == "*" {
            Ok(Precondition::Absent)
        } else {
            Ok(Precondition::None)
        };
    }
    match get(header::IF_MATCH) {
        Some("*") | None => Ok(Precondition::None),
        Some(v) if v.contains(',') => Err(Box::new(plain(
            StatusCode::BAD_REQUEST,
            "Send one etag in If-Match.",
        ))),
        Some(v) => Ok(Precondition::Etag(
            v.trim_start_matches("W/").trim_matches('"').to_owned(),
        )),
    }
}

// ─── entry point ────────────────────────────────────────────────────────────

/// Redirects the well-known addresses clients try first (RFC 6764).
pub(crate) async fn well_known() -> Response {
    (
        StatusCode::MOVED_PERMANENTLY,
        [(header::LOCATION, HeaderValue::from_static("/dav/"))],
    )
        .into_response()
}

/// Handles every request under `/dav`.
pub(crate) async fn handle(
    State(state): State<AppState>,
    DavUser(me): DavUser,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let rest = uri.path().strip_prefix("/dav").unwrap_or("");
    let Some(target) = parse_target(rest) else {
        return plain(StatusCode::NOT_FOUND, "Not found.");
    };
    let mut response = match method.as_str() {
        "OPTIONS" => (StatusCode::OK, [(header::ALLOW, ALLOW)]).into_response(),
        "PROPFIND" => propfind(&state, &me, &target, &headers, &body).await,
        "REPORT" => report(&state, &me, &target, &body).await,
        "GET" | "HEAD" => get(&state, &me, &target, &headers).await,
        "PUT" => put(&state, &me, &target, &headers, &body).await,
        "DELETE" => delete(&state, &me, &target, &headers).await,
        "MKCALENDAR" | "MKCOL" => {
            make_collection(&state, &me, &target, method.as_str(), &body).await
        }
        "PROPPATCH" => proppatch(&state, &me, &target, &body).await,
        _ => (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, ALLOW)]).into_response(),
    };
    response.headers_mut().insert(
        HeaderName::from_static("dav"),
        HeaderValue::from_static(DAV_HEADER),
    );
    response
}

// ─── properties ─────────────────────────────────────────────────────────────

enum Res<'a> {
    Root,
    Principals,
    Principal(&'a str),
    Home(Kind),
    Collection(&'a Collection),
    Object(&'a Collection, &'a ObjectMeta, Option<&'a str>),
}

fn privileges(access: Access) -> String {
    let d = |n: &str| format!("<D:privilege><D:{n}/></D:privilege>");
    let mut out = d("read")
        + "<D:privilege><C:read-free-busy/></D:privilege>"
        + &d("read-current-user-privilege-set");
    if access.can_write() {
        out += &(d("write") + &d("write-content") + &d("bind") + &d("unbind"));
    }
    if access.is_owner() {
        out += &(d("write-properties") + &d("all"));
    }
    out
}

fn collection_display_name(c: &Collection, me: &Principal) -> String {
    if same(&c.owner_email, &me.email) {
        c.display_name.clone()
    } else {
        format!("{} ({})", c.display_name, c.owner_email)
    }
}

fn collection_of<'a>(res: &Res<'a>) -> Option<&'a Collection> {
    match res {
        Res::Collection(c) | Res::Object(c, _, _) => Some(c),
        _ => None,
    }
}

fn prop_value(res: &Res<'_>, me: &Principal, p: &PropName) -> Option<String> {
    let d = |local: &str, inner: &str| element(NS_DAV, local, inner);
    let collection = collection_of;
    match (p.ns.as_str(), p.local.as_str()) {
        (NS_DAV, "resourcetype") => Some(d(
            "resourcetype",
            match res {
                Res::Root | Res::Principals | Res::Home(..) => "<D:collection/>",
                Res::Principal(_) => "<D:collection/><D:principal/>",
                Res::Collection(c) if c.kind == Kind::Calendar => "<D:collection/><C:calendar/>",
                Res::Collection(_) => "<D:collection/><A:addressbook/>",
                Res::Object(..) => "",
            },
        )),
        (NS_DAV, "displayname") => Some(d(
            "displayname",
            &esc(&match res {
                Res::Root => "RustMail".to_owned(),
                Res::Principals => "Principals".to_owned(),
                Res::Principal(email) => (*email).to_owned(),
                Res::Home(Kind::Calendar) => "Calendars".to_owned(),
                Res::Home(Kind::AddressBook) => "Address books".to_owned(),
                Res::Collection(c) => collection_display_name(c, me),
                Res::Object(_, m, _) => m.summary.clone(),
            }),
        )),
        (NS_DAV, "getetag") => match res {
            Res::Object(_, m, _) => Some(d("getetag", &esc(&quoted(&m.etag)))),
            _ => None,
        },
        (NS_DAV, "getcontenttype") => match res {
            Res::Object(_, m, _) if m.component == "VCARD" => {
                Some(d("getcontenttype", "text/vcard; charset=utf-8"))
            }
            Res::Object(_, m, _) => Some(d(
                "getcontenttype",
                &format!("text/calendar; charset=utf-8; component={}", m.component),
            )),
            _ => None,
        },
        (NS_DAV, "getcontentlength") => match res {
            Res::Object(_, m, _) => Some(d("getcontentlength", &m.size.to_string())),
            _ => None,
        },
        (NS_DAV, "current-user-principal") => Some(d(
            "current-user-principal",
            &href(&principal_href(&me.email)),
        )),
        (NS_DAV, "principal-URL") => match res {
            Res::Principal(email) => Some(d("principal-URL", &href(&principal_href(email)))),
            _ => None,
        },
        (NS_DAV, "owner") => {
            collection(res).map(|c| d("owner", &href(&principal_href(&c.owner_email))))
        }
        (NS_DAV, "principal-collection-set") => matches!(res, Res::Root)
            .then(|| d("principal-collection-set", &href("/dav/principals/"))),
        (NS_DAV, "sync-token") => match res {
            Res::Collection(c) => Some(d("sync-token", &format!("{SYNC_PREFIX}{}", c.revision))),
            _ => None,
        },
        (NS_DAV, "current-user-privilege-set") => {
            collection(res).map(|c| d("current-user-privilege-set", &privileges(c.access)))
        }
        (NS_DAV, "supported-report-set") => match res {
            Res::Collection(c) => {
                let report = |ns: &str, n: &str| {
                    format!(
                        "<D:supported-report><D:report>{}</D:report></D:supported-report>",
                        element(ns, n, "")
                    )
                };
                let mut s = report(NS_DAV, "sync-collection");
                if c.kind == Kind::Calendar {
                    s += &(report(NS_CALDAV, "calendar-query")
                        + &report(NS_CALDAV, "calendar-multiget")
                        + &report(NS_CALDAV, "free-busy-query"));
                } else {
                    s += &(report(NS_CARDDAV, "addressbook-query")
                        + &report(NS_CARDDAV, "addressbook-multiget"));
                }
                Some(d("supported-report-set", &s))
            }
            _ => None,
        },
        (NS_CS, "getctag") => match res {
            Res::Collection(c) => Some(element(
                NS_CS,
                "getctag",
                &format!("{SYNC_PREFIX}{}", c.revision),
            )),
            _ => None,
        },
        (NS_CALDAV, "calendar-home-set") => matches!(res, Res::Principal(_)).then(|| {
            element(
                NS_CALDAV,
                "calendar-home-set",
                &href(&home_href(Kind::Calendar, &me.email)),
            )
        }),
        (NS_CARDDAV, "addressbook-home-set") => matches!(res, Res::Principal(_)).then(|| {
            element(
                NS_CARDDAV,
                "addressbook-home-set",
                &href(&home_href(Kind::AddressBook, &me.email)),
            )
        }),
        (NS_CALDAV, "calendar-user-address-set") => match res {
            Res::Principal(email) => Some(element(
                NS_CALDAV,
                "calendar-user-address-set",
                &href(&format!("mailto:{email}")),
            )),
            _ => None,
        },
        (NS_CALDAV, "supported-calendar-component-set") => match res {
            Res::Collection(c) if c.kind == Kind::Calendar => Some(element(
                NS_CALDAV,
                "supported-calendar-component-set",
                "<C:comp name=\"VEVENT\"/><C:comp name=\"VTODO\"/>",
            )),
            _ => None,
        },
        (NS_CALDAV, "supported-calendar-data") => match res {
            Res::Collection(c) if c.kind == Kind::Calendar => Some(element(
                NS_CALDAV,
                "supported-calendar-data",
                "<C:calendar-data content-type=\"text/calendar\" version=\"2.0\"/>",
            )),
            _ => None,
        },
        (NS_CARDDAV, "supported-address-data") => match res {
            Res::Collection(c) if c.kind == Kind::AddressBook => Some(element(
                NS_CARDDAV,
                "supported-address-data",
                "<A:address-data-type content-type=\"text/vcard\" version=\"3.0\"/>",
            )),
            _ => None,
        },
        (NS_CALDAV, "max-resource-size") | (NS_CARDDAV, "max-resource-size") => {
            matches!(res, Res::Collection(_)).then(|| {
                element(
                    &p.ns,
                    "max-resource-size",
                    &rustmail_core::dav::MAX_OBJECT_BYTES.to_string(),
                )
            })
        }
        (NS_CALDAV, "calendar-description") => match res {
            Res::Collection(c) if c.kind == Kind::Calendar => Some(element(
                NS_CALDAV,
                "calendar-description",
                &esc(&c.description),
            )),
            _ => None,
        },
        (NS_CARDDAV, "addressbook-description") => match res {
            Res::Collection(c) if c.kind == Kind::AddressBook => Some(element(
                NS_CARDDAV,
                "addressbook-description",
                &esc(&c.description),
            )),
            _ => None,
        },
        (NS_ICAL, "calendar-color") => match res {
            Res::Collection(c) if c.kind == Kind::Calendar => c
                .color
                .as_ref()
                .map(|col| element(NS_ICAL, "calendar-color", &format!("{col}FF"))),
            _ => None,
        },
        (NS_CALDAV, "calendar-data") => match res {
            Res::Object(_, m, Some(data)) if m.component != "VCARD" => {
                Some(element(NS_CALDAV, "calendar-data", &esc(data)))
            }
            _ => None,
        },
        (NS_CARDDAV, "address-data") => match res {
            Res::Object(_, m, Some(data)) if m.component == "VCARD" => {
                Some(element(NS_CARDDAV, "address-data", &esc(data)))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Properties returned for `allprop` and `propname`.
fn default_props() -> Vec<PropName> {
    [
        "resourcetype",
        "displayname",
        "getetag",
        "getcontenttype",
        "getcontentlength",
        "current-user-principal",
        "owner",
        "supported-report-set",
        "sync-token",
        "current-user-privilege-set",
    ]
    .iter()
    .map(|n| PropName::new(NS_DAV, n))
    .collect()
}

fn add_response(
    ms: &mut Multistatus,
    path: &str,
    res: &Res<'_>,
    me: &Principal,
    req: &PropRequest,
) {
    match req {
        PropRequest::Names => {
            let names: Vec<String> = default_props()
                .iter()
                .filter(|p| prop_value(res, me, p).is_some())
                .map(|p| element(&p.ns, &p.local, ""))
                .collect();
            ms.response(path, &names, &[]);
        }
        PropRequest::All | PropRequest::Props(_) => {
            let wanted = match req {
                PropRequest::Props(list) => list.clone(),
                _ => default_props(),
            };
            let (mut found, mut missing) = (Vec::new(), Vec::new());
            for p in wanted {
                match prop_value(res, me, &p) {
                    Some(v) => found.push(v),
                    // allprop only reports what exists; an explicit request reports what does not.
                    None if matches!(req, PropRequest::Props(_)) => missing.push(p),
                    None => {}
                }
            }
            ms.response(path, &found, &missing);
        }
    }
}

// ─── PROPFIND ───────────────────────────────────────────────────────────────

fn depth(headers: &HeaderMap) -> u8 {
    match headers
        .get("depth")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        Some("0") => 0,
        // "infinity" or missing: one level is as deep as this server goes.
        _ => 1,
    }
}

async fn find_collection(
    state: &AppState,
    me: &Principal,
    kind: Kind,
    owner: &str,
    slug: &str,
) -> Result<Collection, Box<Response>> {
    match state.dav.find(me.user_id, owner, kind, slug).await {
        Ok(c) => Ok(c),
        // A user's first calendar and address book are made on first use, so a client that is
        // given the address directly (without browsing their home first) works too.
        Err(DavError::NotFound)
            if same(owner, &me.email)
                && matches!(
                    (kind, slug),
                    (Kind::Calendar, "default") | (Kind::AddressBook, "contacts")
                ) =>
        {
            state
                .dav
                .ensure_defaults(me.user_id)
                .await
                .map_err(|e| Box::new(dav_error(&e)))?;
            state
                .dav
                .find(me.user_id, owner, kind, slug)
                .await
                .map_err(|e| Box::new(dav_error(&e)))
        }
        Err(e) => Err(Box::new(dav_error(&e))),
    }
}

async fn propfind(
    state: &AppState,
    me: &Principal,
    target: &Target,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let req = match x::parse_propfind(body) {
        Ok(r) => r,
        Err(m) => return plain(StatusCode::BAD_REQUEST, &m),
    };
    let deep = depth(headers) >= 1;
    let mut ms = Multistatus::new();
    let only_me = |email: &str| same(email, &me.email);
    match target {
        Target::Root => {
            add_response(&mut ms, "/dav/", &Res::Root, me, &req);
            if deep {
                add_response(&mut ms, "/dav/principals/", &Res::Principals, me, &req);
            }
        }
        Target::Principals => {
            add_response(&mut ms, "/dav/principals/", &Res::Principals, me, &req);
            if deep {
                add_response(
                    &mut ms,
                    &principal_href(&me.email),
                    &Res::Principal(&me.email),
                    me,
                    &req,
                );
            }
        }
        Target::Principal(email) if only_me(email) => {
            add_response(
                &mut ms,
                &principal_href(&me.email),
                &Res::Principal(&me.email),
                me,
                &req,
            );
        }
        Target::Home(kind, email) if only_me(email) => {
            if let Err(e) = state.dav.ensure_defaults(me.user_id).await {
                return dav_error(&e);
            }
            add_response(
                &mut ms,
                &home_href(*kind, &me.email),
                &Res::Home(*kind),
                me,
                &req,
            );
            if deep {
                match state.dav.collections(me.user_id, *kind).await {
                    Ok(list) => {
                        for c in &list {
                            add_response(
                                &mut ms,
                                &collection_href(c),
                                &Res::Collection(c),
                                me,
                                &req,
                            );
                        }
                    }
                    Err(e) => return dav_error(&e),
                }
            }
        }
        Target::Collection(kind, owner, slug) => {
            let c = match find_collection(state, me, *kind, owner, slug).await {
                Ok(c) => c,
                Err(r) => return *r,
            };
            add_response(
                &mut ms,
                &collection_href(&c),
                &Res::Collection(&c),
                me,
                &req,
            );
            if deep {
                match state.dav.list(me.user_id, c.id).await {
                    Ok(list) => {
                        for m in &list {
                            add_response(
                                &mut ms,
                                &object_href(&c, &m.name),
                                &Res::Object(&c, m, None),
                                me,
                                &req,
                            );
                        }
                    }
                    Err(e) => return dav_error(&e),
                }
            }
        }
        Target::Object(kind, owner, slug, name) => {
            let c = match find_collection(state, me, *kind, owner, slug).await {
                Ok(c) => c,
                Err(r) => return *r,
            };
            match state.dav.get(me.user_id, c.id, name).await {
                Ok(o) => add_response(
                    &mut ms,
                    &object_href(&c, name),
                    &Res::Object(&c, &o.meta, None),
                    me,
                    &req,
                ),
                Err(e) => return dav_error(&e),
            }
        }
        // Someone else's principal or home: not revealed.
        Target::Principal(_) | Target::Home(..) => {
            return plain(StatusCode::NOT_FOUND, "Not found.")
        }
    }
    xml(StatusCode::MULTI_STATUS, ms.finish(None))
}

// ─── GET, PUT, DELETE ───────────────────────────────────────────────────────

fn content_type(m: &ObjectMeta) -> &'static str {
    if m.component == "VCARD" {
        "text/vcard; charset=utf-8"
    } else {
        "text/calendar; charset=utf-8"
    }
}

async fn get(state: &AppState, me: &Principal, target: &Target, headers: &HeaderMap) -> Response {
    let Target::Object(kind, owner, slug, name) = target else {
        return (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, ALLOW)]).into_response();
    };
    let c = match find_collection(state, me, *kind, owner, slug).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    match state.dav.get(me.user_id, c.id, name).await {
        Ok(o) => {
            let unchanged = headers
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.trim_matches('"') == o.meta.etag);
            if unchanged {
                return (StatusCode::NOT_MODIFIED, [etag_header(&o.meta.etag)]).into_response();
            }
            (
                StatusCode::OK,
                [
                    etag_header(&o.meta.etag),
                    (
                        header::CONTENT_TYPE,
                        HeaderValue::from_static(content_type(&o.meta)),
                    ),
                ],
                o.data,
            )
                .into_response()
        }
        Err(e) => dav_error(&e),
    }
}

async fn put(
    state: &AppState,
    me: &Principal,
    target: &Target,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let Target::Object(kind, owner, slug, name) = target else {
        return plain(
            StatusCode::METHOD_NOT_ALLOWED,
            "Create items inside a calendar or address book.",
        );
    };
    let pre = match precondition(headers) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let Ok(text) = std::str::from_utf8(body) else {
        return plain(StatusCode::BAD_REQUEST, "The item must be UTF-8 text.");
    };
    let c = match find_collection(state, me, *kind, owner, slug).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    match state.dav.put(me.user_id, c.id, name, text, &pre).await {
        Ok(stored) => (
            if stored.created {
                StatusCode::CREATED
            } else {
                StatusCode::NO_CONTENT
            },
            [etag_header(&stored.etag)],
        )
            .into_response(),
        Err(e) => dav_error(&e),
    }
}

async fn delete(
    state: &AppState,
    me: &Principal,
    target: &Target,
    headers: &HeaderMap,
) -> Response {
    match target {
        Target::Object(kind, owner, slug, name) => {
            let pre = match precondition(headers) {
                Ok(p) => p,
                Err(r) => return *r,
            };
            let c = match find_collection(state, me, *kind, owner, slug).await {
                Ok(c) => c,
                Err(r) => return *r,
            };
            let if_match = if let Precondition::Etag(e) = &pre {
                Some(e.as_str())
            } else {
                None
            };
            match state.dav.delete(me.user_id, c.id, name, if_match).await {
                Ok(()) => StatusCode::NO_CONTENT.into_response(),
                Err(e) => dav_error(&e),
            }
        }
        Target::Collection(kind, owner, slug) => {
            let c = match find_collection(state, me, *kind, owner, slug).await {
                Ok(c) => c,
                Err(r) => return *r,
            };
            match state.dav.delete_collection(me.user_id, c.id).await {
                Ok(()) => StatusCode::NO_CONTENT.into_response(),
                Err(e) => dav_error(&e),
            }
        }
        _ => (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, ALLOW)]).into_response(),
    }
}

// ─── REPORT ─────────────────────────────────────────────────────────────────

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s.trim(), "%Y%m%dT%H%M%SZ")
        .ok()
        .map(|n| Utc.from_utc_datetime(&n))
}

fn format_time(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn time_range(node: roxmltree::Node<'_, '_>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let tr = node.descendants().find(|n| {
        n.is_element()
            && n.tag_name().namespace() == Some(NS_CALDAV)
            && n.tag_name().name() == "time-range"
    })?;
    let from = tr
        .attribute("start")
        .and_then(parse_time)
        .unwrap_or(DateTime::UNIX_EPOCH);
    let to = tr
        .attribute("end")
        .and_then(parse_time)
        .unwrap_or(from + chrono::Duration::days(3650));
    Some((from, to))
}

/// Whether a calendar object has an occurrence in the window. Objects that do not repeat were
/// already matched exactly by the database.
fn in_window(data: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    if !(data.contains("RRULE") || data.contains("RDATE") || data.contains("RECURRENCE-ID")) {
        return true;
    }
    calendar::occurrences(data, from, to)
        .map(|o| !o.is_empty())
        .unwrap_or(true)
}

fn requested_props(root: roxmltree::Node<'_, '_>) -> Vec<PropName> {
    x::child(root, NS_DAV, "prop")
        .map(x::prop_names)
        .unwrap_or_default()
}

struct Matches<'a> {
    object: &'a rustmail_core::dav::store::Object,
}

fn report_response(
    ms: &mut Multistatus,
    c: &Collection,
    m: &Matches<'_>,
    me: &Principal,
    props: &[PropName],
) {
    let res = Res::Object(c, &m.object.meta, Some(&m.object.data));
    add_response(
        ms,
        &object_href(c, &m.object.meta.name),
        &res,
        me,
        &PropRequest::Props(props.to_vec()),
    );
}

#[derive(Debug)]
struct TextFilter {
    property: String,
    text: String,
    kind: String,
    negate: bool,
}

fn card_filters(root: roxmltree::Node<'_, '_>) -> (Vec<TextFilter>, bool) {
    let filter = x::child(root, NS_CARDDAV, "filter");
    let all = filter
        .and_then(|f| f.attribute("test"))
        .is_some_and(|t| t.eq_ignore_ascii_case("allof"));
    let list = filter
        .map(|f| {
            f.children()
                .filter(|n| n.is_element() && n.tag_name().name() == "prop-filter")
                .filter_map(|pf| {
                    let tm = pf
                        .children()
                        .find(|n| n.is_element() && n.tag_name().name() == "text-match")?;
                    Some(TextFilter {
                        property: pf.attribute("name")?.to_ascii_uppercase(),
                        text: tm.text().unwrap_or("").to_lowercase(),
                        kind: tm
                            .attribute("match-type")
                            .unwrap_or("contains")
                            .to_ascii_lowercase(),
                        negate: tm
                            .attribute("negate-condition")
                            .is_some_and(|v| v.eq_ignore_ascii_case("yes")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (list, all)
}

fn card_matches(data: &str, filters: &[TextFilter], all: bool) -> bool {
    if filters.is_empty() {
        return true;
    }
    let Some(card) = contact::read_fields(data) else {
        return false;
    };
    let check = |f: &TextFilter| {
        let values: Vec<String> = match f.property.as_str() {
            "FN" | "N" => vec![card.name.to_lowercase()],
            "EMAIL" => card.emails.iter().map(|v| v.to_lowercase()).collect(),
            "TEL" => card.phones.iter().map(|v| v.to_lowercase()).collect(),
            "ORG" => vec![card.organisation.to_lowercase()],
            "UID" => vec![card.uid.to_lowercase()],
            "NOTE" => vec![card.notes.to_lowercase()],
            _ => Vec::new(),
        };
        let hit = values.iter().any(|v| match f.kind.as_str() {
            "equals" => *v == f.text,
            "starts-with" => v.starts_with(&f.text),
            "ends-with" => v.ends_with(&f.text),
            _ => v.contains(&f.text),
        });
        hit != f.negate
    };
    if all {
        filters.iter().all(check)
    } else {
        filters.iter().any(check)
    }
}

async fn report(state: &AppState, me: &Principal, target: &Target, body: &[u8]) -> Response {
    let Target::Collection(kind, owner, slug) = target else {
        return plain(
            StatusCode::FORBIDDEN,
            "Reports run on a calendar or address book.",
        );
    };
    let doc = match x::parse(body) {
        Ok(d) => d,
        Err(m) => return plain(StatusCode::BAD_REQUEST, &m),
    };
    let root = doc.root_element();
    let (ns, name) = (
        root.tag_name().namespace().unwrap_or(""),
        root.tag_name().name(),
    );
    let c = match find_collection(state, me, *kind, owner, slug).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let props = requested_props(root);
    let mut ms = Multistatus::new();
    match (ns, name, c.kind) {
        (NS_CALDAV, "calendar-multiget", Kind::Calendar)
        | (NS_CARDDAV, "addressbook-multiget", Kind::AddressBook) => {
            let hrefs: Vec<String> = root
                .children()
                .filter(|n| {
                    n.is_element()
                        && n.tag_name().namespace() == Some(NS_DAV)
                        && n.tag_name().name() == "href"
                })
                .filter_map(|n| n.text().map(|t| t.trim().to_owned()))
                .collect();
            if hrefs.len() > MAX_HREFS {
                return plain(StatusCode::BAD_REQUEST, "Too many items asked for at once.");
            }
            let prefix = collection_href(&c);
            let mut wanted: Vec<(String, String)> = Vec::new();
            for h in &hrefs {
                // A client may send an absolute URL or an encoded path.
                let path = h.split_once("://").map_or(h.as_str(), |(_, rest)| {
                    rest.find('/').map_or("/", |i| &rest[i..])
                });
                let name = x::decode_segment(path).and_then(|p| {
                    x::decode_segment(&prefix)
                        .and_then(|pre| p.strip_prefix(pre.as_str()).map(str::to_owned))
                });
                match name {
                    Some(n) if !n.is_empty() && !n.contains('/') => wanted.push((h.clone(), n)),
                    _ => ms.status(h, "HTTP/1.1 404 Not Found"),
                }
            }
            let names: Vec<String> = wanted.iter().map(|(_, n)| n.clone()).collect();
            let found = match state.dav.get_many(me.user_id, c.id, &names).await {
                Ok(f) => f,
                Err(e) => return dav_error(&e),
            };
            for (original, name) in &wanted {
                match found.iter().find(|o| o.meta.name == *name) {
                    Some(o) => report_response(&mut ms, &c, &Matches { object: o }, me, &props),
                    None => ms.status(original, "HTTP/1.1 404 Not Found"),
                }
            }
            xml(StatusCode::MULTI_STATUS, ms.finish(None))
        }
        (NS_CALDAV, "calendar-query", Kind::Calendar) => {
            let range = time_range(root);
            let comp = root
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "comp-filter")
                .filter_map(|n| n.attribute("name"))
                .find(|n| *n != "VCALENDAR")
                .map(str::to_ascii_uppercase);
            let uid = root
                .descendants()
                .find(|n| {
                    n.is_element()
                        && n.tag_name().name() == "prop-filter"
                        && n.attribute("name")
                            .is_some_and(|v| v.eq_ignore_ascii_case("UID"))
                })
                .and_then(|pf| {
                    pf.descendants()
                        .find(|n| n.is_element() && n.tag_name().name() == "text-match")
                })
                .and_then(|tm| tm.text().map(|t| t.trim().to_owned()));
            let objects = match (&range, comp.as_deref()) {
                (Some((from, to)), Some("VEVENT") | None) => {
                    state
                        .dav
                        .events_in_range(me.user_id, c.id, *from, *to)
                        .await
                }
                _ => state.dav.all(me.user_id, c.id).await,
            };
            let objects = match objects {
                Ok(o) => o,
                Err(e) => return dav_error(&e),
            };
            for o in &objects {
                let kind_ok = comp.as_deref().is_none_or(|k| k == o.meta.component);
                let uid_ok = uid.as_deref().is_none_or(|u| o.meta.uid == u);
                let time_ok = match (&range, o.meta.component.as_str()) {
                    (Some((from, to)), "VEVENT") => in_window(&o.data, *from, *to),
                    (Some(_), _) => false,
                    (None, _) => true,
                };
                if kind_ok && uid_ok && time_ok {
                    report_response(&mut ms, &c, &Matches { object: o }, me, &props);
                }
            }
            xml(StatusCode::MULTI_STATUS, ms.finish(None))
        }
        (NS_CARDDAV, "addressbook-query", Kind::AddressBook) => {
            let (filters, all) = card_filters(root);
            let objects = match state.dav.all(me.user_id, c.id).await {
                Ok(o) => o,
                Err(e) => return dav_error(&e),
            };
            let limit = x::child(root, NS_CARDDAV, "limit")
                .and_then(|l| x::child(l, NS_CARDDAV, "nresults"))
                .and_then(|n| n.text())
                .and_then(|t| t.trim().parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            for o in objects
                .iter()
                .filter(|o| card_matches(&o.data, &filters, all))
                .take(limit)
            {
                report_response(&mut ms, &c, &Matches { object: o }, me, &props);
            }
            xml(StatusCode::MULTI_STATUS, ms.finish(None))
        }
        (NS_DAV, "sync-collection", _) => {
            let token = x::child(root, NS_DAV, "sync-token")
                .and_then(|n| n.text())
                .map(str::trim)
                .unwrap_or("");
            let since = if token.is_empty() {
                Some(0)
            } else {
                token
                    .strip_prefix(SYNC_PREFIX)
                    .and_then(|n| n.parse::<i64>().ok())
            };
            let Some(since) = since else {
                return xml(
                    StatusCode::FORBIDDEN,
                    x::error_body("<D:valid-sync-token/>"),
                );
            };
            let changes = match state.dav.changes_since(me.user_id, c.id, since).await {
                Ok(ch) => ch,
                Err(DavError::Invalid(_)) => {
                    return xml(
                        StatusCode::FORBIDDEN,
                        x::error_body("<D:valid-sync-token/>"),
                    )
                }
                Err(e) => return dav_error(&e),
            };
            let wants_data = props
                .iter()
                .any(|p| p.is(NS_CALDAV, "calendar-data") || p.is(NS_CARDDAV, "address-data"));
            let with_data = if wants_data {
                let names: Vec<String> = changes.changed.iter().map(|m| m.name.clone()).collect();
                match state.dav.get_many(me.user_id, c.id, &names).await {
                    Ok(v) => v,
                    Err(e) => return dav_error(&e),
                }
            } else {
                Vec::new()
            };
            for meta in &changes.changed {
                let data = with_data
                    .iter()
                    .find(|o| o.meta.name == meta.name)
                    .map(|o| o.data.as_str());
                add_response(
                    &mut ms,
                    &object_href(&c, &meta.name),
                    &Res::Object(&c, meta, data),
                    me,
                    &PropRequest::Props(props.clone()),
                );
            }
            for name in &changes.deleted {
                ms.status(&object_href(&c, name), "HTTP/1.1 404 Not Found");
            }
            xml(
                StatusCode::MULTI_STATUS,
                ms.finish(Some(&format!("{SYNC_PREFIX}{}", changes.revision))),
            )
        }
        (NS_CALDAV, "free-busy-query", Kind::Calendar) => {
            let Some((from, to)) = time_range(root) else {
                return plain(
                    StatusCode::BAD_REQUEST,
                    "free-busy-query needs a time-range.",
                );
            };
            match state
                .dav
                .free_busy(me.user_id, &c.owner_email, from, to)
                .await
            {
                Ok(periods) => {
                    let mut text = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//RustMail Suite//EN\r\nBEGIN:VFREEBUSY\r\nDTSTART:{}\r\nDTEND:{}\r\nDTSTAMP:{}\r\n", format_time(from), format_time(to), format_time(Utc::now()));
                    for p in periods {
                        text.push_str(&format!(
                            "FREEBUSY;FBTYPE=BUSY:{}/{}\r\n",
                            format_time(p.start),
                            format_time(p.end)
                        ));
                    }
                    text.push_str("END:VFREEBUSY\r\nEND:VCALENDAR\r\n");
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/calendar; charset=utf-8")],
                        text,
                    )
                        .into_response()
                }
                Err(e) => dav_error(&e),
            }
        }
        _ => xml(
            StatusCode::FORBIDDEN,
            x::error_body("<D:supported-report/>"),
        ),
    }
}

// ─── creating and changing collections ─────────────────────────────────────

/// Settings a client sends when making or changing a collection.
#[derive(Default)]
struct Settings {
    display_name: Option<String>,
    description: Option<String>,
    color: Option<Option<String>>,
}

fn read_settings(root: roxmltree::Node<'_, '_>) -> (Settings, Vec<PropName>) {
    let mut s = Settings::default();
    let mut unsupported = Vec::new();
    for set in root.descendants().filter(|n| {
        n.is_element()
            && matches!(n.tag_name().name(), "set" | "remove")
            && n.tag_name().namespace() == Some(NS_DAV)
    }) {
        let removing = set.tag_name().name() == "remove";
        let Some(prop) = x::child(set, NS_DAV, "prop") else {
            continue;
        };
        for p in prop.children().filter(roxmltree::Node::is_element) {
            let (ns, local) = (p.tag_name().namespace().unwrap_or(""), p.tag_name().name());
            let text = p.text().unwrap_or("").trim().to_owned();
            match (ns, local) {
                (NS_DAV, "displayname") if !removing => s.display_name = Some(text),
                (NS_CALDAV, "calendar-description") | (NS_CARDDAV, "addressbook-description") => {
                    s.description = Some(if removing { String::new() } else { text })
                }
                // Apple clients send #RRGGBBAA; the alpha is dropped.
                (NS_ICAL, "calendar-color") => {
                    s.color = Some(if removing {
                        None
                    } else {
                        Some(text.chars().take(7).collect())
                    })
                }
                _ => unsupported.push(PropName::new(ns, local)),
            }
        }
    }
    (s, unsupported)
}

async fn make_collection(
    state: &AppState,
    me: &Principal,
    target: &Target,
    method: &str,
    body: &[u8],
) -> Response {
    let Target::Collection(kind, owner, slug) = target else {
        return plain(
            StatusCode::FORBIDDEN,
            "Collections are made inside your own calendars or address books home.",
        );
    };
    if !same(owner, &me.email) {
        return plain(
            StatusCode::FORBIDDEN,
            "You can only make collections in your own home.",
        );
    }
    let wanted_kind = if method == "MKCALENDAR" {
        Kind::Calendar
    } else {
        Kind::AddressBook
    };
    if *kind != wanted_kind {
        return plain(
            StatusCode::FORBIDDEN,
            "That kind of collection cannot be made here.",
        );
    }
    if state.dav.find(me.user_id, owner, *kind, slug).await.is_ok() {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "It already exists.");
    }
    let settings = if body.iter().all(u8::is_ascii_whitespace) {
        Settings::default()
    } else {
        match x::parse(body) {
            Ok(doc) => read_settings(doc.root_element()).0,
            Err(m) => return plain(StatusCode::BAD_REQUEST, &m),
        }
    };
    let name = settings
        .display_name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| slug.clone());
    match state
        .dav
        .create_collection(
            me.user_id,
            *kind,
            Some(slug),
            &name,
            settings.description.as_deref().unwrap_or(""),
            settings.color.flatten().as_deref(),
        )
        .await
    {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(e) => dav_error(&e),
    }
}

async fn proppatch(state: &AppState, me: &Principal, target: &Target, body: &[u8]) -> Response {
    let Target::Collection(kind, owner, slug) = target else {
        return plain(
            StatusCode::FORBIDDEN,
            "Properties can be changed on calendars and address books.",
        );
    };
    let c = match find_collection(state, me, *kind, owner, slug).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let doc = match x::parse(body) {
        Ok(d) => d,
        Err(m) => return plain(StatusCode::BAD_REQUEST, &m),
    };
    let (settings, unsupported) = read_settings(doc.root_element());
    let update = CollectionUpdate {
        display_name: settings.display_name.clone(),
        description: settings.description.clone(),
        color: settings.color.clone(),
    };
    if let Err(e) = state.dav.update_collection(me.user_id, c.id, &update).await {
        return dav_error(&e);
    }
    let mut applied = Vec::new();
    if settings.display_name.is_some() {
        applied.push(element(NS_DAV, "displayname", ""));
    }
    if settings.description.is_some() {
        applied.push(if c.kind == Kind::Calendar {
            element(NS_CALDAV, "calendar-description", "")
        } else {
            element(NS_CARDDAV, "addressbook-description", "")
        });
    }
    if settings.color.is_some() {
        applied.push(element(NS_ICAL, "calendar-color", ""));
    }
    let mut groups: Vec<(&str, String)> = Vec::new();
    if !applied.is_empty() {
        groups.push(("HTTP/1.1 200 OK", applied.concat()));
    }
    if !unsupported.is_empty() {
        groups.push((
            "HTTP/1.1 403 Forbidden",
            unsupported
                .iter()
                .map(|p| element(&p.ns, &p.local, ""))
                .collect(),
        ));
    }
    let mut ms = Multistatus::new();
    ms.response_grouped(&collection_href(&c), &groups);
    xml(StatusCode::MULTI_STATUS, ms.finish(None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_parsed_strictly() {
        let t = |p: &str| parse_target(p);
        assert_eq!(t(""), Some(Target::Root));
        assert_eq!(t("/"), Some(Target::Root));
        assert_eq!(t("/principals/"), Some(Target::Principals));
        assert_eq!(
            t("/principals/alice%40example.com"),
            Some(Target::Principal("alice@example.com".into()))
        );
        assert_eq!(
            t("/calendars/alice@example.com/"),
            Some(Target::Home(Kind::Calendar, "alice@example.com".into()))
        );
        assert_eq!(
            t("/addressbooks/a@b.de/contacts"),
            Some(Target::Collection(
                Kind::AddressBook,
                "a@b.de".into(),
                "contacts".into()
            ))
        );
        assert_eq!(
            t("/calendars/a@b.de/default/e%201.ics"),
            Some(Target::Object(
                Kind::Calendar,
                "a@b.de".into(),
                "default".into(),
                "e 1.ics".into()
            ))
        );
        for bad in [
            "/calendars/a@b.de/default/x/y",
            "/other/",
            "/calendars/a/b/../c",
            "/calendars/a/%2e%2e/c",
            "/calendars/a/b/c%2Fd",
            "/calendars/a/b/%00",
            "/calendars/a/b/%ff",
            "/calendars/a/b/%zz",
        ] {
            assert_eq!(t(bad), None, "{bad}");
        }
    }

    #[test]
    fn hrefs_nest_and_encode() {
        assert_eq!(
            home_href(Kind::Calendar, "a@b.de"),
            "/dav/calendars/a@b.de/"
        );
        assert_eq!(principal_href("a b@c.de"), "/dav/principals/a%20b@c.de/");
    }

    #[test]
    fn basic_credentials_are_read_carefully() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::AUTHORIZATION, HeaderValue::from_str(v).unwrap());
            m
        };
        let enc = |s: &str| format!("Basic {}", STANDARD.encode(s));
        assert_eq!(
            basic_credentials(&h(&enc("a@b.de:secret:with:colons"))),
            Some(("a@b.de".into(), "secret:with:colons".into()))
        );
        assert_eq!(
            basic_credentials(&h(&format!("basic {}", STANDARD.encode("a:b")))),
            Some(("a".into(), "b".into()))
        );
        for bad in [
            "Bearer abc",
            "Basic !!!",
            "Basic",
            &enc("nocolon"),
            &format!("Basic {}", "A".repeat(2000)),
        ] {
            assert_eq!(basic_credentials(&h(bad)), None, "{bad}");
        }
        assert_eq!(basic_credentials(&HeaderMap::new()), None);
    }

    #[test]
    fn failed_logins_are_limited_and_forgiven() {
        let l = FailureLimiter::default();
        for _ in 0..FailureLimiter::LIMIT - 1 {
            l.record("user:a");
        }
        assert!(l.blocked("user:a").is_none());
        l.record("user:a");
        assert!(l
            .blocked("user:a")
            .is_some_and(|d| d <= FailureLimiter::WINDOW));
        assert!(l.blocked("user:b").is_none(), "other keys are unaffected");
        l.clear("user:a");
        assert!(l.blocked("user:a").is_none());
    }

    #[test]
    fn preconditions_come_from_headers() {
        let mk = |k: HeaderName, v: &str| {
            let mut m = HeaderMap::new();
            m.insert(k, HeaderValue::from_str(v).unwrap());
            m
        };
        assert_eq!(precondition(&HeaderMap::new()).unwrap(), Precondition::None);
        assert_eq!(
            precondition(&mk(header::IF_NONE_MATCH, "*")).unwrap(),
            Precondition::Absent
        );
        assert_eq!(
            precondition(&mk(header::IF_MATCH, "\"abc\"")).unwrap(),
            Precondition::Etag("abc".into())
        );
        assert_eq!(
            precondition(&mk(header::IF_MATCH, "W/\"abc\"")).unwrap(),
            Precondition::Etag("abc".into())
        );
        assert!(precondition(&mk(header::IF_MATCH, "\"a\", \"b\"")).is_err());
    }

    #[test]
    fn time_ranges_are_read() {
        let doc = roxmltree::Document::parse(r#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="20250101T000000Z" end="20250201T000000Z"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#).unwrap();
        let (from, to) = time_range(doc.root_element()).unwrap();
        assert_eq!(
            (format_time(from), format_time(to)),
            ("20250101T000000Z".into(), "20250201T000000Z".into())
        );
        assert!(parse_time("2025-01-01").is_none());
    }

    #[test]
    fn contact_filters_follow_the_request() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:1\r\nFN:Ada Lovelace\r\nEMAIL:ada@example.org\r\nEND:VCARD\r\n";
        let f = |name: &str, text: &str, kind: &str, negate: bool| TextFilter {
            property: name.into(),
            text: text.into(),
            kind: kind.into(),
            negate,
        };
        assert!(card_matches(card, &[], false));
        assert!(card_matches(
            card,
            &[f("FN", "lovelace", "contains", false)],
            false
        ));
        assert!(!card_matches(
            card,
            &[f("FN", "hopper", "contains", false)],
            false
        ));
        assert!(card_matches(
            card,
            &[f("EMAIL", "ada@example.org", "equals", false)],
            false
        ));
        assert!(
            card_matches(
                card,
                &[
                    f("FN", "ada", "starts-with", false),
                    f("EMAIL", "nope", "contains", false)
                ],
                false
            ),
            "anyof"
        );
        assert!(
            !card_matches(
                card,
                &[
                    f("FN", "ada", "starts-with", false),
                    f("EMAIL", "nope", "contains", false)
                ],
                true
            ),
            "allof"
        );
        assert!(
            card_matches(card, &[f("FN", "hopper", "contains", true)], false),
            "negate-condition"
        );
    }

    #[test]
    fn privileges_grow_with_access() {
        assert!(!privileges(Access::Read).contains("D:write"));
        assert!(
            privileges(Access::ReadWrite).contains("D:write")
                && !privileges(Access::ReadWrite).contains("D:all")
        );
        assert!(privileges(Access::Owner).contains("D:all"));
    }
}
