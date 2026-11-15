//! Calendar and contacts (Phase 9): the storage and rules behind CalDAV, CardDAV and the
//! calendar and contacts pages.
//!
//! - [`calendar`] and [`contact`] read iCalendar and vCard text. They validate what a client
//!   sent and extract the few fields used for queries; the original text is what gets stored
//!   and returned, so nothing a client wrote is lost or reformatted.
//! - [`store`] keeps collections (calendars, address books), their objects (events, tasks,
//!   contacts), sharing and free/busy, and checks who may do what.
//! - [`tokens`] holds app passwords: what calendar and contact apps log in with.
//!
//! Every function that touches a collection takes the acting user and checks their access
//! itself, so a caller cannot forget to.

pub mod calendar;
pub mod contact;
pub mod store;
pub mod tokens;

/// Largest object (one event or contact) accepted, in bytes.
pub const MAX_OBJECT_BYTES: usize = 1024 * 1024;

/// What can go wrong with a calendar or contacts request.
#[derive(Debug, thiserror::Error)]
pub enum DavError {
    /// There is no such collection or object, or the user may not even know it exists.
    #[error("not found")]
    NotFound,
    /// The user may see it but not do this to it.
    #[error("forbidden")]
    Forbidden,
    /// An `If-Match` / `If-None-Match` condition did not hold.
    #[error("precondition failed")]
    PreconditionFailed,
    /// The change clashes with something that exists (for example a UID already in use).
    #[error("{0}")]
    Conflict(String),
    /// The data sent is not acceptable. The text says why, for the person who wrote it.
    #[error("{0}")]
    Invalid(String),
    /// A database failure.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Result type for this module.
pub type DavResult<T> = Result<T, DavError>;

impl From<DavError> for crate::Error {
    fn from(e: DavError) -> Self {
        match e {
            DavError::NotFound => crate::Error::NotFound {
                entity: "calendar item",
            },
            DavError::Forbidden => crate::Error::Forbidden("you may not change this".into()),
            DavError::PreconditionFailed => crate::Error::Conflict(
                "it was changed by someone else; reload and try again".into(),
            ),
            DavError::Conflict(m) => crate::Error::Conflict(m),
            DavError::Invalid(m) => crate::Error::Validation(m),
            DavError::Database(e) => crate::Error::from(e),
        }
    }
}
