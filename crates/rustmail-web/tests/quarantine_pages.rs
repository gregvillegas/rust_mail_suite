//! Integration tests for the quarantine pages. Requires `DATABASE_URL`.
//!
//! They check what the server decides: who may see and act, escaping of hostile
//! content, CSRF protection, redirects, and that actions reach the service.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread::JoinHandle;

use axum::Router;
use axum_extra::extract::cookie::Cookie;
use axum_test::{TestRequest, TestResponse, TestServer};
use chrono::Utc;
use rustmail_core::auth::{AuthConfig, AuthService};
use rustmail_core::domain::{NewDomain, NewUser, UserRole};
use rustmail_core::password::hash_password;
use rustmail_core::pipeline::store::{LogPosition, PipelineStore};
use rustmail_core::pipeline::{EventKind, EventSource, PipelineEvent};
use rustmail_core::quarantine::{QuarantineConfig, QuarantineService};
use rustmail_core::repository::{
    DomainRepository, PgDomainRepository, PgUserRepository, UserRepository,
};
use rustmail_web::assets::AssetSource;
use rustmail_web::auth::{AuthState, DeploymentSecurity, RateLimits};
use rustmail_web::config::RateLimitSettings;
use rustmail_web::{build_app, AppState};
use sqlx::PgPool;

const PASSWORD: &str = "correct-horse-battery-staple";
const MESSAGE: &str = "Return-Path: <x@example.org>\nDelivered-To: spam-quarantine\nSubject: <b>Win big</b>\nX-Spam-Score: 9\n\n<script>alert(1)</script>\nhello & goodbye\n";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rustmail-web-q-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn limits() -> RateLimits {
    let generous = || RateLimitSettings {
        burst: 1_000,
        period_secs: 1,
    };
    RateLimits {
        login: generous(),
        refresh: generous(),
    }
}

struct Site {
    router: Router,
}

impl Site {
    fn client(&self) -> TestServer {
        let mut server = TestServer::new(self.router.clone());
        server.add_header("x-forwarded-for", "198.51.100.7");
        server.add_header("user-agent", "quarantine-tests/1.0");
        server
    }
}

/// Builds the app with users alice (user), dana (domain admin), root (super admin).
/// `quarantine` is `None` to test the unconfigured state.
async fn site(pool: &PgPool, quarantine: Option<QuarantineConfig>) -> Site {
    let domain = PgDomainRepository::new(pool.clone())
        .create(&NewDomain {
            name: "example.com".into(),
            ..Default::default()
        })
        .await
        .expect("domain");
    let users = PgUserRepository::new(pool.clone());
    for (local, role) in [
        ("alice", UserRole::User),
        ("dana", UserRole::DomainAdmin),
        ("root", UserRole::SuperAdmin),
    ] {
        users
            .create(&NewUser {
                domain_id: domain.id,
                local_part: local.into(),
                password_hash: hash_password(PASSWORD).expect("hash"),
                display_name: local.to_uppercase(),
                role,
                locale: "en".into(),
            })
            .await
            .expect("user");
    }
    let auth = AuthState::new(
        AuthService::postgres(
            pool.clone(),
            AuthConfig::new("quarantine-integration-test-secret-32b").expect("config"),
        ),
        DeploymentSecurity {
            cookie_secure: true,
            trust_proxy_headers: true,
        },
    )
    .expect("auth state");
    let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static");
    let mut state =
        AppState::new(auth, pool.clone(), static_dir, AssetSource::Local).expect("state");
    if let Some(config) = quarantine {
        state = state.with_quarantine(QuarantineService::new(pool.clone(), config));
    }
    Site {
        router: build_app(state, limits()).expect("app").0,
    }
}

fn config(dir: &Scratch, smtp: &str) -> QuarantineConfig {
    QuarantineConfig {
        dir: dir.0.clone(),
        reinject_addr: smtp.to_owned(),
        helo: "rustmail.test".into(),
    }
}

fn cookie(name: &str, value: &str) -> Cookie<'static> {
    Cookie::new(name.to_owned(), value.to_owned())
}

struct Creds {
    access: String,
    refresh: String,
    csrf: String,
}

async fn sign_in(server: &TestServer, local: &str) -> Creds {
    let page = server.get("/login").await;
    let token = page.cookie("rm_csrf").value().to_owned();
    let response = server
        .post("/login")
        .add_header("hx-request", "true")
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .form(&[
            ("email", format!("{local}@example.com").as_str()),
            ("password", PASSWORD),
            ("next", "/"),
        ])
        .await;
    response.assert_status_ok();
    Creds {
        access: response.cookie("rm_access").value().to_owned(),
        refresh: response.cookie("rm_refresh").value().to_owned(),
        csrf: response.cookie("rm_csrf").value().to_owned(),
    }
}

fn get_as(server: &TestServer, path: &str, creds: &Creds) -> TestRequest {
    server
        .get(path)
        .add_cookie(cookie("rm_access", &creds.access))
        .add_cookie(cookie("rm_csrf", &creds.csrf))
}

fn post_as(server: &TestServer, path: &str, creds: &Creds) -> TestRequest {
    server
        .post(path)
        .add_header("hx-request", "true")
        .add_cookie(cookie("rm_access", &creds.access))
        .add_cookie(cookie("rm_refresh", &creds.refresh))
        .add_cookie(cookie("rm_csrf", &creds.csrf))
        .add_header("x-csrf-token", creds.csrf.as_str())
}

fn header(response: &TestResponse, name: &str) -> String {
    response
        .header(name)
        .to_str()
        .expect("ascii header")
        .to_owned()
}

fn event(mail_id: &str, recipient: &str, file: &str, reason_virus: Option<&str>) -> PipelineEvent {
    PipelineEvent {
        occurred_at: Utc::now(),
        source: EventSource::Amavis,
        kind: EventKind::Blocked,
        queue_id: Some("Q1".into()),
        mail_id: Some(mail_id.into()),
        verdict: Some(
            if reason_virus.is_some() {
                "INFECTED"
            } else {
                "SPAM"
            }
            .into(),
        ),
        virus_name: reason_virus.map(str::to_owned),
        spam_score: reason_virus.is_none().then_some(12.5),
        sender: "x@example.org".into(),
        recipient: recipient.into(),
        status: None,
        detail: None,
        size_bytes: Some(2048),
        quarantine_ref: Some(file.into()),
    }
}

async fn hold(pool: &PgPool, dir: &Scratch, events: &[PipelineEvent]) {
    for e in events {
        if let Some(file) = &e.quarantine_ref {
            std::fs::write(dir.0.join(file), MESSAGE).expect("write message");
        }
    }
    PipelineStore::new(pool.clone())
        .record(
            "/log",
            LogPosition {
                inode: 1,
                offset: 1,
            },
            events,
        )
        .await
        .expect("record");
}

async fn first_item_id(pool: &PgPool) -> String {
    sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM quarantine_items ORDER BY received_at LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .expect("an item")
    .to_string()
}

/// A one-session SMTP sink using blocking std I/O; returns what the client sent.
fn smtp_sink() -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut write = stream.try_clone().expect("clone");
        let mut read = BufReader::new(stream);
        let mut seen = String::new();
        write.write_all(b"220 sink\r\n").expect("w");
        let mut line = String::new();
        loop {
            line.clear();
            if read.read_line(&mut line).expect("r") == 0 {
                break;
            }
            seen.push_str(&line);
            let up = line.to_ascii_uppercase();
            if up.starts_with("EHLO") || up.starts_with("MAIL") || up.starts_with("RCPT") {
                write.write_all(b"250 ok\r\n").expect("w");
            } else if up.starts_with("DATA") {
                write.write_all(b"354 go\r\n").expect("w");
                loop {
                    line.clear();
                    read.read_line(&mut line).expect("r");
                    seen.push_str(&line);
                    if line == ".\r\n" {
                        break;
                    }
                }
                write.write_all(b"250 queued\r\n").expect("w");
            } else if up.starts_with("QUIT") {
                write.write_all(b"221 bye\r\n").expect("w");
                break;
            }
        }
        seen
    });
    (addr, handle)
}

// ─── access control ─────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn only_super_administrators_may_use_the_quarantine(pool: PgPool) {
    let dir = Scratch::new();
    hold(&pool, &dir, &[event("m1", "a@example.com", "spam-1", None)]).await;
    let id = first_item_id(&pool).await;
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();

    // Not signed in: sent to the sign-in page.
    for path in [
        "/admin/quarantine".to_owned(),
        format!("/admin/quarantine/{id}"),
    ] {
        let response = server.get(&path).await;
        response.assert_status(axum::http::StatusCode::SEE_OTHER);
        assert!(
            header(&response, "location").starts_with("/login"),
            "{path}"
        );
    }

    for local in ["alice", "dana"] {
        let creds = sign_in(&server, local).await;
        for path in [
            "/admin/quarantine".to_owned(),
            format!("/admin/quarantine/{id}"),
        ] {
            get_as(&server, &path, &creds)
                .await
                .assert_status(axum::http::StatusCode::FORBIDDEN);
        }
        for action in ["release", "delete"] {
            post_as(&server, &format!("/admin/quarantine/{id}/{action}"), &creds)
                .await
                .assert_status(axum::http::StatusCode::FORBIDDEN);
        }
    }
    let held: i64 =
        sqlx::query_scalar("SELECT count(*) FROM quarantine_items WHERE status = 'held'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(held, 1, "nothing changed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn actions_need_a_valid_csrf_token(pool: PgPool) {
    let dir = Scratch::new();
    hold(&pool, &dir, &[event("m1", "a@example.com", "spam-1", None)]).await;
    let id = first_item_id(&pool).await;
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;
    for action in ["release", "delete"] {
        let response = server
            .post(&format!("/admin/quarantine/{id}/{action}"))
            .add_header("hx-request", "true")
            .add_cookie(cookie("rm_access", &creds.access))
            .add_cookie(cookie("rm_csrf", &creds.csrf))
            .await; // no X-CSRF-Token header
        response.assert_status(axum::http::StatusCode::FORBIDDEN);
    }
    assert!(dir.0.join("spam-1").exists());
}

// ─── list ───────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn the_list_shows_held_messages_and_escapes_hostile_text(pool: PgPool) {
    let dir = Scratch::new();
    hold(
        &pool,
        &dir,
        &[
            event(
                "m1",
                "a@example.com",
                "virus-1",
                Some("<img src=x onerror=alert(1)>"),
            ),
            event("m2", "b@example.com", "spam-2", None),
        ],
    )
    .await;
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;
    let response = get_as(&server, "/admin/quarantine", &creds).await;
    response.assert_status_ok();
    let html = response.text();
    assert!(html.contains("a@example.com") && html.contains("b@example.com"));
    assert!(
        html.contains("&lt;img src=x onerror=alert(1)&gt;") || html.contains("&#60;img"),
        "{html}"
    );
    assert!(!html.contains("<img src=x"), "virus name must be escaped");
    assert!(html.contains("Viruses held"));
    assert!(html.contains("2 messages"));
    // CSP-friendly: no inline scripts, styles or handlers.
    assert!(!html.contains(" onclick=") && !html.contains(" style="));
}

#[sqlx::test(migrations = "../../migrations")]
async fn filters_and_paging_work(pool: PgPool) {
    let dir = Scratch::new();
    let mut events = vec![event("v1", "alice@example.com", "virus-1", Some("Eicar"))];
    for i in 0..30 {
        events.push(event(
            &format!("s{i}"),
            "bob@example.com",
            &format!("spam-{i}"),
            None,
        ));
    }
    hold(&pool, &dir, &events).await;
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;

    let all = get_as(&server, "/admin/quarantine", &creds).await.text();
    assert!(all.contains("31 messages"));
    assert!(all.contains("page=2"), "a next link exists: 31 > 25");

    let page2 = get_as(&server, "/admin/quarantine?page=2", &creds)
        .await
        .text();
    assert!(page2.contains("Page 2"));

    let viruses = get_as(&server, "/admin/quarantine?kind=virus", &creds)
        .await
        .text();
    assert!(viruses.contains("1 messages") && viruses.contains("alice@example.com"));
    assert!(!viruses.contains("bob@example.com"));

    let search = get_as(&server, "/admin/quarantine?q=ALICE", &creds)
        .await
        .text();
    assert!(search.contains("alice@example.com") && !search.contains("bob@example.com"));

    // Hostile or nonsensical parameters degrade to defaults instead of failing.
    for query in [
        "status=%27%3B--",
        "kind=zzz",
        "page=-4",
        "page=abc",
        "done=%3Cscript%3E",
    ] {
        let response = get_as(&server, &format!("/admin/quarantine?{query}"), &creds).await;
        assert!(
            response.status_code().is_success() || response.status_code().as_u16() == 400,
            "{query}: {}",
            response.status_code()
        );
        assert!(!response.text().contains("<script>alert"), "{query}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn without_quarantine_settings_the_page_explains_why(pool: PgPool) {
    let server = site(&pool, None).await.client();
    let creds = sign_in(&server, "root").await;
    let response = get_as(&server, "/admin/quarantine", &creds).await;
    response.assert_status_ok();
    assert!(response.text().contains("QUARANTINE_DIR"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_page_follows_the_chosen_language(pool: PgPool) {
    let dir = Scratch::new();
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;
    let html = get_as(&server, "/admin/quarantine", &creds)
        .add_header("accept-language", "de")
        .await
        .text();
    assert!(html.contains("Quarantäne"), "{html}");
}

// ─── detail ─────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn the_detail_page_shows_headers_and_an_escaped_excerpt(pool: PgPool) {
    let dir = Scratch::new();
    hold(&pool, &dir, &[event("m1", "a@example.com", "spam-1", None)]).await;
    let id = first_item_id(&pool).await;
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;
    let html = get_as(&server, &format!("/admin/quarantine/{id}"), &creds)
        .await
        .text();
    assert!(html.contains("Win big"));
    assert!(
        !html.contains("<b>Win big</b>"),
        "header values are escaped"
    );
    assert!(
        !html.contains("<script>alert(1)</script>"),
        "body is escaped"
    );
    assert!(html.contains("&lt;script&gt;") || html.contains("&#60;script&#62;"));
    assert!(
        html.contains("hx-post=\"/admin/quarantine/"),
        "action buttons present"
    );
    assert!(html.contains("hx-confirm="), "actions ask first");
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_items_are_not_found(pool: PgPool) {
    let dir = Scratch::new();
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;
    get_as(
        &server,
        "/admin/quarantine/00000000-0000-0000-0000-000000000000",
        &creds,
    )
    .await
    .assert_status(axum::http::StatusCode::NOT_FOUND);
    let bad = get_as(&server, "/admin/quarantine/not-a-uuid", &creds).await;
    assert!(bad.status_code().is_client_error());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_missing_file_still_shows_the_page_with_a_warning(pool: PgPool) {
    let dir = Scratch::new();
    PipelineStore::new(pool.clone())
        .record(
            "/log",
            LogPosition {
                inode: 1,
                offset: 1,
            },
            &[event("m1", "a@example.com", "spam-gone", None)],
        )
        .await
        .unwrap();
    let id = first_item_id(&pool).await;
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;
    let response = get_as(&server, &format!("/admin/quarantine/{id}"), &creds).await;
    response.assert_status_ok();
    assert!(response.text().contains("could not be read"));
}

// ─── actions ────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn releasing_delivers_and_redirects_with_a_notice(pool: PgPool) {
    let dir = Scratch::new();
    hold(&pool, &dir, &[event("m1", "a@example.com", "spam-1", None)]).await;
    let id = first_item_id(&pool).await;
    let (addr, sink) = smtp_sink();
    let server = site(&pool, Some(config(&dir, &addr))).await.client();
    let creds = sign_in(&server, "root").await;

    let response = post_as(&server, &format!("/admin/quarantine/{id}/release"), &creds).await;
    response.assert_status_ok();
    assert_eq!(
        header(&response, "hx-redirect"),
        "/admin/quarantine?done=released"
    );
    let sent = sink.join().expect("sink");
    assert!(sent.contains("RCPT TO:<a@example.com>"), "{sent}");
    assert!(sent.contains("Subject: <b>Win big</b>"));
    assert!(!sent.contains("Delivered-To: spam-quarantine"));

    let status: String = sqlx::query_scalar("SELECT status FROM quarantine_items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "released");
    let audited: String =
        sqlx::query_scalar("SELECT actor_email FROM audit_log WHERE action = 'quarantine.release'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audited, "root@example.com");

    // The notice appears on the list page, and a second release is refused politely.
    let list = get_as(&server, "/admin/quarantine?done=released", &creds)
        .await
        .text();
    assert!(list.contains("was delivered to the recipient"));
    let again = post_as(&server, &format!("/admin/quarantine/{id}/release"), &creds).await;
    assert_eq!(header(&again, "hx-redirect"), "/admin/quarantine?done=gone");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_failed_release_keeps_the_message_and_says_so(pool: PgPool) {
    let dir = Scratch::new();
    hold(&pool, &dir, &[event("m1", "a@example.com", "spam-1", None)]).await;
    let id = first_item_id(&pool).await;
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;
    let response = post_as(&server, &format!("/admin/quarantine/{id}/release"), &creds).await;
    assert_eq!(
        header(&response, "hx-redirect"),
        "/admin/quarantine?done=failed"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM quarantine_items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "held");
    let list = get_as(&server, "/admin/quarantine?done=failed", &creds)
        .await
        .text();
    assert!(list.contains("The action failed"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleting_removes_the_message_and_its_file(pool: PgPool) {
    let dir = Scratch::new();
    hold(&pool, &dir, &[event("m1", "a@example.com", "spam-1", None)]).await;
    let id = first_item_id(&pool).await;
    let server = site(&pool, Some(config(&dir, "127.0.0.1:1")))
        .await
        .client();
    let creds = sign_in(&server, "root").await;
    let response = post_as(&server, &format!("/admin/quarantine/{id}/delete"), &creds).await;
    assert_eq!(
        header(&response, "hx-redirect"),
        "/admin/quarantine?done=deleted"
    );
    assert!(!dir.0.join("spam-1").exists());
    let again = post_as(&server, &format!("/admin/quarantine/{id}/delete"), &creds).await;
    assert_eq!(header(&again, "hx-redirect"), "/admin/quarantine?done=gone");
}
