//! Admin console: domains, users, aliases and groups (Phase 7).
//!
//! All decisions about who may do what live in [`rustmail_core::admin`]; these
//! handlers pass the signed-in administrator in and show the outcome. Forms post
//! with htmx: a problem is shown next to the form (`#form-status`), success
//! redirects to a page that confirms it with a fixed, translated notice.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use rustmail_core::admin::domains::{DomainRow, DomainUpdate};
use rustmail_core::admin::routing::{AliasRow, GroupDetail, GroupRow};
use rustmail_core::admin::users::{NewAccount, UserDetail, UserFilter, UserRow, UserUpdate};
use rustmail_core::domain::{AccountStatus, UserRole};
use rustmail_core::Error;
use serde::Deserialize;
use uuid::Uuid;

use crate::admin_common::{
    alert, fmt_bytes, form_error, mb_field, page_error, parse_mb, redirect_to, AdminCtx,
};
use crate::app::AppState;
use crate::auth::csrf::ensure_cookie;
use crate::pages::{encode_query_value, page_ctx};
use crate::ui::{render, PageCtx};

const PAGE_SIZE: i64 = 25;

fn when(t: Option<DateTime<Utc>>) -> String {
    t.map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M UTC").to_string())
}

fn status_class(s: AccountStatus) -> &'static str {
    match s {
        AccountStatus::Active => "success",
        AccountStatus::Suspended => "warning",
        AccountStatus::Disabled => "secondary",
    }
}

fn enc(s: &str) -> String {
    encode_query_value(s)
}

/// A line the user sees after a successful action, chosen from a fixed list.
fn notice(done: Option<&str>) -> Option<(&'static str, &'static str)> {
    Some(match done? {
        "created" => ("success", "admin-notice-created"),
        "saved" => ("success", "admin-notice-saved"),
        "deleted" => ("success", "admin-notice-deleted"),
        "status" => ("success", "admin-notice-status"),
        "unlocked" => ("success", "admin-notice-unlocked"),
        "signedout" => ("success", "admin-notice-signedout"),
        _ => return None,
    })
}

struct Option2 {
    value: String,
    label: String,
    selected: bool,
}

fn choice(value: impl Into<String>, label: impl Into<String>, current: &str) -> Option2 {
    let value = value.into();
    Option2 {
        selected: value == current,
        value,
        label: label.into(),
    }
}

fn role_key(role: UserRole) -> String {
    format!("admin-role-{}", role.as_str())
}

fn status_key(status: AccountStatus) -> String {
    format!("admin-status-{}", status.as_str())
}

// ─── domains ────────────────────────────────────────────────────────────────

struct DomainLine {
    id: Uuid,
    name: String,
    status_key: String,
    status_class: &'static str,
    users: i64,
    aliases: i64,
    groups: i64,
    used: String,
    quota: String,
    max: String,
}

fn domain_line(ctx: &PageCtx, d: &DomainRow) -> DomainLine {
    DomainLine {
        id: d.id,
        name: d.name.clone(),
        status_key: status_key(d.status),
        status_class: status_class(d.status),
        users: d.users,
        aliases: d.aliases,
        groups: d.groups,
        used: fmt_bytes(d.bytes_used),
        quota: d
            .default_quota_bytes
            .filter(|q| *q > 0)
            .map_or_else(|| ctx.t("admin-unlimited"), fmt_bytes),
        max: d
            .max_mailboxes
            .map_or_else(|| ctx.t("admin-unlimited"), |m| m.to_string()),
    }
}

#[derive(Template)]
#[template(path = "admin/domains.html")]
struct DomainsPage {
    ctx: PageCtx,
    rows: Vec<DomainLine>,
    can_create: bool,
    notice: Option<(&'static str, &'static str)>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DoneQuery {
    done: Option<String>,
}

pub(crate) async fn domains(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/domains");
    let jar = ensure_cookie(&state.auth, jar);
    match state.admin.list_domains(&a.admin).await {
        Ok(rows) => (
            jar,
            render(&DomainsPage {
                rows: rows.iter().map(|d| domain_line(&ctx, d)).collect(),
                can_create: a.admin.is_super(),
                notice: notice(q.done.as_deref()),
                ctx,
            }),
        )
            .into_response(),
        Err(e) => page_error(&e),
    }
}

#[derive(Template)]
#[template(path = "admin/domain.html")]
struct DomainPageT {
    ctx: PageCtx,
    d: DomainLine,
    status_options: Vec<Option2>,
    quota_mb: String,
    max_mailboxes: String,
    can_edit: bool,
    notice: Option<(&'static str, &'static str)>,
}

pub(crate) async fn domain(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<DoneQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/domains");
    let jar = ensure_cookie(&state.auth, jar);
    match state.admin.get_domain(&a.admin, id).await {
        Ok(d) => {
            let current = d.status.as_str();
            let status_options = [
                AccountStatus::Active,
                AccountStatus::Suspended,
                AccountStatus::Disabled,
            ]
            .into_iter()
            .map(|s| choice(s.as_str(), ctx.t(&status_key(s)), current))
            .collect();
            (
                jar,
                render(&DomainPageT {
                    d: domain_line(&ctx, &d),
                    status_options,
                    quota_mb: mb_field(d.default_quota_bytes),
                    max_mailboxes: d.max_mailboxes.map(|m| m.to_string()).unwrap_or_default(),
                    can_edit: a.admin.is_super(),
                    notice: notice(q.done.as_deref()),
                    ctx,
                }),
            )
                .into_response()
        }
        Err(e) => page_error(&e),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct DomainForm {
    name: Option<String>,
    status: Option<String>,
    quota_mb: Option<String>,
    max_mailboxes: Option<String>,
    confirm: Option<String>,
}

fn parse_max(text: &str) -> Result<Option<i32>, Error> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<i32>()
        .ok()
        .filter(|n| *n >= 0)
        .map(Some)
        .ok_or_else(|| Error::Validation("the mailbox limit must be a whole number".into()))
}

pub(crate) async fn domain_create(
    State(state): State<AppState>,
    a: AdminCtx,
    Form(f): Form<DomainForm>,
) -> Response {
    let parsed = (|| {
        Ok::<_, Error>((
            parse_mb(f.quota_mb.as_deref().unwrap_or(""))?,
            parse_max(f.max_mailboxes.as_deref().unwrap_or(""))?,
        ))
    })();
    let (quota, max) = match parsed {
        Ok(v) => v,
        Err(e) => return form_error(&e),
    };
    match state
        .admin
        .create_domain(&a.admin, f.name.as_deref().unwrap_or(""), quota, max)
        .await
    {
        Ok(d) => redirect_to(&format!("/admin/domains/{}?done=created", d.id)),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn domain_update(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<DomainForm>,
) -> Response {
    let parsed = (|| {
        let status = AccountStatus::parse(f.status.as_deref().unwrap_or(""))
            .ok_or_else(|| Error::Validation("choose a status".into()))?;
        Ok::<_, Error>(DomainUpdate {
            status,
            default_quota_bytes: parse_mb(f.quota_mb.as_deref().unwrap_or(""))?,
            max_mailboxes: parse_max(f.max_mailboxes.as_deref().unwrap_or(""))?,
        })
    })();
    match parsed {
        Ok(update) => match state.admin.update_domain(&a.admin, id, &update).await {
            Ok(()) => redirect_to(&format!("/admin/domains/{id}?done=saved")),
            Err(e) => form_error(&e),
        },
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn domain_delete(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<DomainForm>,
) -> Response {
    match state
        .admin
        .delete_domain(&a.admin, id, f.confirm.as_deref().unwrap_or(""))
        .await
    {
        Ok(()) => redirect_to("/admin/domains?done=deleted"),
        Err(e) => form_error(&e),
    }
}

// ─── users ──────────────────────────────────────────────────────────────────

struct UserLine {
    id: Uuid,
    email: String,
    name: String,
    role_key: String,
    status_key: String,
    status_class: &'static str,
    last_login: String,
    used: String,
    quota: String,
    percent: Option<u32>,
    bar_class: &'static str,
}

fn percent_class(p: u32) -> &'static str {
    match p {
        90.. => "danger",
        75..=89 => "warning",
        _ => "success",
    }
}

fn user_line(ctx: &PageCtx, u: &UserRow) -> UserLine {
    let percent = u.quota_bytes.filter(|q| *q > 0).map(|q| {
        ((u.bytes_used as f64 / q as f64) * 100.0)
            .round()
            .clamp(0.0, 999.0) as u32
    });
    UserLine {
        id: u.id,
        email: u.email.clone(),
        name: u.display_name.clone(),
        role_key: role_key(u.role),
        status_key: status_key(u.status),
        status_class: status_class(u.status),
        last_login: u
            .last_login_at
            .map_or_else(|| ctx.t("admin-never"), |t| when(Some(t))),
        used: fmt_bytes(u.bytes_used),
        quota: u
            .quota_bytes
            .filter(|q| *q > 0)
            .map_or_else(|| ctx.t("admin-unlimited"), fmt_bytes),
        percent,
        bar_class: percent.map_or("success", percent_class),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct UsersQuery {
    domain: Option<String>,
    q: Option<String>,
    status: Option<String>,
    role: Option<String>,
    page: Option<i64>,
    done: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/users.html")]
struct UsersPage {
    ctx: PageCtx,
    rows: Vec<UserLine>,
    total: i64,
    domain_options: Vec<Option2>,
    status_options: Vec<Option2>,
    role_options: Vec<Option2>,
    search: String,
    page: i64,
    prev_url: Option<String>,
    next_url: Option<String>,
    new_url: String,
    show_domain_filter: bool,
    notice: Option<(&'static str, &'static str)>,
}

fn users_url(q: &UsersQuery, page: i64) -> String {
    format!(
        "/admin/users?domain={}&q={}&status={}&role={}&page={page}",
        enc(q.domain.as_deref().unwrap_or("")),
        enc(q.q.as_deref().unwrap_or("")),
        enc(q.status.as_deref().unwrap_or("")),
        enc(q.role.as_deref().unwrap_or(""))
    )
}

pub(crate) async fn users(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<UsersQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/users");
    let jar = ensure_cookie(&state.auth, jar);
    let page = q.page.unwrap_or(1).clamp(1, 100_000);
    let domain_id = q.domain.as_deref().and_then(|d| Uuid::parse_str(d).ok());
    let filter = UserFilter {
        domain_id,
        search: q.q.clone().filter(|s| !s.trim().is_empty()),
        status: q.status.as_deref().and_then(AccountStatus::parse),
        role: q.role.as_deref().and_then(UserRole::parse),
        limit: PAGE_SIZE,
        offset: (page - 1) * PAGE_SIZE,
    };
    let (listing, domains) = match tokio::try_join!(
        state.admin.list_users(&a.admin, &filter),
        state.admin.list_domains(&a.admin)
    ) {
        Ok(v) => v,
        Err(e) => return page_error(&e),
    };
    let mut domain_options = vec![choice(
        "",
        ctx.t("admin-all-domains"),
        q.domain.as_deref().unwrap_or(""),
    )];
    domain_options.extend(domains.iter().map(|d| {
        choice(
            d.id.to_string(),
            d.name.clone(),
            q.domain.as_deref().unwrap_or(""),
        )
    }));
    let status_current = q.status.as_deref().unwrap_or("");
    let mut status_options = vec![choice("", ctx.t("admin-any-status"), status_current)];
    status_options.extend(
        [
            AccountStatus::Active,
            AccountStatus::Suspended,
            AccountStatus::Disabled,
        ]
        .into_iter()
        .map(|s| choice(s.as_str(), ctx.t(&status_key(s)), status_current)),
    );
    let role_current = q.role.as_deref().unwrap_or("");
    let mut role_options = vec![choice("", ctx.t("admin-any-role"), role_current)];
    role_options.extend(
        [UserRole::User, UserRole::DomainAdmin, UserRole::SuperAdmin]
            .into_iter()
            .map(|r| choice(r.as_str(), ctx.t(&role_key(r)), role_current)),
    );
    let page_data = UsersPage {
        rows: listing.rows.iter().map(|u| user_line(&ctx, u)).collect(),
        total: listing.total,
        domain_options,
        status_options,
        role_options,
        search: q.q.clone().unwrap_or_default(),
        page,
        prev_url: (page > 1).then(|| users_url(&q, page - 1)),
        next_url: (page * PAGE_SIZE < listing.total).then(|| users_url(&q, page + 1)),
        new_url: format!(
            "/admin/users/new?domain={}",
            enc(q.domain.as_deref().unwrap_or(""))
        ),
        show_domain_filter: a.admin.is_super(),
        notice: notice(q.done.as_deref()),
        ctx,
    };
    (jar, render(&page_data)).into_response()
}

#[derive(Template)]
#[template(path = "admin/user_new.html")]
struct UserNewPage {
    ctx: PageCtx,
    domains: Vec<Option2>,
    roles: Vec<Option2>,
}

pub(crate) async fn user_new(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<UsersQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/users");
    let jar = ensure_cookie(&state.auth, jar);
    let domains = match state.admin.list_domains(&a.admin).await {
        Ok(d) => d,
        Err(e) => return page_error(&e),
    };
    let current = q.domain.as_deref().unwrap_or("");
    let roles = if a.admin.is_super() {
        vec![UserRole::User, UserRole::DomainAdmin, UserRole::SuperAdmin]
    } else {
        vec![UserRole::User]
    };
    (
        jar,
        render(&UserNewPage {
            domains: domains
                .iter()
                .map(|d| {
                    choice(
                        d.name.clone(),
                        d.name.clone(),
                        &domains
                            .iter()
                            .find(|x| x.id.to_string() == current)
                            .map(|x| x.name.clone())
                            .unwrap_or_default(),
                    )
                })
                .collect(),
            roles: roles
                .into_iter()
                .map(|r| choice(r.as_str(), ctx.t(&role_key(r)), "user"))
                .collect(),
            ctx,
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct UserForm {
    local_part: Option<String>,
    domain: Option<String>,
    name: Option<String>,
    role: Option<String>,
    quota_mb: Option<String>,
    password: Option<String>,
    status: Option<String>,
    confirm: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/_secret.html")]
struct SecretPartial {
    ctx: PageCtx,
    heading_key: &'static str,
    email: String,
    password: Option<String>,
    link: String,
}

pub(crate) async fn user_create(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Form(f): Form<UserForm>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/users");
    let parsed = (|| {
        let role = UserRole::parse(f.role.as_deref().unwrap_or("user"))
            .ok_or_else(|| Error::Validation("choose a role".into()))?;
        let local = f.local_part.as_deref().unwrap_or("").trim().to_lowercase();
        let domain = f.domain.as_deref().unwrap_or("").trim().to_lowercase();
        Ok::<_, Error>(NewAccount {
            email: format!("{local}@{domain}"),
            display_name: f.name.clone().unwrap_or_default(),
            role,
            quota_bytes: parse_mb(f.quota_mb.as_deref().unwrap_or(""))?,
            password: f.password.clone().filter(|p| !p.is_empty()),
        })
    })();
    let request = match parsed {
        Ok(r) => r,
        Err(e) => return form_error(&e),
    };
    match state.admin.create_user(&a.admin, request).await {
        Ok(created) => render(&SecretPartial {
            ctx,
            heading_key: "admin-user-created",
            email: created.user.email.clone(),
            password: created.generated_password,
            link: format!("/admin/users/{}", created.user.id),
        }),
        Err(e) => form_error(&e),
    }
}

struct UserDetailView {
    id: Uuid,
    email: String,
    name: String,
    domain: String,
    role: String,
    role_key: String,
    status_key: String,
    status_class: &'static str,
    created: String,
    last_login: String,
    used: String,
    messages: i64,
    effective_quota: String,
    quota_mb: String,
    percent: Option<u32>,
    bar_class: &'static str,
    failed_logins: i32,
    locked_until: String,
    sessions: i64,
    groups: Vec<(Uuid, String)>,
    aliases: Vec<String>,
}

#[derive(Template)]
#[template(path = "admin/user.html")]
struct UserPage {
    ctx: PageCtx,
    u: UserDetailView,
    role_options: Vec<Option2>,
    status_options: Vec<Option2>,
    can_manage: bool,
    can_set_role: bool,
    is_self: bool,
    notice: Option<(&'static str, &'static str)>,
}

fn detail_view(ctx: &PageCtx, d: &UserDetail) -> UserDetailView {
    let line = user_line(ctx, &d.row);
    UserDetailView {
        id: d.row.id,
        email: d.row.email.clone(),
        name: d.row.display_name.clone(),
        domain: d.row.domain.clone(),
        role: d.row.role.as_str().to_owned(),
        role_key: line.role_key,
        status_key: line.status_key,
        status_class: line.status_class,
        created: when(Some(d.row.created_at)),
        last_login: line.last_login,
        used: line.used,
        messages: d.row.messages,
        effective_quota: line.quota,
        quota_mb: mb_field(d.quota_override),
        percent: line.percent,
        bar_class: line.bar_class,
        failed_logins: d.failed_logins,
        locked_until: when(d.locked_until),
        sessions: d.active_sessions,
        groups: d.groups.clone(),
        aliases: d.aliases.clone(),
    }
}

pub(crate) async fn user_page(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<DoneQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/users");
    let jar = ensure_cookie(&state.auth, jar);
    let detail = match state.admin.get_user(&a.admin, id).await {
        Ok(d) => d,
        Err(e) => return page_error(&e),
    };
    let role_current = detail.row.role.as_str();
    let status_current = detail.row.status.as_str();
    let can_manage = a.admin.is_super() || detail.row.role == UserRole::User;
    let page = UserPage {
        u: detail_view(&ctx, &detail),
        role_options: [UserRole::User, UserRole::DomainAdmin, UserRole::SuperAdmin]
            .into_iter()
            .map(|r| choice(r.as_str(), ctx.t(&role_key(r)), role_current))
            .collect(),
        status_options: [
            AccountStatus::Active,
            AccountStatus::Suspended,
            AccountStatus::Disabled,
        ]
        .into_iter()
        .map(|s| choice(s.as_str(), ctx.t(&status_key(s)), status_current))
        .collect(),
        can_manage,
        can_set_role: a.admin.is_super() && detail.row.id != a.admin.user_id,
        is_self: detail.row.id == a.admin.user_id,
        notice: notice(q.done.as_deref()),
        ctx,
    };
    (jar, render(&page)).into_response()
}

pub(crate) async fn user_update(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<UserForm>,
) -> Response {
    let parsed = (|| {
        Ok::<_, Error>(UserUpdate {
            display_name: f.name.clone().unwrap_or_default(),
            role: UserRole::parse(f.role.as_deref().unwrap_or("user"))
                .ok_or_else(|| Error::Validation("choose a role".into()))?,
            quota_bytes: parse_mb(f.quota_mb.as_deref().unwrap_or(""))?,
        })
    })();
    match parsed {
        Ok(update) => match state.admin.update_user(&a.admin, id, &update).await {
            Ok(()) => redirect_to(&format!("/admin/users/{id}?done=saved")),
            Err(e) => form_error(&e),
        },
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn user_status(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<UserForm>,
) -> Response {
    let Some(status) = AccountStatus::parse(f.status.as_deref().unwrap_or("")) else {
        return alert(
            StatusCode::UNPROCESSABLE_ENTITY,
            "danger",
            "Choose a status.",
        );
    };
    match state.admin.set_user_status(&a.admin, id, status).await {
        Ok(()) => redirect_to(&format!("/admin/users/{id}?done=status")),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn user_password(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Form(f): Form<UserForm>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/users");
    let chosen = f.password.clone().filter(|p| !p.is_empty());
    let generated = chosen.is_none();
    match state.admin.reset_password(&a.admin, id, chosen).await {
        Ok(password) => {
            let email = state
                .admin
                .get_user(&a.admin, id)
                .await
                .map(|d| d.row.email)
                .unwrap_or_default();
            render(&SecretPartial {
                ctx,
                heading_key: "admin-password-reset",
                email,
                password: generated.then_some(password),
                link: format!("/admin/users/{id}"),
            })
        }
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn user_unlock(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
) -> Response {
    match state.admin.unlock_user(&a.admin, id).await {
        Ok(()) => redirect_to(&format!("/admin/users/{id}?done=unlocked")),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn user_sessions(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
) -> Response {
    match state.admin.revoke_user_sessions(&a.admin, id).await {
        Ok(_) => redirect_to(&format!("/admin/users/{id}?done=signedout")),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn user_delete(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<UserForm>,
) -> Response {
    match state
        .admin
        .delete_user(&a.admin, id, f.confirm.as_deref().unwrap_or(""))
        .await
    {
        Ok(()) => redirect_to("/admin/users?done=deleted"),
        Err(e) => form_error(&e),
    }
}

// ─── aliases ────────────────────────────────────────────────────────────────

struct AliasLine {
    id: Uuid,
    address: String,
    destination: String,
    enabled: bool,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct AliasQuery {
    domain: Option<String>,
    q: Option<String>,
    page: Option<i64>,
    done: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/aliases.html")]
struct AliasesPage {
    ctx: PageCtx,
    rows: Vec<AliasLine>,
    total: i64,
    domain_options: Vec<Option2>,
    create_domains: Vec<Option2>,
    search: String,
    page: i64,
    prev_url: Option<String>,
    next_url: Option<String>,
    notice: Option<(&'static str, &'static str)>,
}

fn aliases_url(q: &AliasQuery, page: i64) -> String {
    format!(
        "/admin/aliases?domain={}&q={}&page={page}",
        enc(q.domain.as_deref().unwrap_or("")),
        enc(q.q.as_deref().unwrap_or(""))
    )
}

pub(crate) async fn aliases(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<AliasQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/aliases");
    let jar = ensure_cookie(&state.auth, jar);
    let page = q.page.unwrap_or(1).clamp(1, 100_000);
    let domain_id = q.domain.as_deref().and_then(|d| Uuid::parse_str(d).ok());
    let (listing, domains) = match tokio::try_join!(
        state.admin.list_aliases(
            &a.admin,
            domain_id,
            q.q.as_deref(),
            PAGE_SIZE,
            (page - 1) * PAGE_SIZE
        ),
        state.admin.list_domains(&a.admin)
    ) {
        Ok(v) => v,
        Err(e) => return page_error(&e),
    };
    let current = q.domain.as_deref().unwrap_or("");
    let mut domain_options = vec![choice("", ctx.t("admin-all-domains"), current)];
    domain_options.extend(
        domains
            .iter()
            .map(|d| choice(d.id.to_string(), d.name.clone(), current)),
    );
    let data = AliasesPage {
        rows: listing
            .rows
            .iter()
            .map(|r: &AliasRow| AliasLine {
                id: r.id,
                address: r.address.clone(),
                destination: r.destination.clone(),
                enabled: r.enabled,
            })
            .collect(),
        total: listing.total,
        domain_options,
        create_domains: domains
            .iter()
            .map(|d| choice(d.id.to_string(), d.name.clone(), current))
            .collect(),
        search: q.q.clone().unwrap_or_default(),
        page,
        prev_url: (page > 1).then(|| aliases_url(&q, page - 1)),
        next_url: (page * PAGE_SIZE < listing.total).then(|| aliases_url(&q, page + 1)),
        notice: notice(q.done.as_deref()),
        ctx,
    };
    (jar, render(&data)).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct AliasForm {
    domain_id: Option<String>,
    address: Option<String>,
    destination: Option<String>,
}

pub(crate) async fn alias_create(
    State(state): State<AppState>,
    a: AdminCtx,
    Form(f): Form<AliasForm>,
) -> Response {
    let Some(domain_id) = f.domain_id.as_deref().and_then(|d| Uuid::parse_str(d).ok()) else {
        return alert(
            StatusCode::UNPROCESSABLE_ENTITY,
            "danger",
            "Choose a domain.",
        );
    };
    match state
        .admin
        .create_alias(
            &a.admin,
            domain_id,
            f.address.as_deref().unwrap_or(""),
            f.destination.as_deref().unwrap_or(""),
        )
        .await
    {
        Ok(_) => redirect_to("/admin/aliases?done=created"),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn alias_toggle(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<UserForm>,
) -> Response {
    let enable = f.status.as_deref() == Some("enable");
    match state.admin.set_alias_enabled(&a.admin, id, enable).await {
        Ok(()) => redirect_to("/admin/aliases?done=saved"),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn alias_delete(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
) -> Response {
    match state.admin.delete_alias(&a.admin, id).await {
        Ok(()) => redirect_to("/admin/aliases?done=deleted"),
        Err(e) => form_error(&e),
    }
}

// ─── groups ─────────────────────────────────────────────────────────────────

struct GroupLine {
    id: Uuid,
    domain: String,
    name: String,
    address: String,
    members: i64,
}

fn group_line(g: &GroupRow) -> GroupLine {
    GroupLine {
        id: g.id,
        domain: g.domain.clone(),
        name: g.name.clone(),
        address: g.address.clone().unwrap_or_default(),
        members: g.members,
    }
}

#[derive(Template)]
#[template(path = "admin/groups.html")]
struct GroupsPage {
    ctx: PageCtx,
    rows: Vec<GroupLine>,
    domains: Vec<Option2>,
    notice: Option<(&'static str, &'static str)>,
}

pub(crate) async fn groups(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/groups");
    let jar = ensure_cookie(&state.auth, jar);
    let (rows, domains) = match tokio::try_join!(
        state.admin.list_groups(&a.admin, None),
        state.admin.list_domains(&a.admin)
    ) {
        Ok(v) => v,
        Err(e) => return page_error(&e),
    };
    (
        jar,
        render(&GroupsPage {
            rows: rows.iter().map(group_line).collect(),
            domains: domains
                .iter()
                .map(|d| choice(d.id.to_string(), d.name.clone(), ""))
                .collect(),
            notice: notice(q.done.as_deref()),
            ctx,
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct GroupForm {
    domain_id: Option<String>,
    name: Option<String>,
    address: Option<String>,
    description: Option<String>,
    email: Option<String>,
}

pub(crate) async fn group_create(
    State(state): State<AppState>,
    a: AdminCtx,
    Form(f): Form<GroupForm>,
) -> Response {
    let Some(domain_id) = f.domain_id.as_deref().and_then(|d| Uuid::parse_str(d).ok()) else {
        return alert(
            StatusCode::UNPROCESSABLE_ENTITY,
            "danger",
            "Choose a domain.",
        );
    };
    match state
        .admin
        .create_group(
            &a.admin,
            domain_id,
            f.name.as_deref().unwrap_or(""),
            f.address.as_deref(),
            f.description.as_deref().unwrap_or(""),
        )
        .await
    {
        Ok(id) => redirect_to(&format!("/admin/groups/{id}?done=created")),
        Err(e) => form_error(&e),
    }
}

#[derive(Template)]
#[template(path = "admin/group.html")]
struct GroupPage {
    ctx: PageCtx,
    g: GroupLine,
    description: String,
    members: Vec<(Uuid, String, String)>,
    notice: Option<(&'static str, &'static str)>,
}

pub(crate) async fn group_page(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<DoneQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/groups");
    let jar = ensure_cookie(&state.auth, jar);
    match state.admin.get_group(&a.admin, id).await {
        Ok(GroupDetail { group, members }) => (
            jar,
            render(&GroupPage {
                g: group_line(&group),
                description: group.description.clone(),
                members: members
                    .into_iter()
                    .map(|m| (m.user_id, m.email, m.display_name))
                    .collect(),
                notice: notice(q.done.as_deref()),
                ctx,
            }),
        )
            .into_response(),
        Err(e) => page_error(&e),
    }
}

pub(crate) async fn group_member_add(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
    Form(f): Form<GroupForm>,
) -> Response {
    match state
        .admin
        .add_group_member(&a.admin, id, f.email.as_deref().unwrap_or(""))
        .await
    {
        Ok(()) => redirect_to(&format!("/admin/groups/{id}?done=saved")),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn group_member_remove(
    State(state): State<AppState>,
    a: AdminCtx,
    Path((id, user)): Path<(Uuid, Uuid)>,
) -> Response {
    match state.admin.remove_group_member(&a.admin, id, user).await {
        Ok(()) => redirect_to(&format!("/admin/groups/{id}?done=saved")),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn group_delete(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(id): Path<Uuid>,
) -> Response {
    match state.admin.delete_group(&a.admin, id).await {
        Ok(()) => redirect_to("/admin/groups?done=deleted"),
        Err(e) => form_error(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_notices_are_shown() {
        assert!(notice(Some("created")).is_some());
        assert!(notice(Some("<script>")).is_none());
        assert!(notice(None).is_none());
    }

    #[test]
    fn mailbox_limits_are_parsed_strictly() {
        assert_eq!(parse_max("").unwrap(), None);
        assert_eq!(parse_max(" 12 ").unwrap(), Some(12));
        assert_eq!(parse_max("0").unwrap(), Some(0));
        for bad in ["-1", "1.5", "x", "99999999999"] {
            assert!(parse_max(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn usage_colours_follow_how_full_a_mailbox_is() {
        assert_eq!(percent_class(10), "success");
        assert_eq!(percent_class(80), "warning");
        assert_eq!(percent_class(95), "danger");
        assert_eq!(percent_class(300), "danger");
    }

    #[test]
    fn list_urls_encode_their_filters() {
        let q = UsersQuery {
            q: Some("a&b c".into()),
            ..Default::default()
        };
        assert_eq!(
            users_url(&q, 2),
            "/admin/users?domain=&q=a%26b%20c&status=&role=&page=2"
        );
    }
}
