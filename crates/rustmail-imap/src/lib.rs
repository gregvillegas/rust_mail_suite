//! # rustmail-imap
//!
//! Async IMAP client wrapper for RustMail Suite. Used by `rustmail-web` and
//! `rustmail-admin` to read message bodies server-side from Dovecot, since
//! Postgres stores only message metadata (`messages_meta`) and bodies live in
//! Maildir.
//!
//! Phase 0 provides only a buildable skeleton; the `async-imap` client is
//! introduced in Phase 6.
#![deny(warnings)]
#![forbid(unsafe_code)]

/// The crate version, sourced from Cargo at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the crate version string.
#[must_use]
pub fn version() -> &'static str {
    VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_reported() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }
}
