//! Errors from talking to the ManageSieve server.

use std::fmt;

/// Result type for this crate.
pub type SieveResult<T> = Result<T, SieveError>;

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum SieveError {
    /// The server could not be reached or the connection broke.
    #[error("cannot reach the mail server: {0}")]
    Connect(String),
    /// TLS could not be set up.
    #[error("secure connection failed: {0}")]
    Tls(String),
    /// The server said something this client does not understand.
    #[error("unexpected answer from the mail server: {0}")]
    Protocol(String),
    /// The login was refused.
    #[error("the mail server refused the login")]
    Auth,
    /// The server rejected a script. The text is its explanation (line numbers, what is
    /// wrong) and is meant for the person who wrote the script.
    #[error("{0}")]
    Rejected(String),
    /// The server refused for another reason (quota, no such script, ...).
    #[error("the mail server said no: {0}")]
    Refused(String),
    /// An argument was not acceptable (for example a bad script name).
    #[error("{0}")]
    Invalid(String),
}

impl SieveError {
    /// Whether the user's script is at fault (as opposed to the server being down).
    #[must_use]
    pub fn is_script_error(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }
}

impl From<std::io::Error> for SieveError {
    fn from(e: std::io::Error) -> Self {
        Self::Connect(e.to_string())
    }
}

impl From<rustmail_imap::MailError> for SieveError {
    fn from(e: rustmail_imap::MailError) -> Self {
        match e {
            rustmail_imap::MailError::Tls(m) => Self::Tls(m),
            other => Self::Connect(other.to_string()),
        }
    }
}

/// Trims server text for display: one line of printable characters, bounded.
pub(crate) fn tidy(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().take(500) {
        out.push(if c.is_control() && c != '\n' { ' ' } else { c });
    }
    out.trim().to_owned()
}

pub(crate) struct Quoted<'a>(pub &'a str);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        for c in self.0.chars() {
            if c == '"' || c == '\\' {
                f.write_str("\\")?;
            }
            write!(f, "{c}")?;
        }
        f.write_str("\"")
    }
}
