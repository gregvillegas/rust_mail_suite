//! Errors returned by the authentication layer.

use chrono::{DateTime, Utc};
use thiserror::Error;

/// Why an authentication or authorisation step failed.
///
/// Variants are deliberately coarse so responses do not help an attacker:
/// a wrong password and an unknown account are both [`AuthError::InvalidCredentials`].
#[derive(Debug, Error)]
pub enum AuthError {
    /// Unknown account or wrong password.
    #[error("invalid email or password")]
    InvalidCredentials,

    /// The password was right but the account is temporarily locked after too
    /// many failures. Only reported once the password has been verified, so it
    /// cannot be used to probe which accounts exist.
    #[error("account is temporarily locked")]
    AccountLocked {
        /// When the lock expires.
        until: DateTime<Utc>,
    },

    /// The password was right but the account (or its domain) is not active.
    #[error("account is disabled")]
    AccountDisabled,

    /// The access or refresh token is missing, malformed, expired, revoked or
    /// otherwise unusable.
    #[error("invalid or expired token")]
    InvalidToken,

    /// The caller is authenticated but lacks the required role.
    #[error("insufficient privileges")]
    Forbidden,

    /// An unexpected internal failure (database, hashing, ...).
    #[error(transparent)]
    Internal(#[from] crate::Error),
}

impl From<sqlx::Error> for AuthError {
    fn from(err: sqlx::Error) -> Self {
        AuthError::Internal(err.into())
    }
}
