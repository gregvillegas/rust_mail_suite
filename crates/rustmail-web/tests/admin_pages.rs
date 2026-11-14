//! Integration tests for the admin console pages. Requires `DATABASE_URL`.
//!
//! They check what the server decides: who may open and change what, that
//! domain administrators stay inside their domain, CSRF protection, the one-time
//! password handling, and that created users can sign in.
use std::path::PathBuf;

use axum::http::StatusCode;
use axum::Router;
use axum_extra::extract::cookie::Cookie;
use axum_test::{TestRequest, TestResponse, TestServer};
use rustmail_core::auth::{AuthConfig, AuthService};
use rustmail_core::domain::{NewDomain, NewUser, UserRole};
use rustmail_core::password::hash_password;
use rustmail_core::provisioning::{MailboxProvisioner, ProvisionerConfig};
use rustmail_core::repository::{
    DomainRepository, PgDomainRepository, PgUserRepository, UserRepository,
};
use rustmail_web::assets::AssetSource;
use rustmail_web::auth::{AuthState, DeploymentSecurity, RateLimits};
use rustmail_web::config::RateLimitSettings;
use rustmail_web::{build_app, AppState};
use sqlx::PgPool;

const PASSWORD: &str = "correct-horse-battery-staple";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rustmail-web-admin-{}-{}",
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
    _mail: Scratch,
}

impl Site {
    fn client(&self) -> TestServer {
        let mut server = TestServer::new(self.router.clone());
        server.add_header("x-forwarded-for", "198.51.100.7");
        server.add_header("user-agent", "admin-tests/1.0");
        server
    }
}

/// example.com: alice (user), dana (domain admin), root (super admin).
/// other.org: bob (user), olga (domain admin).
async fn site(pool: &PgPool) -> Site {
    let domains = PgDomainRepository::new(pool.clone());
    let users = PgUserRepository::new(pool.clone());
    for (name, accounts) in [
        (
            "example.com",
            vec![
                ("alice", UserRole::User),
                ("dana", UserRole::DomainAdmin),
                ("root", UserRole::SuperAdmin),
            ],
        ),
        (
            "other.org",
            vec![("bob", UserRole::User), ("olga", UserRole::DomainAdmin)],
        ),
    ] {
        let domain = domains
            .create(&NewDomain {
                name: name.into(),
                ..Default::default()
            })
            .await
            .expect("domain");
        for (local, role) in accounts {
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
    }
    let auth = AuthState::new(
        AuthService::postgres(
            pool.clone(),
            AuthConfig::new("admin-integration-test-secret-32bytes!").expect("config"),
        ),
        DeploymentSecurity {
            cookie_secure: true,
            trust_proxy_headers: true,
        },
    )
    .expect("auth state");
    let mail = Scratch::new();
    let state = AppState::new(
        auth,
        pool.clone(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"),
        AssetSource::Local,
    )
    .expect("state")
    .with_mail_dir(MailboxProvisioner::new(ProvisionerConfig::new(&mail.0)));
    Site {
        router: build_app(state, limits()).expect("app").0,
        _mail: mail,
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

async fn sign_in_with(server: &TestServer, email: &str, password: &str) -> Option<Creds> {
    let page = server.get("/login").await;
    let token = page.cookie("rm_csrf").value().to_owned();
    let response = server
        .post("/login")
        .add_header("hx-request", "true")
        .add_header("x-csrf-token", token.as_str())
        .add_cookie(cookie("rm_csrf", &token))
        .form(&[("email", email), ("password", password), ("next", "/")])
        .await;
    if response.status_code() != StatusCode::OK {
        return None;
    }
    Some(Creds {
        access: response.cookie("rm_access").value().to_owned(),
        refresh: response.cookie("rm_refresh").value().to_owned(),
        csrf: response.cookie("rm_csrf").value().to_owned(),
    })
}

async fn sign_in(server: &TestServer, local: &str) -> Creds {
    let domain = if matches!(local, "bob" | "olga") {
        "other.org"
    } else {
        "example.com"
    };
    sign_in_with(server, &format!("{local}@{domain}"), PASSWORD)
        .await
        .expect("sign in")
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

async fn user_id(pool: &PgPool, email: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("user id")
}

async fn user_exists(pool: &PgPool, email: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("count")
        == 1
}

const ADMIN_PAGES: [&str; 9] = [
    "/admin/domains",
    "/admin/users",
    "/admin/users/new",
    "/admin/aliases",
    "/admin/groups",
    "/admin/quota",
    "/admin/audit",
    "/admin/import",
    "/admin/health",
];

// ─── access control ─────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn only_administrators_reach_the_console(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    for path in ADMIN_PAGES {
        let response = server.get(path).await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert!(
            header(&response, "location").starts_with("/login"),
            "{path}"
        );
    }
    let alice = sign_in(&server, "alice").await;
    for path in ADMIN_PAGES.iter().chain(&[
        "/admin/quota/data",
        "/admin/audit.csv",
        "/admin/health/data",
    ]) {
        get_as(&server, path, &alice)
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }
    // A plain user cannot post either, even with valid CSRF.
    post_as(&server, "/admin/domains", &alice)
        .form(&[("name", "evil.example")])
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../../migrations")]
async fn domain_administrators_get_the_pages_for_their_domain_only(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let dana = sign_in(&server, "dana").await;
    for path in ADMIN_PAGES.iter().filter(|p| **p != "/admin/health") {
        get_as(&server, path, &dana).await.assert_status_ok();
    }
    // System health shows server-wide state: super administrators only.
    for path in ["/admin/health", "/admin/health/data"] {
        get_as(&server, path, &dana)
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }
    let root = sign_in(&server, "root").await;
    let health = get_as(&server, "/admin/health", &root).await;
    health.assert_status_ok();
    assert!(health.text().contains("PostgreSQL"));
    get_as(&server, "/admin/health/data", &root)
        .await
        .assert_status_ok();

    // The user list shows her own domain and nobody else's.
    let list = get_as(&server, "/admin/users", &dana).await.text();
    assert!(
        list.contains("alice@example.com") && !list.contains("bob@other.org"),
        "{list}"
    );
    // Another domain's user, filtered explicitly or opened directly, is out of reach.
    let bob = user_id(&pool, "bob@other.org").await;
    get_as(&server, &format!("/admin/users/{bob}"), &dana)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let other_domain: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM domains WHERE name = 'other.org'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let filtered = get_as(
        &server,
        &format!("/admin/users?domain={other_domain}"),
        &dana,
    )
    .await
    .text();
    assert!(!filtered.contains("bob@other.org"));
    // The quota figures are scoped the same way.
    let data = get_as(&server, "/admin/quota/data", &dana).await.text();
    assert!(
        data.contains("example.com") && !data.contains("other.org"),
        "{data}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn domain_administrators_cannot_change_what_only_super_administrators_may(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let dana = sign_in(&server, "dana").await;
    // Creating a domain is refused with an explanation next to the form.
    let r = post_as(&server, "/admin/domains", &dana)
        .form(&[("name", "new.example")])
        .await;
    r.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM domains WHERE name = 'new.example'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(exists, 0);
    // Making an administrator is refused.
    let r = post_as(&server, "/admin/users", &dana)
        .form(&[
            ("local_part", "boss"),
            ("domain", "example.com"),
            ("role", "super_admin"),
            ("name", "Boss"),
        ])
        .await;
    r.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!user_exists(&pool, "boss@example.com").await);
    // Creating a user in another domain is refused.
    let r = post_as(&server, "/admin/users", &dana)
        .form(&[
            ("local_part", "mallory"),
            ("domain", "other.org"),
            ("role", "user"),
        ])
        .await;
    assert_ne!(r.status_code(), StatusCode::OK);
    assert!(!user_exists(&pool, "mallory@other.org").await);
    // Touching another domain's user is refused and changes nothing.
    let bob = user_id(&pool, "bob@other.org").await;
    for path in [
        format!("/admin/users/{bob}/status"),
        format!("/admin/users/{bob}/password"),
        format!("/admin/users/{bob}/sessions"),
    ] {
        let r = post_as(&server, &path, &dana)
            .form(&[("status", "disabled")])
            .await;
        assert_ne!(r.status_code(), StatusCode::OK, "{path}");
    }
    let status: String = sqlx::query_scalar("SELECT status::text FROM users WHERE id = $1")
        .bind(bob)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "active");
}

#[sqlx::test(migrations = "../../migrations")]
async fn changes_need_a_valid_csrf_token(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let root = sign_in(&server, "root").await;
    let r = server
        .post("/admin/domains")
        .add_header("hx-request", "true")
        .add_cookie(cookie("rm_access", &root.access))
        .add_cookie(cookie("rm_csrf", &root.csrf))
        .form(&[("name", "csrf.example")])
        .await;
    r.assert_status(StatusCode::FORBIDDEN);
    let exists: i64 =
        sqlx::query_scalar("SELECT count(*) FROM domains WHERE name = 'csrf.example'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(exists, 0);
}

// ─── the main flow ──────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn an_administrator_creates_a_domain_and_users_who_can_sign_in(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let root = sign_in(&server, "root").await;

    let r = post_as(&server, "/admin/domains", &root)
        .form(&[("name", "Fresh.Example"), ("quota_mb", "50")])
        .await;
    r.assert_status_ok();
    assert!(
        header(&r, "hx-redirect").contains("/admin/domains/"),
        "redirects to the new domain"
    );
    let page = get_as(&server, "/admin/domains", &root).await.text();
    assert!(
        page.contains("fresh.example"),
        "names are normalised to lower case"
    );

    let mut passwords = Vec::new();
    for local in ["one", "two", "three"] {
        let r = post_as(&server, "/admin/users", &root)
            .form(&[
                ("local_part", local),
                ("domain", "fresh.example"),
                ("role", "user"),
                ("name", local),
            ])
            .await;
        r.assert_status_ok();
        let html = r.text();
        let start = html
            .find("data-testid=\"generated-password\">")
            .expect("a generated password is shown")
            + "data-testid=\"generated-password\">".len();
        let end = html[start..].find('<').expect("closing tag") + start;
        passwords.push((local, html[start..end].to_owned()));
    }
    for (local, password) in &passwords {
        let creds = sign_in_with(&server, &format!("{local}@fresh.example"), password).await;
        assert!(
            creds.is_some(),
            "{local} can sign in with the generated password"
        );
    }
    // The password is never stored or shown again: the user page does not contain it.
    let id = user_id(&pool, "one@fresh.example").await;
    let page = get_as(&server, &format!("/admin/users/{id}"), &root)
        .await
        .text();
    assert!(!page.contains(&passwords[0].1));
    // The domain default quota applies.
    assert!(page.contains("50.0 MB"), "{page}");

    // Everything is in the audit log, with the administrator named.
    let audit = get_as(&server, "/admin/audit?action=user.create", &root)
        .await
        .text();
    assert!(audit.contains("root@example.com") && audit.contains("user.create"));
    assert!(
        !audit.contains(&passwords[0].1),
        "passwords are never audited"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn bad_input_is_explained_next_to_the_form(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let root = sign_in(&server, "root").await;
    for form in [
        vec![
            ("local_part", "x y"),
            ("domain", "example.com"),
            ("role", "user"),
        ],
        vec![
            ("local_part", "ok"),
            ("domain", "example.com"),
            ("role", "user"),
            ("quota_mb", "lots"),
        ],
        vec![
            ("local_part", "alice"),
            ("domain", "example.com"),
            ("role", "user"),
        ],
        vec![
            ("local_part", "ok"),
            ("domain", "example.com"),
            ("role", "wizard"),
        ],
        vec![
            ("local_part", "ok"),
            ("domain", "example.com"),
            ("role", "user"),
            ("password", "short"),
        ],
    ] {
        let r = post_as(&server, "/admin/users", &root).form(&form).await;
        r.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.text().contains("alert-danger"), "{form:?}");
    }
    assert!(!user_exists(&pool, "ok@example.com").await);
}

#[sqlx::test(migrations = "../../migrations")]
async fn users_are_deleted_only_after_typing_their_address(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let root = sign_in(&server, "root").await;
    let r = post_as(&server, "/admin/users", &root)
        .form(&[
            ("local_part", "temp"),
            ("domain", "example.com"),
            ("role", "user"),
        ])
        .await;
    r.assert_status_ok();
    let id = user_id(&pool, "temp@example.com").await;
    let wrong = post_as(&server, &format!("/admin/users/{id}/delete"), &root)
        .form(&[("confirm", "nope")])
        .await;
    wrong.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(user_exists(&pool, "temp@example.com").await);
    let ok = post_as(&server, &format!("/admin/users/{id}/delete"), &root)
        .form(&[("confirm", "temp@example.com")])
        .await;
    ok.assert_status_ok();
    assert!(!user_exists(&pool, "temp@example.com").await);
    // Nobody can delete or disable themselves.
    let me = user_id(&pool, "root@example.com").await;
    let r = post_as(&server, &format!("/admin/users/{me}/delete"), &root)
        .form(&[("confirm", "root@example.com")])
        .await;
    assert_ne!(r.status_code(), StatusCode::OK);
    let r = post_as(&server, &format!("/admin/users/{me}/status"), &root)
        .form(&[("status", "disabled")])
        .await;
    assert_ne!(r.status_code(), StatusCode::OK);
    assert!(user_exists(&pool, "root@example.com").await);
}

#[sqlx::test(migrations = "../../migrations")]
async fn aliases_and_groups_round_trip(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let dana = sign_in(&server, "dana").await;
    let domain: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM domains WHERE name = 'example.com'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let r = post_as(&server, "/admin/aliases", &dana)
        .form(&[
            ("domain_id", domain.to_string().as_str()),
            ("address", "info@example.com"),
            ("destination", "alice@example.com"),
        ])
        .await;
    r.assert_status_ok();
    assert!(get_as(&server, "/admin/aliases", &dana)
        .await
        .text()
        .contains("info@example.com"));
    // An alias may not take over an address that belongs to a user.
    let r = post_as(&server, "/admin/aliases", &dana)
        .form(&[
            ("domain_id", domain.to_string().as_str()),
            ("address", "alice@example.com"),
            ("destination", "dana@example.com"),
        ])
        .await;
    r.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    let r = post_as(&server, "/admin/groups", &dana)
        .form(&[
            ("domain_id", domain.to_string().as_str()),
            ("name", "Team"),
            ("address", "team@example.com"),
        ])
        .await;
    r.assert_status_ok();
    let group = header(&r, "hx-redirect");
    let group = group.split('?').next().unwrap().to_owned();
    post_as(&server, &format!("{group}/members"), &dana)
        .form(&[("email", "alice@example.com")])
        .await
        .assert_status_ok();
    let page = get_as(&server, &group, &dana).await.text();
    assert!(page.contains("alice@example.com"));
    // A member from another domain is refused.
    let r = post_as(&server, &format!("{group}/members"), &dana)
        .form(&[("email", "bob@other.org")])
        .await;
    assert_ne!(r.status_code(), StatusCode::OK);
}

// ─── bulk import ────────────────────────────────────────────────────────────

fn multipart(csv: &str) -> (String, Vec<u8>) {
    let boundary = "----rustmail-test-boundary";
    let body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"csv\"\r\n\r\n{csv}\r\n--{boundary}--\r\n");
    (
        format!("multipart/form-data; boundary={boundary}"),
        body.into_bytes(),
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn import_checks_first_creates_on_confirmation_and_hands_out_passwords_once(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let dana = sign_in(&server, "dana").await;
    let csv = "email,name,role,quota_mb,password\nnew1@example.com,New One,user,5,\nnew2@example.com,New Two,user,,\n";

    let (content_type, body) = multipart(csv);
    let preview = post_as(&server, "/admin/import/preview", &dana)
        .content_type(&content_type)
        .bytes(body.into())
        .await;
    preview.assert_status_ok();
    let html = preview.text();
    assert!(
        html.contains("new1@example.com") && html.contains("/admin/import/apply"),
        "{html}"
    );
    assert!(
        !user_exists(&pool, "new1@example.com").await,
        "a preview creates nothing"
    );

    // A file with a problem cannot be applied from the preview.
    let (content_type, body) =
        multipart("email,name,role,quota_mb,password\nstranger@other.org,X,user,,\n");
    let bad = post_as(&server, "/admin/import/preview", &dana)
        .content_type(&content_type)
        .bytes(body.into())
        .await
        .text();
    assert!(
        !bad.contains("/admin/import/apply") && bad.contains("stranger@other.org"),
        "{bad}"
    );

    let applied = post_as(&server, "/admin/import/apply", &dana)
        .form(&[("csv", csv)])
        .await;
    applied.assert_status_ok();
    let html = applied.text();
    assert!(
        user_exists(&pool, "new1@example.com").await
            && user_exists(&pool, "new2@example.com").await
    );
    let link_start = html.find("/admin/import/result/").expect("a download link");
    let link = html[link_start..].split('"').next().unwrap().to_owned();
    assert!(
        !html.contains("generated-password"),
        "passwords are not in the page"
    );

    // Another administrator cannot take the file, and neither can a signed-out client.
    let olga = sign_in(&server, "olga").await;
    get_as(&server, &link, &olga)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    server.get(&link).await.assert_status(StatusCode::SEE_OTHER);
    // The owner gets it once.
    let file = get_as(&server, &link, &dana).await;
    file.assert_status_ok();
    assert!(header(&file, "content-type").starts_with("text/csv"));
    assert!(header(&file, "cache-control").contains("no-store"));
    let text = file.text();
    assert!(text.contains("new1@example.com") && text.contains("new2@example.com"));
    get_as(&server, &link, &dana)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_can_be_filtered_and_exported_within_scope(pool: PgPool) {
    let site = site(&pool).await;
    let server = site.client();
    let root = sign_in(&server, "root").await;
    let dana = sign_in(&server, "dana").await;
    let olga = sign_in(&server, "olga").await;
    post_as(&server, "/admin/users", &dana)
        .form(&[
            ("local_part", "q1"),
            ("domain", "example.com"),
            ("role", "user"),
        ])
        .await
        .assert_status_ok();
    post_as(&server, "/admin/users", &olga)
        .form(&[
            ("local_part", "q2"),
            ("domain", "other.org"),
            ("role", "user"),
        ])
        .await
        .assert_status_ok();

    let mine = get_as(&server, "/admin/audit?action=user.create", &dana)
        .await
        .text();
    assert!(mine.contains("q1@example.com") || mine.contains("dana@example.com"));
    assert!(
        !mine.contains("olga@other.org"),
        "another domain's actions are hidden"
    );
    let export = get_as(&server, "/admin/audit.csv?action=user.create", &dana).await;
    export.assert_status_ok();
    assert!(header(&export, "content-type").starts_with("text/csv"));
    assert!(!export.text().contains("olga@other.org"));

    let all = get_as(&server, "/admin/audit?action=user.create", &root)
        .await
        .text();
    assert!(all.contains("olga@other.org") && all.contains("dana@example.com"));
    let none = get_as(
        &server,
        "/admin/audit?action=user.create&outcome=failure",
        &root,
    )
    .await
    .text();
    assert!(!none.contains("olga@other.org"));
}
