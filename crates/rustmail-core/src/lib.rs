//! # rustmail-core
//!
//! Shared library for RustMail Suite. Houses the domain model, repository
//! traits and their PostgreSQL implementations, database helpers, password
//! hashing, and the crate-wide error type. Authentication (`auth`) is added in
//! Phase 2.
//!
//! This crate is the single source of truth for business logic; the binaries
//! and `rustmail-imap` depend on it, never the other way around.
#![deny(warnings)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod admin;
pub mod auth;
pub mod db;
pub mod domain;
pub mod error;
pub mod mailindex;
pub mod password;
pub mod pipeline;
pub mod policy;
pub mod provisioning;
pub mod quarantine;
pub mod repository;
pub mod seed;
pub mod smtp;

pub use error::{Error, Result};

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
