//! Cookie names and construction.
//!
//! | Cookie | Contents | HttpOnly | SameSite | Path |
//! |--------|----------|----------|----------|------|
//! | `rm_access` | signed access JWT | yes | Lax | `/` |
//! | `rm_refresh` | opaque refresh token | yes | Strict | `/auth` |
//! | `rm_csrf` | signed CSRF token | **no** (scripts must echo it) | Lax | `/` |
//!
//! The refresh cookie is scoped to `/auth` so it is only ever sent to the
//! endpoints that need it. The access cookie lives as long as the session (its
//! JWT inside expires sooner) so the server can still identify the session of a
//! request that arrives just after the JWT lapses, which lets it refresh.

use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::{DateTime, Utc};

/// Name of the access-token cookie.
pub const ACCESS_COOKIE: &str = "rm_access";
/// Name of the refresh-token cookie.
pub const REFRESH_COOKIE: &str = "rm_refresh";
/// Name of the CSRF cookie.
pub const CSRF_COOKIE: &str = "rm_csrf";
/// Header clients must echo the CSRF token in.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// Path the refresh cookie is restricted to.
pub const REFRESH_COOKIE_PATH: &str = "/auth";

fn lifetime(until: DateTime<Utc>) -> time::Duration {
    time::Duration::seconds((until - Utc::now()).num_seconds().max(1))
}

/// Builds the access-token cookie, living until `session_expires_at`.
#[must_use]
pub fn access_cookie(
    token: String,
    session_expires_at: DateTime<Utc>,
    secure: bool,
) -> Cookie<'static> {
    Cookie::build((ACCESS_COOKIE, token))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(lifetime(session_expires_at))
        .build()
}

/// Builds the refresh-token cookie, living until `session_expires_at`.
#[must_use]
pub fn refresh_cookie(
    token: String,
    session_expires_at: DateTime<Utc>,
    secure: bool,
) -> Cookie<'static> {
    Cookie::build((REFRESH_COOKIE, token))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Strict)
        .path(REFRESH_COOKIE_PATH)
        .max_age(lifetime(session_expires_at))
        .build()
}

/// Builds the CSRF cookie. Readable by scripts by design; it is signed, and
/// stealing it requires XSS, which already defeats CSRF protection.
#[must_use]
pub fn csrf_cookie(
    token: String,
    session_expires_at: DateTime<Utc>,
    secure: bool,
) -> Cookie<'static> {
    Cookie::build((CSRF_COOKIE, token))
        .http_only(false)
        .secure(secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(lifetime(session_expires_at))
        .build()
}

/// A cookie that tells the browser to delete `name`. Attributes (notably the
/// path) must match those it was set with.
#[must_use]
pub fn expired_cookie(name: &'static str, path: &'static str, secure: bool) -> Cookie<'static> {
    Cookie::build((name, ""))
        .http_only(name != CSRF_COOKIE)
        .secure(secure)
        .same_site(if name == REFRESH_COOKIE {
            SameSite::Strict
        } else {
            SameSite::Lax
        })
        .path(path)
        .max_age(time::Duration::ZERO)
        .build()
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn in_one_hour() -> DateTime<Utc> {
        Utc::now() + Duration::hours(1)
    }

    #[test]
    fn access_cookie_is_hardened() {
        let c = access_cookie("jwt".into(), in_one_hour(), true);
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.secure(), Some(true));
        assert_eq!(c.same_site(), Some(SameSite::Lax));
        assert_eq!(c.path(), Some("/"));
        assert!(c.max_age().is_some_and(|a| a.whole_seconds() > 3500));
    }

    #[test]
    fn refresh_cookie_is_strict_and_path_scoped() {
        let c = refresh_cookie("rt".into(), in_one_hour(), true);
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.same_site(), Some(SameSite::Strict));
        assert_eq!(c.path(), Some("/auth"));
    }

    #[test]
    fn csrf_cookie_is_script_readable() {
        let c = csrf_cookie("t".into(), in_one_hour(), true);
        assert_eq!(c.http_only(), Some(false));
        assert_eq!(c.secure(), Some(true));
    }

    #[test]
    fn secure_flag_is_configurable_for_local_http() {
        assert_eq!(
            access_cookie("x".into(), in_one_hour(), false).secure(),
            Some(false)
        );
    }

    #[test]
    fn expired_cookies_clear_immediately_and_keep_their_path() {
        let c = expired_cookie(REFRESH_COOKIE, REFRESH_COOKIE_PATH, true);
        assert_eq!(c.value(), "");
        assert_eq!(c.path(), Some("/auth"));
        assert_eq!(c.max_age().map(|a| a.whole_seconds()), Some(0));
    }

    #[test]
    fn already_lapsed_sessions_still_get_a_positive_lifetime() {
        let c = access_cookie("x".into(), Utc::now() - Duration::hours(1), true);
        assert_eq!(c.max_age().map(|a| a.whole_seconds()), Some(1));
    }
}
