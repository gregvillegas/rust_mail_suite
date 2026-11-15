//! # rustmail-sieve
//!
//! A small ManageSieve (RFC 5804) client. RustMail uses it to install each user's
//! Sieve scripts (filters and autoresponder) on Dovecot, logging in as the user
//! through the same master account the webmail uses for IMAP.
//!
//! - [`protocol`]: tokens and responses, including literals.
//! - [`client`]: the session (STARTTLS, `AUTHENTICATE PLAIN`, `PUTSCRIPT`, ...).
//! - [`install`]: the one call the web UI makes.
//!
//! Credentials are only sent after STARTTLS succeeded; a server that does not offer it
//! is refused. Certificate checking follows the IMAP settings.
#![deny(warnings)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod client;
pub mod error;
pub mod protocol;

pub use client::{ScriptInfo, SieveConfig, TlsSession};
pub use error::{SieveError, SieveResult};

/// Stores `scripts` (name, text) for `user` and makes `active` the script Dovecot runs.
///
/// Scripts are stored in the order given, so list the ones that are included before the
/// one that includes them. Nothing is activated unless every script was accepted.
///
/// # Errors
/// [`SieveError::Rejected`] when the server refuses a script (the message says why),
/// otherwise a connection, TLS or login error.
pub async fn install(
    config: &SieveConfig,
    user: &str,
    scripts: &[(&str, &str)],
    active: &str,
) -> SieveResult<()> {
    let mut session = TlsSession::connect(config, user).await?;
    let result = async {
        for (name, content) in scripts {
            session.put(name, content).await?;
        }
        session.activate(active).await
    }
    .await;
    session.logout().await;
    result
}

/// Fetches the stored text of one script, if it exists.
///
/// # Errors
/// A connection, TLS or login error.
pub async fn fetch(config: &SieveConfig, user: &str, name: &str) -> SieveResult<Option<String>> {
    let mut session = TlsSession::connect(config, user).await?;
    let result = session.get(name).await;
    session.logout().await;
    result
}

/// Asks the server whether `content` is a valid script, without storing it.
///
/// # Errors
/// [`SieveError::Rejected`] with the server's explanation, or a connection error.
pub async fn check(config: &SieveConfig, user: &str, content: &str) -> SieveResult<()> {
    let mut session = TlsSession::connect(config, user).await?;
    let result = session.check(content).await;
    session.logout().await;
    result
}
