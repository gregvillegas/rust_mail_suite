//! Authentication settings.

use std::fmt;

use chrono::Duration;

use crate::error::{Error, Result};

/// Minimum accepted length of the signing secret, in bytes (256 bits for HS256).
pub const MIN_SECRET_LEN: usize = 32;

const DEFAULT_ISSUER: &str = "rustmail";
const DEFAULT_ACCESS_TTL_SECS: i64 = 15 * 60;
const DEFAULT_REFRESH_TTL_SECS: i64 = 14 * 24 * 60 * 60;
const DEFAULT_MAX_FAILED_LOGINS: i32 = 5;
const DEFAULT_LOCKOUT_SECS: i64 = 15 * 60;
const DEFAULT_REUSE_GRACE_SECS: i64 = 10;
/// Upper bound for any configured duration (10 years); guards against typos.
const MAX_SECS: i64 = 10 * 365 * 24 * 60 * 60;

/// Settings for [`AuthService`](super::AuthService) and token handling.
#[derive(Clone)]
pub struct AuthConfig {
    secret: Vec<u8>,
    /// `iss` claim written into and required on access tokens.
    pub issuer: String,
    /// Lifetime of an access token.
    pub access_ttl: Duration,
    /// Lifetime of a refresh token / session.
    pub refresh_ttl: Duration,
    /// Consecutive wrong passwords that trigger a lock.
    pub max_failed_logins: i32,
    /// How long an account stays locked.
    pub lockout: Duration,
    /// How long after a refresh token is rotated that replaying it is treated
    /// as a harmless race (two tabs refreshing at once) rather than theft.
    /// After this window, replay revokes all of the user's sessions.
    pub reuse_grace: Duration,
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthConfig")
            .field("secret", &"<redacted>")
            .field("issuer", &self.issuer)
            .field("access_ttl", &self.access_ttl)
            .field("refresh_ttl", &self.refresh_ttl)
            .field("max_failed_logins", &self.max_failed_logins)
            .field("lockout", &self.lockout)
            .field("reuse_grace", &self.reuse_grace)
            .finish()
    }
}

impl AuthConfig {
    /// Creates a config with default lifetimes (15 min access, 14 day refresh,
    /// lock for 15 min after 5 failures).
    ///
    /// # Errors
    /// Returns [`Error::Config`] if `secret` is shorter than [`MIN_SECRET_LEN`].
    pub fn new(secret: impl Into<Vec<u8>>) -> Result<Self> {
        let secret = secret.into();
        if secret.len() < MIN_SECRET_LEN {
            return Err(Error::Config(format!(
                "JWT secret must be at least {MIN_SECRET_LEN} bytes (got {})",
                secret.len()
            )));
        }
        Ok(Self {
            secret,
            issuer: DEFAULT_ISSUER.to_owned(),
            access_ttl: Duration::seconds(DEFAULT_ACCESS_TTL_SECS),
            refresh_ttl: Duration::seconds(DEFAULT_REFRESH_TTL_SECS),
            max_failed_logins: DEFAULT_MAX_FAILED_LOGINS,
            lockout: Duration::seconds(DEFAULT_LOCKOUT_SECS),
            reuse_grace: Duration::seconds(DEFAULT_REUSE_GRACE_SECS),
        })
    }

    /// Loads settings from the environment.
    ///
    /// | Variable | Default |
    /// |----------|---------|
    /// | `JWT_SECRET` | required, at least 32 bytes, not a `change-me` placeholder |
    /// | `JWT_ACCESS_TTL_SECS` | 900 |
    /// | `JWT_REFRESH_TTL_SECS` | 1209600 |
    /// | `AUTH_MAX_FAILED_LOGINS` | 5 |
    /// | `AUTH_LOCKOUT_SECS` | 900 |
    /// | `AUTH_REFRESH_REUSE_GRACE_SECS` | 10 (0 disables the grace window) |
    ///
    /// # Errors
    /// Returns [`Error::Config`] for a missing/weak secret or an invalid number.
    pub fn from_env() -> Result<Self> {
        let secret = std::env::var("JWT_SECRET").map_err(|_| {
            Error::Config("JWT_SECRET is not set (generate one with: openssl rand -hex 32)".into())
        })?;
        if secret.starts_with("change-me") {
            return Err(Error::Config(
                "JWT_SECRET is still the example placeholder; generate one with: openssl rand -hex 32"
                    .into(),
            ));
        }
        let mut config = Self::new(secret.into_bytes())?;
        config.access_ttl = env_duration("JWT_ACCESS_TTL_SECS", DEFAULT_ACCESS_TTL_SECS)?;
        config.refresh_ttl = env_duration("JWT_REFRESH_TTL_SECS", DEFAULT_REFRESH_TTL_SECS)?;
        config.lockout = env_duration("AUTH_LOCKOUT_SECS", DEFAULT_LOCKOUT_SECS)?;
        let grace = env_i64("AUTH_REFRESH_REUSE_GRACE_SECS", DEFAULT_REUSE_GRACE_SECS)?;
        if !(0..=3600).contains(&grace) {
            return Err(Error::Config(
                "AUTH_REFRESH_REUSE_GRACE_SECS must be between 0 and 3600".into(),
            ));
        }
        config.reuse_grace = Duration::seconds(grace);
        let failures = env_i64(
            "AUTH_MAX_FAILED_LOGINS",
            i64::from(DEFAULT_MAX_FAILED_LOGINS),
        )?;
        config.max_failed_logins = i32::try_from(failures)
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| {
                Error::Config("AUTH_MAX_FAILED_LOGINS must be a positive integer".into())
            })?;
        Ok(config)
    }

    /// Sets the access-token lifetime.
    #[must_use]
    pub fn with_access_ttl(mut self, ttl: Duration) -> Self {
        self.access_ttl = ttl;
        self
    }

    /// Sets the refresh-token / session lifetime.
    #[must_use]
    pub fn with_refresh_ttl(mut self, ttl: Duration) -> Self {
        self.refresh_ttl = ttl;
        self
    }

    /// Sets the lockout policy.
    #[must_use]
    pub fn with_lockout(mut self, max_failed_logins: i32, lockout: Duration) -> Self {
        self.max_failed_logins = max_failed_logins;
        self.lockout = lockout;
        self
    }

    /// Sets the refresh-token reuse grace window.
    #[must_use]
    pub fn with_reuse_grace(mut self, grace: Duration) -> Self {
        self.reuse_grace = grace;
        self
    }

    /// The signing secret. Crate-private so it cannot leak through the API.
    pub(crate) fn secret(&self) -> &[u8] {
        &self.secret
    }
}

fn env_i64(name: &str, default: i64) -> Result<i64> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(raw) => raw
            .trim()
            .parse::<i64>()
            .map_err(|_| Error::Config(format!("{name} must be an integer, got '{raw}'"))),
    }
}

fn env_duration(name: &str, default_secs: i64) -> Result<Duration> {
    let secs = env_i64(name, default_secs)?;
    if !(1..=MAX_SECS).contains(&secs) {
        return Err(Error::Config(format!(
            "{name} must be between 1 and {MAX_SECS} seconds"
        )));
    }
    Ok(Duration::seconds(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_secrets_are_rejected() {
        assert!(matches!(
            AuthConfig::new("too-short"),
            Err(Error::Config(_))
        ));
        assert!(AuthConfig::new("x".repeat(MIN_SECRET_LEN)).is_ok());
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let config = AuthConfig::new("a-very-secret-signing-key-of-32-bytes!").expect("config");
        let debug = format!("{config:?}");
        assert!(!debug.contains("a-very-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn defaults_are_sensible() {
        let config = AuthConfig::new("x".repeat(MIN_SECRET_LEN)).expect("config");
        assert_eq!(config.access_ttl, Duration::minutes(15));
        assert_eq!(config.refresh_ttl, Duration::days(14));
        assert_eq!(config.max_failed_logins, 5);
        assert_eq!(config.reuse_grace, Duration::seconds(10));
        assert_eq!(config.issuer, "rustmail");
    }
}
