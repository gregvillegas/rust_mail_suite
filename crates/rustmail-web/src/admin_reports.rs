//! Admin console: quota dashboard, audit log, CSV import and health (Phase 7).

use askama::Template;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use axum_extra::extract::CookieJar;
use chrono::{DateTime, NaiveDate, Utc};
use rustmail_core::admin::audit::AuditSearch;
use rustmail_core::admin::import::{results_csv, ImportPreview, ImportResult};
use rustmail_core::admin::quota::{MailboxQuota, QuotaDashboard};
use rustmail_core::domain::{AuditEntry, AuditOutcome};
use serde::Deserialize;

use crate::admin_common::{alert, fmt_bytes, form_error, page_error, AdminCtx};
use crate::admin_health::{overall, run_checks, Check};
use crate::app::AppState;
use crate::auth::csrf::ensure_cookie;
use crate::pages::{encode_query_value, page_ctx};
use crate::ui::{render, PageCtx, PageError};

const PAGE_SIZE: i64 = 50;

fn enc(s: &str) -> String {
    encode_query_value(s)
}

// ─── quota ──────────────────────────────────────────────────────────────────

struct DomainQuotaLine {
    domain: String,
    mailboxes: i64,
    used: String,
    limited: String,
    unlimited: i64,
}

struct MailboxLine {
    user_id: uuid::Uuid,
    email: String,
    used: String,
    quota: String,
    percent: Option<u32>,
    bar_class: &'static str,
    messages: i64,
}

fn mailbox_line(ctx: &PageCtx, m: &MailboxQuota) -> MailboxLine {
    let percent = m.percent().map(|p| p.round().clamp(0.0, 999.0) as u32);
    MailboxLine {
        user_id: m.user_id,
        email: m.email.clone(),
        used: fmt_bytes(m.used_bytes),
        quota: m
            .quota_bytes
            .map_or_else(|| ctx.t("admin-unlimited"), fmt_bytes),
        percent,
        bar_class: match percent {
            Some(90..) => "danger",
            Some(75..=89) => "warning",
            _ => "success",
        },
        messages: m.messages,
    }
}

#[derive(Template)]
#[template(path = "admin/quota.html")]
struct QuotaPage {
    ctx: PageCtx,
    domains: Vec<DomainQuotaLine>,
    top: Vec<MailboxLine>,
    near: Vec<MailboxLine>,
    total_used: String,
    total_mailboxes: i64,
    chart_script: String,
}

pub(crate) async fn quota(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/quota");
    let dashboard = match state.admin.quota_dashboard(&a.admin).await {
        Ok(d) => d,
        Err(e) => return page_error(&e),
    };
    let page = QuotaPage {
        domains: dashboard
            .domains
            .iter()
            .map(|d| DomainQuotaLine {
                domain: d.domain.clone(),
                mailboxes: d.mailboxes,
                used: fmt_bytes(d.used_bytes),
                limited: fmt_bytes(d.limited_quota_bytes),
                unlimited: d.unlimited_mailboxes,
            })
            .collect(),
        top: dashboard
            .top
            .iter()
            .map(|m| mailbox_line(&ctx, m))
            .collect(),
        near: dashboard
            .near_limit
            .iter()
            .map(|m| mailbox_line(&ctx, m))
            .collect(),
        total_used: fmt_bytes(dashboard.total_used_bytes),
        total_mailboxes: dashboard.total_mailboxes,
        chart_script: ctx.assets.url("vendor/chartjs/4.4.7/chart.umd.min.js"),
        ctx,
    };
    render(&page)
}

/// The same figures as JSON, for the charts. Scoped exactly like the page.
pub(crate) async fn quota_data(State(state): State<AppState>, a: AdminCtx) -> Response {
    match state.admin.quota_dashboard(&a.admin).await {
        Ok(d) => Json::<QuotaDashboard>(d).into_response(),
        Err(e) => page_error(&e),
    }
}

// ─── audit ──────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub(crate) struct AuditQuery {
    actor: Option<String>,
    action: Option<String>,
    outcome: Option<String>,
    from: Option<String>,
    to: Option<String>,
    text: Option<String>,
    page: Option<i64>,
}

fn parse_day(text: Option<&str>) -> Option<NaiveDate> {
    text.and_then(|t| NaiveDate::parse_from_str(t.trim(), "%Y-%m-%d").ok())
}

impl AuditQuery {
    fn search(&self, limit: i64, offset: i64) -> AuditSearch {
        let day_start = |d: NaiveDate| {
            DateTime::<Utc>::from_naive_utc_and_offset(
                d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                Utc,
            )
        };
        AuditSearch {
            actor: self.actor.clone().filter(|s| !s.trim().is_empty()),
            action: self.action.clone().filter(|s| !s.trim().is_empty()),
            outcome: match self.outcome.as_deref() {
                Some("success") => Some(AuditOutcome::Success),
                Some("failure") => Some(AuditOutcome::Failure),
                _ => None,
            },
            from: parse_day(self.from.as_deref()).map(day_start),
            // "to" is a whole day: everything before the next midnight.
            to: parse_day(self.to.as_deref())
                .and_then(|d| d.succ_opt())
                .map(day_start),
            text: self.text.clone().filter(|s| !s.trim().is_empty()),
            limit,
            offset,
        }
    }

    fn query_string(&self, page: Option<i64>) -> String {
        let mut parts = vec![
            format!("actor={}", enc(self.actor.as_deref().unwrap_or(""))),
            format!("action={}", enc(self.action.as_deref().unwrap_or(""))),
            format!("outcome={}", enc(self.outcome.as_deref().unwrap_or(""))),
            format!("from={}", enc(self.from.as_deref().unwrap_or(""))),
            format!("to={}", enc(self.to.as_deref().unwrap_or(""))),
            format!("text={}", enc(self.text.as_deref().unwrap_or(""))),
        ];
        if let Some(p) = page {
            parts.push(format!("page={p}"));
        }
        parts.join("&")
    }
}

struct AuditLine {
    when: String,
    iso: String,
    actor: String,
    action: String,
    ok: bool,
    target: String,
    ip: String,
    details: String,
}

fn audit_line(e: &AuditEntry) -> AuditLine {
    AuditLine {
        when: e.occurred_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        iso: e.occurred_at.to_rfc3339(),
        actor: e.actor_email.clone().unwrap_or_default(),
        action: e.action.clone(),
        ok: e.outcome == AuditOutcome::Success,
        target: match (&e.target_type, &e.target_id) {
            (Some(t), Some(id)) => format!("{t} {id}"),
            (Some(t), None) => t.clone(),
            _ => String::new(),
        },
        ip: e.ip_address.clone().unwrap_or_default(),
        details: if e.details.as_object().is_some_and(serde_json::Map::is_empty) {
            String::new()
        } else {
            serde_json::to_string_pretty(&e.details).unwrap_or_default()
        },
    }
}

struct Choice {
    value: String,
    selected: bool,
}

#[derive(Template)]
#[template(path = "admin/audit.html")]
struct AuditPage {
    ctx: PageCtx,
    rows: Vec<AuditLine>,
    total: i64,
    actions: Vec<Choice>,
    q: AuditForm,
    outcome_success: bool,
    outcome_failure: bool,
    page: i64,
    prev_url: Option<String>,
    next_url: Option<String>,
    export_url: String,
}

struct AuditForm {
    actor: String,
    from: String,
    to: String,
    text: String,
}

pub(crate) async fn audit(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/audit");
    let page = q.page.unwrap_or(1).clamp(1, 100_000);
    let search = q.search(PAGE_SIZE, (page - 1) * PAGE_SIZE);
    let (results, actions) = match tokio::try_join!(
        state.admin.search_audit(&a.admin, &search),
        state.admin.audit_actions(&a.admin)
    ) {
        Ok(v) => v,
        Err(e) => return page_error(&e),
    };
    let current = q.action.as_deref().unwrap_or("");
    let mut action_choices: Vec<Choice> = actions
        .into_iter()
        .map(|v| Choice {
            selected: v == current,
            value: v,
        })
        .collect();
    // Keep a typed prefix such as "user." selectable even if it is not a full action name.
    if !current.is_empty() && !action_choices.iter().any(|c| c.value == current) {
        action_choices.insert(
            0,
            Choice {
                value: current.to_owned(),
                selected: true,
            },
        );
    }
    let data = AuditPage {
        rows: results.entries.iter().map(audit_line).collect(),
        total: results.total,
        actions: action_choices,
        outcome_success: q.outcome.as_deref() == Some("success"),
        outcome_failure: q.outcome.as_deref() == Some("failure"),
        page,
        prev_url: (page > 1).then(|| format!("/admin/audit?{}", q.query_string(Some(page - 1)))),
        next_url: (page * PAGE_SIZE < results.total)
            .then(|| format!("/admin/audit?{}", q.query_string(Some(page + 1)))),
        export_url: format!("/admin/audit.csv?{}", q.query_string(None)),
        q: AuditForm {
            actor: q.actor.clone().unwrap_or_default(),
            from: q.from.clone().unwrap_or_default(),
            to: q.to.clone().unwrap_or_default(),
            text: q.text.clone().unwrap_or_default(),
        },
        ctx,
    };
    render(&data)
}

pub(crate) async fn audit_csv(
    State(state): State<AppState>,
    a: AdminCtx,
    Query(q): Query<AuditQuery>,
) -> Response {
    match state
        .admin
        .export_audit_csv(&a.admin, &q.search(1, 0))
        .await
    {
        Ok(csv) => csv_download("audit-log.csv", csv),
        Err(e) => page_error(&e),
    }
}

fn csv_download(name: &str, csv: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "private, no-store".to_owned()),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'".to_owned(),
            ),
        ],
        csv,
    )
        .into_response()
}

// ─── import ─────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "admin/import.html")]
struct ImportPage {
    ctx: PageCtx,
}

pub(crate) async fn import_page(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/import");
    let jar = ensure_cookie(&state.auth, jar);
    (jar, render(&ImportPage { ctx })).into_response()
}

/// Body limit for the import forms (the CSV limit is 1 MB; this leaves room for the form).
pub const IMPORT_BODY_LIMIT: usize = 1536 * 1024;

struct ProblemLine {
    line: usize,
    email: String,
    message: String,
}

struct ValidLine {
    line: usize,
    email: String,
    name: String,
    role_key: String,
    quota: String,
    has_password: bool,
}

#[derive(Template)]
#[template(path = "admin/_import_preview.html")]
struct PreviewPartial {
    ctx: PageCtx,
    valid: Vec<ValidLine>,
    problems: Vec<ProblemLine>,
    csv: String,
}

fn preview_view(ctx: PageCtx, preview: &ImportPreview, csv: String) -> PreviewPartial {
    PreviewPartial {
        valid: preview
            .valid
            .iter()
            .map(|r| ValidLine {
                line: r.line,
                email: r.email.clone(),
                name: r.name.clone(),
                role_key: format!("admin-role-{}", r.role.as_str()),
                quota: r
                    .quota_bytes
                    .map_or_else(|| ctx.t("admin-inherited"), fmt_bytes),
                has_password: r.password.is_some(),
            })
            .collect(),
        problems: preview
            .problems
            .iter()
            .map(|p| ProblemLine {
                line: p.line,
                email: p.email.clone(),
                message: p.message.clone(),
            })
            .collect(),
        csv,
        ctx,
    }
}

/// Reads the CSV from an uploaded file if there is one, else from the text box.
async fn read_csv(mut multipart: Multipart) -> Result<String, Box<Response>> {
    let mut pasted = String::new();
    let mut file = String::new();
    let bad = || {
        alert(
            StatusCode::UNPROCESSABLE_ENTITY,
            "danger",
            "The upload could not be read (is it larger than 1 MB?).",
        )
    };
    while let Some(field) = multipart.next_field().await.map_err(|_| Box::new(bad()))? {
        let name = field.name().unwrap_or_default().to_owned();
        let bytes = field.bytes().await.map_err(|_| Box::new(bad()))?;
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
            alert(
                StatusCode::UNPROCESSABLE_ENTITY,
                "danger",
                "The file is not UTF-8 text. Save it as CSV (UTF-8) and try again.",
            )
        })?;
        match name.as_str() {
            "file" => file = text,
            "csv" => pasted = text,
            _ => {}
        }
    }
    Ok(if file.trim().is_empty() { pasted } else { file })
}

pub(crate) async fn import_preview(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/import");
    let csv = match read_csv(multipart).await {
        Ok(c) => c,
        Err(response) => return *response,
    };
    match state.admin.preview_import(&a.admin, &csv).await {
        Ok(preview) => render(&preview_view(ctx, &preview, csv)),
        Err(e) => form_error(&e),
    }
}

struct ResultLine {
    line: usize,
    email: String,
    created: bool,
    error: String,
}

#[derive(Template)]
#[template(path = "admin/_import_result.html")]
struct ResultPartial {
    ctx: PageCtx,
    rows: Vec<ResultLine>,
    created: usize,
    failed: usize,
    download: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApplyForm {
    csv: String,
}

pub(crate) async fn import_apply(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
    Form(f): Form<ApplyForm>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/import");
    let results: Vec<ImportResult> = match state.admin.apply_import(&a.admin, &f.csv).await {
        Ok(r) => r,
        Err(e) => return form_error(&e),
    };
    let created = results.iter().filter(|r| r.created).count();
    // Generated passwords go into one downloadable file, not into the page.
    let download = results
        .iter()
        .any(|r| r.generated_password.is_some())
        .then(|| {
            state
                .import_results
                .put(a.admin.user_id, results_csv(&results))
        })
        .map(|token| format!("/admin/import/result/{token}"));
    render(&ResultPartial {
        rows: results
            .iter()
            .map(|r| ResultLine {
                line: r.line,
                email: r.email.clone(),
                created: r.created,
                error: r.error.clone().unwrap_or_default(),
            })
            .collect(),
        created,
        failed: results.len() - created,
        download,
        ctx,
    })
}

pub(crate) async fn import_result(
    State(state): State<AppState>,
    a: AdminCtx,
    Path(token): Path<String>,
) -> Response {
    match state.import_results.take(a.admin.user_id, &token) {
        Some(csv) => csv_download("import-result.csv", csv),
        None => PageError::NotFound.into_response(),
    }
}

// ─── health ─────────────────────────────────────────────────────────────────

struct CheckLine {
    label_key: &'static str,
    subject: String,
    class: &'static str,
    level_key: &'static str,
    detail: String,
}

fn check_line(c: &Check) -> CheckLine {
    CheckLine {
        label_key: c.label_key,
        subject: c.subject.clone(),
        class: c.level.class(),
        level_key: c.level.key(),
        detail: c.detail.clone(),
    }
}

#[derive(Template)]
#[template(path = "admin/health.html")]
struct HealthPage {
    ctx: PageCtx,
    checks: Vec<CheckLine>,
    overall_class: &'static str,
    overall_key: &'static str,
}

#[derive(Template)]
#[template(path = "admin/_health_table.html")]
struct HealthTable {
    ctx: PageCtx,
    checks: Vec<CheckLine>,
    overall_class: &'static str,
    overall_key: &'static str,
}

pub(crate) async fn health(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = a.admin.require_super() {
        return page_error(&e);
    }
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/health");
    let checks = run_checks(&state, &a.admin).await;
    let level = overall(&checks);
    render(&HealthPage {
        checks: checks.iter().map(check_line).collect(),
        overall_class: level.class(),
        overall_key: level.key(),
        ctx,
    })
}

pub(crate) async fn health_table(
    State(state): State<AppState>,
    a: AdminCtx,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = a.admin.require_super() {
        return page_error(&e);
    }
    let ctx = page_ctx(&state, &jar, &headers, Some(&a.web.user), "/admin/health");
    let checks = run_checks(&state, &a.admin).await;
    let level = overall(&checks);
    render(&HealthTable {
        checks: checks.iter().map(check_line).collect(),
        overall_class: level.class(),
        overall_key: level.key(),
        ctx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_dates_cover_whole_days() {
        let q = AuditQuery {
            from: Some("2025-11-10".into()),
            to: Some("2025-11-12".into()),
            ..Default::default()
        };
        let s = q.search(10, 0);
        assert_eq!(s.from.unwrap().to_rfc3339(), "2025-11-10T00:00:00+00:00");
        assert_eq!(
            s.to.unwrap().to_rfc3339(),
            "2025-11-13T00:00:00+00:00",
            "'to' includes the whole day"
        );
        let bad = AuditQuery {
            from: Some("yesterday".into()),
            outcome: Some("maybe".into()),
            ..Default::default()
        };
        let s = bad.search(10, 0);
        assert!(s.from.is_none() && s.outcome.is_none());
    }

    #[test]
    fn export_links_keep_the_filters() {
        let q = AuditQuery {
            actor: Some("a b".into()),
            text: Some("x&y".into()),
            ..Default::default()
        };
        let s = q.query_string(None);
        assert!(
            s.contains("actor=a%20b") && s.contains("text=x%26y") && !s.contains("page="),
            "{s}"
        );
        assert!(q.query_string(Some(3)).ends_with("page=3"));
    }
}
