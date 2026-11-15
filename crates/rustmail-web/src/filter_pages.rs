//! Filters and the autoresponder (Phase 8): `/settings/filters…` and `/settings/autoresponder`.
//!
//! Every signed-in user manages their own mail rules here. The pages edit what is stored
//! in the database (`rustmail_core::filters::store`), then push the generated Sieve
//! script to Dovecot over ManageSieve. If Dovecot cannot be reached the change is still
//! saved and the page says it is not live yet; "Apply to inbox" runs the same rules once
//! over the existing inbox with the fallback engine.

use std::collections::HashSet;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use axum_extra::extract::CookieJar;
use chrono::NaiveDate;
use rustmail_core::domain::User;
use rustmail_core::filters::engine::{evaluate, Facts};
use rustmail_core::filters::sieve::{
    main_script, rules_script, vacation_script, FILTERS_SCRIPT, MAIN_SCRIPT, VACATION_SCRIPT,
};
use rustmail_core::filters::store::{FilterMode, Script, SyncStatus};
use rustmail_core::filters::{Action, Autoresponder, Condition, Field, Op, Rule, MAX_RULES};
use rustmail_core::Error;
use rustmail_imap::session::FlagKind;
use serde::Deserialize;

use crate::admin_common::{alert, form_error, page_error, redirect_to};
use crate::app::AppState;
use crate::auth::csrf::ensure_cookie;
use crate::mail_pages::with_session;
use crate::pages::{page_ctx, WebUser};
use crate::ui::{render, PageCtx, PageError};

/// How many of the newest inbox messages "Apply to inbox" looks at.
const APPLY_LIMIT: u32 = 500;
const CONDITION_ROWS: usize = 3;
/// Body limit for the script editor: a 64 KB script can grow to three times that when
/// form-encoded. Everything else on these pages is small and keeps the global limit.
pub(crate) const SCRIPT_BODY_LIMIT: usize = 256 * 1024;

struct Opt {
    value: String,
    label: String,
    selected: bool,
}

fn opt(value: impl Into<String>, label: impl Into<String>, current: &str) -> Opt {
    let value = value.into();
    Opt {
        selected: value == current,
        value,
        label: label.into(),
    }
}

/// A line above a page reporting what just happened, chosen from a fixed list.
fn notice(done: Option<&str>) -> Option<(&'static str, &'static str)> {
    Some(match done? {
        "saved" => ("success", "filters-notice-saved"),
        "pending" => ("warning", "filters-notice-pending"),
        "failed" => ("danger", "filters-notice-failed"),
        "deleted" => ("success", "filters-notice-deleted"),
        "applied" => ("success", "filters-notice-applied"),
        "synced" => ("success", "filters-notice-synced"),
        "raw-left" => ("success", "filters-notice-raw-left"),
        _ => return None,
    })
}

// ─── pushing to Dovecot ─────────────────────────────────────────────────────

/// What happened when a script was sent to the mail server.
enum Pushed {
    /// Dovecot accepted it: the change is live.
    Live,
    /// Dovecot could not be reached; the change is saved and will be sent on retry.
    Pending(String),
    /// Dovecot refused the script; the text says why.
    Rejected(String),
}

async fn record(
    state: &AppState,
    user: &User,
    script: Script,
    outcome: &Result<(), rustmail_sieve::SieveError>,
) -> Pushed {
    let (message, pushed) = match outcome {
        Ok(()) => (None, Pushed::Live),
        Err(e) if e.is_script_error() => (Some(e.to_string()), Pushed::Rejected(e.to_string())),
        Err(e) => (Some(e.to_string()), Pushed::Pending(e.to_string())),
    };
    if let Err(error) = state
        .filters
        .mark(user.id, script, message.as_deref())
        .await
    {
        tracing::warn!(%error, "could not record the filter install status");
    }
    pushed
}

async fn push_filters(state: &AppState, user: &User) -> Result<Pushed, Error> {
    let Some(config) = state.sieve.as_deref() else {
        let why = "webmail is not configured, so filters cannot be installed".to_owned();
        state
            .filters
            .mark(user.id, Script::Filters, Some(&why))
            .await?;
        return Ok(Pushed::Pending(why));
    };
    let filters = state.filters.filters(user.id).await?;
    let content = match filters.mode {
        FilterMode::Rules => rules_script(&filters.rules),
        FilterMode::Raw => filters.raw_script,
    };
    let outcome = rustmail_sieve::install(
        config,
        &user.email,
        &[(FILTERS_SCRIPT, &content), (MAIN_SCRIPT, &main_script())],
        MAIN_SCRIPT,
    )
    .await;
    Ok(record(state, user, Script::Filters, &outcome).await)
}

async fn push_vacation(state: &AppState, user: &User) -> Result<Pushed, Error> {
    let Some(config) = state.sieve.as_deref() else {
        let why = "webmail is not configured, so the autoresponder cannot be installed".to_owned();
        state
            .filters
            .mark(user.id, Script::Vacation, Some(&why))
            .await?;
        return Ok(Pushed::Pending(why));
    };
    let stored = state.filters.autoresponder(user.id).await?;
    let addresses = state.filters.own_addresses(user.id).await?;
    let content = vacation_script(&stored.settings, &addresses);
    let outcome = rustmail_sieve::install(
        config,
        &user.email,
        &[(VACATION_SCRIPT, &content), (MAIN_SCRIPT, &main_script())],
        MAIN_SCRIPT,
    )
    .await;
    Ok(record(state, user, Script::Vacation, &outcome).await)
}

fn after_push(pushed: &Pushed, base: &str) -> Response {
    match pushed {
        Pushed::Live => redirect_to(&format!("{base}?done=saved")),
        Pushed::Pending(_) => redirect_to(&format!("{base}?done=pending")),
        Pushed::Rejected(_) => redirect_to(&format!("{base}?done=failed")),
    }
}

struct StatusView {
    class: &'static str,
    key: &'static str,
    detail: String,
}

fn status_view(status: &SyncStatus, nothing_saved: bool) -> StatusView {
    match (&status.installed_at, &status.error) {
        (_, Some(e)) => StatusView {
            class: "warning",
            key: "filters-status-pending",
            detail: e.clone(),
        },
        (Some(_), None) => StatusView {
            class: "success",
            key: "filters-status-live",
            detail: String::new(),
        },
        (None, None) if nothing_saved => StatusView {
            class: "secondary",
            key: "filters-status-none",
            detail: String::new(),
        },
        (None, None) => StatusView {
            class: "secondary",
            key: "filters-status-unknown",
            detail: String::new(),
        },
    }
}

// ─── layout pieces ──────────────────────────────────────────────────────────

fn field_key(f: Field) -> String {
    format!("filters-field-{}", f.as_str())
}

fn op_key(o: Op) -> String {
    format!("filters-op-{}", o.as_str())
}

fn summarize(ctx: &PageCtx, rule: &Rule) -> (String, String) {
    let join = ctx.t(if rule.match_all {
        "filters-join-all"
    } else {
        "filters-join-any"
    });
    let conditions = rule
        .conditions
        .iter()
        .map(|c| {
            format!(
                "{} {} “{}”",
                ctx.t(&field_key(c.field)),
                ctx.t(&op_key(c.op)),
                c.value
            )
        })
        .collect::<Vec<_>>()
        .join(&format!(" {join} "));
    let actions = rule
        .actions
        .iter()
        .map(|a| match a {
            Action::MoveTo { folder } => ctx.t_str("filters-act-move", "folder", folder),
            Action::MarkRead => ctx.t("filters-act-read"),
            Action::Discard => ctx.t("filters-act-discard"),
            Action::Forward { address } => ctx.t_str("filters-act-forward", "address", address),
            Action::Stop => ctx.t("filters-act-stop"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    (conditions, actions)
}

// ─── the rule list ──────────────────────────────────────────────────────────

struct RuleLine {
    index: usize,
    name: String,
    enabled: bool,
    when: String,
    then: String,
    first: bool,
    last: bool,
}

#[derive(Template)]
#[template(path = "settings/filters.html")]
struct FiltersPage {
    ctx: PageCtx,
    rules: Vec<RuleLine>,
    status: StatusView,
    raw_mode: bool,
    can_apply: bool,
    can_add: bool,
    notice: Option<(&'static str, &'static str)>,
    count: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct DoneQuery {
    done: Option<String>,
    n: Option<i64>,
    rule: Option<String>,
}

pub(crate) async fn filters(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/settings/filters");
    let jar = ensure_cookie(&state.auth, jar);
    let saved = match state.filters.filters(web.user.id).await {
        Ok(f) => f,
        Err(e) => return page_error(&e),
    };
    let last = saved.rules.len().saturating_sub(1);
    let rules = saved
        .rules
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let (when, then) = summarize(&ctx, r);
            RuleLine {
                index: i,
                name: r.name.clone(),
                enabled: r.enabled,
                when,
                then,
                first: i == 0,
                last: i == last,
            }
        })
        .collect();
    let page = FiltersPage {
        rules,
        status: status_view(
            &saved.status,
            saved.rules.is_empty() && saved.raw_script.is_empty(),
        ),
        raw_mode: saved.mode == FilterMode::Raw,
        can_apply: state.mail.is_some()
            && saved.mode == FilterMode::Rules
            && !saved.rules.is_empty(),
        can_add: saved.rules.len() < MAX_RULES,
        notice: notice(q.done.as_deref()),
        count: q.n.map(|n| n.clamp(0, 100_000)),
        ctx,
    };
    (jar, render(&page)).into_response()
}

async fn load_rules(state: &AppState, user: &User) -> Result<Vec<Rule>, Box<Response>> {
    state
        .filters
        .filters(user.id)
        .await
        .map(|f| f.rules)
        .map_err(|e| Box::new(page_error(&e)))
}

async fn commit(state: &AppState, user: &User, rules: Vec<Rule>, done_base: &str) -> Response {
    if let Err(e) = state.filters.save_rules(user.id, &rules).await {
        return form_error(&e);
    }
    match push_filters(state, user).await {
        Ok(pushed) => after_push(&pushed, done_base),
        Err(e) => form_error(&e),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct IndexForm {
    #[serde(default)]
    enable: Option<String>,
}

pub(crate) async fn rule_toggle(
    State(state): State<AppState>,
    web: WebUser,
    Path(i): Path<usize>,
    Form(f): Form<IndexForm>,
) -> Response {
    let mut rules = match load_rules(&state, &web.user).await {
        Ok(r) => r,
        Err(response) => return *response,
    };
    let Some(rule) = rules.get_mut(i) else {
        return PageError::NotFound.into_response();
    };
    rule.enabled = f.enable.as_deref() == Some("1");
    commit(&state, &web.user, rules, "/settings/filters").await
}

pub(crate) async fn rule_move(
    State(state): State<AppState>,
    web: WebUser,
    Path((i, dir)): Path<(usize, String)>,
) -> Response {
    let mut rules = match load_rules(&state, &web.user).await {
        Ok(r) => r,
        Err(response) => return *response,
    };
    let target = match dir.as_str() {
        "up" if i > 0 => i - 1,
        "down" => i + 1,
        _ => return PageError::NotFound.into_response(),
    };
    if i >= rules.len() || target >= rules.len() {
        return PageError::NotFound.into_response();
    }
    rules.swap(i, target);
    commit(&state, &web.user, rules, "/settings/filters").await
}

pub(crate) async fn rule_delete(
    State(state): State<AppState>,
    web: WebUser,
    Path(i): Path<usize>,
) -> Response {
    let mut rules = match load_rules(&state, &web.user).await {
        Ok(r) => r,
        Err(response) => return *response,
    };
    if i >= rules.len() {
        return PageError::NotFound.into_response();
    }
    rules.remove(i);
    match state.filters.save_rules(web.user.id, &rules).await {
        Ok(()) => match push_filters(&state, &web.user).await {
            Ok(Pushed::Live) => redirect_to("/settings/filters?done=deleted"),
            Ok(other) => after_push(&other, "/settings/filters"),
            Err(e) => form_error(&e),
        },
        Err(e) => form_error(&e),
    }
}

// ─── one rule ───────────────────────────────────────────────────────────────

struct ConditionRow {
    n: usize,
    fields: Vec<Opt>,
    ops: Vec<Opt>,
    value: String,
}

#[derive(Template)]
#[template(path = "settings/rule.html")]
struct RulePage {
    ctx: PageCtx,
    index: Option<usize>,
    name: String,
    match_all: bool,
    rows: Vec<ConditionRow>,
    folders: Vec<Opt>,
    new_folder: String,
    mark_read: bool,
    discard: bool,
    forward: String,
    stop: bool,
}

async fn folder_names(state: &AppState, user: &User) -> Vec<String> {
    if state.mail.is_none() {
        return Vec::new();
    }
    with_session(state, &user.email, async |s| s.folders().await)
        .await
        .map(|f| {
            f.into_iter()
                .filter(|f| f.selectable)
                .map(|f| f.name)
                .collect()
        })
        .unwrap_or_default()
}

fn rule_page(
    ctx: PageCtx,
    index: Option<usize>,
    rule: &Rule,
    mut folder_list: Vec<String>,
) -> RulePage {
    let chosen_folder = rule.actions.iter().find_map(|a| {
        if let Action::MoveTo { folder } = a {
            Some(folder.clone())
        } else {
            None
        }
    });
    // A folder the rule names but that no longer exists must still be shown, or saving would change the rule.
    let mut new_folder = String::new();
    if let Some(f) = &chosen_folder {
        if !folder_list.iter().any(|x| x == f) {
            new_folder.clone_from(f);
        }
    }
    folder_list.retain(|f| !f.eq_ignore_ascii_case("inbox"));
    let current = chosen_folder.as_deref().unwrap_or("");
    let mut folders = vec![opt("", ctx.t("filters-no-folder"), current)];
    folders.extend(folder_list.into_iter().map(|f| opt(f.clone(), f, current)));
    let rows = (0..CONDITION_ROWS)
        .map(|n| {
            let c = rule.conditions.get(n);
            let (field, op) = c.map_or(("from", "contains"), |c| (c.field.as_str(), c.op.as_str()));
            ConditionRow {
                n: n + 1,
                fields: Field::ALL
                    .into_iter()
                    .map(|f| opt(f.as_str(), ctx.t(&field_key(f)), field))
                    .collect(),
                ops: Op::ALL
                    .into_iter()
                    .map(|o| opt(o.as_str(), ctx.t(&op_key(o)), op))
                    .collect(),
                value: c.map(|c| c.value.clone()).unwrap_or_default(),
            }
        })
        .collect();
    let has = |f: fn(&Action) -> bool| rule.actions.iter().any(f);
    RulePage {
        ctx,
        index,
        name: rule.name.clone(),
        match_all: rule.match_all,
        rows,
        folders,
        new_folder,
        mark_read: has(|a| matches!(a, Action::MarkRead)),
        discard: has(|a| matches!(a, Action::Discard)),
        forward: rule
            .actions
            .iter()
            .find_map(|a| {
                if let Action::Forward { address } = a {
                    Some(address.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default(),
        stop: has(|a| matches!(a, Action::Stop)),
    }
}

pub(crate) async fn rule_page_get(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/settings/filters");
    let jar = ensure_cookie(&state.auth, jar);
    let index = q.rule.as_deref().and_then(|r| r.parse::<usize>().ok());
    let rule = match index {
        Some(i) => match load_rules(&state, &web.user).await {
            Ok(rules) => match rules.into_iter().nth(i) {
                Some(r) => r,
                None => return PageError::NotFound.into_response(),
            },
            Err(response) => return *response,
        },
        None => Rule {
            name: String::new(),
            enabled: true,
            match_all: true,
            conditions: Vec::new(),
            actions: Vec::new(),
        },
    };
    let folders = folder_names(&state, &web.user).await;
    (jar, render(&rule_page(ctx, index, &rule, folders))).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct RuleForm {
    index: Option<String>,
    name: Option<String>,
    match_mode: Option<String>,
    c1_field: Option<String>,
    c1_op: Option<String>,
    c1_value: Option<String>,
    c2_field: Option<String>,
    c2_op: Option<String>,
    c2_value: Option<String>,
    c3_field: Option<String>,
    c3_op: Option<String>,
    c3_value: Option<String>,
    folder: Option<String>,
    new_folder: Option<String>,
    mark_read: Option<String>,
    discard: Option<String>,
    forward: Option<String>,
    stop: Option<String>,
}

impl RuleForm {
    fn rule(&self, enabled: bool) -> Result<Rule, Error> {
        let mut conditions = Vec::new();
        for (field, op, value) in [
            (&self.c1_field, &self.c1_op, &self.c1_value),
            (&self.c2_field, &self.c2_op, &self.c2_value),
            (&self.c3_field, &self.c3_op, &self.c3_value),
        ] {
            let value = value.as_deref().unwrap_or("").trim();
            if value.is_empty() {
                continue;
            }
            let field = Field::parse(field.as_deref().unwrap_or(""))
                .ok_or_else(|| Error::Validation("choose what to look at".into()))?;
            let op = Op::parse(op.as_deref().unwrap_or(""))
                .ok_or_else(|| Error::Validation("choose how to compare".into()))?;
            conditions.push(Condition {
                field,
                op,
                value: value.to_owned(),
            });
        }
        let mut actions = Vec::new();
        let typed = self.new_folder.as_deref().unwrap_or("").trim();
        let folder = if typed.is_empty() {
            self.folder.as_deref().unwrap_or("").trim()
        } else {
            typed
        };
        if !folder.is_empty() {
            actions.push(Action::MoveTo {
                folder: folder.to_owned(),
            });
        }
        if self.mark_read.is_some() {
            actions.push(Action::MarkRead);
        }
        if self.discard.is_some() {
            actions.push(Action::Discard);
        }
        if let Some(address) = self
            .forward
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            actions.push(Action::Forward {
                address: address.to_owned(),
            });
        }
        if self.stop.is_some() {
            actions.push(Action::Stop);
        }
        Ok(Rule {
            name: self.name.as_deref().unwrap_or("").trim().to_owned(),
            enabled,
            match_all: self.match_mode.as_deref() != Some("any"),
            conditions,
            actions,
        })
    }
}

pub(crate) async fn rule_save(
    State(state): State<AppState>,
    web: WebUser,
    Form(f): Form<RuleForm>,
) -> Response {
    let mut rules = match load_rules(&state, &web.user).await {
        Ok(r) => r,
        Err(response) => return *response,
    };
    let index = f
        .index
        .as_deref()
        .filter(|i| !i.is_empty())
        .map(str::parse::<usize>);
    let rule = match f.rule(match index {
        Some(Ok(i)) => rules.get(i).is_none_or(|r| r.enabled),
        _ => true,
    }) {
        Ok(rule) => rule,
        Err(e) => return form_error(&e),
    };
    if let Err(e) = rule.validate() {
        return form_error(&e);
    }
    match index {
        Some(Ok(i)) if i < rules.len() => rules[i] = rule,
        Some(_) => return PageError::NotFound.into_response(),
        None => rules.push(rule),
    }
    commit(&state, &web.user, rules, "/settings/filters").await
}

// ─── advanced editor ────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "settings/advanced.html")]
struct AdvancedPage {
    ctx: PageCtx,
    script: String,
    raw_mode: bool,
    status: StatusView,
    editor_script: String,
}

pub(crate) async fn advanced(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/settings/filters");
    let jar = ensure_cookie(&state.auth, jar);
    let saved = match state.filters.filters(web.user.id).await {
        Ok(f) => f,
        Err(e) => return page_error(&e),
    };
    // In rule mode the box starts with the script the rules generate, to build on.
    let raw_mode = saved.mode == FilterMode::Raw;
    let script = if raw_mode {
        saved.raw_script.clone()
    } else {
        rules_script(&saved.rules)
    };
    let page = AdvancedPage {
        editor_script: ctx.assets.url("vendor/codemirror/6/sieve-editor.js"),
        status: status_view(&saved.status, false),
        raw_mode,
        script,
        ctx,
    };
    (jar, render(&page)).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScriptForm {
    script: Option<String>,
}

pub(crate) async fn advanced_save(
    State(state): State<AppState>,
    web: WebUser,
    Form(f): Form<ScriptForm>,
) -> Response {
    let script = f.script.unwrap_or_default().replace("\r\n", "\n");
    // Ask the server first: a script it rejects is shown to the user and never saved.
    if let Some(config) = state.sieve.as_deref() {
        match rustmail_sieve::check(config, &web.user.email, &script).await {
            Ok(()) => {}
            Err(e) if e.is_script_error() => {
                return alert(StatusCode::UNPROCESSABLE_ENTITY, "danger", e.to_string())
            }
            Err(error) => {
                tracing::warn!(%error, "could not pre-check the script; saving it anyway")
            }
        }
    }
    if let Err(e) = state.filters.save_raw(web.user.id, &script).await {
        return form_error(&e);
    }
    match push_filters(&state, &web.user).await {
        Ok(Pushed::Rejected(text)) => alert(StatusCode::UNPROCESSABLE_ENTITY, "danger", text),
        Ok(pushed) => after_push(&pushed, "/settings/filters/advanced"),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn advanced_leave(State(state): State<AppState>, web: WebUser) -> Response {
    if let Err(e) = state.filters.use_rules(web.user.id).await {
        return form_error(&e);
    }
    match push_filters(&state, &web.user).await {
        Ok(Pushed::Live) => redirect_to("/settings/filters?done=raw-left"),
        Ok(other) => after_push(&other, "/settings/filters"),
        Err(e) => form_error(&e),
    }
}

/// Sends the saved filters to the mail server again (after it was unreachable).
pub(crate) async fn retry(State(state): State<AppState>, web: WebUser) -> Response {
    match push_filters(&state, &web.user).await {
        Ok(Pushed::Live) => redirect_to("/settings/filters?done=synced"),
        Ok(Pushed::Pending(text) | Pushed::Rejected(text)) => {
            alert(StatusCode::UNPROCESSABLE_ENTITY, "danger", text)
        }
        Err(e) => form_error(&e),
    }
}

// ─── fallback: apply the rules to the inbox ─────────────────────────────────

pub(crate) async fn apply_to_inbox(State(state): State<AppState>, web: WebUser) -> Response {
    let rules = match load_rules(&state, &web.user).await {
        Ok(r) => r,
        Err(response) => return *response,
    };
    let result = with_session(&state, &web.user.email, async |s| {
        let page = s.list("INBOX", 1, APPLY_LIMIT).await?;
        let mut known: HashSet<String> = s.folders().await?.into_iter().map(|f| f.name).collect();
        let mut handled = 0_i64;
        for m in page.items {
            let facts = Facts {
                from: m.from.iter().map(|a| a.email.clone()).collect(),
                to: m.to.iter().map(|a| a.email.clone()).collect(),
                cc: m.cc.iter().map(|a| a.email.clone()).collect(),
                subject: m.subject.clone(),
                list_id: String::new(),
            };
            let outcome = evaluate(&rules, &facts);
            if outcome.mark_read && !m.flags.seen {
                s.set_flag("INBOX", m.uid, FlagKind::Seen, true).await?;
                handled += 1;
            }
            if let Some(folder) = outcome.move_to {
                if known.insert(folder.clone()) {
                    // Sieve creates missing folders on delivery; do the same here.
                    s.create_folder(&folder).await?;
                }
                s.move_message("INBOX", m.uid, &folder).await?;
                handled += 1;
            }
        }
        Ok(handled)
    })
    .await;
    match result {
        Ok(n) => redirect_to(&format!("/settings/filters?done=applied&n={n}")),
        Err(error) => {
            tracing::warn!(?error, "applying filters to the inbox failed");
            alert(
                StatusCode::UNPROCESSABLE_ENTITY,
                "danger",
                "The inbox could not be filtered. Nothing was lost; try again later.",
            )
        }
    }
}

// ─── autoresponder ──────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "settings/autoresponder.html")]
struct AutoresponderPage {
    ctx: PageCtx,
    a: Autoresponder,
    starts_on: String,
    ends_on: String,
    status: StatusView,
    addresses: Vec<String>,
    notice: Option<(&'static str, &'static str)>,
}

pub(crate) async fn autoresponder(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let ctx = page_ctx(
        &state,
        &jar,
        &headers,
        Some(&web.user),
        "/settings/autoresponder",
    );
    let jar = ensure_cookie(&state.auth, jar);
    let (stored, addresses) = match tokio::try_join!(
        state.filters.autoresponder(web.user.id),
        state.filters.own_addresses(web.user.id)
    ) {
        Ok(v) => v,
        Err(e) => return page_error(&e),
    };
    let day = |d: Option<NaiveDate>| {
        d.map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    };
    let page = AutoresponderPage {
        starts_on: day(stored.settings.starts_on),
        ends_on: day(stored.settings.ends_on),
        status: status_view(
            &stored.status,
            !stored.settings.enabled && stored.settings.body.is_empty(),
        ),
        a: stored.settings,
        addresses,
        notice: notice(q.done.as_deref()),
        ctx,
    };
    (jar, render(&page)).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct AutoresponderForm {
    enabled: Option<String>,
    starts_on: Option<String>,
    ends_on: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    days: Option<String>,
}

fn parse_day(text: Option<&str>, what: &str) -> Result<Option<NaiveDate>, Error> {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(t) => NaiveDate::parse_from_str(t, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| Error::Validation(format!("the {what} date is not a valid date"))),
    }
}

pub(crate) async fn autoresponder_save(
    State(state): State<AppState>,
    web: WebUser,
    Form(f): Form<AutoresponderForm>,
) -> Response {
    let parsed = (|| {
        Ok::<_, Error>(Autoresponder {
            enabled: f.enabled.is_some(),
            starts_on: parse_day(f.starts_on.as_deref(), "start")?,
            ends_on: parse_day(f.ends_on.as_deref(), "end")?,
            subject: f.subject.clone().unwrap_or_default().trim().to_owned(),
            body: f.body.clone().unwrap_or_default().replace("\r\n", "\n"),
            days: f
                .days
                .as_deref()
                .unwrap_or("7")
                .trim()
                .parse()
                .map_err(|_| {
                    Error::Validation("the reply interval must be a number of days".into())
                })?,
        })
    })();
    let settings = match parsed {
        Ok(s) => s,
        Err(e) => return form_error(&e),
    };
    if let Err(e) = state
        .filters
        .save_autoresponder(web.user.id, &settings)
        .await
    {
        return form_error(&e);
    }
    match push_vacation(&state, &web.user).await {
        Ok(Pushed::Rejected(text)) => alert(StatusCode::UNPROCESSABLE_ENTITY, "danger", text),
        Ok(pushed) => after_push(&pushed, "/settings/autoresponder"),
        Err(e) => form_error(&e),
    }
}

pub(crate) async fn autoresponder_retry(State(state): State<AppState>, web: WebUser) -> Response {
    match push_vacation(&state, &web.user).await {
        Ok(Pushed::Live) => redirect_to("/settings/autoresponder?done=synced"),
        Ok(Pushed::Pending(text) | Pushed::Rejected(text)) => {
            alert(StatusCode::UNPROCESSABLE_ENTITY, "danger", text)
        }
        Err(e) => form_error(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> RuleForm {
        RuleForm {
            index: None,
            name: Some("  Boss ".into()),
            match_mode: Some("all".into()),
            c1_field: Some("from".into()),
            c1_op: Some("contains".into()),
            c1_value: Some(" boss@ ".into()),
            c2_field: Some("subject".into()),
            c2_op: Some("is".into()),
            c2_value: Some("".into()),
            c3_field: None,
            c3_op: None,
            c3_value: None,
            folder: Some("Archive".into()),
            new_folder: Some("Boss".into()),
            mark_read: Some("on".into()),
            discard: None,
            forward: Some("".into()),
            stop: None,
        }
    }

    #[test]
    fn the_form_becomes_a_rule() {
        let rule = form().rule(true).unwrap();
        assert_eq!(rule.name, "Boss");
        assert_eq!(
            rule.conditions,
            vec![Condition {
                field: Field::From,
                op: Op::Contains,
                value: "boss@".into()
            }],
            "empty rows are skipped, text is trimmed"
        );
        assert_eq!(
            rule.actions,
            vec![
                Action::MoveTo {
                    folder: "Boss".into()
                },
                Action::MarkRead
            ],
            "a typed folder wins over the list"
        );
        assert!(rule.match_all);
        rule.validate().unwrap();
    }

    #[test]
    fn unknown_choices_are_refused() {
        let mut f = form();
        f.c1_field = Some("received".into());
        assert!(f.rule(true).is_err());
        let mut f = form();
        f.c1_op = Some("matches_regex".into());
        assert!(f.rule(true).is_err());
        let mut f = form();
        f.match_mode = Some("any".into());
        assert!(!f.rule(true).unwrap().match_all);
    }

    #[test]
    fn a_rule_with_no_condition_or_action_does_not_validate() {
        let mut f = form();
        f.c1_value = None;
        assert!(f.rule(true).unwrap().validate().is_err());
        let mut f = form();
        f.new_folder = None;
        f.folder = Some(String::new());
        f.mark_read = None;
        assert!(f.rule(true).unwrap().validate().is_err());
    }

    #[test]
    fn only_known_notices_are_shown() {
        assert!(notice(Some("saved")).is_some());
        assert!(notice(Some("<b>x</b>")).is_none());
    }

    #[test]
    fn days_parse_strictly() {
        assert_eq!(
            parse_day(Some(" 2025-12-24 "), "end").unwrap(),
            NaiveDate::from_ymd_opt(2025, 12, 24)
        );
        assert_eq!(parse_day(Some(""), "end").unwrap(), None);
        assert!(parse_day(Some("24.12.2025"), "end").is_err());
    }
}
