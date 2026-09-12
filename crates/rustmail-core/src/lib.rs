//! # rustmail-core
//!
//! Shared library for RustMail Suite. Houses the domain model, repository
//! traits and their PostgreSQL implementations, the authentication service,
//! configuration loading, and the crate-wide error type.
//!
//! This is the single source of truth for business logic; the binary crates
//! (`rustmail-web`, `rustmail-admin`, `rustmail-cli`) and the `rustmail-imap`
//! library all depend on it, and nothing depends on the binaries.
//!
//! Phase 0 provides only a buildable skeleton. Modules are filled in during
//! their respective phases:
//! - `domain`, `repository` — Phase 1
//! - `auth` — Phase 2
#![deny(warnings)]
#![forbid(unsafe_code)]

/// The crate version, sourced from Cargo at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the crate version string.
///
/// Placeholder used by Phase 0 to prove the workspace builds and links.
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
