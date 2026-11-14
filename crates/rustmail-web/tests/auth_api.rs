//! End-to-end tests of the `/auth/*` HTTP API using `axum-test`, against real
//! PostgreSQL (each test gets its own freshly migrated database). Requires
//! `DATABASE_URL` (see `make dev`).
//!
//! Cookies are passed explicitly rather than saved by the test client, so tests
//! can replay old credentials the way an attacker or a stale tab would.

use std::time::Duration as StdDuration;

use axum::routing::get;
use axum::Router;
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_test::{TestResponse, TestServer};
use chrono::Duration;
use rustmail_core::auth::{AuthConfig, AuthService};
use rustmail_core::domain::{AuditQuery, NewDomain, NewUser, UserRole};
use rustmail_core::password::hash_password;
use rustmail_core::repository::{
    AuditLogRepository, DomainRepository, PgAuditLogRepository, PgDomainRepository,
    PgUserRepository, UserRepository,
};
use rustmail_web::auth::{self, ApiError, AuthState, AuthUser, DeploymentSecurity, RateLimits};
use rustmail_web::config::RateLimitSettings;
use serde_json::{json, Value};
use sqlx::PgPool;

const PASSWORD: &str = "correct-horse-battery-staple";
const CLIENT_IP: &str = "198.51.100.1";

// ─── harness ────────────────────────────────────────────────────────────────

fn generous() -> RateLimitSettings {
    RateLimitSettings {
        burst: 1_000,
        period_secs: 1,
    }
}

fn base_config() -> AuthConfig {
    AuthConfig::new("web-integration-test-secret-32-bytes!").expect("config")
}

struct App {
    router: Router,
    state: AuthState,
}

impl App {
    /// A test client whose requests all appear to come from `ip`.
    fn client(&self, ip: &str) -> TestServer {
        let mut server = TestServer::new(self.router.clone());
        server.add_header("x-forwarded-for", ip);
        server.add_header("user-agent", "auth-api-tests/1.0");
        server
    }
}

async fn admin_ping(user: AuthUser) -> Result<&'static str, ApiError> {
    user.require_role(UserRole::DomainAdmin)?;
    Ok("pong")
}

/// Seeds `example.com` with a regular user `alice` and a domain admin `dana`,
/// and builds the app (auth routes plus a role-gated `/admin/ping`).
async fn app(pool: &PgPool, config: AuthConfig, limits: RateLimits) -> App {
    let domain = PgDomainRepository::new(pool.clone())
        .create(&NewDomain {
            name: "example.com".into(),
            ..Default::default()
        })
        .await
        .expect("domain");
    let users = PgUserRepository::new(pool.clone());
    for (local, role) in [("alice", UserRole::User), ("dana", UserRole::DomainAdmin)] {
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

    let state = AuthState::new(
        AuthService::postgres(pool.clone(), config),
        DeploymentSecurity {
            cookie_secure: true,
            trust_proxy_headers: true,
        },
    )
    .expect("state");
    let (auth_routes, _janitor) = auth::router(state.clone(), limits).expect("router");
    let admin = Router::new()
        .route("/admin/ping", get(admin_ping))
        .with_state(state.clone());
    App {
        router: auth_routes.merge(admin),
        state,
    }
}

fn default_limits() -> RateLimits {
    RateLimits {
        login: generous(),
        refresh: generous(),
    }
}

fn cookie(name: &str, value: &str) -> Cookie<'static> {
    Cookie::new(name.to_owned(), value.to_owned())
}

/// Credentials a browser would hold after logging in.
#[derive(Clone)]
struct Creds {
    access: String,
    refresh: String,
    csrf: String,
}

impl Creds {
    fn from(response: &TestResponse) -> Self {
        let csrf = response.cookie("rm_csrf").value().to_owned();
        Self {
            access: response.cookie("rm_access").value().to_owned(),
            refresh: response.cookie("rm_refresh").value().to_owned(),
            csrf,
        }
    }
}

/// `GET /auth/csrf` as an anonymous visitor; returns the token.
async fn anon_csrf(server: &TestServer) -> String {
    let response = server.get("/auth/csrf").await;
    response.assert_status_ok();
    let token = response.json::<Value>()["csrf_token"]
        .as_str()
        .expect("token")
        .to_owned();
    assert_eq!(response.cookie("rm_csrf").value(), token);
    token
}

async fn login(server: &TestServer, email: &str, password: &str) -> TestResponse {
    let token = anon_csrf(server).await;
    server
        .post("/auth/login")
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .json(&json!({ "email": email, "password": password }))
        .await
}

async fn login_ok(server: &TestServer, email: &str) -> Creds {
    let response = login(server, email, PASSWORD).await;
    response.assert_status_ok();
    Creds::from(&response)
}

/// A state-changing request carrying the full set of cookies plus the CSRF header.
fn authed_post(server: &TestServer, path: &str, creds: &Creds) -> axum_test::TestRequest {
    server
        .post(path)
        .add_header("x-csrf-token", creds.csrf.as_str())
        .add_cookie(cookie("rm_csrf", &creds.csrf))
        .add_cookie(cookie("rm_access", &creds.access))
        .add_cookie(cookie("rm_refresh", &creds.refresh))
}

async fn me(server: &TestServer, access: &str) -> TestResponse {
    server
        .get("/auth/me")
        .add_cookie(cookie("rm_access", access))
        .await
}

/// The raw `Set-Cookie` header line for `name`, lower-cased, as the browser sees it.
fn raw_set_cookie(response: &TestResponse, name: &str) -> String {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|line| line.starts_with(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no Set-Cookie for {name}"))
        .to_ascii_lowercase()
}

fn error_code(response: &TestResponse) -> String {
    response.json::<Value>()["error"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

// ─── CSRF ───────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn csrf_endpoint_issues_a_cookie_matching_the_token(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let response = server.get("/auth/csrf").await;
    response.assert_status_ok();

    let csrf = response.cookie("rm_csrf");
    assert!(
        !raw_set_cookie(&response, "rm_csrf").contains("httponly"),
        "scripts must be able to read the CSRF cookie"
    );
    assert_eq!(csrf.secure(), Some(true));
    assert_eq!(csrf.same_site(), Some(SameSite::Lax));
    assert_eq!(response.header("cache-control"), "no-store");
}

#[sqlx::test(migrations = "../../migrations")]
async fn login_requires_a_genuine_matching_csrf_token(pool: PgPool) {
    let app = app(&pool, base_config(), default_limits()).await;
    let server = app.client(CLIENT_IP);
    let body = json!({ "email": "alice@example.com", "password": PASSWORD });

    let good = anon_csrf(&server).await;
    let other = anon_csrf(&server).await;
    let wrong_context = app.state.csrf.issue("some-other-session-id");

    let attempts: Vec<(&str, axum_test::TestRequest)> = vec![
        ("no token at all", server.post("/auth/login").json(&body)),
        (
            "header without cookie",
            server
                .post("/auth/login")
                .add_header("x-csrf-token", good.as_str())
                .json(&body),
        ),
        (
            "cookie without header",
            server
                .post("/auth/login")
                .add_cookie(cookie("rm_csrf", &good))
                .json(&body),
        ),
        (
            "header and cookie differ",
            server
                .post("/auth/login")
                .add_header("x-csrf-token", good.as_str())
                .add_cookie(cookie("rm_csrf", &other))
                .json(&body),
        ),
        (
            "matching but forged",
            server
                .post("/auth/login")
                .add_header("x-csrf-token", "AAAA.BBBB")
                .add_cookie(cookie("rm_csrf", "AAAA.BBBB"))
                .json(&body),
        ),
        (
            "genuine token issued for another session",
            server
                .post("/auth/login")
                .add_header("x-csrf-token", wrong_context.as_str())
                .add_cookie(cookie("rm_csrf", &wrong_context))
                .json(&body),
        ),
    ];
    for (label, request) in attempts {
        let response = request.await;
        assert_eq!(response.status_code(), 403, "{label}");
        assert_eq!(error_code(&response), "csrf_failed", "{label}");
    }

    // None of those reached the credential check.
    let audit = PgAuditLogRepository::new(pool)
        .list(&AuditQuery::default())
        .await
        .unwrap();
    assert!(audit.is_empty(), "CSRF failures must not attempt a login");

    // The same request with a proper token succeeds.
    login(&server, "alice@example.com", PASSWORD)
        .await
        .assert_status_ok();
}

// ─── login ──────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn login_sets_hardened_cookies_and_exposes_no_secrets(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let response = login(&server, "Alice@Example.com", PASSWORD).await;
    response.assert_status_ok();
    assert_eq!(response.header("cache-control"), "no-store");

    let access = response.cookie("rm_access");
    assert_eq!(access.http_only(), Some(true));
    assert_eq!(access.secure(), Some(true));
    assert_eq!(access.same_site(), Some(SameSite::Lax));
    assert_eq!(access.path(), Some("/"));

    let refresh = response.cookie("rm_refresh");
    assert_eq!(refresh.http_only(), Some(true));
    assert_eq!(refresh.secure(), Some(true));
    assert_eq!(refresh.same_site(), Some(SameSite::Strict));
    assert_eq!(
        refresh.path(),
        Some("/auth"),
        "only sent to the auth endpoints"
    );

    let csrf = response.cookie("rm_csrf");
    assert!(!raw_set_cookie(&response, "rm_csrf").contains("httponly"));
    for name in ["rm_access", "rm_refresh"] {
        assert!(
            raw_set_cookie(&response, name).contains("httponly"),
            "{name} must be HttpOnly"
        );
    }

    let text = response.text();
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["user"]["email"], "alice@example.com");
    assert_eq!(body["user"]["role"], "user");
    assert_eq!(body["csrf_token"], csrf.value());
    // Tokens and secrets live in cookies only.
    for secret in [
        access.value(),
        refresh.value(),
        PASSWORD,
        "password_hash",
        "argon2",
    ] {
        assert!(!text.contains(secret), "login response leaked {secret:?}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn bad_credentials_are_indistinguishable_over_http(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);

    let wrong = login(&server, "alice@example.com", "not-the-password").await;
    let unknown = login(&server, "nobody@example.com", "not-the-password").await;
    for response in [&wrong, &unknown] {
        assert_eq!(response.status_code(), 401);
        assert_eq!(error_code(response), "invalid_credentials");
        assert!(response.maybe_cookie("rm_access").is_none());
        assert!(response.maybe_cookie("rm_refresh").is_none());
    }
    assert_eq!(
        wrong.text(),
        unknown.text(),
        "identical bodies, no enumeration"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn malformed_login_requests_are_rejected_early(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let token = anon_csrf(&server).await;
    let post = || {
        server
            .post("/auth/login")
            .add_header("x-csrf-token", token.as_str())
            .add_cookie(cookie("rm_csrf", &token))
    };

    // Not JSON: a cross-site HTML form cannot send application/json.
    let response = post().text("email=alice@example.com&password=x").await;
    assert_eq!(response.status_code(), 415);

    let response = post()
        .json(&json!({ "email": "alice@example.com", "password": "" }))
        .await;
    assert_eq!(response.status_code(), 400);

    let long_password = "x".repeat(2_000);
    let response = post()
        .json(&json!({ "email": "alice@example.com", "password": long_password }))
        .await;
    assert_eq!(response.status_code(), 400);

    let response = post()
        .json(&json!({ "email": "alice@example.com", "password": "x", "admin": true }))
        .await;
    assert_eq!(response.status_code(), 422, "unknown fields are refused");

    let huge = "x".repeat(40 * 1024);
    let response = post()
        .json(&json!({ "email": "alice@example.com", "password": huge }))
        .await;
    assert_eq!(response.status_code(), 413);
}

#[sqlx::test(migrations = "../../migrations")]
async fn lockout_is_reported_with_retry_after_once_the_password_is_right(pool: PgPool) {
    let config = base_config().with_lockout(3, Duration::seconds(60));
    let server = app(&pool, config, default_limits()).await.client(CLIENT_IP);

    for _ in 0..3 {
        let response = login(&server, "alice@example.com", "wrong-password-1").await;
        assert_eq!(response.status_code(), 401);
    }
    let response = login(&server, "alice@example.com", PASSWORD).await;
    assert_eq!(response.status_code(), 403);
    assert_eq!(error_code(&response), "account_locked");
    let retry_after: i64 = response
        .header("retry-after")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (1..=60).contains(&retry_after),
        "retry-after was {retry_after}"
    );
    assert!(response.maybe_cookie("rm_access").is_none());
}

// ─── authenticated requests ─────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn me_requires_a_valid_access_cookie(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let creds = login_ok(&server, "alice@example.com").await;

    let response = me(&server, &creds.access).await;
    response.assert_status_ok();
    let body: Value = response.json();
    assert_eq!(body["email"], "alice@example.com");
    assert!(body.get("password_hash").is_none());

    let no_cookie = server.get("/auth/me").await;
    assert_eq!(no_cookie.status_code(), 401);
    assert_eq!(error_code(&no_cookie), "invalid_token");

    let mut tampered = creds.access.clone();
    tampered.push('x');
    assert_eq!(me(&server, &tampered).await.status_code(), 401);
    assert_eq!(me(&server, "garbage").await.status_code(), 401);
}

#[sqlx::test(migrations = "../../migrations")]
async fn role_gating_returns_401_403_or_200(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let user = login_ok(&server, "alice@example.com").await;
    let admin = login_ok(&server, "dana@example.com").await;
    let ping = |access: Option<&str>| {
        let request = server.get("/admin/ping");
        match access {
            Some(token) => request.add_cookie(cookie("rm_access", token)),
            None => request,
        }
    };

    let anonymous = ping(None).await;
    assert_eq!(anonymous.status_code(), 401);

    let forbidden = ping(Some(&user.access)).await;
    assert_eq!(forbidden.status_code(), 403);
    assert_eq!(error_code(&forbidden), "forbidden");

    let allowed = ping(Some(&admin.access)).await;
    allowed.assert_status_ok();
    assert_eq!(allowed.text(), "pong");
}

// ─── refresh ────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn refresh_rotates_every_cookie_and_retires_the_old_session(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let first = login_ok(&server, "alice@example.com").await;

    let response = authed_post(&server, "/auth/refresh", &first).await;
    response.assert_status_ok();
    let second = Creds::from(&response);
    assert_ne!(second.access, first.access);
    assert_ne!(second.refresh, first.refresh);
    assert_ne!(second.csrf, first.csrf);
    assert_eq!(response.json::<Value>()["csrf_token"], second.csrf.as_str());

    me(&server, &second.access).await.assert_status_ok();
    assert_eq!(
        me(&server, &first.access).await.status_code(),
        401,
        "old session is gone"
    );

    // Replay inside the grace window: rejected, but nobody is logged out.
    let replay = authed_post(&server, "/auth/refresh", &first).await;
    assert_eq!(replay.status_code(), 401);
    assert!(
        replay.maybe_cookie("rm_access").is_none(),
        "a failed refresh must not touch cookies"
    );
    me(&server, &second.access).await.assert_status_ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn replaying_a_stolen_refresh_token_logs_everyone_out(pool: PgPool) {
    let config = base_config().with_reuse_grace(Duration::zero());
    let server = app(&pool, config, default_limits()).await.client(CLIENT_IP);
    let victim = login_ok(&server, "alice@example.com").await;
    let rotated = Creds::from(&authed_post(&server, "/auth/refresh", &victim).await);
    me(&server, &rotated.access).await.assert_status_ok();

    // The thief replays the token the victim already rotated away from.
    let replay = authed_post(&server, "/auth/refresh", &victim).await;
    assert_eq!(replay.status_code(), 401);

    assert_eq!(
        me(&server, &rotated.access).await.status_code(),
        401,
        "victim's new session is revoked too"
    );
    assert_eq!(
        authed_post(&server, "/auth/refresh", &rotated)
            .await
            .status_code(),
        401
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn refresh_works_after_the_access_token_lapses(pool: PgPool) {
    let config = base_config().with_access_ttl(Duration::seconds(1));
    let server = app(&pool, config, default_limits()).await.client(CLIENT_IP);
    let creds = login_ok(&server, "alice@example.com").await;

    tokio::time::sleep(StdDuration::from_millis(1300)).await;
    assert_eq!(
        me(&server, &creds.access).await.status_code(),
        401,
        "JWT expired"
    );

    // The CSRF token is bound to the session, which the lapsed (but validly
    // signed) access cookie still identifies, so refresh is not locked out.
    let response = authed_post(&server, "/auth/refresh", &creds).await;
    response.assert_status_ok();
    me(&server, &Creds::from(&response).access)
        .await
        .assert_status_ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn refresh_needs_the_cookie_and_csrf(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let creds = login_ok(&server, "alice@example.com").await;

    // No CSRF header: rejected before touching the session.
    let response = server
        .post("/auth/refresh")
        .add_cookie(cookie("rm_refresh", &creds.refresh))
        .add_cookie(cookie("rm_access", &creds.access))
        .await;
    assert_eq!(response.status_code(), 403);
    me(&server, &creds.access).await.assert_status_ok();

    // CSRF fine but no refresh cookie.
    let token = anon_csrf(&server).await;
    let response = server
        .post("/auth/refresh")
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .await;
    assert_eq!(response.status_code(), 401);
    assert_eq!(error_code(&response), "invalid_token");
}

// ─── logout ─────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn logout_clears_cookies_and_revokes_the_session_immediately(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let creds = login_ok(&server, "alice@example.com").await;
    let other_device = login_ok(&server, "alice@example.com").await;

    let response = authed_post(&server, "/auth/logout", &creds).await;
    assert_eq!(response.status_code(), 204);
    for (name, path) in [
        ("rm_access", "/"),
        ("rm_refresh", "/auth"),
        ("rm_csrf", "/"),
    ] {
        let cleared = response.cookie(name);
        assert_eq!(cleared.value(), "", "{name} cleared");
        assert_eq!(
            cleared.max_age().map(|a| a.whole_seconds()),
            Some(0),
            "{name} expires now"
        );
        assert_eq!(
            cleared.path(),
            Some(path),
            "{name} path must match for the browser to delete it"
        );
    }

    // The still-unexpired access token no longer works, and neither does refresh.
    assert_eq!(me(&server, &creds.access).await.status_code(), 401);
    assert_eq!(
        authed_post(&server, "/auth/refresh", &creds)
            .await
            .status_code(),
        401
    );
    // Other devices are untouched.
    me(&server, &other_device.access).await.assert_status_ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn logout_is_csrf_protected_and_idempotent(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let creds = login_ok(&server, "alice@example.com").await;

    // A cross-site page cannot log the user out: no CSRF header, nothing happens.
    let response = server
        .post("/auth/logout")
        .add_cookie(cookie("rm_access", &creds.access))
        .add_cookie(cookie("rm_refresh", &creds.refresh))
        .await;
    assert_eq!(response.status_code(), 403);
    me(&server, &creds.access).await.assert_status_ok();

    // An anonymous token does not satisfy a logged-in session's context either.
    let anon = anon_csrf(&server).await;
    let response = server
        .post("/auth/logout")
        .add_header("x-csrf-token", anon.as_str())
        .add_cookie(cookie("rm_csrf", &anon))
        .add_cookie(cookie("rm_access", &creds.access))
        .await;
    assert_eq!(response.status_code(), 403);
    me(&server, &creds.access).await.assert_status_ok();

    assert_eq!(
        authed_post(&server, "/auth/logout", &creds)
            .await
            .status_code(),
        204
    );
    assert_eq!(
        authed_post(&server, "/auth/logout", &creds)
            .await
            .status_code(),
        204
    );

    // With no session at all, logout still succeeds with an anonymous token.
    let anon = anon_csrf(&server).await;
    let response = server
        .post("/auth/logout")
        .add_header("x-csrf-token", anon.as_str())
        .add_cookie(cookie("rm_csrf", &anon))
        .await;
    assert_eq!(response.status_code(), 204);
}

// ─── rate limiting ──────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn login_is_rate_limited_per_client_ip(pool: PgPool) {
    // Burst of 3 that effectively never refills during the test.
    let limits = RateLimits {
        login: RateLimitSettings {
            burst: 3,
            period_secs: 3_600,
        },
        refresh: generous(),
    };
    let app = app(&pool, base_config(), limits).await;
    let attacker = app.client("203.0.113.66");

    for _ in 0..3 {
        let response = login(&attacker, "alice@example.com", "wrong-password-1").await;
        assert_eq!(response.status_code(), 401);
    }

    // The fourth attempt is stopped before CSRF or credentials are even examined.
    let blocked = attacker
        .post("/auth/login")
        .json(&json!({ "email": "x@example.com", "password": "x" }))
        .await;
    assert_eq!(blocked.status_code(), 429);
    assert_eq!(error_code(&blocked), "rate_limited");
    let retry_after: u64 = blocked
        .header("retry-after")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(retry_after > 0);

    // Even the right password is refused while limited...
    assert_eq!(
        login(&attacker, "alice@example.com", PASSWORD)
            .await
            .status_code(),
        429
    );

    // ...but another client is unaffected,
    let bystander = app.client("203.0.113.77");
    login(&bystander, "alice@example.com", PASSWORD)
        .await
        .assert_status_ok();

    // and the limit is per route: refresh has its own bucket.
    let refresh = attacker.post("/auth/refresh").await;
    assert_ne!(refresh.status_code(), 429);
}

#[sqlx::test(migrations = "../../migrations")]
async fn forged_forwarded_for_prefixes_do_not_evade_the_limit(pool: PgPool) {
    let limits = RateLimits {
        login: RateLimitSettings {
            burst: 2,
            period_secs: 3_600,
        },
        refresh: generous(),
    };
    let app = app(&pool, base_config(), limits).await;
    // The attacker prepends fresh fake addresses; our proxy appends the real one.
    let hit = |fake: &str| {
        let mut server = TestServer::new(app.router.clone());
        server.add_header("x-forwarded-for", format!("{fake}, 203.0.113.99"));
        server
    };

    for fake in ["1.1.1.1", "2.2.2.2"] {
        let server = hit(fake);
        let response = server.post("/auth/login").json(&json!({})).await;
        assert_ne!(response.status_code(), 429);
    }
    let response = hit("3.3.3.3").post("/auth/login").json(&json!({})).await;
    assert_eq!(
        response.status_code(),
        429,
        "the rightmost (proxy-added) address is the key"
    );
}

// ─── audit ──────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn http_requests_leave_an_audit_trail_with_the_client_address(pool: PgPool) {
    let server = app(&pool, base_config(), default_limits())
        .await
        .client(CLIENT_IP);
    let _ = login(&server, "alice@example.com", "wrong-password-1").await;
    let creds = login_ok(&server, "alice@example.com").await;
    authed_post(&server, "/auth/logout", &creds).await;

    let mut entries = PgAuditLogRepository::new(pool)
        .list(&AuditQuery::default())
        .await
        .unwrap();
    entries.reverse();
    let summary: Vec<(&str, String)> = entries
        .iter()
        .map(|e| (e.action.as_str(), format!("{:?}", e.outcome)))
        .collect();
    assert_eq!(
        summary,
        [
            ("auth.login", "Failure".to_owned()),
            ("auth.login", "Success".to_owned()),
            ("auth.logout", "Success".to_owned()),
        ]
    );
    assert!(entries
        .iter()
        .all(|e| e.ip_address.as_deref() == Some(CLIENT_IP)));
    assert!(entries
        .iter()
        .all(|e| e.user_agent.as_deref() == Some("auth-api-tests/1.0")));
}
