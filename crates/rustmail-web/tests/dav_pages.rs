//! Integration tests for the calendar, contacts and app-password pages. Requires `DATABASE_URL`.
//!
//! They check what the server decides: who may see and change what, that events and contacts
//! made in the page appear to calendar apps (and the reverse), that the page never edits
//! something it cannot write back faithfully, and that nothing hostile is rendered as markup.

use std::path::PathBuf;

use axum::http::StatusCode;
use axum_extra::extract::cookie::Cookie;
use axum_test::{TestRequest, TestResponse, TestServer};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rustmail_core::auth::{AuthConfig, AuthService};
use rustmail_core::dav::store::{DavStore, GranteeRef, Kind, Precondition};
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
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-staple";

struct World {
    server: TestServer,
    store: DavStore,
    pool: PgPool,
}

async fn world(pool: &PgPool) -> World {
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
            AuthConfig::new("dav-pages-test-secret-32-bytes-long!!").unwrap(),
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
    let mut server = TestServer::new(
        build_app(
            state,
            RateLimits {
                login: generous(),
                refresh: generous(),
            },
        )
        .unwrap()
        .0,
    );
    server.add_header("x-forwarded-for", "198.51.100.7");
    World {
        server,
        store: DavStore::new(pool.clone()),
        pool: pool.clone(),
    }
}

struct Creds {
    access: String,
    refresh: String,
    csrf: String,
}

fn cookie(n: &str, v: &str) -> Cookie<'static> {
    Cookie::new(n.to_owned(), v.to_owned())
}

async fn sign_in(w: &World, email: &str) -> Creds {
    let page = w.server.get("/login").await;
    let token = page.cookie("rm_csrf").value().to_owned();
    let r = w
        .server
        .post("/login")
        .add_header("hx-request", "true")
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .form(&[("email", email), ("password", PASSWORD), ("next", "/")])
        .await;
    r.assert_status_ok();
    Creds {
        access: r.cookie("rm_access").value().to_owned(),
        refresh: r.cookie("rm_refresh").value().to_owned(),
        csrf: r.cookie("rm_csrf").value().to_owned(),
    }
}

fn get_as(w: &World, path: &str, c: &Creds) -> TestRequest {
    w.server
        .get(path)
        .add_cookie(cookie("rm_access", &c.access))
        .add_cookie(cookie("rm_csrf", &c.csrf))
}

fn post_as(w: &World, path: &str, c: &Creds) -> TestRequest {
    w.server
        .post(path)
        .add_header("hx-request", "true")
        .add_cookie(cookie("rm_access", &c.access))
        .add_cookie(cookie("rm_refresh", &c.refresh))
        .add_cookie(cookie("rm_csrf", &c.csrf))
        .add_header("x-csrf-token", c.csrf.as_str())
}

fn hx_redirect(r: &TestResponse) -> String {
    r.header("hx-redirect").to_str().unwrap().to_owned()
}

async fn id_of(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn calendar_of(w: &World, email: &str) -> Uuid {
    let user = id_of(&w.pool, email).await;
    w.store.ensure_defaults(user).await.unwrap();
    w.store
        .collections(user, Kind::Calendar)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.access.is_owner())
        .unwrap()
        .id
}

async fn book_of(w: &World, email: &str) -> Uuid {
    let user = id_of(&w.pool, email).await;
    w.store.ensure_defaults(user).await.unwrap();
    w.store
        .collections(user, Kind::AddressBook)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.access.is_owner())
        .unwrap()
        .id
}

fn event_form<'a>(
    calendar: &'a str,
    title: &'a str,
    start: &'a str,
    end: &'a str,
) -> Vec<(&'static str, &'a str)> {
    vec![
        ("collection", calendar),
        ("uid", "page-event-1"),
        ("title", title),
        ("start", start),
        ("end", end),
        ("repeat", "never"),
        ("tz", "Europe/Berlin"),
    ]
}

const PAGES: [&str; 8] = [
    "/calendar",
    "/calendar/event",
    "/calendar/freebusy",
    "/contacts",
    "/contacts/edit",
    "/settings/collections",
    "/settings/apps",
    "/calendar/events?start=2025-03-01&end=2025-04-01",
];

#[sqlx::test(migrations = "../../migrations")]
async fn the_pages_need_a_session_and_posts_need_csrf(pool: PgPool) {
    let w = world(&pool).await;
    for path in PAGES {
        let r = w.server.get(path).await;
        r.assert_status(StatusCode::SEE_OTHER);
        assert!(
            r.header("location").to_str().unwrap().starts_with("/login"),
            "{path}"
        );
    }
    let alice = sign_in(&w, "alice@example.com").await;
    for path in PAGES {
        get_as(&w, path, &alice).await.assert_status_ok();
    }
    let cal = calendar_of(&w, "alice@example.com").await.to_string();
    let no_csrf = w
        .server
        .post("/calendar/event")
        .add_cookie(cookie("rm_access", &alice.access))
        .form(&event_form(
            &cal,
            "x",
            "2025-03-10T09:00",
            "2025-03-10T10:00",
        ))
        .await;
    no_csrf.assert_status(StatusCode::FORBIDDEN);
    let none: i64 = sqlx::query_scalar("SELECT count(*) FROM dav_objects")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(none, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_event_made_in_the_page_reaches_calendar_apps_and_back(pool: PgPool) {
    let w = world(&pool).await;
    let alice = sign_in(&w, "alice@example.com").await;
    let cal = calendar_of(&w, "alice@example.com").await.to_string();

    // 09:00 in Berlin (UTC+1 in March, before the clocks change) is 08:00 UTC.
    let r = post_as(&w, "/calendar/event", &alice)
        .form(&event_form(
            &cal,
            "Planning; with, commas",
            "2025-03-10T09:00",
            "2025-03-10T10:00",
        ))
        .await;
    r.assert_status_ok();
    assert_eq!(hx_redirect(&r), "/calendar?date=2025-03-10");

    // The feed the calendar view reads.
    let feed = get_as(
        &w,
        "/calendar/events?start=2025-03-01T00:00:00%2B01:00&end=2025-04-01T00:00:00%2B02:00",
        &alice,
    )
    .await;
    feed.assert_status_ok();
    let events: serde_json::Value = serde_json::from_str(&feed.text()).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 1, "{events}");
    let e = &events[0];
    assert_eq!(
        (
            e["title"].as_str(),
            e["start"].as_str(),
            e["end"].as_str(),
            e["allDay"].as_bool()
        ),
        (
            Some("Planning; with, commas"),
            Some("2025-03-10T08:00:00+00:00"),
            Some("2025-03-10T09:00:00+00:00"),
            Some(false)
        )
    );
    let url = e["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/calendar/event?c="), "{url}");

    // The same event, as a calendar app reads it over CalDAV.
    let token = rustmail_core::dav::tokens::AppPasswords::new(pool.clone())
        .create(id_of(&pool, "alice@example.com").await, "app")
        .await
        .unwrap();
    let auth = format!(
        "Basic {}",
        STANDARD.encode(format!("alice@example.com:{token}"))
    );
    let ics = w
        .server
        .get("/dav/calendars/alice@example.com/default/page-event-1.ics")
        .add_header("authorization", auth.as_str())
        .await;
    ics.assert_status_ok();
    assert!(
        ics.text().contains("DTSTART:20250310T080000Z")
            && ics.text().contains("SUMMARY:Planning\\; with\\, commas"),
        "{}",
        ics.text()
    );

    // Opening it shows the form filled in; saving changes it in place.
    let edit = get_as(&w, &url, &alice).await;
    edit.assert_status_ok();
    let html = edit.text();
    assert!(
        html.contains("value=\"Planning; with, commas\"")
            || html.contains("Planning; with, commas"),
        "{html}"
    );
    let etag = html
        .split("name=\"etag\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    let mut change = event_form(
        &cal,
        "Planning (moved)",
        "2025-03-11T14:00",
        "2025-03-11T15:30",
    );
    change.push(("object", "page-event-1.ics"));
    change.push(("etag", etag.as_str()));
    post_as(&w, "/calendar/event", &alice)
        .form(&change)
        .await
        .assert_status_ok();
    let after = w
        .server
        .get("/dav/calendars/alice@example.com/default/page-event-1.ics")
        .add_header("authorization", auth.as_str())
        .await
        .text();
    assert!(
        after.contains("DTSTART:20250311T130000Z")
            && after.contains("Planning (moved)")
            && after.matches("BEGIN:VEVENT").count() == 1,
        "{after}"
    );
    // Saving again with the old etag is a conflict, not a silent overwrite.
    let stale = post_as(&w, "/calendar/event", &alice).form(&change).await;
    stale.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(stale.text().contains("alert-danger"));

    // An event a calendar app adds appears in the page.
    let theirs = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//x//EN\r\nBEGIN:VEVENT\r\nUID:from-app\r\nDTSTAMP:20250101T000000Z\r\nSUMMARY:From the app\r\nDTSTART:20250312T100000Z\r\nDTEND:20250312T110000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    w.server
        .put("/dav/calendars/alice@example.com/default/app.ics")
        .add_header("authorization", auth.as_str())
        .text(theirs)
        .await
        .assert_status(StatusCode::CREATED);
    let feed = get_as(
        &w,
        "/calendar/events?start=2025-03-01T00:00:00Z&end=2025-04-01T00:00:00Z",
        &alice,
    )
    .await
    .text();
    assert!(
        feed.contains("From the app") && feed.contains("Planning (moved)"),
        "{feed}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn repeating_all_day_and_bad_events(pool: PgPool) {
    let w = world(&pool).await;
    let alice = sign_in(&w, "alice@example.com").await;
    let cal = calendar_of(&w, "alice@example.com").await.to_string();
    let mut weekly = event_form(&cal, "Standup", "2025-03-10T09:00", "2025-03-10T09:15");
    weekly[3] = ("start", "2025-03-10T09:00");
    weekly[5] = ("repeat", "weekly");
    weekly.push(("repeat_until", "2025-03-31"));
    post_as(&w, "/calendar/event", &alice)
        .form(&weekly)
        .await
        .assert_status_ok();
    let feed: serde_json::Value = serde_json::from_str(
        &get_as(
            &w,
            "/calendar/events?start=2025-03-01T00:00:00Z&end=2025-04-30T00:00:00Z",
            &alice,
        )
        .await
        .text(),
    )
    .unwrap();
    let starts: Vec<&str> = feed
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["start"].as_str().unwrap())
        .collect();
    // Mondays 10, 17, 24, 31 March at 09:00 Berlin: 08:00 UTC, and 07:00 UTC after the clocks change on the 30th.
    assert_eq!(
        starts,
        [
            "2025-03-10T08:00:00+00:00",
            "2025-03-17T08:00:00+00:00",
            "2025-03-24T08:00:00+00:00",
            "2025-03-31T07:00:00+00:00"
        ]
    );

    let day = vec![
        ("collection", cal.as_str()),
        ("uid", "holiday"),
        ("title", "Holiday"),
        ("all_day", "on"),
        ("start", "2025-06-01"),
        ("end", "2025-06-03"),
        ("repeat", "never"),
        ("tz", "Europe/Berlin"),
    ];
    post_as(&w, "/calendar/event", &alice)
        .form(&day)
        .await
        .assert_status_ok();
    let feed: serde_json::Value = serde_json::from_str(
        &get_as(
            &w,
            "/calendar/events?start=2025-05-25T00:00:00Z&end=2025-06-30T00:00:00Z",
            &alice,
        )
        .await
        .text(),
    )
    .unwrap();
    let h = &feed[0];
    assert_eq!(
        (
            h["allDay"].as_bool(),
            h["start"].as_str(),
            h["end"].as_str()
        ),
        (Some(true), Some("2025-06-01"), Some("2025-06-04")),
        "an all-day event ends on the day after its last day"
    );

    for (what, form) in [
        (
            "empty title",
            event_form(&cal, "  ", "2025-03-10T09:00", "2025-03-10T10:00"),
        ),
        (
            "ends before it starts",
            event_form(&cal, "x", "2025-03-10T10:00", "2025-03-10T09:00"),
        ),
        (
            "not a date",
            event_form(&cal, "x", "tomorrow", "2025-03-10T09:00"),
        ),
        (
            "no calendar",
            event_form("", "x", "2025-03-10T09:00", "2025-03-10T10:00"),
        ),
    ] {
        let r = post_as(&w, "/calendar/event", &alice).form(&form).await;
        assert_eq!(r.status_code(), StatusCode::UNPROCESSABLE_ENTITY, "{what}");
    }
    let wide = get_as(
        &w,
        "/calendar/events?start=2020-01-01T00:00:00Z&end=2030-01-01T00:00:00Z",
        &alice,
    )
    .await;
    assert_ne!(
        wide.status_code(),
        StatusCode::OK,
        "a ten-year window is refused"
    );
    get_as(&w, "/calendar/events?start=x&end=y", &alice)
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../migrations")]
async fn events_the_page_cannot_write_back_are_never_rewritten_by_it(pool: PgPool) {
    let w = world(&pool).await;
    let alice = sign_in(&w, "alice@example.com").await;
    let cal = calendar_of(&w, "alice@example.com").await;
    let alice_id = id_of(&pool, "alice@example.com").await;
    // What Thunderbird typically sends: an alarm, a guest and an app-specific field.
    let rich = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Mozilla.org//EN\r\nBEGIN:VEVENT\r\nUID:rich\r\nDTSTAMP:20250101T000000Z\r\nSUMMARY:Board meeting\r\nDTSTART:20250310T090000Z\r\nDTEND:20250310T100000Z\r\nATTENDEE;CN=Bob:mailto:bob@example.com\r\nX-MOZ-GENERATION:3\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    w.store
        .put(alice_id, cal, "rich.ics", rich, &Precondition::None)
        .await
        .unwrap();

    let page = get_as(&w, &format!("/calendar/event?c={cal}&o=rich.ics"), &alice).await;
    page.assert_status_ok();
    let html = page.text();
    assert!(
        html.contains("event-readonly")
            && html.contains("Board meeting")
            && !html.contains("id=\"event-form\""),
        "shown but not editable"
    );
    // A forged save is refused and the event is byte-for-byte what it was.
    let cal_text = cal.to_string();
    let mut forged = event_form(&cal_text, "Hacked", "2025-03-10T09:00", "2025-03-10T10:00");
    forged.push(("object", "rich.ics"));
    forged.push(("uid", "rich"));
    let r = post_as(&w, "/calendar/event", &alice).form(&forged).await;
    r.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        w.store.get(alice_id, cal, "rich.ics").await.unwrap().data,
        rich
    );
    // It can still be deleted from the page.
    post_as(&w, "/calendar/event/delete", &alice)
        .form(&[
            ("collection", cal.to_string().as_str()),
            ("object", "rich.ics"),
        ])
        .await
        .assert_status_ok();
    assert!(w.store.get(alice_id, cal, "rich.ics").await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_read_only_calendar_cannot_be_changed_from_the_page(pool: PgPool) {
    let w = world(&pool).await;
    let alice_id = id_of(&pool, "alice@example.com").await;
    let cal = calendar_of(&w, "alice@example.com").await;
    let simple = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:u1\r\nDTSTAMP:20250101T000000Z\r\nSUMMARY:Team lunch\r\nDTSTART:20250310T120000Z\r\nDTEND:20250310T130000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    w.store
        .put(alice_id, cal, "lunch.ics", simple, &Precondition::None)
        .await
        .unwrap();
    w.store
        .share(
            alice_id,
            cal,
            &GranteeRef::UserEmail("bob@example.com".into()),
            false,
        )
        .await
        .unwrap();
    let bob = sign_in(&w, "bob@example.com").await;

    let feed = get_as(
        &w,
        "/calendar/events?start=2025-03-01T00:00:00Z&end=2025-04-01T00:00:00Z",
        &bob,
    )
    .await
    .text();
    assert!(
        feed.contains("Team lunch") && feed.contains("\"writable\":false"),
        "{feed}"
    );
    let page = get_as(&w, &format!("/calendar/event?c={cal}&o=lunch.ics"), &bob)
        .await
        .text();
    assert!(
        page.contains("event-readonly") && !page.contains("id=\"event-form\""),
        "{page}"
    );
    let cal_text = cal.to_string();
    let mut attempt = event_form(
        &cal_text,
        "Bob was here",
        "2025-03-10T12:00",
        "2025-03-10T13:00",
    );
    attempt.push(("object", "lunch.ics"));
    post_as(&w, "/calendar/event", &bob)
        .form(&attempt)
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    attempt[1] = ("uid", "new-one");
    attempt.retain(|(k, _)| *k != "object");
    post_as(&w, "/calendar/event", &bob)
        .form(&attempt)
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    let del = post_as(&w, "/calendar/event/delete", &bob)
        .form(&[
            ("collection", cal.to_string().as_str()),
            ("object", "lunch.ics"),
        ])
        .await;
    assert_ne!(del.status_code(), StatusCode::OK);
    assert_eq!(
        w.store.get(alice_id, cal, "lunch.ics").await.unwrap().data,
        simple,
        "nothing changed"
    );
    assert_eq!(w.store.list(alice_id, cal).await.unwrap().len(), 1);
    // A stranger sees none of it.
    let zed = sign_in(&w, "zed@other.org").await;
    assert!(!get_as(
        &w,
        "/calendar/events?start=2025-03-01T00:00:00Z&end=2025-04-01T00:00:00Z",
        &zed
    )
    .await
    .text()
    .contains("Team lunch"));
    get_as(&w, &format!("/calendar/event?c={cal}&o=lunch.ics"), &zed)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn titles_are_escaped_not_rendered(pool: PgPool) {
    let w = world(&pool).await;
    let alice = sign_in(&w, "alice@example.com").await;
    let cal = calendar_of(&w, "alice@example.com").await.to_string();
    post_as(&w, "/calendar/event", &alice)
        .form(&event_form(
            &cal,
            "<script>alert(1)</script><img src=x onerror=1>",
            "2025-03-10T09:00",
            "2025-03-10T10:00",
        ))
        .await
        .assert_status_ok();
    let html = get_as(
        &w,
        &format!("/calendar/event?c={cal}&o=page-event-1.ics"),
        &alice,
    )
    .await
    .text();
    assert!(
        !html.contains("<script>alert(1)")
            && !html.contains("<img src=x")
            && (html.contains("&lt;script&gt;") || html.contains("&#60;script&#62;")),
        "{html}"
    );
    // The JSON feed carries it as data; the view shows it as text.
    let feed = get_as(
        &w,
        "/calendar/events?start=2025-03-01T00:00:00Z&end=2025-04-01T00:00:00Z",
        &alice,
    )
    .await;
    assert!(feed
        .header("content-type")
        .to_str()
        .unwrap()
        .starts_with("application/json"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn free_busy_works_inside_the_domain_only(pool: PgPool) {
    let w = world(&pool).await;
    let alice_id = id_of(&pool, "alice@example.com").await;
    let cal = calendar_of(&w, "alice@example.com").await;
    w.store.put(alice_id, cal, "m.ics", "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:m\r\nDTSTAMP:20250101T000000Z\r\nSUMMARY:SECRET MERGER\r\nDTSTART:20250310T090000Z\r\nDTEND:20250310T103000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n", &Precondition::None).await.unwrap();
    let bob = sign_in(&w, "bob@example.com").await;
    let r = get_as(
        &w,
        "/calendar/freebusy?user=alice%40example.com&date=2025-03-10",
        &bob,
    )
    .await;
    r.assert_status_ok();
    let html = r.text();
    assert!(
        html.contains("09:00 – 10:30") && !html.contains("SECRET"),
        "times only: {html}"
    );
    let zed = sign_in(&w, "zed@other.org").await;
    let blocked = get_as(
        &w,
        "/calendar/freebusy?user=alice%40example.com&date=2025-03-10",
        &zed,
    )
    .await
    .text();
    assert!(
        blocked.contains("no one with that address") && !blocked.contains("09:00"),
        "{blocked}"
    );
    let unknown = get_as(&w, "/calendar/freebusy?user=nobody%40example.com", &bob)
        .await
        .text();
    assert!(unknown.contains("no one with that address"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn contacts_are_made_found_edited_without_losing_fields_and_deleted(pool: PgPool) {
    let w = world(&pool).await;
    let alice = sign_in(&w, "alice@example.com").await;
    let alice_id = id_of(&pool, "alice@example.com").await;
    let book = book_of(&w, "alice@example.com").await;
    let b = book.to_string();
    let form = |name: &str, emails: &str| {
        vec![
            ("book", b.clone()),
            ("uid", "page-contact".to_owned()),
            ("name", name.to_owned()),
            ("organisation", "Analytical Engines".to_owned()),
            ("emails", emails.to_owned()),
            ("phones", "+44 20 7946 0000".to_owned()),
            ("notes", "Met at the conference".to_owned()),
        ]
    };
    let r = post_as(&w, "/contacts/save", &alice)
        .form(&form("Ada Lovelace", "ada@example.org\nada@home.example"))
        .await;
    r.assert_status_ok();
    assert_eq!(hx_redirect(&r), format!("/contacts?b={book}&done=saved"));
    let list = get_as(&w, &format!("/contacts?b={book}"), &alice)
        .await
        .text();
    assert!(
        list.contains("Ada Lovelace") && list.contains("ada@example.org, ada@home.example"),
        "{list}"
    );
    let found = get_as(&w, &format!("/contacts?b={book}&q=ADA%40HOME"), &alice)
        .await
        .text();
    assert!(found.contains("Ada Lovelace"));
    assert!(get_as(&w, &format!("/contacts?b={book}&q=nobody"), &alice)
        .await
        .text()
        .contains("contacts-empty"));
    assert!(
        get_as(&w, &format!("/contacts?b={book}&q=%25"), &alice)
            .await
            .text()
            .contains("contacts-empty"),
        "% is searched literally"
    );

    // A phone added a photo and a custom field; editing in the page keeps them.
    let existing = w
        .store
        .get(alice_id, book, "page-contact.vcf")
        .await
        .unwrap()
        .data;
    let with_extras = existing.replace(
        "END:VCARD",
        "PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ\r\nX-CUSTOM:keep me\r\nEND:VCARD",
    );
    w.store
        .put(
            alice_id,
            book,
            "page-contact.vcf",
            &with_extras,
            &Precondition::None,
        )
        .await
        .unwrap();
    let edit = get_as(
        &w,
        &format!("/contacts/edit?b={book}&o=page-contact.vcf"),
        &alice,
    )
    .await;
    edit.assert_status_ok();
    let html = edit.text();
    let etag = html
        .split("name=\"etag\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    let mut change = form("Augusta Ada King", "ada@example.org");
    change.push(("object", "page-contact.vcf".to_owned()));
    change.push(("etag", etag));
    post_as(&w, "/contacts/save", &alice)
        .form(&change)
        .await
        .assert_status_ok();
    let saved = w
        .store
        .get(alice_id, book, "page-contact.vcf")
        .await
        .unwrap()
        .data;
    assert!(
        saved.contains("FN:Augusta Ada King")
            && saved.contains("PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ")
            && saved.contains("X-CUSTOM:keep me")
            && !saved.contains("ada@home.example"),
        "{saved}"
    );

    for (what, f) in [
        ("no name", form(" ", "a@b.de")),
        ("bad email", form("X", "not-an-email")),
        ("bad uid", {
            let mut f = form("X", "a@b.de");
            f[1].1 = "a b".into();
            f
        }),
    ] {
        let r = post_as(&w, "/contacts/save", &alice).form(&f).await;
        assert_eq!(r.status_code(), StatusCode::UNPROCESSABLE_ENTITY, "{what}");
    }
    post_as(&w, "/contacts/delete", &alice)
        .form(&[("book", b.as_str()), ("object", "page-contact.vcf")])
        .await
        .assert_status_ok();
    assert!(w
        .store
        .get(alice_id, book, "page-contact.vcf")
        .await
        .is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_shared_address_book_is_read_only_for_a_reader_and_hidden_from_others(pool: PgPool) {
    let w = world(&pool).await;
    let alice_id = id_of(&pool, "alice@example.com").await;
    let book = book_of(&w, "alice@example.com").await;
    w.store
        .put(
            alice_id,
            book,
            "a.vcf",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c1\r\nFN:Ada Lovelace\r\nEND:VCARD\r\n",
            &Precondition::None,
        )
        .await
        .unwrap();
    w.store
        .share(
            alice_id,
            book,
            &GranteeRef::UserEmail("bob@example.com".into()),
            false,
        )
        .await
        .unwrap();
    let bob = sign_in(&w, "bob@example.com").await;
    let page = get_as(&w, &format!("/contacts?b={book}"), &bob)
        .await
        .text();
    assert!(
        page.contains("Ada Lovelace")
            && page.contains("shared with you read-only")
            && !page.contains("New contact"),
        "{page}"
    );
    let b = book.to_string();
    let form = vec![
        ("book", b.as_str()),
        ("uid", "x"),
        ("name", "Bob's"),
        ("emails", ""),
        ("phones", ""),
        ("notes", ""),
        ("organisation", ""),
    ];
    post_as(&w, "/contacts/save", &bob)
        .form(&form)
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    post_as(&w, "/contacts/delete", &bob)
        .form(&[("book", b.as_str()), ("object", "a.vcf")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    get_as(&w, &format!("/contacts/edit?b={book}"), &bob)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    assert_eq!(w.store.list(alice_id, book).await.unwrap().len(), 1);
    let zed = sign_in(&w, "zed@other.org").await;
    assert!(
        !get_as(&w, &format!("/contacts?b={book}"), &zed)
            .await
            .text()
            .contains("Ada Lovelace"),
        "a stranger falls back to their own book"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn calendars_are_managed_and_shared_from_the_settings_page(pool: PgPool) {
    let w = world(&pool).await;
    let alice = sign_in(&w, "alice@example.com").await;
    let bob = sign_in(&w, "bob@example.com").await;
    let page = get_as(&w, "/settings/collections", &alice).await.text();
    assert!(
        page.contains("/dav/calendars/alice@example.com/default/")
            && page.contains("/dav/addressbooks/alice@example.com/contacts/"),
        "{page}"
    );

    let r = post_as(&w, "/settings/collections/create", &alice)
        .form(&[("kind", "calendar"), ("name", "Work"), ("color", "#336699")])
        .await;
    r.assert_status_ok();
    let work = w
        .store
        .collections(id_of(&pool, "alice@example.com").await, Kind::Calendar)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.display_name == "Work")
        .unwrap();
    assert_eq!(work.color.as_deref(), Some("#336699"));
    post_as(&w, "/settings/collections/create", &alice)
        .form(&[("kind", "calendar"), ("name", ""), ("color", "")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    post_as(
        &w,
        &format!("/settings/collections/{}/update", work.id),
        &alice,
    )
    .form(&[
        ("name", "Work stuff"),
        ("color", "#112233"),
        ("description", "Meetings"),
    ])
    .await
    .assert_status_ok();

    // Sharing: read-only by email, refused across domains and for unknown people.
    let share = |path: String, who: &str, write: bool| {
        let mut f = vec![("with_user".to_owned(), who.to_owned())];
        if write {
            f.push(("can_write".to_owned(), "on".to_owned()));
        }
        (path, f)
    };
    let (p, f) = share(
        format!("/settings/collections/{}/share", work.id),
        "bob@example.com",
        false,
    );
    post_as(&w, &p, &alice).form(&f).await.assert_status_ok();
    assert!(
        get_as(&w, "/settings/collections", &bob)
            .await
            .text()
            .contains("Work stuff"),
        "bob sees it"
    );
    for who in ["zed@other.org", "nobody@example.com"] {
        let (p, f) = share(
            format!("/settings/collections/{}/share", work.id),
            who,
            false,
        );
        post_as(&w, &p, &alice)
            .form(&f)
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let r = post_as(
        &w,
        &format!("/settings/collections/{}/share", work.id),
        &alice,
    )
    .form(&[
        ("with_user", "bob@example.com"),
        ("with_group", &Uuid::new_v4().to_string()),
    ])
    .await;
    assert_eq!(
        r.status_code(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "one grantee at a time"
    );
    // Bob cannot manage what is only shared with him.
    for path in [
        format!("/settings/collections/{}/update", work.id),
        format!("/settings/collections/{}/delete", work.id),
        format!("/settings/collections/{}/share", work.id),
    ] {
        let r = post_as(&w, &path, &bob)
            .form(&[
                ("name", "Mine"),
                ("confirm", "Work stuff"),
                ("with_user", "alice@example.com"),
            ])
            .await;
        assert_ne!(r.status_code(), StatusCode::OK, "{path}");
    }
    // Stopping the share.
    let id = id_of(&pool, "alice@example.com").await;
    let sh = w.store.shares(id, work.id).await.unwrap().remove(0);
    post_as(
        &w,
        &format!("/settings/collections/{}/unshare/{}", work.id, sh.id),
        &alice,
    )
    .await
    .assert_status_ok();
    assert!(!get_as(&w, "/settings/collections", &bob)
        .await
        .text()
        .contains("Work stuff"));

    // Deleting needs the name typed.
    let wrong = post_as(
        &w,
        &format!("/settings/collections/{}/delete", work.id),
        &alice,
    )
    .form(&[("confirm", "work")])
    .await;
    wrong.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(w.store.collection(id, work.id).await.is_ok());
    post_as(
        &w,
        &format!("/settings/collections/{}/delete", work.id),
        &alice,
    )
    .form(&[("confirm", "Work stuff")])
    .await
    .assert_status_ok();
    assert!(w.store.collection(id, work.id).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn app_passwords_are_shown_once_work_for_calendar_apps_and_can_be_revoked(pool: PgPool) {
    let w = world(&pool).await;
    let alice = sign_in(&w, "alice@example.com").await;
    let bob = sign_in(&w, "bob@example.com").await;
    assert!(get_as(&w, "/settings/apps", &alice)
        .await
        .text()
        .contains("apps-empty"));
    let r = post_as(&w, "/settings/apps", &alice)
        .form(&[("name", "Thunderbird laptop")])
        .await;
    r.assert_status_ok();
    let html = r.text();
    let token = html
        .split("data-testid=\"app-token\">")
        .nth(1)
        .unwrap()
        .split('<')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(token.len(), 32);
    assert!(
        html.contains("/dav/calendars/alice@example.com/default/")
            && html.contains("/dav/addressbooks/alice@example.com/contacts/")
    );
    // The list never shows the token again.
    let list = get_as(&w, "/settings/apps", &alice).await.text();
    assert!(
        list.contains("Thunderbird laptop") && !list.contains(&token),
        "{list}"
    );
    // It signs in to CalDAV.
    let auth = format!(
        "Basic {}",
        STANDARD.encode(format!("alice@example.com:{token}"))
    );
    let dav = |a: &str| {
        w.server
            .method(axum::http::Method::OPTIONS, "/dav/")
            .add_header("authorization", a.to_owned())
    };
    dav(&auth).await.assert_status_ok();
    // Bob cannot revoke it; alice can, and it stops working at once.
    let id: Uuid = sqlx::query_scalar("SELECT id FROM app_passwords")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        post_as(&w, &format!("/settings/apps/{id}/revoke"), &bob)
            .await
            .status_code(),
        StatusCode::NOT_FOUND
    );
    dav(&auth).await.assert_status_ok();
    post_as(&w, &format!("/settings/apps/{id}/revoke"), &alice)
        .await
        .assert_status_ok();
    dav(&auth).await.assert_status(StatusCode::UNAUTHORIZED);
    for name in ["", &"x".repeat(65)] {
        post_as(&w, "/settings/apps", &alice)
            .form(&[("name", name)])
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    }
}
