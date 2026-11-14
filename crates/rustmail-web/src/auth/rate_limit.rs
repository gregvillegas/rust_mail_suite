//! Per-client-IP rate limiting for the credential endpoints.
//!
//! Built on `tower_governor` (a token bucket per key). The key is the client's
//! IP address, taken from the TCP peer by default. Behind a reverse proxy the
//! peer is the proxy, so every client would share one bucket; in that setup set
//! `TRUST_PROXY_HEADERS=true` to use the proxy's `X-Forwarded-For` instead.
//! Never enable that without a proxy that overwrites the header, or clients can
//! forge their IP and sidestep the limit.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::Router;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::{GovernorError, GovernorLayer};

use super::error::json_response;
use crate::config::RateLimitSettings;

/// Works out the client's IP address.
///
/// With `trust_proxy` the **rightmost** `X-Forwarded-For` entry is used, which
/// is the one appended by our own trusted proxy; entries to its left are
/// client-controlled and ignored. `X-Real-IP` is the fallback, then the TCP peer.
#[must_use]
pub fn client_ip(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    trust_proxy: bool,
) -> Option<IpAddr> {
    if trust_proxy {
        if let Some(ip) = forwarded_ip(headers) {
            return Some(ip);
        }
    }
    peer.map(|p| p.ip())
}

fn forwarded_ip(headers: &HeaderMap) -> Option<IpAddr> {
    let from_xff = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .and_then(|s| s.trim().parse().ok());
    from_xff.or_else(|| {
        headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse().ok())
    })
}

/// IPv6 clients usually own a whole /64, so keying on the full address would
/// let one machine mint unlimited buckets. Collapse to the /64 prefix.
fn bucket_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            let mut octets = v6.octets();
            octets[8..].fill(0);
            IpAddr::V6(Ipv6Addr::from(octets))
        }
    }
}

/// Rate-limit key: the client IP (IPv6 collapsed to its /64).
#[derive(Debug, Clone)]
pub struct ClientIpKeyExtractor {
    trust_proxy_headers: bool,
}

impl ClientIpKeyExtractor {
    /// Creates an extractor; see [`client_ip`] for what `trust_proxy_headers` does.
    #[must_use]
    pub fn new(trust_proxy_headers: bool) -> Self {
        Self {
            trust_proxy_headers,
        }
    }
}

impl KeyExtractor for ClientIpKeyExtractor {
    type Key = IpAddr;

    fn name(&self) -> &'static str {
        "client IP"
    }

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);
        client_ip(req.headers(), peer, self.trust_proxy_headers)
            .map(bucket_key)
            .ok_or(GovernorError::UnableToExtractKey)
    }

    fn key_name(&self, key: &Self::Key) -> Option<String> {
        Some(key.to_string())
    }
}

/// Turns limiter failures into JSON responses.
fn error_response(error: GovernorError) -> Response<Body> {
    match error {
        GovernorError::TooManyRequests { wait_time, .. } => {
            let mut response = json_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Too many attempts. Please wait and try again.",
            );
            if let Ok(value) = HeaderValue::from_str(&wait_time.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            response
        }
        other => {
            // Almost always: the server was started without connect info and
            // without trusted proxy headers, so there is no client IP to key on.
            tracing::error!(error = %other, "rate limiter could not identify the client");
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Internal server error.",
            )
        }
    }
}

/// Clears stale per-IP state from the limiters so memory stays bounded.
#[derive(Default)]
pub struct RateLimitJanitor {
    sweeps: Vec<Box<dyn Fn() + Send + Sync>>,
}

impl RateLimitJanitor {
    /// Drops limiter entries that have fully recovered.
    pub fn sweep(&self) {
        for sweep in &self.sweeps {
            sweep();
        }
    }

    /// Sweeps every `every` on a background task.
    pub fn spawn(self, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                self.sweep();
            }
        })
    }
}

/// Wraps `router` in a per-client-IP limiter and registers it for cleanup.
///
/// The middleware type is inferred here rather than named, which is why this is
/// a function that applies the layer instead of one that returns it.
///
/// # Errors
/// Fails if `settings` has a zero burst or period.
pub fn apply<S>(
    router: Router<S>,
    settings: RateLimitSettings,
    trust_proxy_headers: bool,
    janitor: &mut RateLimitJanitor,
) -> Result<Router<S>>
where
    S: Clone + Send + Sync + 'static,
{
    let config = GovernorConfigBuilder::default()
        .per_second(settings.period_secs)
        .burst_size(settings.burst)
        .key_extractor(ClientIpKeyExtractor::new(trust_proxy_headers))
        .finish()
        .ok_or_else(|| anyhow!("rate limit burst and period must be non-zero"))?;
    let config = Arc::new(config);

    let limiter = config.limiter().clone();
    janitor
        .sweeps
        .push(Box::new(move || limiter.retain_recent()));

    Ok(router.layer(GovernorLayer::new(config).error_handler(error_response)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).expect("header value"));
        }
        map
    }

    fn peer(addr: &str) -> Option<SocketAddr> {
        Some(addr.parse().expect("socket addr"))
    }

    #[test]
    fn peer_address_is_used_when_proxy_headers_are_not_trusted() {
        let h = headers(&[("x-forwarded-for", "9.9.9.9")]);
        assert_eq!(
            client_ip(&h, peer("10.0.0.5:4000"), false),
            Some("10.0.0.5".parse().expect("ip"))
        );
    }

    #[test]
    fn rightmost_forwarded_entry_wins_so_clients_cannot_spoof_it() {
        // The client sent "1.2.3.4"; our proxy appended the real address.
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 203.0.113.50")]);
        assert_eq!(
            client_ip(&h, peer("10.0.0.5:4000"), true),
            Some("203.0.113.50".parse().expect("ip"))
        );
    }

    #[test]
    fn falls_back_through_real_ip_then_peer() {
        let real = headers(&[("x-real-ip", "198.51.100.4")]);
        assert_eq!(
            client_ip(&real, peer("10.0.0.5:1"), true),
            Some("198.51.100.4".parse().expect("ip"))
        );
        let junk = headers(&[("x-forwarded-for", "not-an-ip")]);
        assert_eq!(
            client_ip(&junk, peer("10.0.0.5:1"), true),
            Some("10.0.0.5".parse().expect("ip"))
        );
        assert_eq!(client_ip(&HeaderMap::new(), None, true), None);
    }

    #[test]
    fn ipv6_clients_share_a_bucket_per_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa:bbbb:cccc:dddd".parse().expect("ip");
        let b: IpAddr = "2001:db8:1:2:1111:2222:3333:4444".parse().expect("ip");
        let other: IpAddr = "2001:db8:1:3::1".parse().expect("ip");
        assert_eq!(bucket_key(a), bucket_key(b));
        assert_ne!(bucket_key(a), bucket_key(other));
        let v4: IpAddr = "192.0.2.1".parse().expect("ip");
        assert_eq!(bucket_key(v4), v4);
    }

    #[test]
    fn zero_settings_are_rejected() {
        let mut janitor = RateLimitJanitor::default();
        let result = apply(
            Router::<()>::new(),
            RateLimitSettings {
                burst: 0,
                period_secs: 1,
            },
            false,
            &mut janitor,
        );
        assert!(result.is_err());
    }
}
