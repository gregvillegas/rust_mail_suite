//! Pieces shared by the admin pages: the acting administrator, error handling,
//! and the short-lived store for bulk-import results.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rustmail_core::admin::Admin;
use rustmail_core::Error;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::ClientMeta;
use crate::pages::{AdminUser, WebUser};
use crate::ui::{render_with_status, PageError};

const HX_REDIRECT: HeaderName = HeaderName::from_static("hx-redirect");
/// How long an import result stays downloadable.
pub const IMPORT_RESULT_TTL: Duration = Duration::from_secs(600);
/// Import results kept at most, across all administrators.
const IMPORT_RESULT_LIMIT: usize = 50;

/// A signed-in administrator: the web session plus the scoped [`Admin`] the core
/// service checks everything against.
pub struct AdminCtx {
    /// The session.
    pub web: WebUser,
    /// The acting administrator.
    pub admin: Admin,
}

impl FromRequestParts<AppState> for AdminCtx {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let AdminUser(web) = AdminUser::from_request_parts(parts, state).await?;
        let ClientMeta(client) = match ClientMeta::from_request_parts(parts, state).await {
            Ok(meta) => meta,
            Err(never) => match never {},
        };
        let admin = Admin::from_user(&web.user, client.ip_address)
            .map_err(|_| PageError::Forbidden.into_response())?;
        Ok(Self { web, admin })
    }
}

#[derive(Template)]
#[template(path = "admin/_alert.html")]
struct AlertPartial {
    class: &'static str,
    text: String,
}

/// A bootstrap alert fragment for the `#form-status` area of a form.
#[must_use]
pub fn alert(status: StatusCode, class: &'static str, text: impl Into<String>) -> Response {
    render_with_status(
        status,
        &AlertPartial {
            class,
            text: text.into(),
        },
    )
}

/// `HX-Redirect` to `location` (which must be one of our own paths).
#[must_use]
pub fn redirect_to(location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(value) => (StatusCode::OK, [(HX_REDIRECT, value)]).into_response(),
        Err(_) => PageError::Internal.into_response(),
    }
}

/// Turns a service error into the response for a page (GET): the right error page.
#[must_use]
pub fn page_error(error: &Error) -> Response {
    match error {
        Error::NotFound { .. } => PageError::NotFound.into_response(),
        Error::Forbidden(_) => PageError::Forbidden.into_response(),
        Error::Validation(_) | Error::Conflict(_) => PageError::BadRequest.into_response(),
        other => {
            tracing::error!(error = %other, "admin page failed");
            PageError::Internal.into_response()
        }
    }
}

/// Turns a service error into the response for a form (POST): an explanation next
/// to the form when the administrator can fix it, an error page otherwise.
#[must_use]
pub fn form_error(error: &Error) -> Response {
    match error {
        Error::Validation(text)
        | Error::Conflict(text)
        | Error::Provisioning(text)
        | Error::Forbidden(text) => alert(StatusCode::UNPROCESSABLE_ENTITY, "danger", text.clone()),
        Error::NotFound { .. } => PageError::NotFound.into_response(),
        other => {
            tracing::error!(error = %other, "admin action failed");
            PageError::Internal.into_response()
        }
    }
}

/// Parses a number of megabytes from a form field into bytes. Empty = `None`.
///
/// # Errors
/// [`Error::Validation`] for anything that is not a whole number in range.
pub fn parse_mb(text: &str) -> Result<Option<i64>, Error> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let mb: i64 = text
        .parse()
        .map_err(|_| Error::Validation("the quota must be a whole number of megabytes".into()))?;
    if !(0..=rustmail_core::admin::import::MAX_QUOTA_MB).contains(&mb) {
        return Err(Error::Validation("the quota is out of range".into()));
    }
    Ok(Some(mb * 1024 * 1024))
}

/// Human-readable size, such as `3.4 MB`.
#[must_use]
pub fn fmt_bytes(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b < 1024.0 {
        format!("{} B", bytes.max(0))
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", b / (1024.0 * 1024.0 * 1024.0))
    }
}

/// A quota for display: `Unlimited` text is supplied by the caller's translation.
#[must_use]
pub fn mb_field(bytes: Option<i64>) -> String {
    bytes
        .map(|b| (b / (1024 * 1024)).to_string())
        .unwrap_or_default()
}

/// Import results held briefly so the administrator can download them once.
///
/// Passwords are in these files, so entries belong to one administrator, expire
/// quickly, and are removed when downloaded.
#[derive(Default)]
pub struct ImportResults {
    entries: Mutex<HashMap<String, (Uuid, Instant, String)>>,
}

impl ImportResults {
    /// Stores `csv` for `owner` and returns the token that fetches it.
    pub fn put(&self, owner: Uuid, csv: String) -> String {
        // 256 bits from the operating system's CSPRNG (two 128-bit passwords).
        let token = format!(
            "{}{}",
            rustmail_core::password::generate_password(),
            rustmail_core::password::generate_password()
        );
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        entries.retain(|_, (_, at, _)| now.duration_since(*at) < IMPORT_RESULT_TTL);
        while entries.len() >= IMPORT_RESULT_LIMIT {
            let oldest = entries
                .iter()
                .min_by_key(|(_, (_, at, _))| *at)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(key) => entries.remove(&key),
                None => break,
            };
        }
        entries.insert(token.clone(), (owner, now, csv));
        token
    }

    /// Takes the file for `token` if it belongs to `owner` and has not expired.
    pub fn take(&self, owner: Uuid, token: &str) -> Option<String> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let (stored_owner, at, _) = entries.get(token)?;
        if *stored_owner != owner || Instant::now().duration_since(*at) >= IMPORT_RESULT_TTL {
            return None;
        }
        entries.remove(token).map(|(_, _, csv)| csv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn megabytes_are_parsed_strictly() {
        assert_eq!(parse_mb("").unwrap(), None);
        assert_eq!(parse_mb(" 5 ").unwrap(), Some(5 * 1024 * 1024));
        assert_eq!(parse_mb("0").unwrap(), Some(0));
        for bad in ["-1", "1.5", "abc", "99999999999", "1e3"] {
            assert!(parse_mb(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
        assert_eq!(fmt_bytes(-5), "0 B");
        assert_eq!(mb_field(Some(2 * 1024 * 1024)), "2");
        assert_eq!(mb_field(None), "");
    }

    #[test]
    fn import_results_are_private_single_use_and_bounded() {
        let store = ImportResults::default();
        let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
        let token = store.put(alice, "secret,csv".into());
        assert_eq!(token.len(), 64);
        assert_eq!(
            store.take(bob, &token),
            None,
            "another administrator cannot fetch it"
        );
        assert_eq!(store.take(alice, "nope"), None);
        assert_eq!(store.take(alice, &token).as_deref(), Some("secret,csv"));
        assert_eq!(store.take(alice, &token), None, "it can be downloaded once");
        for i in 0..(IMPORT_RESULT_LIMIT + 10) {
            store.put(alice, format!("{i}"));
        }
        assert!(store.entries.lock().unwrap().len() <= IMPORT_RESULT_LIMIT);
    }
}
