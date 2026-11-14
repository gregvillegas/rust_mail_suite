//! `/auth/*` endpoints.
//!
//! Tokens only ever travel in cookies, never in response bodies, so page
//! scripts cannot read them. Responses carry the CSRF token (which scripts do
//! need) and non-sensitive user details.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Duration, Utc};
use rustmail_core::auth::{peek_claims, AuthError, ClientInfo, LoginRequest, TokenPair};
use rustmail_core::domain::{User, UserRole};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::cookies::{
    access_cookie, csrf_cookie, expired_cookie, refresh_cookie, ACCESS_COOKIE, CSRF_COOKIE,
    REFRESH_COOKIE, REFRESH_COOKIE_PATH,
};
use super::csrf::session_context;
use super::error::ApiError;
use super::extractor::{AuthUser, ClientMeta};
use super::state::AuthState;

const MAX_EMAIL_CHARS: usize = 320;
const MAX_PASSWORD_CHARS: usize = 1024;

/// Credentials posted to `/auth/login`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginBody {
    email: String,
    password: String,
}

/// The public parts of a user account. Never includes secrets.
#[derive(Debug, Serialize)]
pub struct UserBody {
    /// Account id.
    pub id: Uuid,
    /// Full e-mail address.
    pub email: String,
    /// Name shown in the UI.
    pub display_name: String,
    /// Authorisation level.
    pub role: UserRole,
    /// UI locale tag.
    pub locale: String,
}

impl From<&User> for UserBody {
    fn from(user: &User) -> Self {
        Self {
            id: user.id,
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            role: user.role,
            locale: user.locale.clone(),
        }
    }
}

/// Returned by login and refresh. Tokens themselves are set as cookies.
#[derive(Debug, Serialize)]
pub struct SessionBody {
    /// The signed-in user.
    pub user: UserBody,
    /// When the access token (and so the need to refresh) lapses.
    pub access_expires_at: DateTime<Utc>,
    /// Fresh CSRF token for this session; send it as `X-CSRF-Token`.
    pub csrf_token: String,
}

/// Returned by `GET /auth/csrf`.
#[derive(Debug, Serialize)]
pub struct CsrfBody {
    /// Token to send as `X-CSRF-Token` (it is also set as a cookie).
    pub csrf_token: String,
}

/// Sets the three session cookies and returns the new CSRF token.
pub(crate) fn start_cookies(
    state: &AuthState,
    jar: CookieJar,
    tokens: &TokenPair,
) -> (CookieJar, String) {
    let secure = state.security.cookie_secure;
    let csrf = state.csrf.issue(&tokens.session_id.to_string());
    let jar = jar
        .add(access_cookie(
            tokens.access_token.clone(),
            tokens.refresh_expires_at,
            secure,
        ))
        .add(refresh_cookie(
            tokens.refresh_token.clone(),
            tokens.refresh_expires_at,
            secure,
        ))
        .add(csrf_cookie(csrf.clone(), tokens.refresh_expires_at, secure));
    (jar, csrf)
}

/// Tells the browser to drop all three session cookies.
pub(crate) fn clear_cookies(state: &AuthState, jar: CookieJar) -> CookieJar {
    let secure = state.security.cookie_secure;
    jar.add(expired_cookie(ACCESS_COOKIE, "/", secure))
        .add(expired_cookie(REFRESH_COOKIE, REFRESH_COOKIE_PATH, secure))
        .add(expired_cookie(CSRF_COOKIE, "/", secure))
}

fn session_response(
    state: &AuthState,
    jar: CookieJar,
    user: &User,
    tokens: &TokenPair,
) -> Response {
    let (jar, csrf_token) = start_cookies(state, jar, tokens);
    let body = SessionBody {
        user: UserBody::from(user),
        access_expires_at: tokens.access_expires_at,
        csrf_token,
    };
    (jar, Json(body)).into_response()
}

/// `GET /auth/csrf`: issues a CSRF token for the caller's current context.
pub(super) async fn csrf(State(state): State<AuthState>, jar: CookieJar) -> impl IntoResponse {
    let context = session_context(&state, &jar);
    let token = state.csrf.issue(&context);
    let lifetime = state.auth.config().refresh_ttl.max(Duration::minutes(1));
    let jar = jar.add(csrf_cookie(
        token.clone(),
        Utc::now() + lifetime,
        state.security.cookie_secure,
    ));
    (jar, Json(CsrfBody { csrf_token: token }))
}

/// `POST /auth/login`: verifies credentials and starts a session.
pub(super) async fn login(
    State(state): State<AuthState>,
    ClientMeta(client): ClientMeta,
    jar: CookieJar,
    Json(body): Json<LoginBody>,
) -> Result<Response, ApiError> {
    if body.email.chars().count() > MAX_EMAIL_CHARS {
        return Err(ApiError::bad_request("E-mail address is too long."));
    }
    if body.password.is_empty() || body.password.chars().count() > MAX_PASSWORD_CHARS {
        return Err(ApiError::bad_request(
            "Password must be between 1 and 1024 characters.",
        ));
    }
    let outcome = state
        .auth
        .login(LoginRequest {
            email: body.email,
            password: body.password,
            client,
        })
        .await?;
    Ok(session_response(
        &state,
        jar,
        &outcome.user,
        &outcome.tokens,
    ))
}

/// `POST /auth/refresh`: rotates the session using the refresh cookie.
///
/// A failed refresh deliberately does **not** clear cookies: two tabs may race,
/// and the loser must not wipe the cookies the winner just set. Cookies are
/// cleared only when the account has been disabled.
pub(super) async fn refresh(
    State(state): State<AuthState>,
    ClientMeta(client): ClientMeta,
    jar: CookieJar,
) -> Response {
    let Some(token) = jar.get(REFRESH_COOKIE).map(|c| c.value().to_owned()) else {
        return ApiError::from(AuthError::InvalidToken).into_response();
    };
    match state.auth.refresh(&token, client).await {
        Ok(outcome) => session_response(&state, jar, &outcome.user, &outcome.tokens),
        Err(AuthError::AccountDisabled) => {
            let jar = clear_cookies(&state, jar);
            (jar, ApiError::from(AuthError::AccountDisabled)).into_response()
        }
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// Ends the session(s) the request's cookies refer to and returns a jar that
/// clears them. Shared by the JSON and HTML logout endpoints.
///
/// Idempotent: unknown or already ended sessions are fine.
pub(crate) async fn end_session(
    state: &AuthState,
    client: ClientInfo,
    jar: CookieJar,
) -> Result<CookieJar, ApiError> {
    if let Some(token) = jar.get(REFRESH_COOKIE) {
        state
            .auth
            .logout_by_refresh_token(token.value(), client.clone())
            .await?;
    }
    // Also end the session named by the access cookie, which may be the only
    // credential left if the refresh cookie was lost.
    if let Some(claims) = jar
        .get(ACCESS_COOKIE)
        .and_then(|c| peek_claims(state.auth.config(), c.value()))
    {
        state.auth.logout(claims.sid, client).await?;
    }
    Ok(clear_cookies(state, jar))
}

/// `POST /auth/logout`: ends the session and clears the cookies. Idempotent.
pub(super) async fn logout(
    State(state): State<AuthState>,
    ClientMeta(client): ClientMeta,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let jar = end_session(&state, client, jar).await?;
    Ok((jar, StatusCode::NO_CONTENT).into_response())
}

/// `GET /auth/me`: the signed-in user.
pub(super) async fn me(
    State(state): State<AuthState>,
    user: AuthUser,
) -> Result<Json<UserBody>, ApiError> {
    let user = state.auth.current_user(&user.0).await?;
    Ok(Json(UserBody::from(&user)))
}
