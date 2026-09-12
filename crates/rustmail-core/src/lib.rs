//! # rustmail-core
//!
//! Shared library for RustMail Suite. Houses domain models, repository traits
//! and their Postgres implementations, the authentication service, config
//! loading, and error types.
//!
//! This is Phase 0 scaffolding: the crate compiles as an empty library. Modules
//! (`domain`, `repository`, `auth`) are populated in later phases.
#![deny(warnings)]
#![forbid(unsafe_code)]

/// Returns the crate version string (from `CARGO_PKG_VERSION`).
///
/// Used as a trivial smoke-test target during Phase 0 and as a build sanity
/// check for downstream crates.
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
