//! End-to-end tests of the HTML UI using `axum-test`, against real PostgreSQL
//! (each test gets its own freshly migrated database). Requires `DATABASE_URL`.
//!
//! These cover what the server decides: markup, headers, redirects, cookies,
//! localisation, escaping, error pages, static files. Behaviour that only exists
//! in a browser (Alpine, htmx swaps, the theme toggle) is covered by the browser
//! smoke test described in PHASE_3_COMPLETE.md.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use axum::Router;
use axum_extra::extract::cookie::Cookie;
use axum_test::{TestRequest, TestResponse, TestServer};
use chrono::Duration;
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

// ─── harness ────────────────────────────────────────────────────────────────

fn static_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static")
}

fn generous() -> RateLimitSettings {
    RateLimitSettings {
        burst: 1_000,
        period_secs: 1,
    }
}

fn limits() -> RateLimits {
    RateLimits {
        login: generous(),
        refresh: generous(),
    }
}

fn auth_config() -> AuthConfig {
    AuthConfig::new("pages-integration-test-secret-32-bytes").expect("config")
}

struct Site {
    router: Router,
}

impl Site {
    fn client(&self, ip: &str) -> TestServer {
        let mut server = TestServer::new(self.router.clone());
        server.add_header("x-forwarded-for", ip);
        server.add_header("user-agent", "pages-tests/1.0");
        server
    }
}

/// Builds the full app with `example.com` and three users: `alice` (user),
/// `dana` (domain admin) and `root` (super admin).
async fn site(pool: &PgPool, source: AssetSource, config: AuthConfig, limits: RateLimits) -> Site {
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
        AuthService::postgres(pool.clone(), config),
        DeploymentSecurity {
            cookie_secure: true,
            trust_proxy_headers: true,
        },
    )
    .expect("auth state");
    let state = AppState::new(auth, pool.clone(), static_dir(), source).expect("app state");
    let (router, _janitor) = build_app(state, limits).expect("app");
    Site { router }
}

async fn default_site(pool: &PgPool) -> Site {
    site(pool, AssetSource::Local, auth_config(), limits()).await
}

fn cookie(name: &str, value: &str) -> Cookie<'static> {
    Cookie::new(name.to_owned(), value.to_owned())
}

fn htmx(request: TestRequest) -> TestRequest {
    request.add_header("hx-request", "true")
}

#[derive(Clone)]
struct Creds {
    access: String,
    refresh: String,
    csrf: String,
}

impl Creds {
    fn from(response: &TestResponse) -> Self {
        Self {
            access: response.cookie("rm_access").value().to_owned(),
            refresh: response.cookie("rm_refresh").value().to_owned(),
            csrf: response.cookie("rm_csrf").value().to_owned(),
        }
    }
}

/// `GET /login` as a visitor; returns the CSRF token from the cookie it sets.
async fn visitor_csrf(server: &TestServer) -> String {
    let response = server.get("/login").await;
    response.assert_status_ok();
    response.cookie("rm_csrf").value().to_owned()
}

/// Submits the sign-in form the way htmx does.
async fn submit_login(
    server: &TestServer,
    email: &str,
    password: &str,
    next: &str,
) -> TestResponse {
    let token = visitor_csrf(server).await;
    htmx(server.post("/login"))
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .form(&[("email", email), ("password", password), ("next", next)])
        .await
}

async fn sign_in(server: &TestServer, email: &str) -> Creds {
    let response = submit_login(server, email, PASSWORD, "/").await;
    response.assert_status_ok();
    Creds::from(&response)
}

fn with_session(request: TestRequest, creds: &Creds) -> TestRequest {
    request
        .add_cookie(cookie("rm_access", &creds.access))
        .add_cookie(cookie("rm_csrf", &creds.csrf))
}

fn post_as(server: &TestServer, path: &str, creds: &Creds) -> TestRequest {
    with_session(htmx(server.post(path)), creds)
        .add_cookie(cookie("rm_refresh", &creds.refresh))
        .add_header("x-csrf-token", creds.csrf.as_str())
}

fn location(response: &TestResponse) -> String {
    response
        .header("location")
        .to_str()
        .expect("location is ascii")
        .to_owned()
}

fn header_str(response: &TestResponse, name: &str) -> String {
    response
        .header(name)
        .to_str()
        .expect("header is ascii")
        .to_owned()
}

/// Every `href`/`src` value pointing at `/static/` in `html`.
fn static_refs(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    for attr in ["href=\"", "src=\""] {
        for (idx, _) in html.match_indices(attr) {
            let rest = &html[idx + attr.len()..];
            if let Some(end) = rest.find('"') {
                let value = &rest[..end];
                if value.starts_with("/static/") {
                    found.push(value.replace("&amp;", "&"));
                }
            }
        }
    }
    found
}

// ─── sign-in page ───────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn login_page_renders_an_accessible_form(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let response = server.get("/login").await;
    response.assert_status_ok();
    assert!(header_str(&response, "content-type").starts_with("text/html"));
    let html = response.text();

    assert!(html.contains("<html lang=\"en\""));
    assert!(html.contains("<title>Sign in · RustMail</title>"));
    assert!(html.contains("hx-post=\"/login\""));
    // Labelled inputs with the right autofill hints.
    assert!(html.contains("<label for=\"login-email\""));
    assert!(html.contains("id=\"login-email\"") && html.contains("autocomplete=\"username\""));
    assert!(html.contains("<label for=\"login-password\""));
    assert!(html.contains("autocomplete=\"current-password\""));
    assert!(html.contains("href=\"#main\""), "skip link");
    assert!(html.contains("<noscript>"));
    // The CSRF cookie the form will echo.
    assert!(response.maybe_cookie("rm_csrf").is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn pages_use_no_inline_scripts_styles_or_event_handlers(pool: PgPool) {
    let site = default_site(&pool).await;
    let server = site.client("198.51.100.1");
    let creds = sign_in(&server, "dana@example.com").await;

    let mut pages = vec![
        server.get("/login").await.text(),
        server.get("/session/renew").await.text(),
    ];
    pages.push(with_session(server.get("/"), &creds).await.text());
    pages.push(with_session(server.get("/admin"), &creds).await.text());
    pages.push(server.get("/no-such-page").await.text());
    for html in pages {
        // Every <script> must be external.
        for (idx, _) in html.match_indices("<script") {
            let tag = &html[idx..html[idx..].find('>').map_or(html.len(), |e| idx + e)];
            assert!(tag.contains("src="), "inline script: {tag}");
        }
        for forbidden in [
            " style=\"",
            " onclick=",
            " onload=",
            " onerror=",
            " onsubmit=",
            "javascript:",
        ] {
            assert!(!html.contains(forbidden), "found {forbidden:?}");
        }
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_page_sends_the_security_headers(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    for path in [
        "/login",
        "/healthz",
        "/auth/csrf",
        "/no-such-page",
        "/static/css/app.css",
    ] {
        let response = server.get(path).await;
        let csp = header_str(&response, "content-security-policy");
        assert!(
            csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"),
            "{path}"
        );
        assert!(
            csp.contains("object-src 'none'") && csp.contains("base-uri 'none'"),
            "{path}"
        );
        assert!(
            !csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"),
            "{path}"
        );
        assert_eq!(
            header_str(&response, "x-content-type-options"),
            "nosniff",
            "{path}"
        );
        assert_eq!(header_str(&response, "x-frame-options"), "DENY", "{path}");
        assert_eq!(
            header_str(&response, "referrer-policy"),
            "same-origin",
            "{path}"
        );
        assert!(
            header_str(&response, "permissions-policy").contains("camera=()"),
            "{path}"
        );
        assert_eq!(
            header_str(&response, "cross-origin-opener-policy"),
            "same-origin",
            "{path}"
        );
    }
    // Pages and API responses must never be cached; static files have their own policy.
    for path in ["/login", "/no-such-page", "/auth/csrf"] {
        assert_eq!(
            header_str(&server.get(path).await, "cache-control"),
            "no-store",
            "{path}"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn cdn_mode_adds_integrity_a_fallback_and_allows_only_the_cdn(pool: PgPool) {
    let server = site(&pool, AssetSource::Cdn, auth_config(), limits())
        .await
        .client("198.51.100.1");
    let response = server.get("/login").await;
    let html = response.text();

    for lib in ["bootstrap", "htmx", "alpine"] {
        assert!(html.contains(&format!("data-lib=\"{lib}\"")), "{lib}");
    }
    assert_eq!(html.matches("integrity=\"sha384-").count(), 3);
    assert_eq!(html.matches("crossorigin=\"anonymous\"").count(), 3);
    assert_eq!(html.matches("data-fallback=\"/static/vendor/").count(), 3);
    assert!(html.contains("https://cdn.jsdelivr.net/npm/bootstrap@5.3.9/"));
    assert!(
        html.contains("js/asset-fallback.js"),
        "the fallback script is only needed in CDN mode"
    );

    let csp = header_str(&response, "content-security-policy");
    assert!(csp.contains("script-src 'self' https://cdn.jsdelivr.net;"));
    assert!(csp.contains("style-src 'self' https://cdn.jsdelivr.net;"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn local_mode_never_references_a_third_party_origin(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let response = server.get("/login").await;
    let html = response.text();

    assert!(!html.contains("jsdelivr") && !html.contains("integrity="));
    assert!(!html.contains("asset-fallback.js"));
    assert!(
        !html.contains("http://") && !html.contains("https://"),
        "no absolute URLs at all"
    );
    assert!(!header_str(&response, "content-security-policy").contains("jsdelivr"));
    assert!(html.contains("/static/vendor/bootstrap/5.3.9/bootstrap.min.css?v="));
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_static_reference_on_every_page_resolves(pool: PgPool) {
    for source in [AssetSource::Local, AssetSource::Cdn] {
        let site = bare_site(&pool, source).await;
        let server = site.client("198.51.100.1");
        let mut seen = 0;
        for path in ["/login", "/session/renew"] {
            for url in static_refs(&server.get(path).await.text()) {
                let response = server.get(&url).await;
                response.assert_status_ok();
                assert!(url.contains("?v="), "{url} should be content-addressed");
                seen += 1;
            }
        }
        assert!(seen >= 5, "{source:?}: only {seen} static references found");
    }
}

/// An app with no seeded users, for tests that only fetch public pages.
async fn bare_site(pool: &PgPool, source: AssetSource) -> Site {
    let auth = AuthState::new(
        AuthService::postgres(pool.clone(), auth_config()),
        DeploymentSecurity {
            cookie_secure: true,
            trust_proxy_headers: true,
        },
    )
    .expect("auth state");
    let state = AppState::new(auth, pool.clone(), static_dir(), source).expect("app state");
    Site {
        router: build_app(state, limits()).expect("app").0,
    }
}

// ─── localisation ───────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn language_follows_accept_language_then_the_cookie(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");

    let german = server
        .get("/login")
        .add_header("accept-language", "de-DE,de;q=0.9,en;q=0.5")
        .await
        .text();
    assert!(german.contains("<html lang=\"de\""));
    assert!(german.contains("Anmelden") && german.contains("E-Mail-Adresse"));
    assert!(!german.contains("Email address"));

    // An explicit choice beats the browser's preference.
    let english = server
        .get("/login")
        .add_header("accept-language", "de")
        .add_cookie(cookie("rm_lang", "en"))
        .await
        .text();
    assert!(english.contains("<html lang=\"en\"") && english.contains("Email address"));

    // Unsupported languages fall back to English; junk cookies are ignored.
    let fallback = server
        .get("/login")
        .add_header("accept-language", "fr-FR,ja;q=0.8")
        .add_cookie(cookie("rm_lang", "tlh"))
        .await
        .text();
    assert!(fallback.contains("<html lang=\"en\""));
}

#[sqlx::test(migrations = "../../migrations")]
async fn choosing_a_language_sets_a_cookie_and_refreshes(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let token = visitor_csrf(&server).await;
    let post = |lang: &str| {
        htmx(server.post("/lang"))
            .add_header("x-csrf-token", token.as_str())
            .add_cookie(cookie("rm_csrf", &token))
            .form(&[("lang", lang)])
    };

    let response = post("de").await;
    response.assert_status_ok();
    assert_eq!(header_str(&response, "hx-refresh"), "true");
    let lang = response.cookie("rm_lang");
    assert_eq!(lang.value(), "de");
    assert_eq!(lang.path(), Some("/"));
    assert!(lang.max_age().is_some_and(|a| a.whole_days() >= 364));

    assert_eq!(
        post("klingon").await.status_code(),
        400,
        "unknown languages are refused"
    );

    // Without CSRF the preference cannot be changed by another site.
    let forged = server.post("/lang").form(&[("lang", "de")]).await;
    assert_eq!(forged.status_code(), 403);
}

// ─── redirects for signed-out visitors ──────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn protected_pages_redirect_to_sign_in_and_remember_where_you_were(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");

    let root = server.get("/").await;
    assert_eq!(root.status_code(), 303);
    assert_eq!(
        location(&root),
        "/login",
        "no point remembering the default page"
    );

    let admin = server.get("/admin").await;
    assert_eq!(admin.status_code(), 303);
    assert_eq!(location(&admin), "/login?next=%2Fadmin");

    let partial = server.get("/partials/activity").await;
    assert_eq!(partial.status_code(), 303);
}

#[sqlx::test(migrations = "../../migrations")]
async fn htmx_requests_get_hx_redirect_instead_of_a_redirect_they_would_swallow(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let response = htmx(server.get("/partials/activity"))
        .add_header("hx-current-url", "http://localhost:8080/admin?tab=users")
        .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        header_str(&response, "hx-redirect"),
        "/login?next=%2Fadmin%3Ftab%3Dusers"
    );
    assert!(response.text().is_empty());

    // A hostile HX-Current-URL cannot steer the redirect off-site.
    let hostile = htmx(server.get("/partials/activity"))
        .add_header("hx-current-url", "http://localhost:8080//evil.example/x")
        .await;
    assert_eq!(header_str(&hostile, "hx-redirect"), "/login");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_lapsed_but_validly_signed_token_goes_to_silent_renewal(pool: PgPool) {
    let config = auth_config().with_access_ttl(Duration::seconds(1));
    let server = site(&pool, AssetSource::Local, config, limits())
        .await
        .client("198.51.100.1");
    let creds = sign_in(&server, "alice@example.com").await;

    tokio::time::sleep(StdDuration::from_millis(1300)).await;
    let lapsed = with_session(server.get("/admin"), &creds).await;
    assert_eq!(lapsed.status_code(), 303);
    assert_eq!(location(&lapsed), "/session/renew?next=%2Fadmin");

    // A forged or garbage token has nothing to renew.
    let garbage = server
        .get("/admin")
        .add_cookie(cookie("rm_access", "garbage"))
        .await;
    assert_eq!(location(&garbage), "/login?next=%2Fadmin");

    // The renewal page itself is public, has no navigation, and loads its script.
    let renew = server.get("/session/renew").await;
    renew.assert_status_ok();
    let html = renew.text();
    assert!(html.contains("js/session.js") && html.contains("id=\"renew-failed\" hidden"));
    assert!(!html.contains("<nav"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn signed_in_visitors_skip_the_form_but_not_to_an_off_site_url(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let creds = sign_in(&server, "alice@example.com").await;

    let already = with_session(server.get("/login?next=%2Fadmin"), &creds).await;
    assert_eq!(already.status_code(), 303);
    assert_eq!(location(&already), "/admin");

    for hostile in [
        "//evil.example",
        "https://evil.example",
        "/%5Cevil.example",
        "/login",
    ] {
        let response = with_session(server.get(&format!("/login?next={hostile}")), &creds).await;
        assert_eq!(
            location(&response),
            "/",
            "next={hostile} must not be followed"
        );
    }
}

// ─── signing in ─────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn signing_in_sets_cookies_and_redirects_via_htmx(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");

    let response = submit_login(&server, " Alice@Example.COM ", PASSWORD, "/admin").await;
    response.assert_status_ok();
    assert_eq!(header_str(&response, "hx-redirect"), "/admin");
    assert!(response.text().is_empty());
    for name in ["rm_access", "rm_refresh", "rm_csrf"] {
        assert!(response.maybe_cookie(name).is_some(), "{name}");
    }
    let raw: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_ascii_lowercase))
        .collect();
    assert!(raw
        .iter()
        .any(|c| c.starts_with("rm_access=") && c.contains("httponly") && c.contains("secure")));
    assert!(raw.iter().any(|c| c.starts_with("rm_refresh=")
        && c.contains("path=/auth")
        && c.contains("samesite=strict")));

    // The default destination, and an off-site one being neutralised.
    assert_eq!(
        header_str(
            &submit_login(&server, "alice@example.com", PASSWORD, "").await,
            "hx-redirect"
        ),
        "/"
    );
    assert_eq!(
        header_str(
            &submit_login(&server, "alice@example.com", PASSWORD, "//evil.example").await,
            "hx-redirect"
        ),
        "/"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn wrong_credentials_return_a_form_fragment_with_an_escaped_error(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let hostile = "\"><script>alert(1)</script>";

    let response = submit_login(&server, hostile, "not-the-password", "/").await;
    assert_eq!(response.status_code(), 401);
    let html = response.text();

    assert!(
        !html.contains("<html") && !html.contains("<body"),
        "a fragment, not a page"
    );
    assert!(html.contains("id=\"login-form\"") && html.contains("Invalid email or password."));
    assert!(html.contains("role=\"alert\"") && html.contains("aria-invalid=\"true\""));
    // The typed address comes back, escaped; the password never does.
    assert!(
        html.contains("&lt;script&gt;") || html.contains("&#60;script&#62;"),
        "the typed address must come back HTML-escaped: {html}"
    );
    assert!(!html.contains("<script>alert(1)"));
    assert!(!html.contains("not-the-password"));
    assert!(response.maybe_cookie("rm_access").is_none());

    // Unknown account and wrong password look the same.
    let known = submit_login(&server, "alice@example.com", "not-the-password", "/").await;
    let unknown = submit_login(&server, "nobody@example.com", "not-the-password", "/").await;
    assert_eq!(known.status_code(), unknown.status_code());
    assert!(
        known.text().contains("Invalid email or password.")
            && unknown.text().contains("Invalid email or password.")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn blank_fields_are_a_validation_error(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    for (email, password) in [("", "x"), ("alice@example.com", ""), ("  ", " ")] {
        let response = submit_login(&server, email, password, "/").await;
        assert_eq!(response.status_code(), 422, "{email:?}/{password:?}");
        assert!(response
            .text()
            .contains("Enter your email address and password."));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn lockout_is_explained_in_the_users_language(pool: PgPool) {
    let config = auth_config().with_lockout(3, Duration::seconds(600));
    let server = site(&pool, AssetSource::Local, config, limits())
        .await
        .client("198.51.100.1");
    for _ in 0..3 {
        assert_eq!(
            submit_login(&server, "alice@example.com", "wrong-password-1", "/")
                .await
                .status_code(),
            401
        );
    }
    let locked = submit_login(&server, "alice@example.com", PASSWORD, "/").await;
    assert_eq!(locked.status_code(), 403);
    assert!(
        locked.text().contains("Try again in 10 minutes."),
        "{}",
        locked.text()
    );
    let retry: i64 = header_str(&locked, "retry-after").parse().expect("seconds");
    assert!((1..=600).contains(&retry));

    // Same condition, German browser: the message and the plural follow the language.
    let token = visitor_csrf(&server).await;
    let german = htmx(server.post("/login"))
        .add_header("accept-language", "de")
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .form(&[
            ("email", "alice@example.com"),
            ("password", PASSWORD),
            ("next", "/"),
        ])
        .await;
    assert!(
        german.text().contains("in 10 Minuten erneut"),
        "{}",
        german.text()
    );
}

// ─── CSRF and rate limiting on the form ─────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn csrf_failures_on_htmx_requests_become_a_toast_not_json(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let form = [
        ("email", "alice@example.com"),
        ("password", PASSWORD),
        ("next", "/"),
    ];

    let toast = htmx(server.post("/login")).form(&form).await;
    assert_eq!(toast.status_code(), 403);
    assert_eq!(header_str(&toast, "hx-retarget"), "#rm-toasts");
    assert_eq!(header_str(&toast, "hx-reswap"), "beforeend");
    let html = toast.text();
    assert!(html.contains("role=\"alert\"") && html.contains("Your page is out of date."));
    assert!(!html.contains("csrf_failed"), "no raw JSON in the page");

    // The same failure for a non-htmx client stays machine-readable.
    let api = server.post("/login").form(&form).await;
    assert_eq!(api.status_code(), 403);
    assert!(header_str(&api, "content-type").starts_with("application/json"));
    assert!(api.text().contains("csrf_failed"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_html_and_json_logins_share_one_rate_limit_bucket(pool: PgPool) {
    let limits = RateLimits {
        login: RateLimitSettings {
            burst: 4,
            period_secs: 3_600,
        },
        refresh: generous(),
    };
    let site = site(&pool, AssetSource::Local, auth_config(), limits).await;
    let attacker = site.client("203.0.113.66");

    // Two attempts through the JSON API...
    for _ in 0..2 {
        let token = attacker
            .get("/auth/csrf")
            .await
            .cookie("rm_csrf")
            .value()
            .to_owned();
        let response = attacker
            .post("/auth/login")
            .add_header("x-csrf-token", token.as_str())
            .add_cookie(cookie("rm_csrf", &token))
            .json(&serde_json::json!({ "email": "alice@example.com", "password": "wrong-password-1" }))
            .await;
        assert_eq!(response.status_code(), 401);
    }
    // ...and two through the form use up the same budget of four.
    for _ in 0..2 {
        assert_eq!(
            submit_login(&attacker, "alice@example.com", "wrong-password-1", "/")
                .await
                .status_code(),
            401
        );
    }

    // The fifth, from the form (htmx), is blocked and shown as a localised toast.
    let blocked = htmx(attacker.post("/login"))
        .form(&[("email", "x@example.com"), ("password", "x")])
        .await;
    assert_eq!(blocked.status_code(), 429);
    assert_eq!(header_str(&blocked, "hx-retarget"), "#rm-toasts");
    assert!(blocked.text().contains("Too many attempts."));
    assert!(header_str(&blocked, "retry-after").parse::<u64>().is_ok());
    // Even the correct password is refused while limited.
    assert_eq!(
        submit_login(&attacker, "alice@example.com", PASSWORD, "/")
            .await
            .status_code(),
        429
    );

    // A different client of the same app is unaffected.
    let bystander = site.client("203.0.113.77");
    assert_eq!(
        submit_login(&bystander, "alice@example.com", PASSWORD, "/")
            .await
            .status_code(),
        200
    );
}

// ─── signed-in pages ────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_shows_the_user_and_the_navigation_their_role_allows(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");

    let alice = sign_in(&server, "alice@example.com").await;
    let html = with_session(server.get("/"), &alice).await.text();
    assert!(html.contains("Welcome back, ALICE"));
    assert!(html.contains("alice@example.com") && html.contains("User</span>"));
    assert!(html.contains("hx-get=\"/partials/activity\"") && html.contains("hx-trigger=\"load\""));
    assert!(html.contains("aria-current=\"page\""));
    assert!(
        !html.contains("href=\"/admin\""),
        "plain users get no admin link"
    );
    assert!(html.contains("hx-post=\"/logout\"") && html.contains("hx-post=\"/logout-all\""));

    let dana = sign_in(&server, "dana@example.com").await;
    let html = with_session(server.get("/"), &dana).await.text();
    assert!(html.contains("href=\"/admin\"") && html.contains("Domain administrator"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_renders_in_german_and_pluralises(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let creds = sign_in(&server, "alice@example.com").await;

    let html = with_session(server.get("/"), &creds)
        .add_header("accept-language", "de")
        .await
        .text();
    assert!(
        html.contains("Willkommen zurück, ALICE") && html.contains("Von allen Geräten abmelden")
    );
    assert!(html.contains("<html lang=\"de\""));

    // The activity table has one successful sign-in so far.
    let activity = with_session(htmx(server.get("/partials/activity")), &creds)
        .add_header("accept-language", "de")
        .await
        .text();
    assert!(activity.contains("1 Ereignis") && !activity.contains("1 Ereignisse"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn activity_lists_the_users_own_sign_ins_and_escapes_everything(pool: PgPool) {
    let site = default_site(&pool).await;
    let server = site.client("198.51.100.1");
    // A failed attempt from a client with a hostile user agent. It needs its own
    // client: headers added per request would come after the default one.
    let mut attacker = TestServer::new(site.router.clone());
    attacker.add_header("x-forwarded-for", "198.51.100.1");
    attacker.add_header("user-agent", "<img src=x onerror=alert(1)>");
    let token = visitor_csrf(&attacker).await;
    htmx(attacker.post("/login"))
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .form(&[
            ("email", "alice@example.com"),
            ("password", "wrong-password-1"),
            ("next", "/"),
        ])
        .await;
    let creds = sign_in(&server, "alice@example.com").await;

    let html = with_session(htmx(server.get("/partials/activity")), &creds)
        .await
        .text();
    assert!(html.contains("2 events"));
    assert!(html.contains("badge text-bg-success") && html.contains("badge text-bg-danger"));
    assert!(html.contains("198.51.100.1"));
    assert!(
        html.contains("&lt;img") || html.contains("&#60;img"),
        "escaped: {html}"
    );
    assert!(
        !html.contains("<img"),
        "user agents are attacker-controlled and must be escaped"
    );
    assert!(html.contains("<time datetime=\""));

    // Only your own history: another user's attempts never appear.
    let dana = sign_in(&server, "dana@example.com").await;
    let html = with_session(htmx(server.get("/partials/activity")), &dana)
        .await
        .text();
    assert!(html.contains("1 event") && !html.contains("2 events"));
}

// ─── signing out ────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn signing_out_clears_cookies_and_kills_the_session(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let creds = sign_in(&server, "alice@example.com").await;

    let response = post_as(&server, "/logout", &creds).await;
    response.assert_status_ok();
    assert_eq!(header_str(&response, "hx-redirect"), "/login");
    for name in ["rm_access", "rm_refresh", "rm_csrf"] {
        assert_eq!(response.cookie(name).value(), "", "{name} cleared");
    }
    // Replaying the old cookie no longer works.
    let replay = with_session(server.get("/"), &creds).await;
    assert_eq!(replay.status_code(), 303);

    // Logging out needs CSRF too: another site cannot do it for the user.
    let creds = sign_in(&server, "alice@example.com").await;
    let forged = with_session(htmx(server.post("/logout")), &creds).await;
    assert_eq!(forged.status_code(), 403);
    with_session(server.get("/"), &creds)
        .await
        .assert_status_ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn sign_out_everywhere_ends_every_session(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let laptop = sign_in(&server, "alice@example.com").await;
    let phone = sign_in(&server, "alice@example.com").await;
    let other_user = sign_in(&server, "dana@example.com").await;

    let response = post_as(&server, "/logout-all", &laptop).await;
    assert_eq!(header_str(&response, "hx-redirect"), "/login");
    assert_eq!(response.cookie("rm_access").value(), "");

    for creds in [&laptop, &phone] {
        assert_eq!(
            with_session(server.get("/"), creds).await.status_code(),
            303
        );
    }
    with_session(server.get("/"), &other_user)
        .await
        .assert_status_ok();

    // Anonymous callers cannot trigger it.
    let token = visitor_csrf(&server).await;
    let anon = htmx(server.post("/logout-all"))
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .await;
    assert!(header_str(&anon, "hx-redirect").starts_with("/login"));
}

// ─── administration ─────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn admin_pages_are_gated_by_role(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let alice = sign_in(&server, "alice@example.com").await;
    let dana = sign_in(&server, "dana@example.com").await;

    // A plain user gets a localised, full-page 403 (with their account menu).
    let denied = with_session(server.get("/admin"), &alice).await;
    assert_eq!(denied.status_code(), 403);
    let html = denied.text();
    assert!(html.contains("Access denied") && html.contains("<p class=\"display-1"));
    assert!(
        html.contains("ALICE"),
        "signed-in visitors keep their account menu on error pages"
    );
    let german = with_session(server.get("/admin"), &alice)
        .add_header("accept-language", "de")
        .await;
    assert!(german.text().contains("Zugriff verweigert"));

    // The same refusal on a partial request is a toast, not a page inside a page.
    let partial = with_session(htmx(server.get("/admin/partials/overview")), &alice).await;
    assert_eq!(partial.status_code(), 403);
    assert_eq!(header_str(&partial, "hx-retarget"), "#rm-toasts");
    assert!(!partial.text().contains("<html"));

    let ok = with_session(server.get("/admin"), &dana).await;
    ok.assert_status_ok();
    let html = ok.text();
    assert!(html.contains("hx-get=\"/admin/partials/overview\"") && html.contains("every 30s"));
    assert!(html.contains("aria-label=\"Administration\"") && html.contains("Overview"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn admin_overview_shows_server_wide_figures_only_to_super_admins(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let dana = sign_in(&server, "dana@example.com").await;
    let root = sign_in(&server, "root@example.com").await;

    let for_domain_admin = with_session(htmx(server.get("/admin/partials/overview")), &dana)
        .await
        .text();
    assert!(
        for_domain_admin.contains("Connected ("),
        "{for_domain_admin}"
    );
    assert!(for_domain_admin.contains(rustmail_core::version()));
    assert!(
        !for_domain_admin.contains("Domains"),
        "other tenants' figures stay hidden until Phase 7 scopes them"
    );

    let for_super_admin = with_session(htmx(server.get("/admin/partials/overview")), &root)
        .await
        .text();
    assert!(for_super_admin.contains("Domains") && for_super_admin.contains("Connected ("));
    assert!(
        for_super_admin.contains("rm-stat\">1<"),
        "one domain exists"
    );
    assert!(for_super_admin.contains("Updated ") && for_super_admin.contains("UTC"));
}

// ─── errors ─────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_urls_get_a_localised_404_page_or_toast(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");

    let page = server.get("/definitely/not/here").await;
    assert_eq!(page.status_code(), 404);
    let html = page.text();
    assert!(
        html.contains("Page not found")
            && html.contains("The page you are looking for does not exist.")
    );
    assert!(html.contains("<html") && html.contains("Back to dashboard"));
    assert!(
        !html.contains("/definitely/not/here"),
        "the path is never reflected into the page"
    );

    let german = server
        .get("/nope")
        .add_header("accept-language", "de")
        .await;
    assert!(german.text().contains("Seite nicht gefunden"));

    let toast = htmx(server.get("/nope")).await;
    assert_eq!(toast.status_code(), 404);
    assert_eq!(header_str(&toast, "hx-retarget"), "#rm-toasts");
    assert!(!toast.text().contains("<html"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn wrong_methods_and_oversized_bodies_are_rejected(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    assert_eq!(server.post("/healthz").await.status_code(), 405);
    let token = visitor_csrf(&server).await;
    let huge = "x".repeat(40 * 1024);
    let response = htmx(server.post("/login"))
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .form(&[("email", "a@example.com"), ("password", huge.as_str())])
        .await;
    assert_eq!(response.status_code(), 413);
}

// ─── static files ───────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn static_files_are_served_with_the_right_cache_policy(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let html = server.get("/login").await.text();
    let versioned = static_refs(&html)
        .into_iter()
        .find(|u| u.contains("css/app.css"))
        .expect("app.css is linked");
    assert!(versioned.contains("?v="));

    let hashed = server.get(&versioned).await;
    hashed.assert_status_ok();
    assert!(header_str(&hashed, "content-type").starts_with("text/css"));
    assert_eq!(
        header_str(&hashed, "cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(header_str(&hashed, "x-content-type-options"), "nosniff");

    // Without the content hash the browser must revalidate.
    let plain = server.get("/static/css/app.css").await;
    plain.assert_status_ok();
    assert_eq!(header_str(&plain, "cache-control"), "no-cache");
    assert!(
        plain.headers().contains_key("last-modified"),
        "so revalidation can be a cheap 304"
    );

    let js = server.get("/static/js/app.js").await;
    assert!(header_str(&js, "content-type").contains("javascript"));

    // Compressed and fingerprinted vendor files are served as-is.
    server
        .get("/static/vendor/htmx/2.0.11/htmx.min.js")
        .await
        .assert_status_ok();
    server.get("/static/favicon.svg").await.assert_status_ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn static_serving_cannot_escape_its_directory(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let attempts = [
        "/static/../Cargo.toml",
        "/static/%2e%2e/Cargo.toml",
        "/static/..%2fCargo.toml",
        "/static/%2e%2e%2f%2e%2e%2fCargo.toml",
        "/static/css/../../Cargo.toml",
        "/static/....//Cargo.toml",
        "/static//etc/passwd",
        "/static/%2Fetc%2Fpasswd",
        "/static/css/app.css%00.png",
    ];
    for path in attempts {
        let response = server.get(path).await;
        let body = response.text();
        assert!(
            response.status_code() != 200
                || !(body.contains("[package]") || body.contains("[workspace]")),
            "{path} leaked a file outside static/"
        );
        assert!(!body.contains("root:"), "{path}");
    }
    // Directories and missing files are plain 404s: no listing, no index.
    for path in [
        "/static/",
        "/static/css/",
        "/static/vendor",
        "/static/missing.css",
    ] {
        assert_eq!(server.get(path).await.status_code(), 404, "{path}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn healthz_answers_without_authentication(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.1");
    let response = server.get("/healthz").await;
    response.assert_status_ok();
    assert_eq!(response.text(), "ok");
}

/// htmx copies `style` attributes between old and new elements when it swaps
/// content (and a boosted form submission does exactly that). The CSP forbids inline
/// styles, so the browser would block it and log an error. Found by the browser
/// smoke test on the quarantine filter form.
#[sqlx::test(migrations = "../../migrations")]
async fn htmx_is_told_not_to_settle_style_attributes(pool: PgPool) {
    let server = default_site(&pool).await.client("198.51.100.20");
    let html = server.get("/login").await.text();
    assert!(
        html.contains(r#""attributesToSettle":["class","width","height"]"#),
        "the htmx-config meta must exclude `style`"
    );
}
