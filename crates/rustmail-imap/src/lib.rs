//! # rustmail-imap
//!
//! Async IMAP client wrapper for RustMail Suite. Used by the web and admin
//! crates to fetch message bodies server-side from Dovecot.
//!
//! This is Phase 0 scaffolding: the crate compiles as an empty library. The
//! IMAP client is implemented in Phase 6.
#![deny(warnings)]
#![forbid(unsafe_code)]

/// Returns the crate version string (from `CARGO_PKG_VERSION`).
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_reported() {
        assert!(!version().is_empty());
    }
}
