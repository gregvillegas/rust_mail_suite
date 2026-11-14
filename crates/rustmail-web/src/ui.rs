//! Page rendering: the per-request context, template structs and error pages.

use std::sync::Arc;

use askama::Template;
use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rustmail_core::domain::{User, UserRole};
use uuid::Uuid;

use crate::assets::Assets;
use crate::i18n::{Locale, Translator};

/// The signed-in user as templates see them.
#[derive(Debug, Clone)]
pub struct UserView {
    /// Account id.
    pub id: Uuid,
    /// Full address.
    pub email: String,
    /// Display name, or the part of the address before the `@` if none is set.
    pub name: String,
    /// Authorisation level.
    pub role: UserRole,
}

impl From<&User> for UserView {
    fn from(user: &User) -> Self {
        let name = if user.display_name.trim().is_empty() {
            user.local_part.clone()
        } else {
            user.display_name.clone()
        };
        Self {
            id: user.id,
            email: user.email.clone(),
            name,
            role: user.role,
        }
    }
}

/// Everything a layout needs, available to templates as `ctx`.
#[derive(Clone)]
pub struct PageCtx {
    locale: Locale,
    /// Asset URLs and the CDN/local choice.
    pub assets: Arc<Assets>,
    /// The signed-in user, if any.
    pub user: Option<UserView>,
    path: String,
}

impl PageCtx {
    /// Creates a context for a request to `path`.
    #[must_use]
    pub fn new(locale: Locale, assets: Arc<Assets>, user: Option<UserView>, path: &str) -> Self {
        Self {
            locale,
            assets,
            user,
            path: path.to_owned(),
        }
    }

    fn translator(&self) -> Translator {
        Translator::new(self.locale)
    }

    /// The message for `key` in the page's language.
    #[must_use]
    pub fn t(&self, key: &str) -> String {
        self.translator().t(key)
    }

    /// The message for `key` with one string argument.
    #[must_use]
    pub fn t_str(&self, key: &str, name: &'static str, value: &str) -> String {
        self.translator().with_str(key, name, value)
    }

    /// The message for `key` with one numeric argument. Takes a reference
    /// because templates borrow every expression they pass.
    #[must_use]
    pub fn t_num(&self, key: &str, name: &'static str, value: &i64) -> String {
        self.translator().with_num(key, name, *value)
    }

    /// The `<html lang>` value.
    #[must_use]
    pub fn lang(&self) -> &'static str {
        self.locale.code()
    }

    /// Languages offered in the language menu.
    #[must_use]
    pub fn locales(&self) -> &'static [Locale] {
        Locale::ALL
    }

    /// Whether `locale` is the page's language.
    #[must_use]
    pub fn is_locale(&self, locale: &Locale) -> bool {
        *locale == self.locale
    }

    /// Whether the signed-in user may see administration pages.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.user
            .as_ref()
            .is_some_and(|u| u.role.at_least(UserRole::DomainAdmin))
    }

    /// Whether the signed-in user is a super administrator.
    #[must_use]
    pub fn is_super_admin(&self) -> bool {
        self.user
            .as_ref()
            .is_some_and(|u| u.role == UserRole::SuperAdmin)
    }

    /// The signed-in user's role as a translated label.
    #[must_use]
    pub fn role_label(&self) -> String {
        let key = match self.user.as_ref().map(|u| u.role) {
            Some(UserRole::SuperAdmin) => "role-super-admin",
            Some(UserRole::DomainAdmin) => "role-domain-admin",
            _ => "role-user",
        };
        self.t(key)
    }

    /// Whether the current page is `prefix` or beneath it (`/` matches only itself).
    #[must_use]
    pub fn nav_active(&self, prefix: &str) -> bool {
        if prefix == "/" {
            self.path == "/"
        } else {
            self.path == prefix || self.path.starts_with(&format!("{prefix}/"))
        }
    }

    /// Whether the current page is exactly `path`.
    #[must_use]
    pub fn nav_exact(&self, path: &str) -> bool {
        self.path == path
    }

    /// The server version, shown in the footer.
    #[must_use]
    pub fn version(&self) -> &'static str {
        rustmail_core::version()
    }
}

/// Form state for the sign-in form.
#[derive(Debug, Clone, Default)]
pub struct LoginFormView {
    /// Pre-filled address (never the password).
    pub email: String,
    /// Where to go after signing in; always a safe local path.
    pub next: String,
    /// Translated error to show above the form.
    pub error: Option<String>,
}

/// One line of the recent sign-in table.
#[derive(Debug, Clone)]
pub struct ActivityRow {
    /// Human-readable time (UTC).
    pub when: String,
    /// Machine-readable time for the `<time>` element.
    pub iso: String,
    /// Whether the attempt succeeded.
    pub success: bool,
    /// Client IP address.
    pub address: String,
    /// Client user agent, shortened.
    pub device: String,
}

/// The sign-in page.
#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginPage {
    /// Layout context.
    pub ctx: PageCtx,
    /// Form state.
    pub form: LoginFormView,
}

/// The sign-in form on its own (the htmx swap target).
#[derive(Template)]
#[template(path = "partials/login_form.html")]
pub struct LoginFormPartial {
    /// Layout context.
    pub ctx: PageCtx,
    /// Form state.
    pub form: LoginFormView,
}

/// The user dashboard.
#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    /// Layout context.
    pub ctx: PageCtx,
}

/// The recent sign-in activity table.
#[derive(Template)]
#[template(path = "partials/activity.html")]
pub struct ActivityPartial {
    /// Layout context.
    pub ctx: PageCtx,
    /// Newest first.
    pub rows: Vec<ActivityRow>,
}

/// The administration landing page.
#[derive(Template)]
#[template(path = "admin/index.html")]
pub struct AdminIndexPage {
    /// Layout context.
    pub ctx: PageCtx,
}

/// The administration overview cards (refreshed periodically via htmx).
#[derive(Template)]
#[template(path = "partials/admin_overview.html")]
pub struct AdminOverviewPartial {
    /// Layout context.
    pub ctx: PageCtx,
    /// Whether to show the server-wide domain count (super administrators only).
    pub show_domains: bool,
    /// Number of hosted domains.
    pub domains: usize,
    /// Whether the database answered.
    pub db_ok: bool,
    /// Database round-trip time in milliseconds.
    pub db_ms: i64,
    /// When the figures were taken.
    pub refreshed: String,
}

/// A full-page error.
#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    /// Layout context.
    pub ctx: PageCtx,
    /// HTTP status code shown on the page.
    pub status: u16,
    /// Heading.
    pub title: String,
    /// Explanation.
    pub message: String,
}

/// The "restoring your session" page.
#[derive(Template)]
#[template(path = "renew.html")]
pub struct RenewPage {
    /// Layout context.
    pub ctx: PageCtx,
}

/// A dismissible error notification, swapped into the toast area.
#[derive(Template)]
#[template(path = "partials/toast.html")]
pub struct ToastPartial {
    /// Layout context.
    pub ctx: PageCtx,
    /// Already-translated text.
    pub message: String,
}

/// Renders a template, turning a template failure into a plain 500.
///
/// Template errors are programmer errors (the templates are checked at compile
/// time, so this is mostly a formatting failure); details go to the log only.
pub fn render<T: Template>(template: &T) -> Response {
    match template.render() {
        Ok(html) => html_response(StatusCode::OK, html),
        Err(error) => {
            tracing::error!(%error, "template rendering failed");
            PageError::Internal.into_response()
        }
    }
}

/// Like [`render`] but with an explicit status (e.g. 422 for a form error).
pub fn render_with_status<T: Template>(status: StatusCode, template: &T) -> Response {
    let mut response = render(template);
    if response.status() == StatusCode::OK {
        *response.status_mut() = status;
    }
    response
}

fn html_response(status: StatusCode, html: String) -> Response {
    let mut response = Response::new(Body::from(html));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

/// A failure that should be shown as an error page (or toast, for htmx).
///
/// The response itself is empty and carries a marker; the error middleware in
/// `app` turns it into a localised page, because only it has the request's
/// language and the application state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageError {
    /// 404.
    NotFound,
    /// 403.
    Forbidden,
    /// 500.
    Internal,
    /// 400: the request itself is wrong (bad folder name, bad address).
    BadRequest,
    /// 503: a service the page depends on, such as the mail server, is not reachable.
    Unavailable,
}

impl PageError {
    /// The HTTP status.
    #[must_use]
    pub fn status(self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// The numeric code used in message keys: `error-<code>-title` and `-body`.
    #[must_use]
    pub fn code(self) -> u16 {
        self.status().as_u16()
    }
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = self.status();
        response.extensions_mut().insert(self);
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetSource;

    fn ctx(path: &str, role: Option<UserRole>) -> PageCtx {
        let static_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static");
        let assets = Arc::new(Assets::load(&static_dir, AssetSource::Local).expect("assets"));
        let user = role.map(|role| UserView {
            id: Uuid::nil(),
            email: "a@example.com".into(),
            name: "A".into(),
            role,
        });
        PageCtx::new(Locale::En, assets, user, path)
    }

    #[test]
    fn navigation_highlighting() {
        let home = ctx("/", None);
        assert!(home.nav_active("/") && !home.nav_active("/admin"));
        let admin = ctx("/admin", None);
        assert!(admin.nav_active("/admin") && !admin.nav_active("/"));
        let deep = ctx("/admin/users", None);
        assert!(deep.nav_active("/admin") && !deep.nav_exact("/admin"));
        assert!(
            !ctx("/administrator", None).nav_active("/admin"),
            "prefix must end at a segment"
        );
    }

    #[test]
    fn admin_visibility_follows_the_role() {
        assert!(!ctx("/", None).is_admin());
        assert!(!ctx("/", Some(UserRole::User)).is_admin());
        assert!(ctx("/", Some(UserRole::DomainAdmin)).is_admin());
        assert!(!ctx("/", Some(UserRole::DomainAdmin)).is_super_admin());
        assert!(ctx("/", Some(UserRole::SuperAdmin)).is_super_admin());
    }

    #[test]
    fn user_view_falls_back_to_the_local_part() {
        let now = chrono::Utc::now();
        let mut user = User {
            id: Uuid::nil(),
            domain_id: Uuid::nil(),
            local_part: "alice".into(),
            email: "alice@example.com".into(),
            password_hash: String::new(),
            display_name: "  ".into(),
            role: UserRole::User,
            status: rustmail_core::domain::AccountStatus::Active,
            locale: "en".into(),
            last_login_at: None,
            failed_login_count: 0,
            locked_until: None,
            created_at: now,
            updated_at: now,
        };
        assert_eq!(UserView::from(&user).name, "alice");
        user.display_name = "Alice A.".into();
        assert_eq!(UserView::from(&user).name, "Alice A.");
    }

    #[test]
    fn page_errors_carry_status_and_a_marker() {
        for (error, code) in [
            (PageError::NotFound, 404),
            (PageError::Forbidden, 403),
            (PageError::Internal, 500),
        ] {
            let response = error.into_response();
            assert_eq!(response.status().as_u16(), code);
            assert_eq!(response.extensions().get::<PageError>(), Some(&error));
            assert_eq!(error.code(), code);
        }
    }
}
