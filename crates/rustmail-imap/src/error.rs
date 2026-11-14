//! Errors from talking to the mail servers.

use thiserror::Error;

/// Everything that can go wrong in this crate.
#[derive(Debug, Error)]
pub enum MailError {
    /// Settings are missing or unusable.
    #[error("mail configuration: {0}")]
    Config(String),
    /// A server could not be reached or the connection dropped.
    #[error("cannot reach the mail server: {0}")]
    Connect(String),
    /// TLS could not be established (certificate not trusted, protocol error).
    #[error("secure connection failed: {0}")]
    Tls(String),
    /// The server refused the credentials.
    #[error("the mail server refused the login")]
    Auth,
    /// The IMAP server answered with an error or something unexpected.
    #[error("IMAP error: {0}")]
    Imap(String),
    /// The requested folder or message does not exist (any more).
    #[error("not found")]
    NotFound,
    /// The caller supplied something unusable (bad folder name, address, size).
    #[error("{0}")]
    Invalid(String),
    /// A message or upload exceeds a size limit.
    #[error("too large: {0}")]
    TooLarge(String),
    /// A message could not be parsed.
    #[error("could not read the message: {0}")]
    Parse(String),
    /// Postfix refused the message or the submission failed.
    #[error("sending failed: {0}")]
    Send(String),
    /// A database error while reading or writing the search index.
    #[error("index error: {0}")]
    Index(String),
}

/// Result alias for this crate.
pub type MailResult<T> = Result<T, MailError>;

impl From<async_imap::error::Error> for MailError {
    fn from(error: async_imap::error::Error) -> Self {
        use async_imap::error::Error as E;
        match error {
            E::Io(e) => Self::Connect(e.to_string()),
            E::No(text) | E::Bad(text) => Self::Imap(text),
            other => Self::Imap(other.to_string()),
        }
    }
}

impl From<rustmail_core::Error> for MailError {
    fn from(error: rustmail_core::Error) -> Self {
        match error {
            rustmail_core::Error::Validation(text) => Self::Invalid(text),
            other => Self::Index(other.to_string()),
        }
    }
}

impl From<std::io::Error> for MailError {
    fn from(error: std::io::Error) -> Self {
        Self::Connect(error.to_string())
    }
}
