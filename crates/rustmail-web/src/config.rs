//! Web server settings, loaded from the environment.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

use crate::assets::AssetSource;

/// A token-bucket rate limit: `burst` requests at once, refilling one every
/// `period_secs` seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitSettings {
    /// Requests allowed back-to-back.
    pub burst: u32,
    /// Seconds to earn back one request.
    pub period_secs: u64,
}

/// Everything the web server needs from its environment.
#[derive(Clone)]
pub struct WebConfig {
    /// Address to listen on.
    pub bind_addr: SocketAddr,
    /// PostgreSQL connection URL. Contains credentials, so it is never logged.
    pub database_url: String,
    /// Mark cookies `Secure`. Keep `true` outside local HTTP development.
    pub cookie_secure: bool,
    /// Take the client IP from `X-Forwarded-For` / `X-Real-IP`. Only enable when
    /// exactly one trusted reverse proxy sits in front and sets those headers;
    /// otherwise clients can spoof their IP and dodge rate limiting.
    pub trust_proxy_headers: bool,
    /// Limit on `POST /auth/login`, per client IP.
    pub login_rate: RateLimitSettings,
    /// Limit on `POST /auth/refresh`, per client IP.
    pub refresh_rate: RateLimitSettings,
    /// Directory served under `/static`. Must be deployed alongside the binary.
    pub static_dir: PathBuf,
    /// Where Bootstrap, htmx and Alpine are loaded from.
    pub asset_source: AssetSource,
}

impl fmt::Debug for WebConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebConfig")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .field("cookie_secure", &self.cookie_secure)
            .field("trust_proxy_headers", &self.trust_proxy_headers)
            .field("login_rate", &self.login_rate)
            .field("refresh_rate", &self.refresh_rate)
            .field("static_dir", &self.static_dir)
            .field("asset_source", &self.asset_source)
            .finish()
    }
}

impl WebConfig {
    /// Loads settings from the environment.
    ///
    /// | Variable | Default |
    /// |----------|---------|
    /// | `DATABASE_URL` | required |
    /// | `WEB_BIND_ADDR` | `127.0.0.1:8080` |
    /// | `COOKIE_SECURE` | `true` |
    /// | `TRUST_PROXY_HEADERS` | `false` |
    /// | `LOGIN_RATE_BURST` / `LOGIN_RATE_PERIOD_SECS` | 5 / 12 |
    /// | `REFRESH_RATE_BURST` / `REFRESH_RATE_PERIOD_SECS` | 20 / 3 |
    /// | `STATIC_DIR` | the crate's own `static/` directory (set it when deploying) |
    /// | `ASSET_SOURCE` | `cdn` (`local` for air-gapped or privacy-sensitive installs) |
    ///
    /// # Errors
    /// Fails if `DATABASE_URL` is missing or a value cannot be parsed.
    pub fn from_env() -> Result<Self> {
        let database_url =
            std::env::var("DATABASE_URL").map_err(|_| anyhow!("DATABASE_URL is not set"))?;
        let bind_addr = std::env::var("WEB_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
            .parse()
            .context("WEB_BIND_ADDR must look like 127.0.0.1:8080")?;
        Ok(Self {
            bind_addr,
            database_url,
            cookie_secure: env_bool("COOKIE_SECURE", true)?,
            trust_proxy_headers: env_bool("TRUST_PROXY_HEADERS", false)?,
            login_rate: rate("LOGIN_RATE", 5, 12)?,
            refresh_rate: rate("REFRESH_RATE", 20, 3)?,
            static_dir: std::env::var_os("STATIC_DIR").map_or_else(
                || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/static")),
                PathBuf::from,
            ),
            asset_source: AssetSource::parse(
                &std::env::var("ASSET_SOURCE").unwrap_or_else(|_| "cdn".to_owned()),
            )
            .map_err(|e| anyhow!(e))?,
        })
    }
}

fn rate(prefix: &str, burst: u32, period_secs: u64) -> Result<RateLimitSettings> {
    let burst = env_num(&format!("{prefix}_BURST"), u64::from(burst))?;
    let period_secs = env_num(&format!("{prefix}_PERIOD_SECS"), period_secs)?;
    if burst == 0 || period_secs == 0 {
        return Err(anyhow!(
            "{prefix}_BURST and {prefix}_PERIOD_SECS must be at least 1"
        ));
    }
    Ok(RateLimitSettings {
        burst: u32::try_from(burst).context("rate-limit burst is too large")?,
        period_secs,
    })
}

fn env_num(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(raw) => raw
            .trim()
            .parse()
            .with_context(|| format!("{name} must be a number, got '{raw}'")),
    }
}

fn env_bool(name: &str, default: bool) -> Result<bool> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(anyhow!("{name} must be true or false, got '{raw}'")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_database_url() {
        let config = WebConfig {
            bind_addr: "127.0.0.1:8080".parse().expect("addr"),
            database_url: "postgres://user:hunter2@db/rustmail".into(),
            cookie_secure: true,
            trust_proxy_headers: false,
            login_rate: RateLimitSettings {
                burst: 5,
                period_secs: 12,
            },
            refresh_rate: RateLimitSettings {
                burst: 20,
                period_secs: 3,
            },
            static_dir: PathBuf::from("static"),
            asset_source: AssetSource::Cdn,
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2") && debug.contains("<redacted>"));
    }

    #[test]
    fn zero_rate_limits_are_rejected() {
        std::env::set_var("RMTEST_ZERO_BURST", "0");
        assert!(rate("RMTEST_ZERO", 5, 12).is_err());
        std::env::remove_var("RMTEST_ZERO_BURST");
        assert_eq!(
            rate("RMTEST_UNSET", 5, 12).expect("defaults"),
            RateLimitSettings {
                burst: 5,
                period_secs: 12
            }
        );
    }
}
