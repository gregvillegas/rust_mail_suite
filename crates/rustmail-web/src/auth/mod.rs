//! HTTP authentication layer: cookies, CSRF, rate limiting and `/auth/*` routes.
//!
//! | Route | Method | CSRF | Rate limit |
//! |-------|--------|------|------------|
//! | `/auth/csrf` | GET | n/a | no |
//! | `/auth/login` | POST | yes | yes |
//! | `/auth/refresh` | POST | yes | yes |
//! | `/auth/logout` | POST | yes | no |
//! | `/auth/me` | GET | n/a | no |
//!
//! Use [`AuthUser`] as an extractor on any JSON handler that needs a signed-in
//! user. HTML pages use `pages::WebUser`, which redirects instead of returning JSON.
//!
//! Two ways to mount the routes:
//! - [`router`]: the self-contained JSON API, as used by the API tests.
//! - [`routes`]: the unlayered groups, so the application can merge its HTML
//!   endpoints into the same CSRF and rate-limit layers (the HTML login must
//!   share the JSON login's rate-limit bucket, or it would be a way around it).

pub mod cookies;
pub mod csrf;
pub mod error;
pub mod extractor;
pub(crate) mod handlers;
pub mod rate_limit;
pub mod state;

use anyhow::Result;
use axum::extract::{DefaultBodyLimit, FromRef, Request};
use axum::http::{header, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

pub use error::ApiError;
pub use extractor::{AuthUser, ClientMeta};
pub use handlers::{CsrfBody, LoginBody, SessionBody, UserBody};
pub use rate_limit::RateLimitJanitor;
pub use state::{AuthState, DeploymentSecurity};

use crate::config::RateLimitSettings;

/// Largest request body accepted by the auth routes.
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// Per-route rate limits.
#[derive(Debug, Clone, Copy)]
pub struct RateLimits {
    /// Limit on `POST /auth/login` (and the HTML login form).
    pub login: RateLimitSettings,
    /// Limit on `POST /auth/refresh`.
    pub refresh: RateLimitSettings,
}

/// The `/auth/*` endpoints grouped by the protections they need. None of the
/// groups carries CSRF or rate-limit layers yet; the caller applies them.
pub struct AuthRoutes<S> {
    /// `POST /auth/login`: CSRF + the login rate limit.
    pub login: Router<S>,
    /// `POST /auth/refresh`: CSRF + the refresh rate limit.
    pub refresh: Router<S>,
    /// `POST /auth/logout`: CSRF only.
    pub logout: Router<S>,
    /// `GET /auth/csrf` and `GET /auth/me`: read-only, no extra layers.
    pub open: Router<S>,
}

/// Builds the unlayered `/auth/*` route groups for an application state `S`
/// that can provide an [`AuthState`].
pub fn routes<S>() -> AuthRoutes<S>
where
    S: Clone + Send + Sync + 'static,
    AuthState: FromRef<S>,
{
    AuthRoutes {
        login: Router::new().route("/auth/login", post(handlers::login)),
        refresh: Router::new().route("/auth/refresh", post(handlers::refresh)),
        logout: Router::new().route("/auth/logout", post(handlers::logout)),
        open: Router::new()
            .route("/auth/csrf", get(handlers::csrf))
            .route("/auth/me", get(handlers::me)),
    }
}

/// Auth responses carry credentials or session state and must never be cached.
pub async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Builds the self-contained `/auth/*` JSON API.
///
/// Layer order matters: the rate limiter is outermost so abusive clients are
/// rejected before any CSRF or credential work happens.
///
/// # Errors
/// Fails if a rate limit has a zero burst or period.
pub fn router(state: AuthState, limits: RateLimits) -> Result<(Router, RateLimitJanitor)> {
    let trust = state.security.trust_proxy_headers;
    let mut janitor = RateLimitJanitor::default();
    let csrf = || middleware::from_fn_with_state(state.clone(), csrf::csrf_protect);
    let groups = routes::<AuthState>();

    let login = rate_limit::apply(
        groups.login.layer(csrf()),
        limits.login,
        trust,
        &mut janitor,
    )?;
    let refresh = rate_limit::apply(
        groups.refresh.layer(csrf()),
        limits.refresh,
        trust,
        &mut janitor,
    )?;
    let logout = groups.logout.layer(csrf());

    let app = Router::new()
        .merge(login)
        .merge(refresh)
        .merge(logout)
        .merge(groups.open)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn(no_store))
        .with_state(state);
    Ok((app, janitor))
}
