//! Application assembly: shared state, routing and cross-cutting middleware.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{DefaultBodyLimit, FromRef, Request, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{any, get};
use axum::Router;
use axum_extra::extract::CookieJar;
use rustmail_core::admin::AdminService;
use rustmail_core::dav::store::DavStore;
use rustmail_core::dav::tokens::AppPasswords;
use rustmail_core::db::PgPool;
use rustmail_core::domain::User;
use rustmail_core::filters::store::FilterStore;
use rustmail_core::provisioning::MailboxProvisioner;
use rustmail_core::quarantine::QuarantineService;
use rustmail_sieve::SieveConfig;
use serde::Deserialize;
use tower_http::services::ServeDir;

use crate::admin_common::ImportResults;
use crate::assets::{AssetSource, Assets, STATIC_PREFIX};
use crate::auth::cookies::ACCESS_COOKIE;
use crate::auth::{self, rate_limit, AuthState, RateLimitJanitor, RateLimits};
use crate::dav_server::{self, FailureLimiter};
use crate::mail::MailService;
use crate::pages::{self, is_htmx, resolve_locale};
use crate::ui::{render, ErrorPage, PageCtx, PageError, ToastPartial, UserView};

/// Largest request body accepted by dynamic routes (forms and JSON are tiny).
const MAX_BODY_BYTES: usize = auth::MAX_BODY_BYTES;
/// Largest JSON error body the middleware will read back to translate it.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

const HX_RETARGET: HeaderName = HeaderName::from_static("hx-retarget");
const HX_RESWAP: HeaderName = HeaderName::from_static("hx-reswap");

/// Everything the HTML routes need. Embeds [`AuthState`] so the JSON auth
/// routes and extractors work unchanged.
#[derive(Clone)]
pub struct AppState {
    /// Authentication service, CSRF and deployment settings.
    pub auth: AuthState,
    /// Database pool for pages that query data directly.
    pub pool: PgPool,
    /// Static asset URLs and the CDN/local choice.
    pub assets: Arc<Assets>,
    /// Quarantine service, present when `QUARANTINE_DIR` is configured.
    pub quarantine: Option<Arc<QuarantineService>>,
    /// Webmail, present when `MAIL_MASTER_PASSWORD` is configured.
    pub mail: Option<Arc<MailService>>,
    /// Administration service (scoped CRUD, audit, quota, import).
    pub admin: Arc<AdminService>,
    /// Bulk-import result files awaiting a one-time download.
    pub import_results: Arc<ImportResults>,
    /// Saved filters and autoresponder settings.
    pub filters: Arc<FilterStore>,
    /// Where ManageSieve is, present when webmail is configured.
    pub sieve: Option<Arc<SieveConfig>>,
    /// Calendars, address books, sharing and free/busy.
    pub dav: Arc<DavStore>,
    /// App passwords for calendar and contact apps.
    pub app_passwords: Arc<AppPasswords>,
    /// Failed CalDAV/CardDAV sign-ins, to slow down guessing.
    pub dav_failures: Arc<FailureLimiter>,
    static_dir: Arc<PathBuf>,
    csp: Arc<HeaderValue>,
}

impl FromRef<AppState> for AuthState {
    fn from_ref(state: &AppState) -> AuthState {
        state.auth.clone()
    }
}

impl AppState {
    /// Builds the state, hashing the static files under `static_dir`.
    ///
    /// # Errors
    /// Fails if the static directory is unreadable or lacks files the templates
    /// need, so a misconfigured deployment stops at startup instead of serving
    /// a page with broken styles.
    pub fn new(
        auth: AuthState,
        pool: PgPool,
        static_dir: PathBuf,
        source: AssetSource,
    ) -> io::Result<Self> {
        let assets = Assets::load(&static_dir, source)?;
        let csp = HeaderValue::from_str(&assets.content_security_policy())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Self {
            auth,
            pool: pool.clone(),
            assets: Arc::new(assets),
            quarantine: None,
            mail: None,
            admin: Arc::new(AdminService::new(pool.clone(), None)),
            filters: Arc::new(FilterStore::new(pool.clone())),
            sieve: None,
            dav: Arc::new(DavStore::new(pool.clone())),
            app_passwords: Arc::new(AppPasswords::new(pool.clone())),
            dav_failures: Arc::new(FailureLimiter::default()),
            import_results: Arc::new(ImportResults::default()),
            static_dir: Arc::new(static_dir),
            csp: Arc::new(csp),
        })
    }
}

impl AppState {
    /// Enables webmail.
    #[must_use]
    pub fn with_mail(mut self, service: MailService) -> Self {
        // ManageSieve lives next to IMAP; a bad SIEVE_PORT only disables filter installation.
        self.sieve = match SieveConfig::from_mail(&service.config) {
            Ok(config) => Some(Arc::new(config)),
            Err(error) => {
                tracing::warn!(%error, "filters will not be installed on the mail server");
                None
            }
        };
        self.mail = Some(Arc::new(service));
        self
    }

    /// Lets the console create and remove Maildirs (needs write access to `VMAIL_BASE`).
    #[must_use]
    pub fn with_mail_dir(mut self, provisioner: MailboxProvisioner) -> Self {
        self.admin = Arc::new(AdminService::new(
            self.pool.clone(),
            Some(Arc::new(provisioner)),
        ));
        self
    }
    /// Enables the quarantine pages.
    #[must_use]
    pub fn with_quarantine(mut self, service: QuarantineService) -> Self {
        self.quarantine = Some(Arc::new(service));
        self
    }
}

/// Builds the application: pages, partials, the `/auth/*` API and `/static`.
///
/// Returns the janitor to spawn (see [`RateLimitJanitor::spawn`]) so rate-limit
/// memory stays bounded.
///
/// # Errors
/// Fails if a rate limit is misconfigured.
pub fn build_app(state: AppState, limits: RateLimits) -> Result<(Router, RateLimitJanitor)> {
    let trust = state.auth.security.trust_proxy_headers;
    let mut janitor = RateLimitJanitor::default();
    let csrf = || middleware::from_fn_with_state(state.auth.clone(), auth::csrf::csrf_protect);
    let api = auth::routes::<AppState>();
    let pages = pages::routes();

    // The HTML login form and the JSON login endpoint share one rate-limit
    // bucket: otherwise alternating between them doubles an attacker's budget.
    let login = rate_limit::apply(
        api.login.merge(pages.login_post).layer(csrf()),
        limits.login,
        trust,
        &mut janitor,
    )?;
    let refresh = rate_limit::apply(
        api.refresh.layer(csrf()),
        limits.refresh,
        trust,
        &mut janitor,
    )?;
    let csrf_only = api.logout.merge(pages.csrf_posts).layer(csrf());

    // CalDAV and CardDAV. Basic auth with app passwords, no cookies, so no CSRF layer; the body
    // limit is raised for calendar and contact uploads.
    let dav = Router::new()
        .route("/dav", any(dav_server::handle))
        .route("/dav/", any(dav_server::handle))
        .route("/dav/{*path}", any(dav_server::handle))
        .route("/.well-known/caldav", any(dav_server::well_known))
        .route("/.well-known/carddav", any(dav_server::well_known))
        .layer(DefaultBodyLimit::max(dav_server::DAV_BODY_LIMIT));
    let dynamic = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(dav)
        .merge(login)
        .merge(refresh)
        .merge(csrf_only)
        .merge(api.open)
        .merge(pages.gets)
        .fallback(|| async { PageError::NotFound })
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        // Order matters. `render_errors` replaces some responses wholesale, so
        // `no_store` must sit outside it or the replacement would lose the header.
        .layer(middleware::from_fn_with_state(state.clone(), render_errors))
        .layer(middleware::from_fn(auth::no_store))
        .with_state(state.clone());

    let static_files = Router::new()
        .nest_service(
            STATIC_PREFIX,
            ServeDir::new(state.static_dir.as_path()).append_index_html_on_directories(false),
        )
        .layer(middleware::from_fn(static_cache_headers));

    let app = Router::new()
        .merge(static_files)
        .merge(dynamic)
        .layer(middleware::from_fn_with_state(state, security_headers));
    Ok((app, janitor))
}

// ─── middleware ─────────────────────────────────────────────────────────────

/// Adds the security response headers to everything, including static files.
///
/// Existing values win, so a handler can tighten or relax a header for itself.
async fn security_headers(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let defaults: [(HeaderName, HeaderValue); 6] = [
        (header::CONTENT_SECURITY_POLICY, state.csp.as_ref().clone()),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ),
        (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        (
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
        ),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ),
    ];
    for (name, value) in defaults {
        headers.entry(name).or_insert(value);
    }
    response
}

/// Long-lived caching for content-addressed URLs (`?v=<hash>`); everything else
/// is revalidated every time.
async fn static_cache_headers(request: Request, next: Next) -> Response {
    let versioned = request.uri().query().is_some_and(|q| {
        q.split('&')
            .any(|pair| pair.starts_with("v=") && pair.len() > 2)
    });
    let mut response = next.run(request).await;
    if response.status().is_success() {
        let policy = if versioned {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    }
    response
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

/// Turns errors into something fit to show.
///
/// - A handler that returns a [`PageError`] gets a localised error page, or for
///   htmx requests a dismissible toast.
/// - An htmx request that was answered with a JSON error by one of the API
///   layers (CSRF failure, rate limit) gets the same toast, so the user sees a
///   message instead of a raw JSON blob swapped into the page.
async fn render_errors(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let headers = request.headers().clone();
    let htmx = is_htmx(&headers);
    let jar = CookieJar::from_headers(&headers);
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;

    if let Some(kind) = response.extensions().get::<PageError>().copied() {
        let ctx = error_ctx(&state, &jar, &headers, &path).await;
        let body_key = format!("error-{}-body", kind.code());
        if htmx {
            return toast_response(&ctx, kind.status(), &body_key, None);
        }
        let page = ErrorPage {
            status: kind.code(),
            title: ctx.t(&format!("error-{}-title", kind.code())),
            message: ctx.t(&body_key),
            ctx,
        };
        let mut rendered = render(&page);
        *rendered.status_mut() = kind.status();
        return rendered;
    }

    let status = response.status();
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if htmx && (status.is_client_error() || status.is_server_error()) && is_json {
        let retry_after = response.headers().get(header::RETRY_AFTER).cloned();
        let (_, body) = response.into_parts();
        let code = axum::body::to_bytes(body, MAX_ERROR_BODY_BYTES)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ErrorBody>(&bytes).ok())
            .map(|b| b.error)
            .unwrap_or_default();
        let key = match code.as_str() {
            "csrf_failed" => "toast-csrf",
            "rate_limited" => "toast-rate-limited",
            "forbidden" => "toast-forbidden",
            _ => "toast-server",
        };
        let ctx = error_ctx(&state, &jar, &headers, &path).await;
        return toast_response(&ctx, status, key, retry_after);
    }
    response
}

/// Layout context for an error page. Shows the account menu if the visitor
/// happens to be signed in; any failure just means a signed-out layout.
async fn error_ctx(
    state: &AppState,
    jar: &CookieJar,
    headers: &axum::http::HeaderMap,
    path: &str,
) -> PageCtx {
    let user: Option<User> = match jar.get(ACCESS_COOKIE) {
        Some(cookie) => match state.auth.auth.authenticate(cookie.value()).await {
            Ok(context) => state.auth.auth.current_user(&context).await.ok(),
            Err(_) => None,
        },
        None => None,
    };
    PageCtx::new(
        resolve_locale(jar, headers),
        state.assets.clone(),
        user.as_ref().map(UserView::from),
        path,
    )
}

/// An error fragment that htmx drops into the toast area instead of the page.
fn toast_response(
    ctx: &PageCtx,
    status: StatusCode,
    key: &str,
    retry_after: Option<HeaderValue>,
) -> Response {
    let toast = ToastPartial {
        message: ctx.t(key),
        ctx: ctx.clone(),
    };
    let mut response = render(&toast);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(HX_RETARGET, HeaderValue::from_static("#rm-toasts"));
    headers.insert(HX_RESWAP, HeaderValue::from_static("beforeend"));
    if let Some(value) = retry_after {
        headers.insert(header::RETRY_AFTER, value);
    }
    response
}
