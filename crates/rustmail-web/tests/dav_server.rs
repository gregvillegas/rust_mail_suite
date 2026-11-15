//! Protocol tests for the CalDAV / CardDAV server. Requires `DATABASE_URL`.
//!
//! They speak real WebDAV (PROPFIND, REPORT, PUT, …) to the server and check what a client
//! such as Thunderbird would see: sign-in rules, discovery, the object lifecycle, every
//! supported report, sharing and permissions, and how hostile input is handled.

use std::path::PathBuf;

use axum::http::{Method, StatusCode};
use axum_test::{TestRequest, TestResponse, TestServer};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rustmail_core::auth::{AuthConfig, AuthService};
use rustmail_core::dav::store::{DavStore, GranteeRef};
use rustmail_core::dav::tokens::AppPasswords;
use rustmail_core::domain::{NewDomain, NewUser, UserRole};
use rustmail_core::password::hash_password;
use rustmail_core::repository::{
    DomainRepository, PgDomainRepository, PgUserRepository, UserRepository,
};
use rustmail_web::assets::AssetSource;
use rustmail_web::auth::{AuthState, DeploymentSecurity, RateLimits};
use rustmail_web::config::RateLimitSettings;
use rustmail_web::{build_app, AppState};
use sqlx::PgPool;

const PASSWORD: &str = "correct-horse-battery-staple";
const D: &str = "DAV:";
const C: &str = "urn:ietf:params:xml:ns:caldav";
const A: &str = "urn:ietf:params:xml:ns:carddav";

struct Site {
    server: TestServer,
    alice: String,
    bob: String,
    zed: String,
    store: DavStore,
}

async fn site(pool: &PgPool) -> Site {
    let domains = PgDomainRepository::new(pool.clone());
    let users = PgUserRepository::new(pool.clone());
    let example = domains
        .create(&NewDomain {
            name: "example.com".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let other = domains
        .create(&NewDomain {
            name: "other.org".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    for (domain, local) in [
        (example.id, "alice"),
        (example.id, "bob"),
        (other.id, "zed"),
    ] {
        users
            .create(&NewUser {
                domain_id: domain,
                local_part: local.into(),
                password_hash: hash_password(PASSWORD).unwrap(),
                display_name: local.into(),
                role: UserRole::User,
                locale: "en".into(),
            })
            .await
            .unwrap();
    }
    let auth = AuthState::new(
        AuthService::postgres(
            pool.clone(),
            AuthConfig::new("dav-integration-test-secret-32-bytes!!").unwrap(),
        ),
        DeploymentSecurity {
            cookie_secure: true,
            trust_proxy_headers: true,
        },
    )
    .unwrap();
    let state = AppState::new(
        auth,
        pool.clone(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"),
        AssetSource::Local,
    )
    .unwrap();
    let generous = || RateLimitSettings {
        burst: 1_000,
        period_secs: 1,
    };
    let router = build_app(
        state,
        RateLimits {
            login: generous(),
            refresh: generous(),
        },
    )
    .unwrap()
    .0;
    let mut server = TestServer::new(router);
    server.add_header("x-forwarded-for", "198.51.100.7");
    let apps = AppPasswords::new(pool.clone());
    let mut tokens = Vec::new();
    for (id, email) in [
        ("alice", "alice@example.com"),
        ("bob", "bob@example.com"),
        ("zed", "zed@other.org"),
    ] {
        let uid: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
            .bind(email)
            .fetch_one(pool)
            .await
            .unwrap();
        tokens.push((id, apps.create(uid, "test").await.unwrap()));
    }
    let store = DavStore::new(pool.clone());
    let token = |who: &str| tokens.iter().find(|(n, _)| *n == who).unwrap().1.clone();
    Site {
        server,
        alice: token("alice"),
        bob: token("bob"),
        zed: token("zed"),
        store,
    }
}

fn basic(email: &str, token: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{email}:{token}")))
}

fn dav(site: &Site, method: &str, path: &str, email: &str, token: &str) -> TestRequest {
    site.server
        .method(Method::from_bytes(method.as_bytes()).unwrap(), path)
        .add_header("authorization", basic(email, token))
}

fn alice(site: &Site, method: &str, path: &str) -> TestRequest {
    dav(site, method, path, "alice@example.com", &site.alice)
}

fn bob(site: &Site, method: &str, path: &str) -> TestRequest {
    dav(site, method, path, "bob@example.com", &site.bob)
}

const CAL: &str = "/dav/calendars/alice@example.com/default/";
const BOOK: &str = "/dav/addressbooks/alice@example.com/contacts/";

fn event(uid: &str, summary: &str, start: &str, end: &str, extra: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20250101T000000Z\r\nSUMMARY:{summary}\r\nDTSTART:{start}\r\nDTEND:{end}\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n")
}

fn card(uid: &str, name: &str, email: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nN:;{name};;;\r\nEMAIL:{email}\r\nEND:VCARD\r\n")
}

async fn put(site: &Site, path: &str, body: &str) -> TestResponse {
    alice(site, "PUT", path)
        .content_type("text/calendar")
        .text(body)
        .await
}

/// The `href` of each `response` element of a multistatus.
fn hrefs(body: &str) -> Vec<String> {
    let doc = roxmltree::Document::parse(body).unwrap_or_else(|e| panic!("not XML ({e}): {body}"));
    doc.descendants()
        .filter(|n| n.tag_name().name() == "response")
        .filter_map(|r| r.children().find(|c| c.tag_name().name() == "href"))
        .filter_map(|h| h.text().map(str::to_owned))
        .collect()
}

/// The text of property `{ns}local` of the response whose href is `href`.
fn prop(body: &str, href: &str, ns: &str, local: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(body).unwrap();
    let response = doc.descendants().find(|r| {
        r.tag_name().name() == "response"
            && r.children()
                .any(|c| c.tag_name().name() == "href" && c.text() == Some(href))
    })?;
    let p = response
        .descendants()
        .find(|n| n.tag_name().name() == local && n.tag_name().namespace() == Some(ns))?;
    Some(
        p.descendants()
            .filter(roxmltree::Node::is_text)
            .filter_map(|n| n.text())
            .collect::<String>(),
    )
}

fn status(body: &str, href: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(body).unwrap();
    let r = doc.descendants().find(|r| {
        r.tag_name().name() == "response"
            && r.children()
                .any(|c| c.tag_name().name() == "href" && c.text() == Some(href))
    })?;
    r.children()
        .find(|c| c.tag_name().name() == "status")
        .and_then(|s| s.text().map(str::to_owned))
}

fn propfind_body(props: &str) -> String {
    format!("<?xml version=\"1.0\"?><d:propfind xmlns:d=\"{D}\" xmlns:c=\"{C}\" xmlns:a=\"{A}\" xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:i=\"http://apple.com/ns/ical/\"><d:prop>{props}</d:prop></d:propfind>")
}

// ─── signing in ─────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn only_a_valid_app_password_gets_in(pool: PgPool) {
    let s = site(&pool).await;
    let r = s
        .server
        .method(Method::from_bytes(b"PROPFIND").unwrap(), "/dav/")
        .await;
    r.assert_status(StatusCode::UNAUTHORIZED);
    assert!(r
        .header("www-authenticate")
        .to_str()
        .unwrap()
        .starts_with("Basic realm="));

    for (what, who, pass) in [
        (
            "the account password",
            "alice@example.com",
            PASSWORD.to_owned(),
        ),
        ("a wrong token", "alice@example.com", "0".repeat(32)),
        ("bob's token for alice", "alice@example.com", s.bob.clone()),
        ("an empty password", "alice@example.com", String::new()),
        ("an unknown user", "nobody@example.com", s.alice.clone()),
    ] {
        let r = dav(&s, "PROPFIND", "/dav/", who, &pass).await;
        assert_eq!(r.status_code(), StatusCode::UNAUTHORIZED, "{what}");
    }
    alice(&s, "PROPFIND", "/dav/")
        .add_header("depth", "0")
        .await
        .assert_status(StatusCode::MULTI_STATUS);
    // The user name is not case-sensitive; the token is exact.
    dav(&s, "PROPFIND", "/dav/", "ALICE@Example.COM", &s.alice)
        .add_header("depth", "0")
        .await
        .assert_status(StatusCode::MULTI_STATUS);
    // A web session cookie is not a way in.
    let page = s.server.get("/login").await;
    let csrf = page.cookie("rm_csrf").value().to_owned();
    let login = s
        .server
        .post("/login")
        .add_header("hx-request", "true")
        .add_header("x-csrf-token", csrf.as_str())
        .add_cookie(axum_extra::extract::cookie::Cookie::new(
            "rm_csrf",
            csrf.clone(),
        ))
        .form(&[
            ("email", "alice@example.com"),
            ("password", PASSWORD),
            ("next", "/"),
        ])
        .await;
    login.assert_status_ok();
    let access = login.cookie("rm_access").value().to_owned();
    s.server
        .method(Method::from_bytes(b"PROPFIND").unwrap(), "/dav/")
        .add_cookie(axum_extra::extract::cookie::Cookie::new(
            "rm_access",
            access,
        ))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn repeated_failures_are_slowed_down_and_revoking_works_at_once(pool: PgPool) {
    let s = site(&pool).await;
    for _ in 0..10 {
        dav(&s, "OPTIONS", "/dav/", "alice@example.com", "wrong")
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    let r = dav(&s, "OPTIONS", "/dav/", "alice@example.com", &s.alice).await;
    r.assert_status(StatusCode::TOO_MANY_REQUESTS);
    assert!(
        r.header("retry-after")
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 0
    );

    // The limit is per client address and per user name: from the same address nobody gets in
    // while it lasts, however good their credentials.
    bob(&s, "OPTIONS", "/dav/")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_revoked_app_password_stops_working_immediately(pool: PgPool) {
    let s = site(&pool).await;
    alice(&s, "OPTIONS", "/dav/").await.assert_status_ok();
    let apps = AppPasswords::new(pool.clone());
    let uid: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'alice@example.com'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let id = apps.list(uid).await.unwrap()[0].id;
    apps.revoke(uid, id).await.unwrap();
    alice(&s, "OPTIONS", "/dav/")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn clients_are_pointed_at_the_server_by_the_well_known_addresses(pool: PgPool) {
    let s = site(&pool).await;
    for path in ["/.well-known/caldav", "/.well-known/carddav"] {
        let r = s.server.get(path).await;
        r.assert_status(StatusCode::MOVED_PERMANENTLY);
        assert_eq!(r.header("location").to_str().unwrap(), "/dav/");
    }
    let r = alice(&s, "OPTIONS", "/dav/").await;
    let dav_header = r.header("dav").to_str().unwrap().to_owned();
    assert!(
        dav_header.contains("calendar-access") && dav_header.contains("addressbook"),
        "{dav_header}"
    );
    assert!(r.header("allow").to_str().unwrap().contains("REPORT"));
}

// ─── discovery ──────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn a_client_can_discover_calendars_and_address_books_from_the_root(pool: PgPool) {
    let s = site(&pool).await;
    // 1. Who am I?
    let root = alice(&s, "PROPFIND", "/dav/")
        .add_header("depth", "0")
        .text(propfind_body("<d:current-user-principal/>"))
        .await;
    root.assert_status(StatusCode::MULTI_STATUS);
    let body = root.text();
    assert_eq!(
        prop(&body, "/dav/", D, "current-user-principal").as_deref(),
        Some("/dav/principals/alice@example.com/")
    );
    // 2. Where are my calendars and address books?
    let principal = alice(&s, "PROPFIND", "/dav/principals/alice@example.com/")
        .add_header("depth", "0")
        .text(propfind_body(
            "<c:calendar-home-set/><a:addressbook-home-set/><d:displayname/>",
        ))
        .await
        .text();
    let p = "/dav/principals/alice@example.com/";
    assert_eq!(
        prop(&principal, p, C, "calendar-home-set").as_deref(),
        Some("/dav/calendars/alice@example.com/")
    );
    assert_eq!(
        prop(&principal, p, A, "addressbook-home-set").as_deref(),
        Some("/dav/addressbooks/alice@example.com/")
    );
    // 3. What is in them?
    let cals = alice(&s, "PROPFIND", "/dav/calendars/alice@example.com/").add_header("depth", "1").text(propfind_body("<d:resourcetype/><d:displayname/><cs:getctag/><d:sync-token/><i:calendar-color/><c:supported-calendar-component-set/><d:current-user-privilege-set/>")).await.text();
    assert_eq!(hrefs(&cals), ["/dav/calendars/alice@example.com/", CAL]);
    assert_eq!(
        prop(&cals, CAL, D, "displayname").as_deref(),
        Some("Calendar")
    );
    let doc = roxmltree::Document::parse(&cals).unwrap();
    assert!(
        doc.descendants()
            .any(|n| n.tag_name().name() == "calendar" && n.tag_name().namespace() == Some(C)),
        "marked as a calendar"
    );
    assert!(prop(&cals, CAL, D, "sync-token")
        .unwrap()
        .starts_with("urn:rustmail:sync:"));
    assert!(prop(&cals, CAL, "http://calendarserver.org/ns/", "getctag").is_some());
    assert!(prop(&cals, CAL, D, "current-user-privilege-set").is_some());
    let books = alice(&s, "PROPFIND", "/dav/addressbooks/alice@example.com/")
        .add_header("depth", "1")
        .text(propfind_body("<d:resourcetype/><d:displayname/>"))
        .await
        .text();
    assert_eq!(
        hrefs(&books),
        ["/dav/addressbooks/alice@example.com/", BOOK]
    );
    assert!(roxmltree::Document::parse(&books)
        .unwrap()
        .descendants()
        .any(|n| n.tag_name().name() == "addressbook" && n.tag_name().namespace() == Some(A)));
    // An unknown property is reported as such, not made up.
    let odd = alice(&s, "PROPFIND", CAL)
        .add_header("depth", "0")
        .text(propfind_body("<d:displayname/><x:odd xmlns:x=\"urn:x\"/>"))
        .await
        .text();
    assert!(
        odd.contains("404 Not Found") && odd.contains("odd") && odd.contains("200 OK"),
        "{odd}"
    );
    // allprop with no body works too.
    alice(&s, "PROPFIND", CAL)
        .add_header("depth", "0")
        .await
        .assert_status(StatusCode::MULTI_STATUS);
    // Nobody else's principal or home is visible.
    for path in [
        "/dav/principals/bob@example.com/",
        "/dav/calendars/bob@example.com/",
        "/dav/addressbooks/zed@other.org/",
    ] {
        alice(&s, "PROPFIND", path)
            .add_header("depth", "0")
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}

// ─── the life of an event ───────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn an_event_is_created_read_changed_and_deleted_with_etags(pool: PgPool) {
    let s = site(&pool).await;
    let path = format!("{CAL}standup.ics");
    let v1 = event("u1", "Standup", "20250310T090000Z", "20250310T093000Z", "");

    let created = put(&s, &path, &v1).await;
    created.assert_status(StatusCode::CREATED);
    let etag = created.header("etag").to_str().unwrap().to_owned();
    assert!(etag.starts_with('"') && etag.ends_with('"'));

    let got = alice(&s, "GET", &path).await;
    got.assert_status_ok();
    assert_eq!(got.text(), v1, "returned exactly as stored, CRLF and all");
    assert!(got
        .header("content-type")
        .to_str()
        .unwrap()
        .starts_with("text/calendar"));
    assert_eq!(got.header("etag").to_str().unwrap(), etag);
    alice(&s, "GET", &path)
        .add_header("if-none-match", etag.as_str())
        .await
        .assert_status(StatusCode::NOT_MODIFIED);

    // It is listed with its etag and type.
    let list = alice(&s, "PROPFIND", CAL)
        .add_header("depth", "1")
        .text(propfind_body(
            "<d:getetag/><d:getcontenttype/><d:getcontentlength/>",
        ))
        .await
        .text();
    assert_eq!(
        prop(&list, &path, D, "getetag").as_deref(),
        Some(etag.as_str())
    );
    assert!(prop(&list, &path, D, "getcontenttype")
        .unwrap()
        .contains("component=VEVENT"));
    assert_eq!(
        prop(&list, &path, D, "getcontentlength").unwrap(),
        v1.len().to_string()
    );

    // Creating over it, or changing it from a stale copy, is refused.
    alice(&s, "PUT", &path)
        .add_header("if-none-match", "*")
        .text(&v1)
        .await
        .assert_status(StatusCode::PRECONDITION_FAILED);
    let v2 = event(
        "u1",
        "Standup (moved)",
        "20250310T100000Z",
        "20250310T103000Z",
        "",
    );
    alice(&s, "PUT", &path)
        .add_header("if-match", "\"stale\"")
        .text(&v2)
        .await
        .assert_status(StatusCode::PRECONDITION_FAILED);
    let updated = alice(&s, "PUT", &path)
        .add_header("if-match", etag.as_str())
        .text(&v2)
        .await;
    updated.assert_status(StatusCode::NO_CONTENT);
    assert_ne!(updated.header("etag").to_str().unwrap(), etag);
    assert_eq!(alice(&s, "GET", &path).await.text(), v2);

    // Another event cannot reuse the UID.
    let clash = alice(&s, "PUT", &format!("{CAL}copy.ics"))
        .text(event(
            "u1",
            "Copy",
            "20250311T090000Z",
            "20250311T093000Z",
            "",
        ))
        .await;
    clash.assert_status(StatusCode::CONFLICT);

    alice(&s, "DELETE", &path)
        .add_header("if-match", etag.as_str())
        .await
        .assert_status(StatusCode::PRECONDITION_FAILED);
    alice(&s, "DELETE", &path)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    alice(&s, "GET", &path)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    alice(&s, "DELETE", &path)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn bad_uploads_are_refused_with_a_reason(pool: PgPool) {
    let s = site(&pool).await;
    let r = put(&s, &format!("{CAL}x.ics"), "this is not a calendar").await;
    r.assert_status(StatusCode::BAD_REQUEST);
    assert!(r.text().contains("iCalendar"), "{}", r.text());
    let r = alice(&s, "PUT", &format!("{BOOK}x.vcf"))
        .text(event(
            "u",
            "Wrong kind",
            "20250310T090000Z",
            "20250310T093000Z",
            "",
        ))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    put(
        &s,
        &format!("{CAL}no%20good.ics"),
        &event("u", "x", "20250310T090000Z", "20250310T093000Z", ""),
    )
    .await
    .assert_status(StatusCode::BAD_REQUEST);
    alice(&s, "PUT", &format!("{CAL}bin.ics"))
        .bytes(vec![0xff, 0xfe, 0x00].into())
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    // Too large: cut off by the body limit before it is read.
    let big = format!(
        "{}{}",
        event("big", "Big", "20250310T090000Z", "20250310T093000Z", ""),
        "X".repeat(2 * 1024 * 1024)
    );
    alice(&s, "PUT", &format!("{CAL}big.ics"))
        .text(&big)
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    // A collection cannot be PUT; unsupported methods say so.
    alice(&s, "PUT", CAL)
        .text("x")
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    alice(&s, "COPY", &format!("{CAL}a.ics"))
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    alice(&s, "LOCK", &format!("{CAL}a.ics"))
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    let missing: i64 = sqlx::query_scalar("SELECT count(*) FROM dav_objects")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(missing, 0, "nothing was stored");
}

// ─── reports ────────────────────────────────────────────────────────────────

fn query_body(start: &str, end: &str) -> String {
    format!("<c:calendar-query xmlns:d=\"{D}\" xmlns:c=\"{C}\"><d:prop><d:getetag/><c:calendar-data/></d:prop><c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:time-range start=\"{start}\" end=\"{end}\"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>")
}

#[sqlx::test(migrations = "../../migrations")]
async fn calendar_reports_find_events_by_time_href_and_uid(pool: PgPool) {
    let s = site(&pool).await;
    let weekly = event(
        "w",
        "Weekly",
        "20250106T090000Z",
        "20250106T100000Z",
        "RRULE:FREQ=WEEKLY;COUNT=4\r\n",
    );
    let once = event("o", "Offsite", "20250115T090000Z", "20250115T170000Z", "");
    for (n, e) in [("weekly.ics", &weekly), ("once.ics", &once)] {
        put(&s, &format!("{CAL}{n}"), e)
            .await
            .assert_status(StatusCode::CREATED);
    }
    let all_events = |r: TestResponse| hrefs(&r.text());

    // January: both. The weekly event's later Mondays are inside the window.
    let jan = alice(&s, "REPORT", CAL)
        .add_header("depth", "1")
        .text(query_body("20250101T000000Z", "20250201T000000Z"))
        .await;
    jan.assert_status(StatusCode::MULTI_STATUS);
    let body = jan.text();
    let mut found = hrefs(&body);
    found.sort();
    assert_eq!(
        found,
        [format!("{CAL}once.ics"), format!("{CAL}weekly.ics")]
    );
    assert_eq!(
        prop(&body, &format!("{CAL}once.ics"), C, "calendar-data").as_deref(),
        Some(once.as_str()),
        "data survives XML with its CRLF line ends"
    );
    // Only the last weekly occurrence (27 Jan) falls in this window.
    assert_eq!(
        all_events(
            alice(&s, "REPORT", CAL)
                .text(query_body("20250127T000000Z", "20250128T000000Z"))
                .await
        ),
        [format!("{CAL}weekly.ics")]
    );
    // A window with nothing in it, and one after the series ended.
    assert!(all_events(
        alice(&s, "REPORT", CAL)
            .text(query_body("20250121T000000Z", "20250122T000000Z"))
            .await
    )
    .is_empty());
    assert!(all_events(
        alice(&s, "REPORT", CAL)
            .text(query_body("20250301T000000Z", "20250401T000000Z"))
            .await
    )
    .is_empty());

    // By UID, with no time range.
    let by_uid = format!("<c:calendar-query xmlns:d=\"{D}\" xmlns:c=\"{C}\"><d:prop><d:getetag/></d:prop><c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:prop-filter name=\"UID\"><c:text-match>o</c:text-match></c:prop-filter></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>");
    assert_eq!(
        all_events(alice(&s, "REPORT", CAL).text(&by_uid).await),
        [format!("{CAL}once.ics")]
    );

    // Multiget: what was asked for, with 404 for what is not there or outside the collection.
    let multiget = format!(
        "<c:calendar-multiget xmlns:d=\"{D}\" xmlns:c=\"{C}\"><d:prop><d:getetag/><c:calendar-data/></d:prop><d:href>{CAL}once.ics</d:href><d:href>{CAL}missing.ics</d:href><d:href>/dav/calendars/bob@example.com/default/x.ics</d:href><d:href>{CAL}../../x</d:href></c:calendar-multiget>"
    );
    let r = alice(&s, "REPORT", CAL).text(&multiget).await;
    r.assert_status(StatusCode::MULTI_STATUS);
    let body = r.text();
    assert_eq!(
        prop(&body, &format!("{CAL}once.ics"), C, "calendar-data").as_deref(),
        Some(once.as_str())
    );
    for gone in [
        format!("{CAL}missing.ics"),
        "/dav/calendars/bob@example.com/default/x.ics".to_owned(),
        format!("{CAL}../../x"),
    ] {
        assert!(
            status(&body, &gone).is_some_and(|s| s.contains("404")),
            "{gone}: {body}"
        );
    }
    // Reports that do not exist, or that belong to the other kind of collection, are refused.
    alice(&s, "REPORT", CAL)
        .text(format!("<x:nope xmlns:x=\"{D}\"/>"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    alice(&s, "REPORT", BOOK)
        .text(query_body("20250101T000000Z", "20250201T000000Z"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    alice(&s, "REPORT", CAL)
        .text("<broken")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sync_collection_reports_only_what_changed(pool: PgPool) {
    let s = site(&pool).await;
    let sync = |token: &str| {
        format!("<d:sync-collection xmlns:d=\"{D}\" xmlns:c=\"{C}\"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/><c:calendar-data/></d:prop></d:sync-collection>")
    };
    for (n, uid) in [("a.ics", "ua"), ("b.ics", "ub")] {
        put(
            &s,
            &format!("{CAL}{n}"),
            &event(uid, uid, "20250310T090000Z", "20250310T100000Z", ""),
        )
        .await
        .assert_status(StatusCode::CREATED);
    }
    let first = alice(&s, "REPORT", CAL).text(sync("")).await;
    first.assert_status(StatusCode::MULTI_STATUS);
    let body = first.text();
    assert_eq!(hrefs(&body).len(), 2, "the first sync lists everything");
    assert!(
        prop(&body, &format!("{CAL}a.ics"), C, "calendar-data")
            .unwrap()
            .contains("UID:ua"),
        "data is included when asked for"
    );
    let token = roxmltree::Document::parse(&body)
        .unwrap()
        .root_element()
        .children()
        .find(|n| n.tag_name().name() == "sync-token")
        .and_then(|n| n.text().map(str::to_owned))
        .unwrap();

    put(
        &s,
        &format!("{CAL}c.ics"),
        &event("uc", "uc", "20250311T090000Z", "20250311T100000Z", ""),
    )
    .await;
    alice(&s, "DELETE", &format!("{CAL}a.ics"))
        .await
        .assert_status(StatusCode::NO_CONTENT);
    let next = alice(&s, "REPORT", CAL).text(sync(&token)).await.text();
    let mut seen = hrefs(&next);
    seen.sort();
    assert_eq!(
        seen,
        [format!("{CAL}a.ics"), format!("{CAL}c.ics")],
        "the new item and the removed one; not b.ics"
    );
    assert!(status(&next, &format!("{CAL}a.ics"))
        .unwrap()
        .contains("404"));
    let token2 = roxmltree::Document::parse(&next)
        .unwrap()
        .root_element()
        .children()
        .find(|n| n.tag_name().name() == "sync-token")
        .and_then(|n| n.text().map(str::to_owned))
        .unwrap();
    assert_ne!(token2, token);
    assert!(
        hrefs(&alice(&s, "REPORT", CAL).text(sync(&token2)).await.text()).is_empty(),
        "nothing changed since"
    );

    // A token the server did not issue is refused in the way clients understand.
    for bad in ["garbage", "urn:rustmail:sync:99999", "http://other/sync/1"] {
        let r = alice(&s, "REPORT", CAL).text(sync(bad)).await;
        r.assert_status(StatusCode::FORBIDDEN);
        assert!(r.text().contains("valid-sync-token"), "{bad}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn free_busy_query_returns_times_without_titles(pool: PgPool) {
    let s = site(&pool).await;
    put(
        &s,
        &format!("{CAL}m.ics"),
        &event(
            "m",
            "TOP SECRET MERGER",
            "20250310T090000Z",
            "20250310T100000Z",
            "",
        ),
    )
    .await;
    let q = format!("<c:free-busy-query xmlns:c=\"{C}\"><c:time-range start=\"20250310T000000Z\" end=\"20250311T000000Z\"/></c:free-busy-query>");
    let r = alice(&s, "REPORT", CAL).text(&q).await;
    r.assert_status_ok();
    let text = r.text();
    assert!(
        text.contains("BEGIN:VFREEBUSY")
            && text.contains("FREEBUSY;FBTYPE=BUSY:20250310T090000Z/20250310T100000Z"),
        "{text}"
    );
    assert!(!text.contains("SECRET"));
    // A colleague in the same domain, once allowed to reach the calendar, gets the same; for others it is simply unavailable.
    s.store
        .share(
            uuid_of(&pool, "alice@example.com").await,
            s.store
                .collections(
                    uuid_of(&pool, "alice@example.com").await,
                    rustmail_core::dav::store::Kind::Calendar,
                )
                .await
                .unwrap()[0]
                .id,
            &GranteeRef::UserEmail("bob@example.com".into()),
            false,
        )
        .await
        .unwrap();
    let theirs = bob(&s, "REPORT", CAL).text(&q).await;
    theirs.assert_status_ok();
    assert!(
        theirs.text().contains("20250310T090000Z/20250310T100000Z")
            && !theirs.text().contains("SECRET")
    );
    alice(&s, "REPORT", CAL)
        .text(format!("<c:free-busy-query xmlns:c=\"{C}\"/>"))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

async fn uuid_of(pool: &PgPool, email: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn contacts_are_stored_queried_and_returned_intact(pool: PgPool) {
    let s = site(&pool).await;
    let ada = card("c1", "Ada Lovelace", "ada@example.org");
    let grace = card("c2", "Grace Hopper", "grace@navy.example");
    for (n, c) in [("ada.vcf", &ada), ("grace.vcf", &grace)] {
        let r = alice(&s, "PUT", &format!("{BOOK}{n}"))
            .content_type("text/vcard")
            .text(c)
            .await;
        r.assert_status(StatusCode::CREATED);
    }
    let got = alice(&s, "GET", &format!("{BOOK}ada.vcf")).await;
    assert_eq!(got.text(), ada);
    assert!(got
        .header("content-type")
        .to_str()
        .unwrap()
        .starts_with("text/vcard"));
    let query = |name: &str, text: &str, kind: &str| {
        format!("<a:addressbook-query xmlns:d=\"{D}\" xmlns:a=\"{A}\"><d:prop><d:getetag/><a:address-data/></d:prop><a:filter><a:prop-filter name=\"{name}\"><a:text-match match-type=\"{kind}\">{text}</a:text-match></a:prop-filter></a:filter></a:addressbook-query>")
    };
    let by_email = alice(&s, "REPORT", BOOK)
        .text(query("EMAIL", "NAVY", "contains"))
        .await
        .text();
    assert_eq!(hrefs(&by_email), [format!("{BOOK}grace.vcf")]);
    assert_eq!(
        prop(&by_email, &format!("{BOOK}grace.vcf"), A, "address-data").as_deref(),
        Some(grace.as_str())
    );
    assert_eq!(
        hrefs(
            &alice(&s, "REPORT", BOOK)
                .text(query("FN", "ada", "starts-with"))
                .await
                .text()
        ),
        [format!("{BOOK}ada.vcf")]
    );
    assert!(hrefs(
        &alice(&s, "REPORT", BOOK)
            .text(query("FN", "nobody", "equals"))
            .await
            .text()
    )
    .is_empty());
    let everyone = format!("<a:addressbook-query xmlns:d=\"{D}\" xmlns:a=\"{A}\"><d:prop><d:getetag/></d:prop></a:addressbook-query>");
    assert_eq!(
        hrefs(&alice(&s, "REPORT", BOOK).text(&everyone).await.text()).len(),
        2
    );
    let multiget = format!("<a:addressbook-multiget xmlns:d=\"{D}\" xmlns:a=\"{A}\"><d:prop><a:address-data/></d:prop><d:href>{BOOK}ada.vcf</d:href></a:addressbook-multiget>");
    assert!(alice(&s, "REPORT", BOOK)
        .text(&multiget)
        .await
        .text()
        .contains("Ada Lovelace"));
    // A card without a UID or name is refused with a reason.
    let r = alice(&s, "PUT", &format!("{BOOK}bad.vcf"))
        .text("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:No Uid\r\nEND:VCARD\r\n")
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    assert!(r.text().contains("UID"));
}

// ─── who may do what ────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn other_users_cannot_see_or_change_a_collection(pool: PgPool) {
    let s = site(&pool).await;
    let path = format!("{CAL}secret.ics");
    put(
        &s,
        &path,
        &event("u1", "Private", "20250310T090000Z", "20250310T100000Z", ""),
    )
    .await;
    for (who, email, token) in [
        ("bob", "bob@example.com", s.bob.clone()),
        ("zed", "zed@other.org", s.zed.clone()),
    ] {
        let go = |m: &str, p: &str| dav(&s, m, p, email, &token);
        go("GET", &path).await.assert_status(StatusCode::NOT_FOUND);
        go("PUT", &format!("{CAL}x.ics"))
            .text(event("u2", "x", "20250310T090000Z", "20250310T100000Z", ""))
            .await
            .assert_status(StatusCode::NOT_FOUND);
        go("DELETE", &path)
            .await
            .assert_status(StatusCode::NOT_FOUND);
        go("PROPFIND", CAL)
            .add_header("depth", "1")
            .await
            .assert_status(StatusCode::NOT_FOUND);
        go("REPORT", CAL)
            .text(query_body("20250101T000000Z", "20260101T000000Z"))
            .await
            .assert_status(StatusCode::NOT_FOUND);
        let r = go("DELETE", CAL).await;
        assert_eq!(r.status_code(), StatusCode::NOT_FOUND, "{who}");
    }
    alice(&s, "GET", &path).await.assert_status_ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_shared_calendar_appears_for_the_colleague_with_the_right_rights(pool: PgPool) {
    let s = site(&pool).await;
    let path = format!("{CAL}lunch.ics");
    put(
        &s,
        &path,
        &event(
            "u1",
            "Team lunch",
            "20250310T120000Z",
            "20250310T130000Z",
            "",
        ),
    )
    .await;
    let alice_id = uuid_of(&pool, "alice@example.com").await;
    let cal_id = s
        .store
        .collections(alice_id, rustmail_core::dav::store::Kind::Calendar)
        .await
        .unwrap()[0]
        .id;
    s.store
        .share(
            alice_id,
            cal_id,
            &GranteeRef::UserEmail("bob@example.com".into()),
            false,
        )
        .await
        .unwrap();

    // Bob finds it in his own home listing, at alice's address, labelled with its owner.
    let home = bob(&s, "PROPFIND", "/dav/calendars/bob@example.com/")
        .add_header("depth", "1")
        .text(propfind_body(
            "<d:displayname/><d:owner/><d:current-user-privilege-set/>",
        ))
        .await
        .text();
    let listed = hrefs(&home);
    assert!(
        listed.contains(&CAL.to_owned())
            && listed.contains(&"/dav/calendars/bob@example.com/default/".to_owned()),
        "{listed:?}"
    );
    assert_eq!(
        prop(&home, CAL, D, "displayname").as_deref(),
        Some("Calendar (alice@example.com)")
    );
    assert_eq!(
        prop(&home, CAL, D, "owner").as_deref(),
        Some("/dav/principals/alice@example.com/")
    );
    assert!(prop(&home, CAL, D, "current-user-privilege-set").is_some());
    // 20 January is a Monday, so the weekly event does occur then.
    let privileges = |body: &str| {
        roxmltree::Document::parse(body)
            .unwrap()
            .descendants()
            .filter(|n| {
                n.tag_name().namespace() == Some(D)
                    && matches!(
                        n.tag_name().name(),
                        "write" | "write-content" | "bind" | "unbind"
                    )
            })
            .count()
    };
    assert_eq!(
        privileges(
            &home
                .split("<D:response>")
                .find(|r| r.contains("alice@example.com/default"))
                .map(|r| format!(
                    "<x xmlns:D=\"{D}\" xmlns:C=\"{C}\">{}",
                    r.replace("</D:response>", "</x>")
                        .replace("</D:multistatus>", "")
                ))
                .unwrap_or_default()
        ),
        0,
        "read-only has no write privilege"
    );

    // Read-only: reading works, writing does not, and nothing changed.
    bob(&s, "GET", &path).await.assert_status_ok();
    bob(&s, "REPORT", CAL)
        .text(query_body("20250301T000000Z", "20250401T000000Z"))
        .await
        .assert_status(StatusCode::MULTI_STATUS);
    let attempt = event("u2", "Bob's", "20250311T090000Z", "20250311T100000Z", "");
    bob(&s, "PUT", &format!("{CAL}bobs.ics"))
        .text(&attempt)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    bob(&s, "DELETE", &path)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    bob(&s, "DELETE", CAL)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    bob(&s, "PROPPATCH", CAL).text(format!("<d:propertyupdate xmlns:d=\"{D}\"><d:set><d:prop><d:displayname>Mine</d:displayname></d:prop></d:set></d:propertyupdate>")).await.assert_status(StatusCode::FORBIDDEN);
    alice(&s, "GET", &path).await.assert_status_ok();

    // Read/write: bob may add and change, alice sees it, but bob still cannot delete the calendar.
    s.store
        .share(
            alice_id,
            cal_id,
            &GranteeRef::UserEmail("bob@example.com".into()),
            true,
        )
        .await
        .unwrap();
    bob(&s, "PUT", &format!("{CAL}bobs.ics"))
        .text(&attempt)
        .await
        .assert_status(StatusCode::CREATED);
    assert!(alice(&s, "GET", &format!("{CAL}bobs.ics"))
        .await
        .text()
        .contains("Bob's"));
    bob(&s, "DELETE", CAL)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // Withdrawn: it disappears for bob.
    let share = s.store.shares(alice_id, cal_id).await.unwrap().remove(0);
    s.store.unshare(alice_id, cal_id, share.id).await.unwrap();
    bob(&s, "GET", &path)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert!(!hrefs(
        &bob(&s, "PROPFIND", "/dav/calendars/bob@example.com/")
            .add_header("depth", "1")
            .await
            .text()
    )
    .contains(&CAL.to_owned()));
}

#[sqlx::test(migrations = "../../migrations")]
async fn collections_can_be_made_renamed_and_removed_by_a_client(pool: PgPool) {
    let s = site(&pool).await;
    let work = "/dav/calendars/alice@example.com/work/";
    let body = format!("<c:mkcalendar xmlns:d=\"{D}\" xmlns:c=\"{C}\" xmlns:i=\"http://apple.com/ns/ical/\"><d:set><d:prop><d:displayname>Work</d:displayname><c:calendar-description>Meetings</c:calendar-description><i:calendar-color>#3366CCFF</i:calendar-color></d:prop></d:set></c:mkcalendar>");
    alice(&s, "MKCALENDAR", work)
        .text(&body)
        .await
        .assert_status(StatusCode::CREATED);
    alice(&s, "MKCALENDAR", work)
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    let info = alice(&s, "PROPFIND", work)
        .add_header("depth", "0")
        .text(propfind_body(
            "<d:displayname/><i:calendar-color/><c:calendar-description/>",
        ))
        .await
        .text();
    assert_eq!(
        (
            prop(&info, work, D, "displayname").as_deref(),
            prop(&info, work, "http://apple.com/ns/ical/", "calendar-color").as_deref(),
            prop(&info, work, C, "calendar-description").as_deref()
        ),
        (Some("Work"), Some("#3366CCFF"), Some("Meetings"))
    );
    // Nobody makes collections in someone else's home, and a calendar is not made with MKCOL.
    alice(&s, "MKCALENDAR", "/dav/calendars/bob@example.com/sneaky/")
        .await
        .assert_status(StatusCode::FORBIDDEN);
    alice(&s, "MKCOL", "/dav/calendars/alice@example.com/plain/")
        .await
        .assert_status(StatusCode::FORBIDDEN);
    alice(
        &s,
        "MKCALENDAR",
        "/dav/calendars/alice@example.com/Bad%20Slug/",
    )
    .await
    .assert_status(StatusCode::BAD_REQUEST);
    alice(&s, "MKCOL", "/dav/addressbooks/alice@example.com/friends/")
        .text("")
        .await
        .assert_status(StatusCode::CREATED);

    let patch = format!("<d:propertyupdate xmlns:d=\"{D}\" xmlns:c=\"{C}\"><d:set><d:prop><d:displayname>Work stuff</d:displayname><d:getetag>hack</d:getetag></d:prop></d:set></d:propertyupdate>");
    let r = alice(&s, "PROPPATCH", work).text(&patch).await;
    r.assert_status(StatusCode::MULTI_STATUS);
    assert!(
        r.text().contains("200 OK") && r.text().contains("403 Forbidden"),
        "{}",
        r.text()
    );
    assert_eq!(
        prop(
            &alice(&s, "PROPFIND", work)
                .add_header("depth", "0")
                .text(propfind_body("<d:displayname/>"))
                .await
                .text(),
            work,
            D,
            "displayname"
        )
        .as_deref(),
        Some("Work stuff")
    );

    put(
        &s,
        &format!("{work}a.ics"),
        &event("u1", "x", "20250310T090000Z", "20250310T100000Z", ""),
    )
    .await
    .assert_status(StatusCode::CREATED);
    alice(&s, "DELETE", work)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    alice(&s, "GET", &format!("{work}a.ics"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn hostile_requests_are_refused_safely(pool: PgPool) {
    let s = site(&pool).await;
    // XML tricks and absurd sizes.
    let dtd = "<?xml version=\"1.0\"?><!DOCTYPE x [<!ENTITY a \"aaaa\">]><d:propfind xmlns:d=\"DAV:\"><d:prop><d:displayname>&a;</d:displayname></d:prop></d:propfind>";
    alice(&s, "PROPFIND", CAL)
        .text(dtd)
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    alice(&s, "PROPFIND", CAL)
        .text("<d:propfind xmlns:d=\"DAV:\"><d:prop>")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    alice(&s, "REPORT", CAL)
        .bytes(vec![b'<'; 3 * 1024 * 1024].into())
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    // Paths that try to leave their place, or name things that cannot exist.
    for path in [
        "/dav/calendars/alice@example.com/default/%2e%2e/%2e%2e/x",
        "/dav/calendars/alice@example.com/default/a%2Fb.ics",
        "/dav/calendars/alice@example.com/default/%00",
        "/dav/calendars/alice@example.com/default/a/b/c",
        "/dav/nothing/here",
        "/dav/calendars/alice@example.com/nosuch/",
    ] {
        let r = alice(&s, "PROPFIND", path).add_header("depth", "0").await;
        assert_eq!(r.status_code(), StatusCode::NOT_FOUND, "{path}");
    }
    // Hostile text in what a client stores cannot break the XML around it.
    let nasty = event(
        "x",
        "</D:href><script>alert(1)</script>&amp;",
        "20250310T090000Z",
        "20250310T100000Z",
        "",
    );
    put(&s, &format!("{CAL}nasty.ics"), &nasty)
        .await
        .assert_status(StatusCode::CREATED);
    let list = alice(&s, "PROPFIND", CAL)
        .add_header("depth", "1")
        .text(propfind_body("<d:displayname/>"))
        .await
        .text();
    assert_eq!(
        prop(&list, &format!("{CAL}nasty.ics"), D, "displayname").as_deref(),
        Some("</D:href><script>alert(1)</script>&amp;"),
        "escaped on the way out, equal on the way back"
    );
    assert!(!list.contains("<script>"));
    let report = alice(&s, "REPORT", CAL)
        .text(query_body("20250101T000000Z", "20260101T000000Z"))
        .await
        .text();
    assert_eq!(
        prop(&report, &format!("{CAL}nasty.ics"), C, "calendar-data").as_deref(),
        Some(nasty.as_str())
    );
    // DAV responses are never cached.
    assert!(alice(&s, "PROPFIND", CAL)
        .add_header("depth", "0")
        .await
        .header("cache-control")
        .to_str()
        .unwrap()
        .contains("no-store"));
}
