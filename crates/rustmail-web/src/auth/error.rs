//! JSON error responses for the auth API.

use axum::body::Body;
use axum::http::{header, HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
use chrono::Utc;
use rustmail_core::auth::AuthError;

/// Builds a `{"error": code, "message": message}` JSON response.
#[must_use]
pub fn json_response(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": code, "message": message }).to_string();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// An error that renders as a JSON response with a stable `error` code.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retry_after_secs: Option<i64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
            retry_after_secs: None,
        }
    }

    /// The request failed CSRF validation.
    #[must_use]
    pub fn csrf() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "csrf_failed",
            "Missing or invalid CSRF token.",
        )
    }

    /// The request body was unacceptable.
    #[must_use]
    pub fn bad_request(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// The HTTP status this error maps to.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl From<AuthError> for ApiError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::InvalidCredentials => Self::new(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "Invalid email or password.",
            ),
            AuthError::AccountLocked { until } => {
                let mut api = Self::new(
                    StatusCode::FORBIDDEN,
                    "account_locked",
                    "Too many failed attempts. The account is temporarily locked.",
                );
                api.retry_after_secs = Some((until - Utc::now()).num_seconds().max(1));
                api
            }
            AuthError::AccountDisabled => Self::new(
                StatusCode::FORBIDDEN,
                "account_disabled",
                "This account is disabled.",
            ),
            AuthError::InvalidToken => Self::new(
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "Authentication is required or has expired.",
            ),
            AuthError::Forbidden => Self::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "You do not have permission to do that.",
            ),
            AuthError::Internal(inner) => {
                // Details go to the log, never to the client.
                tracing::error!(error = %inner, "internal error in auth API");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "Internal server error.",
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let mut response = json_response(self.status, self.code, self.message);
        if let Some(secs) = self.retry_after_secs {
            if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use rustmail_core::Error;

    use super::*;

    #[test]
    fn auth_errors_map_to_stable_statuses_and_codes() {
        let cases = [
            (
                AuthError::InvalidCredentials,
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
            ),
            (
                AuthError::InvalidToken,
                StatusCode::UNAUTHORIZED,
                "invalid_token",
            ),
            (
                AuthError::AccountDisabled,
                StatusCode::FORBIDDEN,
                "account_disabled",
            ),
            (AuthError::Forbidden, StatusCode::FORBIDDEN, "forbidden"),
        ];
        for (error, status, code) in cases {
            let api = ApiError::from(error);
            assert_eq!((api.status(), api.code), (status, code));
        }
    }

    #[test]
    fn locked_accounts_advertise_retry_after() {
        let until = Utc::now() + chrono::Duration::seconds(90);
        let api = ApiError::from(AuthError::AccountLocked { until });
        assert_eq!(api.status(), StatusCode::FORBIDDEN);
        assert!(api.retry_after_secs.is_some_and(|s| (60..=90).contains(&s)));
        let response = api.into_response();
        assert!(response.headers().contains_key(header::RETRY_AFTER));
    }

    #[test]
    fn internal_errors_do_not_leak_details() {
        let api = ApiError::from(AuthError::Internal(Error::Config(
            "db password is hunter2".into(),
        )));
        assert_eq!(api.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!api.message.contains("hunter2"));
    }
}
