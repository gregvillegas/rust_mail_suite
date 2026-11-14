//! Authentication and session management.
//!
//! - [`AuthService`]: login, refresh (with rotation and reuse detection),
//!   logout and per-request authentication.
//! - [`tokens`]: signed access tokens and opaque refresh tokens.
//! - [`csrf`]: signed double-submit CSRF tokens.
//! - [`AuthConfig`]: lifetimes, lockout policy and the signing secret.
//!
//! This module has no HTTP dependencies; cookies, rate limiting and routing
//! live in `rustmail-web`.

pub mod config;
pub mod csrf;
pub mod error;
pub mod service;
pub mod tokens;

pub use config::AuthConfig;
pub use csrf::{CsrfProtector, ANONYMOUS_CONTEXT};
pub use error::AuthError;
pub use service::{AuthContext, AuthOutcome, AuthService, ClientInfo, LoginRequest};
pub use tokens::{peek_claims, Claims, TokenPair};
