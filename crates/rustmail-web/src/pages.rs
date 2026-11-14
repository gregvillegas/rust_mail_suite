//! HTML pages and their htmx partials.
//!
//! | Route | Method | Auth | Notes |
//! |-------|--------|------|-------|
//! | `/` | GET | user | dashboard |
//! | `/login` | GET | none | sign-in page |
//! | `/login` | POST | none | form post; rate limited with `/auth/login` |
//! | `/logout`, `/logout-all` | POST | user | end this / every session |
//! | `/lang` | POST | none | choose the UI language |
//! | `/session/renew` | GET | none | silently refresh an expired access token |
//! | `/partials/activity` | GET | user | recent sign-in table |
//! | `/admin` | GET | admin | administration landing page |
//! | `/admin/partials/overview` | GET | admin | overview cards |
//! | `/mail…` | GET, POST, WebSocket | signed in | webmail (see [`crate::mail_pages`]) |
//! | `/admin/quarantine…` | GET, POST | super admin | quarantine (see [`crate::quarantine_pages`]) |
//!
//! All state-changing routes sit behind the CSRF middleware (see `app`).

use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use rustmail_core::auth::{AuthContext, AuthError, LoginRequest};
use rustmail_core::domain::{AuditOutcome, AuditQuery, User, UserRole};
use rustmail_core::repository::{
    AuditLogRepository, DomainRepository, PgAuditLogRepository, PgDomainRepository,
};
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::cookies::ACCESS_COOKIE;
use crate::auth::csrf::ensure_cookie;
use crate::auth::handlers::{clear_cookies, end_session, start_cookies};
use crate::auth::ClientMeta;
use crate::i18n::{Locale, LANG_COOKIE};
use crate::quarantine_pages;
use crate::ui::{
    render, render_with_status, ActivityPartial, ActivityRow, AdminIndexPage, AdminOverviewPartial,
    DashboardPage, LoginFormPartial, LoginFormView, LoginPage, PageCtx, PageError, RenewPage,
    UserView,
};
use crate::{admin_pages, admin_reports, mail_pages, mail_ws};

const HX_REDIRECT: HeaderName = HeaderName::from_static("hx-redirect");
const HX_REFRESH: HeaderName = HeaderName::from_static("hx-refresh");
const MAX_EMAIL_CHARS: usize = 320;
const MAX_PASSWORD_CHARS: usize = 1024;
const ACTIVITY_ROWS: i64 = 10;
const DEVICE_CHARS: usize = 80;

/// The page routes grouped by the protection they need.
pub struct PageRoutes {
    /// `POST /login`: CSRF plus the login rate limit.
    pub login_post: Router<AppState>,
    /// State-changing routes that need CSRF only.
    pub csrf_posts: Router<AppState>,
    /// Read-only pages.
    pub gets: Router<AppState>,
}

/// Builds the page routes.
pub fn routes() -> PageRoutes {
    PageRoutes {
        login_post: Router::new().route("/login", post(login_submit)),
        csrf_posts: Router::new()
            .route("/logout", post(logout_submit))
            .route("/logout-all", post(logout_all_submit))
            .route("/lang", post(language_submit))
            .route(
                "/mail/send",
                post(mail_pages::send).layer(axum::extract::DefaultBodyLimit::max(
                    mail_pages::SEND_BODY_LIMIT,
                )),
            )
            .route("/mail/message/{uid}/flag", post(mail_pages::flag))
            .route("/mail/message/{uid}/move", post(mail_pages::move_to))
            .route("/mail/message/{uid}/delete", post(mail_pages::delete))
            .route("/mail/message/{uid}/spam", post(mail_pages::spam))
            .route("/mail/folders/create", post(mail_pages::folder_create))
            .route("/mail/folders/rename", post(mail_pages::folder_rename))
            .route("/mail/folders/delete", post(mail_pages::folder_delete))
            .route("/admin/domains", post(admin_pages::domain_create))
            .route("/admin/domains/{id}", post(admin_pages::domain_update))
            .route(
                "/admin/domains/{id}/delete",
                post(admin_pages::domain_delete),
            )
            .route("/admin/users", post(admin_pages::user_create))
            .route("/admin/users/{id}", post(admin_pages::user_update))
            .route("/admin/users/{id}/status", post(admin_pages::user_status))
            .route(
                "/admin/users/{id}/password",
                post(admin_pages::user_password),
            )
            .route("/admin/users/{id}/unlock", post(admin_pages::user_unlock))
            .route(
                "/admin/users/{id}/sessions",
                post(admin_pages::user_sessions),
            )
            .route("/admin/users/{id}/delete", post(admin_pages::user_delete))
            .route("/admin/aliases", post(admin_pages::alias_create))
            .route(
                "/admin/aliases/{id}/toggle",
                post(admin_pages::alias_toggle),
            )
            .route(
                "/admin/aliases/{id}/delete",
                post(admin_pages::alias_delete),
            )
            .route("/admin/groups", post(admin_pages::group_create))
            .route(
                "/admin/groups/{id}/members",
                post(admin_pages::group_member_add),
            )
            .route(
                "/admin/groups/{id}/members/{user}/remove",
                post(admin_pages::group_member_remove),
            )
            .route("/admin/groups/{id}/delete", post(admin_pages::group_delete))
            .route(
                "/admin/import/preview",
                post(admin_reports::import_preview).layer(axum::extract::DefaultBodyLimit::max(
                    admin_reports::IMPORT_BODY_LIMIT,
                )),
            )
            .route("/admin/import/apply", post(admin_reports::import_apply))
            .route(
                "/admin/quarantine/{id}/release",
                post(quarantine_pages::release),
            )
            .route(
                "/admin/quarantine/{id}/delete",
                post(quarantine_pages::delete),
            ),
        gets: Router::new()
            .route("/", get(dashboard))
            .route("/login", get(login_page))
            .route("/session/renew", get(renew_page))
            .route("/partials/activity", get(activity_partial))
            .route("/admin", get(admin_index))
            .route("/mail", get(mail_pages::index))
            .route("/mail/inbox", get(mail_pages::inbox_redirect))
            .route("/mail/list", get(mail_pages::list_partial))
            .route("/mail/folders", get(mail_pages::folders_page))
            .route("/mail/folders/nav", get(mail_pages::folder_nav_partial))
            .route("/mail/compose", get(mail_pages::compose_page))
            .route("/mail/message/{uid}", get(mail_pages::preview))
            .route("/mail/message/{uid}/body", get(mail_pages::body))
            .route(
                "/mail/message/{uid}/attachment/{index}",
                get(mail_pages::attachment),
            )
            .route("/mail/ws", get(mail_ws::socket))
            .route("/admin/domains", get(admin_pages::domains))
            .route("/admin/domains/{id}", get(admin_pages::domain))
            .route("/admin/users", get(admin_pages::users))
            .route("/admin/users/new", get(admin_pages::user_new))
            .route("/admin/users/{id}", get(admin_pages::user_page))
            .route("/admin/aliases", get(admin_pages::aliases))
            .route("/admin/groups", get(admin_pages::groups))
            .route("/admin/groups/{id}", get(admin_pages::group_page))
            .route("/admin/quota", get(admin_reports::quota))
            .route("/admin/quota/data", get(admin_reports::quota_data))
            .route("/admin/audit", get(admin_reports::audit))
            .route("/admin/audit.csv", get(admin_reports::audit_csv))
            .route("/admin/import", get(admin_reports::import_page))
            .route(
                "/admin/import/result/{token}",
                get(admin_reports::import_result),
            )
            .route("/admin/health", get(admin_reports::health))
            .route("/admin/health/data", get(admin_reports::health_table))
            .route("/admin/quarantine", get(quarantine_pages::list))
            .route("/admin/quarantine/{id}", get(quarantine_pages::detail))
            .route("/admin/partials/overview", get(admin_overview_partial)),
    }
}

// ─── request helpers ────────────────────────────────────────────────────────

/// Whether the request came from htmx.
#[must_use]
pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

/// The language for a request: explicit cookie, then `Accept-Language`, then English.
#[must_use]
pub fn resolve_locale(jar: &CookieJar, headers: &HeaderMap) -> Locale {
    if let Some(locale) = jar
        .get(LANG_COOKIE)
        .and_then(|c| Locale::from_code(c.value()))
    {
        return locale;
    }
    Locale::negotiate(
        headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok()),
    )
}

pub(crate) fn page_ctx(
    state: &AppState,
    jar: &CookieJar,
    headers: &HeaderMap,
    user: Option<&User>,
    path: &str,
) -> PageCtx {
    PageCtx::new(
        resolve_locale(jar, headers),
        state.assets.clone(),
        user.map(UserView::from),
        path,
    )
}

/// Reduces a user-supplied post-login destination to a safe same-site path.
///
/// Accepts only absolute paths (`/x?y`); rejects other schemes, protocol-relative
/// URLs (`//host`), backslash tricks (`/\host`), control characters, and anything
/// that would bounce straight back to the sign-in flow. Falls back to `/`.
#[must_use]
pub fn safe_next(raw: &str) -> String {
    let candidate = raw.trim();
    let mut chars = candidate.chars();
    let ok = chars.next() == Some('/')
        && !matches!(chars.next(), Some('/' | '\\'))
        && !candidate.chars().any(|c| c.is_control() || c == '\\')
        && candidate.len() <= 512;
    if !ok {
        return "/".to_owned();
    }
    let path = candidate.split(['?', '#']).next().unwrap_or("/");
    if path == "/login" || path.starts_with("/session/") {
        return "/".to_owned();
    }
    candidate.to_owned()
}

/// Percent-encodes a value for use in a query string.
#[must_use]
pub fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Where to send the user back to after sign-in or renewal: the page they asked
/// for, or for htmx requests the page they are currently on.
fn return_path(parts: &Parts) -> String {
    let uri = if is_htmx(&parts.headers) {
        parts
            .headers
            .get("hx-current-url")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<Uri>().ok())
    } else if parts.method == Method::GET {
        Some(parts.uri.clone())
    } else {
        None
    };
    let target = uri
        .as_ref()
        .and_then(|u| u.path_and_query())
        .map_or("/", |pq| pq.as_str());
    safe_next(target)
}

fn with_next(base: &str, next: &str) -> String {
    if next == "/" {
        base.to_owned()
    } else {
        format!("{base}?next={}", encode_query_value(next))
    }
}

/// A redirect that works for both navigations (303) and htmx requests, which
/// ignore redirects on partial requests and need `HX-Redirect` instead.
fn redirect(htmx: bool, location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(value) if htmx => (StatusCode::OK, [(HX_REDIRECT, value)]).into_response(),
        Ok(_) => Redirect::to(location).into_response(),
        Err(_) => PageError::Internal.into_response(),
    }
}

fn hx_redirect(location: &'static str) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(HX_REDIRECT, HeaderValue::from_static(location))],
    )
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut cut: String = s.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

// ─── extractors ─────────────────────────────────────────────────────────────

/// A signed-in user on an HTML route.
///
/// Unlike the JSON `AuthUser`, rejection is a redirect: to the sign-in page, or
/// to `/session/renew` when the access token merely lapsed (a validly signed
/// token whose session may still be refreshable).
pub struct WebUser {
    /// The verified token context.
    pub context: AuthContext,
    /// The user, loaded fresh from the database.
    pub user: User,
}

impl FromRequestParts<AppState> for WebUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let htmx = is_htmx(&parts.headers);
        let jar = CookieJar::from_headers(&parts.headers);
        let to_login = |parts: &Parts| redirect(htmx, &with_next("/login", &return_path(parts)));

        let Some(token) = jar.get(ACCESS_COOKIE).map(|c| c.value().to_owned()) else {
            return Err(to_login(parts));
        };
        let context = match state.auth.auth.authenticate(&token).await {
            Ok(context) => context,
            Err(AuthError::InvalidToken) => {
                let signed =
                    rustmail_core::auth::peek_claims(state.auth.auth.config(), &token).is_some();
                return Err(if signed {
                    redirect(htmx, &with_next("/session/renew", &return_path(parts)))
                } else {
                    to_login(parts)
                });
            }
            Err(error) => {
                tracing::error!(%error, "authentication failed unexpectedly");
                return Err(PageError::Internal.into_response());
            }
        };
        let user = match state.auth.auth.current_user(&context).await {
            Ok(user) if user.status.is_active() => user,
            Ok(_) | Err(AuthError::InvalidToken) => return Err(to_login(parts)),
            Err(error) => {
                tracing::error!(%error, "could not load the signed-in user");
                return Err(PageError::Internal.into_response());
            }
        };
        Ok(Self { context, user })
    }
}

/// A signed-in user holding at least the domain-administrator role.
pub struct AdminUser(pub WebUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let web = WebUser::from_request_parts(parts, state).await?;
        // Authorise on the freshly loaded role, not the one baked into the token.
        if web.user.role.at_least(UserRole::DomainAdmin) {
            Ok(Self(web))
        } else {
            Err(PageError::Forbidden.into_response())
        }
    }
}

// ─── sign in / out ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct NextQuery {
    next: Option<String>,
}

async fn login_page(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Query(query): Query<NextQuery>,
) -> Response {
    let next = safe_next(query.next.as_deref().unwrap_or("/"));
    // Already signed in: no need to show the form.
    if let Some(cookie) = jar.get(ACCESS_COOKIE) {
        if state.auth.auth.authenticate(cookie.value()).await.is_ok() {
            return redirect(false, &next);
        }
    }
    let ctx = page_ctx(&state, &jar, &headers, None, "/login");
    let form = LoginFormView {
        email: String::new(),
        next,
        error: None,
    };
    // The form needs a CSRF token to echo; hand one out with the page.
    let jar = ensure_cookie(&state.auth, jar);
    (jar, render(&LoginPage { ctx, form })).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    password: String,
    #[serde(default)]
    next: String,
}

async fn login_submit(
    State(state): State<AppState>,
    ClientMeta(client): ClientMeta,
    jar: CookieJar,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let next = safe_next(&form.next);
    let ctx = page_ctx(&state, &jar, &headers, None, "/login");
    let failure = |status: StatusCode, message: String, email: &str, retry_after: Option<i64>| {
        let view = LoginFormView {
            email: email.to_owned(),
            next: next.clone(),
            error: Some(message),
        };
        let mut response = render_with_status(
            status,
            &LoginFormPartial {
                ctx: ctx.clone(),
                form: view,
            },
        );
        if let Some(secs) = retry_after.and_then(|s| HeaderValue::from_str(&s.to_string()).ok()) {
            response.headers_mut().insert(header::RETRY_AFTER, secs);
        }
        response
    };

    let email = form.email.trim();
    if email.is_empty()
        || form.password.is_empty()
        || email.chars().count() > MAX_EMAIL_CHARS
        || form.password.chars().count() > MAX_PASSWORD_CHARS
    {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            ctx.t("login-error-missing"),
            email,
            None,
        );
    }

    let request = LoginRequest {
        email: email.to_owned(),
        password: form.password.clone(),
        client,
    };
    match state.auth.auth.login(request).await {
        Ok(outcome) => {
            let (jar, _csrf) = start_cookies(&state.auth, jar, &outcome.tokens);
            match HeaderValue::from_str(&next) {
                Ok(location) => (StatusCode::OK, jar, [(HX_REDIRECT, location)]).into_response(),
                Err(_) => (jar, hx_redirect("/")).into_response(),
            }
        }
        Err(AuthError::InvalidCredentials) => failure(
            StatusCode::UNAUTHORIZED,
            ctx.t("login-error-invalid"),
            email,
            None,
        ),
        Err(AuthError::AccountLocked { until }) => {
            let seconds = (until - Utc::now()).num_seconds().max(1);
            let minutes = (seconds + 59) / 60;
            failure(
                StatusCode::FORBIDDEN,
                ctx.t_num("login-error-locked", "minutes", &minutes),
                email,
                Some(seconds),
            )
        }
        Err(AuthError::AccountDisabled) => failure(
            StatusCode::FORBIDDEN,
            ctx.t("login-error-disabled"),
            email,
            None,
        ),
        Err(error) => {
            tracing::error!(%error, "login failed unexpectedly");
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                ctx.t("login-error-generic"),
                email,
                None,
            )
        }
    }
}

async fn logout_submit(
    State(state): State<AppState>,
    ClientMeta(client): ClientMeta,
    jar: CookieJar,
) -> Response {
    match end_session(&state.auth, client, jar).await {
        Ok(jar) => (jar, hx_redirect("/login")).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn logout_all_submit(
    State(state): State<AppState>,
    ClientMeta(client): ClientMeta,
    jar: CookieJar,
    web: WebUser,
) -> Response {
    match state.auth.auth.logout_all(web.user.id, client).await {
        Ok(_) => (clear_cookies(&state.auth, jar), hx_redirect("/login")).into_response(),
        Err(error) => crate::auth::ApiError::from(error).into_response(),
    }
}

#[derive(Deserialize)]
struct LanguageForm {
    lang: String,
}

async fn language_submit(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<LanguageForm>,
) -> Response {
    let Some(locale) = Locale::from_code(&form.lang) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let cookie = Cookie::build((LANG_COOKIE, locale.code()))
        .http_only(true)
        .secure(state.auth.security.cookie_secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::days(365))
        .build();
    // Reload so the whole page, including boosted content, renders in the new language.
    (
        StatusCode::OK,
        jar.add(cookie),
        [(HX_REFRESH, HeaderValue::from_static("true"))],
    )
        .into_response()
}

async fn renew_page(State(state): State<AppState>, jar: CookieJar, headers: HeaderMap) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, None, "/session/renew");
    render(&RenewPage { ctx })
}

// ─── dashboard ──────────────────────────────────────────────────────────────

async fn dashboard(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/");
    let jar = ensure_cookie(&state.auth, jar);
    (jar, render(&DashboardPage { ctx })).into_response()
}

async fn activity_partial(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/");
    let entries = PgAuditLogRepository::new(state.pool.clone())
        .list(&AuditQuery {
            actor_user_id: Some(web.user.id),
            action: Some("auth.login".to_owned()),
            limit: ACTIVITY_ROWS,
            offset: 0,
        })
        .await;
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!(%error, "could not load sign-in activity");
            return PageError::Internal.into_response();
        }
    };
    let rows = entries
        .into_iter()
        .map(|e| ActivityRow {
            when: e.occurred_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            iso: e.occurred_at.to_rfc3339(),
            success: e.outcome == AuditOutcome::Success,
            address: e.ip_address.unwrap_or_else(|| "–".to_owned()),
            device: truncate_chars(e.user_agent.as_deref().unwrap_or("–"), DEVICE_CHARS),
        })
        .collect();
    render(&ActivityPartial { ctx, rows })
}

// ─── administration ─────────────────────────────────────────────────────────

async fn admin_index(
    State(state): State<AppState>,
    admin: AdminUser,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&admin.0.user), "/admin");
    let jar = ensure_cookie(&state.auth, jar);
    (jar, render(&AdminIndexPage { ctx })).into_response()
}

async fn admin_overview_partial(
    State(state): State<AppState>,
    admin: AdminUser,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&admin.0.user), "/admin");
    let (db_ok, db_ms) = match rustmail_core::db::ping(&state.pool).await {
        Ok(elapsed) => (true, i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)),
        Err(error) => {
            tracing::warn!(%error, "database ping failed");
            (false, 0)
        }
    };
    // Server-wide figures are only for super administrators: until domain-admin
    // scoping exists (Phase 7), they would reveal other tenants' data.
    let show_domains = admin.0.user.role == UserRole::SuperAdmin;
    let domains = if show_domains {
        PgDomainRepository::new(state.pool.clone())
            .list()
            .await
            .map_or(0, |d| d.len())
    } else {
        0
    };
    render(&AdminOverviewPartial {
        ctx,
        show_domains,
        domains,
        db_ok,
        db_ms,
        refreshed: Utc::now().format("%H:%M:%S UTC").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_next_accepts_only_local_paths() {
        for ok in [
            "/",
            "/admin",
            "/admin?tab=users",
            "/a/b/c",
            "/search?q=a%20b#top",
        ] {
            assert_eq!(safe_next(ok), ok, "{ok:?}");
        }
        for bad in [
            "",
            "admin",
            "https://evil.example",
            "http://evil.example/x",
            "//evil.example",
            "///evil.example",
            "/\\evil.example",
            "/ok\\..\\evil",
            "javascript:alert(1)",
            "data:text/html,x",
            "/path\nwith-newline",
            "/tab\there",
            "\u{0}/x",
        ] {
            assert_eq!(safe_next(bad), "/", "{bad:?} must be rejected");
        }
    }

    #[test]
    fn safe_next_avoids_redirect_loops() {
        for looped in [
            "/login",
            "/login?next=/x",
            "/session/renew",
            "/session/renew?next=/",
        ] {
            assert_eq!(safe_next(looped), "/", "{looped:?}");
        }
        assert_eq!(
            safe_next("/loginfo"),
            "/loginfo",
            "only the exact path is special"
        );
    }

    #[test]
    fn safe_next_bounds_the_length() {
        let long = format!("/{}", "a".repeat(600));
        assert_eq!(safe_next(&long), "/");
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(
            encode_query_value("/admin?tab=users&x=1"),
            "%2Fadmin%3Ftab%3Dusers%26x%3D1"
        );
        assert_eq!(encode_query_value("a b"), "a%20b");
        assert_eq!(encode_query_value("safe-_.~09azAZ"), "safe-_.~09azAZ");
        assert_eq!(encode_query_value("é"), "%C3%A9");
        assert_eq!(with_next("/login", "/"), "/login");
        assert_eq!(with_next("/login", "/admin"), "/login?next=%2Fadmin");
    }

    #[test]
    fn htmx_detection_and_locale_resolution() {
        let mut headers = HeaderMap::new();
        assert!(!is_htmx(&headers));
        headers.insert("hx-request", HeaderValue::from_static("true"));
        assert!(is_htmx(&headers));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT_LANGUAGE,
            HeaderValue::from_static("de-DE,de;q=0.9"),
        );
        assert_eq!(resolve_locale(&CookieJar::new(), &headers), Locale::De);
        // An explicit cookie beats the browser's preference.
        let jar = CookieJar::new().add(Cookie::new(LANG_COOKIE, "en"));
        assert_eq!(resolve_locale(&jar, &headers), Locale::En);
        // A junk cookie is ignored rather than trusted.
        let jar = CookieJar::new().add(Cookie::new(LANG_COOKIE, "klingon"));
        assert_eq!(resolve_locale(&jar, &headers), Locale::De);
    }

    #[test]
    fn long_user_agents_are_shortened_on_a_character_boundary() {
        assert_eq!(truncate_chars("short", 80), "short");
        let shortened = truncate_chars(&"é".repeat(100), 10);
        assert_eq!(shortened.chars().count(), 10);
        assert!(shortened.ends_with('…'));
    }
}
