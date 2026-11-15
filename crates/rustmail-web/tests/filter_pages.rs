//! Integration tests for the filter and autoresponder pages. Requires `DATABASE_URL`.
//!
//! The mail server is not configured here, so saves succeed but report that the change is
//! not active yet; installing on a real Dovecot is covered by `scripts/browser-smoke/filters.mjs`.

use std::path::PathBuf;

use axum::http::StatusCode;
use axum::Router;
use axum_extra::extract::cookie::Cookie;
use axum_test::{TestRequest, TestResponse, TestServer};
use rustmail_core::auth::{AuthConfig, AuthService};
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

async fn site(pool: &PgPool) -> TestServer {
    let domain = PgDomainRepository::new(pool.clone())
        .create(&NewDomain {
            name: "example.com".into(),
            ..Default::default()
        })
        .await
        .expect("domain");
    let users = PgUserRepository::new(pool.clone());
    for local in ["alice", "bob"] {
        users
            .create(&NewUser {
                domain_id: domain.id,
                local_part: local.into(),
                password_hash: hash_password(PASSWORD).expect("hash"),
                display_name: local.to_uppercase(),
                role: UserRole::User,
                locale: "en".into(),
            })
            .await
            .expect("user");
    }
    let auth = AuthState::new(
        AuthService::postgres(
            pool.clone(),
            AuthConfig::new("filters-integration-test-secret-32b!").expect("config"),
        ),
        DeploymentSecurity {
            cookie_secure: true,
            trust_proxy_headers: true,
        },
    )
    .expect("auth state");
    let state = AppState::new(
        auth,
        pool.clone(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"),
        AssetSource::Local,
    )
    .expect("state");
    let router: Router = build_app(state, limits()).expect("app").0;
    let mut server = TestServer::new(router);
    server.add_header("x-forwarded-for", "198.51.100.7");
    server.add_header("user-agent", "filter-tests/1.0");
    server
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

fn get_as(server: &TestServer, path: &str, c: &Creds) -> TestRequest {
    server
        .get(path)
        .add_cookie(cookie("rm_access", &c.access))
        .add_cookie(cookie("rm_csrf", &c.csrf))
}

fn post_as(server: &TestServer, path: &str, c: &Creds) -> TestRequest {
    server
        .post(path)
        .add_header("hx-request", "true")
        .add_cookie(cookie("rm_access", &c.access))
        .add_cookie(cookie("rm_refresh", &c.refresh))
        .add_cookie(cookie("rm_csrf", &c.csrf))
        .add_header("x-csrf-token", c.csrf.as_str())
}

fn header(r: &TestResponse, name: &str) -> String {
    r.header(name).to_str().expect("ascii header").to_owned()
}

fn boss_form<'a>(name: &'a str, from: &'a str, folder: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![
        ("name", name),
        ("match_mode", "all"),
        ("c1_field", "from"),
        ("c1_op", "contains"),
        ("c1_value", from),
        ("c2_field", "subject"),
        ("c2_op", "contains"),
        ("c2_value", ""),
        ("c3_field", "to"),
        ("c3_op", "is"),
        ("c3_value", ""),
        ("folder", ""),
        ("new_folder", folder),
        ("mark_read", "on"),
    ]
}

const PAGES: [&str; 4] = [
    "/settings/filters",
    "/settings/filters/rule",
    "/settings/filters/advanced",
    "/settings/autoresponder",
];

#[sqlx::test(migrations = "../../migrations")]
async fn the_pages_need_a_session(pool: PgPool) {
    let server = site(&pool).await;
    for path in PAGES {
        let r = server.get(path).await;
        r.assert_status(StatusCode::SEE_OTHER);
        assert!(header(&r, "location").starts_with("/login"), "{path}");
    }
    let alice = sign_in(&server, "alice").await;
    for path in PAGES {
        get_as(&server, path, &alice).await.assert_status_ok();
    }
    // Without the CSRF token nothing changes.
    let r = server
        .post("/settings/filters/rule")
        .add_cookie(cookie("rm_access", &alice.access))
        .form(&boss_form("Boss", "boss@", "Boss"))
        .await;
    r.assert_status(StatusCode::FORBIDDEN);
    assert!(get_as(&server, "/settings/filters", &alice)
        .await
        .text()
        .contains("filters-empty"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_rule_is_created_listed_edited_reordered_and_deleted(pool: PgPool) {
    let server = site(&pool).await;
    let alice = sign_in(&server, "alice").await;

    let r = post_as(&server, "/settings/filters/rule", &alice)
        .form(&boss_form("Boss", "boss@", "Boss"))
        .await;
    r.assert_status_ok();
    // The mail server is not configured in this test, so the change is saved but not live.
    assert_eq!(header(&r, "hx-redirect"), "/settings/filters?done=pending");
    post_as(&server, "/settings/filters/rule", &alice)
        .form(&boss_form("News", "news@", "News"))
        .await
        .assert_status_ok();

    let page = get_as(&server, "/settings/filters", &alice).await.text();
    assert!(
        page.contains("Boss")
            && page.contains("boss@")
            && page.contains("move to Boss")
            && page.contains("mark as read"),
        "{page}"
    );
    assert!(
        page.find("Boss").unwrap() < page.find("News").unwrap(),
        "in the order created"
    );
    assert!(
        page.contains("Not active yet"),
        "the page says it is not live"
    );

    // Edit rule 0.
    let mut form = boss_form("Chief", "chief@", "Chief");
    form.push(("index", "0"));
    post_as(&server, "/settings/filters/rule", &alice)
        .form(&form)
        .await
        .assert_status_ok();
    let edit = get_as(&server, "/settings/filters/rule?rule=0", &alice)
        .await
        .text();
    assert!(
        edit.contains("value=\"Chief\"")
            && edit.contains("value=\"chief@\"")
            && edit.contains("value=\"Chief\""),
        "{edit}"
    );

    // Reorder, toggle, delete.
    post_as(&server, "/settings/filters/0/move/down", &alice)
        .await
        .assert_status_ok();
    let page = get_as(&server, "/settings/filters", &alice).await.text();
    assert!(
        page.find("News").unwrap() < page.find("Chief").unwrap(),
        "moved down"
    );
    post_as(&server, "/settings/filters/0/toggle", &alice)
        .form(&[("enable", "0")])
        .await
        .assert_status_ok();
    assert!(get_as(&server, "/settings/filters", &alice)
        .await
        .text()
        .contains("Disabled"));
    let r = post_as(&server, "/settings/filters/0/delete", &alice).await;
    r.assert_status_ok();
    assert!(!get_as(&server, "/settings/filters", &alice)
        .await
        .text()
        .contains("News"));
    // Out-of-range positions are refused.
    for path in [
        "/settings/filters/9/delete",
        "/settings/filters/9/toggle",
        "/settings/filters/9/move/up",
        "/settings/filters/0/move/sideways",
    ] {
        let r = post_as(&server, path, &alice)
            .form(&[("enable", "1")])
            .await;
        assert_eq!(r.status_code(), StatusCode::NOT_FOUND, "{path}");
    }
    get_as(&server, "/settings/filters/rule?rule=9", &alice)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rules_belong_to_one_user(pool: PgPool) {
    let server = site(&pool).await;
    let alice = sign_in(&server, "alice").await;
    let bob = sign_in(&server, "bob").await;
    post_as(&server, "/settings/filters/rule", &alice)
        .form(&boss_form("Alice only", "a@", "A"))
        .await
        .assert_status_ok();
    assert!(!get_as(&server, "/settings/filters", &bob)
        .await
        .text()
        .contains("Alice only"));
    // Bob's index 0 is his own (empty) list: it cannot reach Alice's rule.
    post_as(&server, "/settings/filters/0/delete", &bob)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert!(get_as(&server, "/settings/filters", &alice)
        .await
        .text()
        .contains("Alice only"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn bad_rules_are_explained_and_not_saved(pool: PgPool) {
    let server = site(&pool).await;
    let alice = sign_in(&server, "alice").await;
    let mut no_condition = boss_form("Empty", "", "X");
    no_condition.retain(|(k, _)| *k != "c1_value");
    let mut no_action = boss_form("NoAction", "x@", "");
    no_action.retain(|(k, _)| *k != "mark_read");
    let mut odd_field = boss_form("Odd", "x@", "X");
    odd_field.retain(|(k, _)| *k != "c1_field");
    odd_field.push(("c1_field", "received"));
    let mut bad_forward = boss_form("Fwd", "x@", "X");
    bad_forward.push(("forward", "not-an-address"));
    let mut bad_folder = boss_form("Folder", "x@", "../etc");
    bad_folder.retain(|(k, _)| *k != "new_folder");
    bad_folder.push(("new_folder", "../etc"));
    for (what, form) in [
        ("no condition", no_condition),
        ("no action", no_action),
        ("odd field", odd_field),
        ("bad forward", bad_forward),
        ("bad folder", bad_folder),
    ] {
        let r = post_as(&server, "/settings/filters/rule", &alice)
            .form(&form)
            .await;
        r.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.text().contains("alert-danger"), "{what}");
    }
    assert!(
        get_as(&server, "/settings/filters", &alice)
            .await
            .text()
            .contains("filters-empty"),
        "nothing was saved"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn rule_names_cannot_inject_markup(pool: PgPool) {
    let server = site(&pool).await;
    let alice = sign_in(&server, "alice").await;
    post_as(&server, "/settings/filters/rule", &alice)
        .form(&boss_form(
            "<script>alert(1)</script>",
            "<img src=x onerror=1>",
            "X",
        ))
        .await
        .assert_status_ok();
    for path in ["/settings/filters", "/settings/filters/rule?rule=0"] {
        let page = get_as(&server, path, &alice).await.text();
        assert!(
            !page.contains("<script>alert(1)</script>") && !page.contains("<img src=x"),
            "{path}"
        );
        assert!(
            page.contains("&lt;script&gt;") || page.contains("&#60;script&#62;"),
            "{path}: escaped"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_advanced_editor_switches_modes(pool: PgPool) {
    let server = site(&pool).await;
    let alice = sign_in(&server, "alice").await;
    post_as(&server, "/settings/filters/rule", &alice)
        .form(&boss_form("Boss", "boss@", "Boss"))
        .await
        .assert_status_ok();
    // In rule mode the editor starts from the generated script.
    let page = get_as(&server, "/settings/filters/advanced", &alice)
        .await
        .text();
    assert!(
        page.contains("fileinto :create") && page.contains("boss@"),
        "{page}"
    );

    let script = "require \"fileinto\";\r\nif header :contains \"subject\" \"invoice\" { fileinto \"Invoices\"; }\r\n";
    let r = post_as(&server, "/settings/filters/advanced", &alice)
        .form(&[("script", script)])
        .await;
    r.assert_status_ok();
    let page = get_as(&server, "/settings/filters", &alice).await.text();
    assert!(page.contains("own Sieve script"), "{page}");
    assert!(
        !page.contains("filters-table"),
        "the rule list is hidden while the script is used"
    );
    let editor = get_as(&server, "/settings/filters/advanced", &alice)
        .await
        .text();
    assert!(
        editor.contains("Invoices") && !editor.contains("\r\n"),
        "stored with plain newlines"
    );

    // Oversized or binary scripts are refused.
    let big = "#".repeat(70_000);
    post_as(&server, "/settings/filters/advanced", &alice)
        .form(&[("script", big.as_str())])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    // A 64 KB script is fine; far more than that is cut off before it is read.
    let max = "#".repeat(64 * 1024);
    post_as(&server, "/settings/filters/advanced", &alice)
        .form(&[("script", max.as_str())])
        .await
        .assert_status_ok();
    let huge = "#".repeat(300_000);
    post_as(&server, "/settings/filters/advanced", &alice)
        .form(&[("script", huge.as_str())])
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    post_as(&server, "/settings/filters/advanced", &alice)
        .form(&[("script", "a\u{1}b")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // Going back keeps the rules.
    post_as(&server, "/settings/filters/advanced/leave", &alice)
        .await
        .assert_status_ok();
    let page = get_as(&server, "/settings/filters", &alice).await.text();
    assert!(
        page.contains("filters-table") && page.contains("Boss"),
        "{page}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_autoresponder_is_saved_validated_and_private(pool: PgPool) {
    let server = site(&pool).await;
    let alice = sign_in(&server, "alice").await;
    let bob = sign_in(&server, "bob").await;
    let good = [
        ("enabled", "on"),
        ("starts_on", "2025-12-01"),
        ("ends_on", "2025-12-24"),
        ("subject", "Away"),
        ("body", "I am out.\r\nBack on the 25th."),
        ("days", "3"),
    ];
    let r = post_as(&server, "/settings/autoresponder", &alice)
        .form(&good)
        .await;
    r.assert_status_ok();
    assert_eq!(
        header(&r, "hx-redirect"),
        "/settings/autoresponder?done=pending"
    );
    let page = get_as(&server, "/settings/autoresponder", &alice)
        .await
        .text();
    assert!(
        page.contains("value=\"2025-12-01\"")
            && page.contains("value=\"2025-12-24\"")
            && page.contains("value=\"Away\"")
            && page.contains("Back on the 25th."),
        "{page}"
    );
    assert!(
        page.contains("alice@example.com"),
        "lists the addresses it answers for"
    );
    assert!(
        !get_as(&server, "/settings/autoresponder", &bob)
            .await
            .text()
            .contains("Back on the 25th."),
        "other users see their own"
    );

    for (what, form) in [
        (
            "end before start",
            vec![
                ("enabled", "on"),
                ("starts_on", "2025-12-24"),
                ("ends_on", "2025-12-01"),
                ("body", "x"),
                ("days", "3"),
            ],
        ),
        (
            "too many days",
            vec![("enabled", "on"), ("body", "x"), ("days", "99")],
        ),
        (
            "not a number",
            vec![("enabled", "on"), ("body", "x"), ("days", "abc")],
        ),
        (
            "no message",
            vec![("enabled", "on"), ("body", " "), ("days", "3")],
        ),
        (
            "bad date",
            vec![
                ("enabled", "on"),
                ("starts_on", "24.12.2025"),
                ("body", "x"),
                ("days", "3"),
            ],
        ),
        (
            "multi-line subject",
            vec![
                ("enabled", "on"),
                ("subject", "a\nb"),
                ("body", "x"),
                ("days", "3"),
            ],
        ),
    ] {
        let r = post_as(&server, "/settings/autoresponder", &alice)
            .form(&form)
            .await;
        assert_eq!(r.status_code(), StatusCode::UNPROCESSABLE_ENTITY, "{what}");
    }
    // Switching it off keeps the text for next time.
    post_as(&server, "/settings/autoresponder", &alice)
        .form(&[("subject", "Away"), ("body", "I am out."), ("days", "3")])
        .await
        .assert_status_ok();
    let page = get_as(&server, "/settings/autoresponder", &alice)
        .await
        .text();
    assert!(
        !page.contains("checked") && page.contains("I am out."),
        "{page}"
    );
}
