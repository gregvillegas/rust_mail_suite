//! CSRF middleware (signed double-submit cookie).
//!
//! Every request that is not a safe method (`GET`, `HEAD`, `OPTIONS`, `TRACE`)
//! must carry the same signed token in the `rm_csrf` cookie and the
//! `X-CSRF-Token` header, and the token must be a genuine one issued for the
//! caller's session. See [`rustmail_core::auth::csrf`] for the scheme.
//!
//! The JSON login endpoint is additionally protected by requiring
//! `Content-Type: application/json`, which a cross-site HTML form cannot send.

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use rustmail_core::auth::{peek_claims, ANONYMOUS_CONTEXT};

use super::cookies::{ACCESS_COOKIE, CSRF_COOKIE, CSRF_HEADER};
use super::error::ApiError;
use super::state::AuthState;

/// The CSRF context for a request: the session id if the caller holds a
/// validly signed access cookie (even an expired one, so refresh still works),
/// otherwise the anonymous context.
pub(crate) fn session_context(state: &AuthState, jar: &CookieJar) -> String {
    jar.get(ACCESS_COOKIE)
        .and_then(|cookie| peek_claims(state.auth.config(), cookie.value()))
        .map_or_else(
            || ANONYMOUS_CONTEXT.to_owned(),
            |claims| claims.sid.to_string(),
        )
}

/// Makes sure the caller holds a CSRF cookie that is valid for their current
/// context (their session, or anonymous), issuing a fresh one if not. Page
/// handlers call this so forms always have a token to echo.
pub(crate) fn ensure_cookie(state: &AuthState, jar: CookieJar) -> CookieJar {
    let context = session_context(state, &jar);
    if jar
        .get(CSRF_COOKIE)
        .is_some_and(|c| state.csrf.verify(&context, c.value()))
    {
        return jar;
    }
    let lifetime = state
        .auth
        .config()
        .refresh_ttl
        .max(chrono::Duration::minutes(1));
    let token = state.csrf.issue(&context);
    jar.add(super::cookies::csrf_cookie(
        token,
        chrono::Utc::now() + lifetime,
        state.security.cookie_secure,
    ))
}

fn is_safe(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// Rejects state-changing requests that fail CSRF validation.
pub async fn csrf_protect(
    State(state): State<AuthState>,
    request: Request,
    next: Next,
) -> Response {
    if is_safe(request.method()) {
        return next.run(request).await;
    }
    let jar = CookieJar::from_headers(request.headers());
    let header = request
        .headers()
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok());
    let cookie = jar.get(CSRF_COOKIE).map(|c| c.value());

    let context = session_context(&state, &jar);
    match (header, cookie) {
        (Some(header), Some(cookie)) if header == cookie && state.csrf.verify(&context, header) => {
            next.run(request).await
        }
        _ => ApiError::csrf().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_only_methods_are_exempt() {
        for safe in [Method::GET, Method::HEAD, Method::OPTIONS, Method::TRACE] {
            assert!(is_safe(&safe), "{safe}");
        }
        for unsafe_method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(!is_safe(&unsafe_method), "{unsafe_method}");
        }
    }
}
