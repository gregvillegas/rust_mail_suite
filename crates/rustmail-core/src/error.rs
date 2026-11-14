//! Crate-wide error type.

use thiserror::Error;

/// Errors produced by `rustmail-core`.
#[derive(Debug, Error)]
pub enum Error {
    /// The requested entity does not exist.
    #[error("{entity} not found")]
    NotFound {
        /// Human-readable entity name, e.g. `"user"`.
        entity: &'static str,
    },

    /// The operation conflicts with existing data (duplicate or still referenced).
    #[error("conflict: {0}")]
    Conflict(String),

    /// The signed-in administrator may not do this (wrong role, or another domain).
    #[error("not allowed: {0}")]
    Forbidden(String),
    /// Caller-supplied input failed validation.
    #[error("invalid input: {0}")]
    Validation(String),

    /// Password hashing or hash parsing failed.
    #[error("password hashing error: {0}")]
    PasswordHash(String),

    /// An unexpected database error.
    #[error("database error: {0}")]
    Database(#[source] sqlx::Error),

    /// Creating or inspecting mail directories on disk failed.
    #[error("mail storage error: {0}")]
    Provisioning(String),

    /// Required configuration is missing or invalid.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// Applying migrations failed.
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}

/// Convenience result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

impl From<sqlx::Error> for Error {
    /// Maps constraint violations to domain-level errors so callers never have
    /// to inspect SQLSTATE codes. The raw SQL message is deliberately not
    /// surfaced; only the constraint name is.
    fn from(err: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &err {
            let constraint = db.constraint().unwrap_or("unknown").to_owned();
            if db.is_unique_violation() {
                return Error::Conflict(format!("already exists ({constraint})"));
            }
            if db.is_foreign_key_violation() {
                return Error::Conflict(format!("referenced data conflict ({constraint})"));
            }
            if db.is_check_violation() {
                return Error::Validation(format!("constraint violated ({constraint})"));
            }
        }
        Error::Database(err)
    }
}

/// Builds a [`Error::NotFound`] for the given entity name.
pub(crate) fn not_found(entity: &'static str) -> Error {
    Error::NotFound { entity }
}
