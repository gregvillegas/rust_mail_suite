//! The quarantine pages (Phase 5): list, inspect, release and delete.
//!
//! | Route | Method | Access | Purpose |
//! |---|---|---|---|
//! | `/admin/quarantine` | GET | super admin | filtered, paged list |
//! | `/admin/quarantine/{id}` | GET | super admin | headers and an excerpt |
//! | `/admin/quarantine/{id}/release` | POST | super admin + CSRF | deliver to the recipient |
//! | `/admin/quarantine/{id}/delete` | POST | super admin + CSRF | delete |
//!
//! Only super administrators may use them: until domain-admin scoping exists
//! (Phase 7), a domain administrator could otherwise read other tenants' mail.

use std::sync::Arc;

use askama::Template;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use rustmail_core::auth::ClientInfo;
use rustmail_core::domain::UserRole;
use rustmail_core::quarantine::{
    Actor, MessagePreview, QuarantineCounts, QuarantineFilter, QuarantineItem, QuarantineKind,
    QuarantineService, QuarantineStatus,
};
use rustmail_core::Error;
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::csrf::ensure_cookie;
use crate::auth::ClientMeta;
use crate::pages::{encode_query_value, page_ctx, WebUser};
use crate::ui::{render, PageCtx, PageError};

const PAGE_SIZE: i64 = 25;
const MAX_SEARCH_CHARS: usize = 200;
const HX_REDIRECT: HeaderName = HeaderName::from_static("hx-redirect");

/// A signed-in super administrator.
pub struct SuperAdmin(pub WebUser);

impl FromRequestParts<AppState> for SuperAdmin {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let web = WebUser::from_request_parts(parts, state).await?;
        // Authorise on the freshly loaded role, not the one baked into the token.
        if web.user.role == UserRole::SuperAdmin {
            Ok(Self(web))
        } else {
            Err(PageError::Forbidden.into_response())
        }
    }
}

/// A line above the list reporting what just happened.
struct Notice {
    class: &'static str,
    key: &'static str,
}

/// A choice in a `<select>`.
struct Choice {
    value: &'static str,
    label: &'static str,
    selected: bool,
}

struct Row {
    id: Uuid,
    iso: String,
    when: String,
    badge: &'static str,
    kind_key: &'static str,
    status_key: &'static str,
    reason: String,
    sender: String,
    recipient: String,
    size: String,
}

struct ItemView {
    id: Uuid,
    iso: String,
    when: String,
    badge: &'static str,
    kind_key: &'static str,
    status_key: &'static str,
    confirm_release_key: &'static str,
    reason: String,
    sender: String,
    recipient: String,
    size: String,
    held: bool,
    resolved: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/quarantine.html")]
struct QuarantineListPage {
    ctx: PageCtx,
    configured: bool,
    notice: Option<Notice>,
    counts: QuarantineCounts,
    status_options: Vec<Choice>,
    kind_options: Vec<Choice>,
    search: String,
    total: i64,
    rows: Vec<Row>,
    page: i64,
    prev_url: Option<String>,
    next_url: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/quarantine_item.html")]
struct QuarantineItemPage {
    ctx: PageCtx,
    item: ItemView,
    preview: Option<MessagePreview>,
}

fn kind_key(kind: QuarantineKind) -> &'static str {
    match kind {
        QuarantineKind::Virus => "quarantine-kind-virus",
        QuarantineKind::Spam => "quarantine-kind-spam",
        QuarantineKind::Banned => "quarantine-kind-banned",
        QuarantineKind::BadHeader => "quarantine-kind-bad-header",
        QuarantineKind::Other => "quarantine-kind-other",
    }
}

fn badge(kind: QuarantineKind) -> &'static str {
    match kind {
        QuarantineKind::Virus => "danger",
        QuarantineKind::Spam => "warning",
        QuarantineKind::Banned | QuarantineKind::BadHeader => "secondary",
        QuarantineKind::Other => "light",
    }
}

fn status_key(status: QuarantineStatus) -> &'static str {
    match status {
        QuarantineStatus::Held => "quarantine-status-held",
        QuarantineStatus::Released => "quarantine-status-released",
        QuarantineStatus::Deleted => "quarantine-status-deleted",
    }
}

/// Human-readable size such as `3.4 KB`.
fn human_size(bytes: Option<i64>) -> String {
    let Some(bytes) = bytes else {
        return "–".to_owned();
    };
    let bytes = u64::try_from(bytes).unwrap_or(0);
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let (value, unit) = if bytes < 1024 * 1024 {
        (bytes as f64 / 1024.0, "KB")
    } else {
        (bytes as f64 / (1024.0 * 1024.0), "MB")
    };
    format!("{value:.1} {unit}")
}

fn when(item: &QuarantineItem) -> (String, String) {
    (
        item.received_at.to_rfc3339(),
        item.received_at.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

fn row_of(item: &QuarantineItem) -> Row {
    let (iso, when) = when(item);
    Row {
        id: item.id,
        iso,
        when,
        badge: badge(item.kind),
        kind_key: kind_key(item.kind),
        status_key: status_key(item.status),
        reason: item.reason.clone(),
        sender: if item.sender.is_empty() {
            "<>".to_owned()
        } else {
            item.sender.clone()
        },
        recipient: item.recipient.clone(),
        size: human_size(item.size_bytes),
    }
}

fn item_view(item: &QuarantineItem) -> ItemView {
    let (iso, when) = when(item);
    ItemView {
        id: item.id,
        iso,
        when,
        badge: badge(item.kind),
        kind_key: kind_key(item.kind),
        status_key: status_key(item.status),
        confirm_release_key: if item.kind == QuarantineKind::Virus {
            "quarantine-confirm-release-virus"
        } else {
            "quarantine-confirm-release"
        },
        reason: item.reason.clone(),
        sender: if item.sender.is_empty() {
            "<>".to_owned()
        } else {
            item.sender.clone()
        },
        recipient: item.recipient.clone(),
        size: human_size(item.size_bytes),
        held: item.status == QuarantineStatus::Held,
        resolved: item
            .resolved_at
            .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()),
    }
}

/// Query parameters of the list page.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    page: Option<i64>,
    done: Option<String>,
}

/// The state selected by `?status=`: `Some(None)` means "any".
fn parse_status(value: Option<&str>) -> Option<QuarantineStatus> {
    match value {
        None | Some("") => Some(QuarantineStatus::Held),
        Some("any") => None,
        Some(other) => QuarantineStatus::parse(other).or(Some(QuarantineStatus::Held)),
    }
}

fn parse_kind(value: Option<&str>) -> Option<QuarantineKind> {
    value
        .filter(|v| !v.is_empty())
        .and_then(QuarantineKind::parse)
}

fn notice_for(done: Option<&str>) -> Option<Notice> {
    Some(match done? {
        "released" => Notice {
            class: "success",
            key: "quarantine-notice-released",
        },
        "deleted" => Notice {
            class: "success",
            key: "quarantine-notice-deleted",
        },
        "failed" => Notice {
            class: "danger",
            key: "quarantine-notice-failed",
        },
        "gone" => Notice {
            class: "warning",
            key: "quarantine-notice-gone",
        },
        _ => return None,
    })
}

fn list_url(status: &str, kind: &str, search: &str, page: i64) -> String {
    format!(
        "/admin/quarantine?status={}&kind={}&q={}&page={page}",
        encode_query_value(status),
        encode_query_value(kind),
        encode_query_value(search)
    )
}

fn service(state: &AppState) -> Option<Arc<QuarantineService>> {
    state.quarantine.clone()
}

pub(crate) async fn list(
    State(state): State<AppState>,
    admin: SuperAdmin,
    jar: CookieJar,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let ctx = page_ctx(
        &state,
        &jar,
        &headers,
        Some(&admin.0.user),
        "/admin/quarantine",
    );
    let jar = ensure_cookie(&state.auth, jar);
    let Some(service) = service(&state) else {
        return (
            jar,
            render(&QuarantineListPage {
                ctx,
                configured: false,
                notice: None,
                counts: QuarantineCounts::default(),
                status_options: Vec::new(),
                kind_options: Vec::new(),
                search: String::new(),
                total: 0,
                rows: Vec::new(),
                page: 1,
                prev_url: None,
                next_url: None,
            }),
        )
            .into_response();
    };

    let status = parse_status(query.status.as_deref());
    let kind = parse_kind(query.kind.as_deref());
    let search: String = query
        .q
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(MAX_SEARCH_CHARS)
        .collect();
    let page = query.page.unwrap_or(1).clamp(1, 100_000);
    let filter = QuarantineFilter {
        status,
        kind,
        search: Some(search.clone()),
        limit: PAGE_SIZE,
        offset: (page - 1) * PAGE_SIZE,
    };
    let (listing, counts) = match tokio::try_join!(service.list(&filter), service.counts()) {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%error, "could not load the quarantine");
            return PageError::Internal.into_response();
        }
    };

    let status_value = status.map_or("any", QuarantineStatus::as_str);
    let kind_value = kind.map_or("", QuarantineKind::as_str);
    let choice = |value: &'static str, label: &'static str, current: &str| Choice {
        value,
        label,
        selected: value == current,
    };
    let status_options = vec![
        choice("held", "quarantine-status-held", status_value),
        choice("released", "quarantine-status-released", status_value),
        choice("deleted", "quarantine-status-deleted", status_value),
        choice("any", "quarantine-status-all", status_value),
    ];
    let kind_options = vec![
        choice("", "quarantine-kind-all", kind_value),
        choice("virus", "quarantine-kind-virus", kind_value),
        choice("spam", "quarantine-kind-spam", kind_value),
        choice("banned", "quarantine-kind-banned", kind_value),
        choice("bad-header", "quarantine-kind-bad-header", kind_value),
        choice("other", "quarantine-kind-other", kind_value),
    ];
    let has_next = page * PAGE_SIZE < listing.total;
    let page_template = QuarantineListPage {
        ctx,
        configured: true,
        notice: notice_for(query.done.as_deref()),
        counts,
        status_options,
        kind_options,
        total: listing.total,
        rows: listing.items.iter().map(row_of).collect(),
        page,
        prev_url: (page > 1).then(|| list_url(status_value, kind_value, &search, page - 1)),
        next_url: has_next.then(|| list_url(status_value, kind_value, &search, page + 1)),
        search,
    };
    (jar, render(&page_template)).into_response()
}

pub(crate) async fn detail(
    State(state): State<AppState>,
    admin: SuperAdmin,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::NotFound.into_response();
    };
    let item = match service.get(id).await {
        Ok(item) => item,
        Err(Error::NotFound { .. }) => return PageError::NotFound.into_response(),
        Err(error) => {
            tracing::error!(%error, "could not load a quarantine item");
            return PageError::Internal.into_response();
        }
    };
    // A deleted message has no file any more; only held and released ones can be read.
    let preview = if item.status == QuarantineStatus::Deleted {
        None
    } else {
        match service.preview(id).await {
            Ok(preview) => Some(preview),
            Err(error) => {
                tracing::warn!(%error, "could not read a quarantined message");
                None
            }
        }
    };
    let ctx = page_ctx(
        &state,
        &jar,
        &headers,
        Some(&admin.0.user),
        "/admin/quarantine",
    );
    let jar = ensure_cookie(&state.auth, jar);
    (
        jar,
        render(&QuarantineItemPage {
            ctx,
            item: item_view(&item),
            preview,
        }),
    )
        .into_response()
}

fn back(done: &'static str) -> Response {
    let location = match done {
        "released" => "/admin/quarantine?done=released",
        "deleted" => "/admin/quarantine?done=deleted",
        "gone" => "/admin/quarantine?done=gone",
        _ => "/admin/quarantine?done=failed",
    };
    (
        StatusCode::OK,
        [(HX_REDIRECT, HeaderValue::from_static(location))],
    )
        .into_response()
}

fn actor(admin: &SuperAdmin, client: ClientInfo) -> Actor {
    Actor {
        user_id: admin.0.user.id,
        email: admin.0.user.email.clone(),
        ip_address: client.ip_address,
    }
}

pub(crate) async fn release(
    State(state): State<AppState>,
    admin: SuperAdmin,
    ClientMeta(client): ClientMeta,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::NotFound.into_response();
    };
    match service.release(id, &actor(&admin, client)).await {
        Ok(_) => back("released"),
        Err(Error::NotFound { .. } | Error::Conflict(_)) => back("gone"),
        Err(error) => {
            tracing::error!(%error, "releasing a quarantined message failed");
            back("failed")
        }
    }
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    admin: SuperAdmin,
    ClientMeta(client): ClientMeta,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::NotFound.into_response();
    };
    match service.delete(id, &actor(&admin, client)).await {
        Ok(()) => back("deleted"),
        Err(Error::NotFound { .. } | Error::Conflict(_)) => back("gone"),
        Err(error) => {
            tracing::error!(%error, "deleting a quarantined message failed");
            back("failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human_size(None), "–");
        assert_eq!(human_size(Some(0)), "0 B");
        assert_eq!(human_size(Some(1023)), "1023 B");
        assert_eq!(human_size(Some(1536)), "1.5 KB");
        assert_eq!(human_size(Some(5 * 1024 * 1024)), "5.0 MB");
        assert_eq!(human_size(Some(-5)), "0 B");
    }

    #[test]
    fn filters_fall_back_to_safe_defaults() {
        assert_eq!(parse_status(None), Some(QuarantineStatus::Held));
        assert_eq!(parse_status(Some("")), Some(QuarantineStatus::Held));
        assert_eq!(parse_status(Some("any")), None);
        assert_eq!(
            parse_status(Some("deleted")),
            Some(QuarantineStatus::Deleted)
        );
        assert_eq!(parse_status(Some("garbage")), Some(QuarantineStatus::Held));
        assert_eq!(parse_kind(Some("virus")), Some(QuarantineKind::Virus));
        assert_eq!(parse_kind(Some("")), None);
        assert_eq!(parse_kind(Some("x")), None);
    }

    #[test]
    fn only_known_notices_are_shown() {
        assert!(notice_for(Some("released")).is_some());
        assert!(notice_for(Some("<script>")).is_none());
        assert!(notice_for(None).is_none());
    }

    #[test]
    fn pagination_links_encode_the_search() {
        assert_eq!(
            list_url("held", "", "a&b c", 2),
            "/admin/quarantine?status=held&kind=&q=a%26b%20c&page=2"
        );
    }
}
