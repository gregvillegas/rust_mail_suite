//! Request extractors: the authenticated user and the client's address.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::header::USER_AGENT;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use rustmail_core::auth::{AuthContext, AuthError, ClientInfo};
use rustmail_core::domain::UserRole;

use super::cookies::ACCESS_COOKIE;
use super::error::ApiError;
use super::rate_limit::client_ip;
use super::state::AuthState;

/// The authenticated caller. Extracting it validates the access cookie, checks
/// the session is still active, and rejects with `401` otherwise.
///
/// ```ignore
/// async fn handler(user: AuthUser) -> Result<String, ApiError> {
///     user.require_role(UserRole::DomainAdmin)?;
///     Ok(format!("hello {}", user.0.user_id))
/// }
/// ```
#[derive(Debug, Clone)]
pub struct AuthUser(pub AuthContext);

impl AuthUser {
    /// Requires at least `minimum`; otherwise a `403` [`ApiError`].
    ///
    /// # Errors
    /// Returns a `403` error if the caller's role is too low.
    pub fn require_role(&self, minimum: UserRole) -> Result<(), ApiError> {
        self.0.require_role(minimum).map_err(ApiError::from)
    }
}

impl<S> FromRequestParts<S> for AuthUser
where
    AuthState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state = AuthState::from_ref(state);
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(ACCESS_COOKIE)
            .ok_or(AuthError::InvalidToken)?
            .value()
            .to_owned();
        let context = state.auth.authenticate(&token).await?;
        Ok(Self(context))
    }
}

/// Who is calling, for session records and the audit log.
#[derive(Debug, Clone)]
pub struct ClientMeta(pub ClientInfo);

impl<S> FromRequestParts<S> for ClientMeta
where
    AuthState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state = AuthState::from_ref(state);
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);
        let ip = client_ip(&parts.headers, peer, state.security.trust_proxy_headers);
        let user_agent = parts
            .headers
            .get(USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Ok(Self(ClientInfo {
            ip_address: ip.map(|ip| ip.to_string()),
            user_agent,
        }))
    }
}
