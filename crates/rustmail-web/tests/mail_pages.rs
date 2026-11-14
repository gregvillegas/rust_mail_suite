//! Webmail routes that can be checked without a mail server. Requires `DATABASE_URL`.
//!
//! The real behaviour (reading, sending, folders, WebSocket) needs Dovecot and
//! Postfix and is covered by `scripts/browser-smoke/webmail.mjs` and the live tests
//! in `rustmail-imap`. Here: who can reach what, and what happens when webmail is
//! not configured.

use std::path::PathBuf;

use axum::http::StatusCode;
use axum::Router;
use axum_extra::extract::cookie::Cookie;
use axum_test::TestServer;
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

async fn site(pool: &PgPool) -> Router {
    let domain = PgDomainRepository::new(pool.clone())
        .create(&NewDomain {
            name: "example.com".into(),
            ..Default::default()
        })
        .await
        .expect("domain");
    PgUserRepository::new(pool.clone())
        .create(&NewUser {
            domain_id: domain.id,
            local_part: "alice".into(),
            password_hash: hash_password(PASSWORD).expect("hash"),
            display_name: "Alice".into(),
            role: UserRole::User,
            locale: "en".into(),
        })
        .await
        .expect("user");
    let auth = AuthState::new(
        AuthService::postgres(
            pool.clone(),
            AuthConfig::new("mail-pages-test-secret-32-bytes-long").expect("config"),
        ),
        DeploymentSecurity {
            cookie_secure: true,
            trust_proxy_headers: true,
        },
    )
    .expect("auth state");
    let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static");
    let state = AppState::new(auth, pool.clone(), static_dir, AssetSource::Local).expect("state");
    build_app(state, limits()).expect("app").0
}

fn client(router: Router) -> TestServer {
    let mut server = TestServer::new(router);
    server.add_header("x-forwarded-for", "198.51.100.30");
    server
}

struct Creds {
    access: String,
    csrf: String,
}

async fn sign_in(server: &TestServer) -> Creds {
    let page = server.get("/login").await;
    let token = page.cookie("rm_csrf").value().to_owned();
    let response = server
        .post("/login")
        .add_header("hx-request", "true")
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(Cookie::new("rm_csrf", token.clone()))
        .form(&[
            ("email", "alice@example.com"),
            ("password", PASSWORD),
            ("next", "/"),
        ])
        .await;
    response.assert_status_ok();
    Creds {
        access: response.cookie("rm_access").value().to_owned(),
        csrf: response.cookie("rm_csrf").value().to_owned(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_mail_route_needs_a_session(pool: PgPool) {
    let server = client(site(&pool).await);
    for path in [
        "/mail",
        "/mail/list",
        "/mail/folders",
        "/mail/folders/nav",
        "/mail/compose",
        "/mail/message/1?folder=INBOX",
        "/mail/message/1/body?folder=INBOX",
        "/mail/message/1/attachment/0?folder=INBOX",
        "/mail/ws",
    ] {
        let response = server.get(path).await;
        assert!(
            response.status_code() == StatusCode::SEE_OTHER
                && response
                    .header("location")
                    .to_str()
                    .unwrap()
                    .starts_with("/login"),
            "{path}: {}",
            response.status_code()
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_inbox_shortcut_leads_to_the_mail_page(pool: PgPool) {
    let server = client(site(&pool).await);
    let response = server.get("/mail/inbox").await;
    response.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        response.header("location").to_str().unwrap(),
        "/mail?folder=INBOX"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn mail_actions_need_csrf_protection(pool: PgPool) {
    let server = client(site(&pool).await);
    let creds = sign_in(&server).await;
    for path in [
        "/mail/send",
        "/mail/message/1/flag",
        "/mail/message/1/move",
        "/mail/message/1/delete",
        "/mail/message/1/spam",
        "/mail/folders/create",
        "/mail/folders/rename",
        "/mail/folders/delete",
    ] {
        // Signed in, but without the CSRF header: refused before anything happens.
        let response = server
            .post(path)
            .add_cookie(Cookie::new("rm_access", creds.access.clone()))
            .add_cookie(Cookie::new("rm_csrf", creds.csrf.clone()))
            .await;
        response.assert_status(StatusCode::FORBIDDEN);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn without_webmail_settings_the_pages_say_so(pool: PgPool) {
    let server = client(site(&pool).await);
    let creds = sign_in(&server).await;
    for path in ["/mail", "/mail/compose", "/mail/folders"] {
        let response = server
            .get(path)
            .add_cookie(Cookie::new("rm_access", creds.access.clone()))
            .add_cookie(Cookie::new("rm_csrf", creds.csrf.clone()))
            .await;
        response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
        assert!(response.text().contains("MAIL_MASTER_PASSWORD"), "{path}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_navigation_offers_mail_to_everyone_signed_in(pool: PgPool) {
    let server = client(site(&pool).await);
    let creds = sign_in(&server).await;
    let html = server
        .get("/")
        .add_cookie(Cookie::new("rm_access", creds.access.clone()))
        .add_cookie(Cookie::new("rm_csrf", creds.csrf.clone()))
        .await
        .text();
    assert!(html.contains("href=\"/mail\""), "{html}");
    // And the script that powers it is loaded like every other script: from our own origin.
    assert!(html.contains("/static/js/mail.js"), "{html}");
    assert!(!html.contains("<script>"), "no inline scripts");
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_mail_script_has_no_dynamic_code_execution(pool: PgPool) {
    // The CSP forbids eval; keep the file honest so a later edit cannot sneak it in.
    let _ = pool;
    let js = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static/js/mail.js"),
    )
    .expect("mail.js");
    for forbidden in [
        "eval(",
        "new Function",
        "innerHTML",
        "document.write",
        "setTimeout(\"",
    ] {
        assert!(
            !js.contains(forbidden),
            "{forbidden} must not appear in mail.js"
        );
    }
}
