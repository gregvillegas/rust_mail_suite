//! The webmail pages (Phase 6).
//!
//! | Route | Method | Purpose |
//! |---|---|---|
//! | `/mail`, `/mail/inbox` | GET | three-pane view: folders, message list, preview |
//! | `/mail/list`, `/mail/folders/nav` | GET | htmx partials that refresh the list and the folder list |
//! | `/mail/message/{uid}` | GET | preview (htmx) or the full page with that message open |
//! | `/mail/message/{uid}/body` | GET | sanitised HTML of a message, for the sandboxed iframe |
//! | `/mail/message/{uid}/attachment/{index}` | GET | download an attachment |
//! | `/mail/compose` | GET | new message, reply, reply all, forward |
//! | `/mail/send` | POST | send (multipart: attachments) |
//! | `/mail/message/{uid}/flag`, `/move`, `/delete`, `/spam` | POST | act on a message |
//! | `/mail/folders` | GET | manage folders |
//! | `/mail/folders/create`, `/rename`, `/delete` | POST | folder changes |
//!
//! Every handler acts as the signed-in user, through the master login (see
//! [`rustmail_imap::config`]); there is no way to name another user. A message is
//! always addressed by folder and UID, both validated before they reach IMAP.

use std::sync::Arc;

use askama::Template;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Datelike, Utc};
use rustmail_core::domain::User;
use rustmail_core::mailindex::IndexedMessage;
use rustmail_imap::compose::{
    build, forward_body, forward_subject, parse_recipients, quote_for_reply, reply_all_recipients,
    reply_subject, Draft, OutAttachment,
};
use rustmail_imap::folder::{Folder, SpecialUse};
use rustmail_imap::message::{self, Address, Flags, ParsedMessage};
use rustmail_imap::session::{FlagKind, RawMessage};
use rustmail_imap::{submit, MailError, MailSession};
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::csrf::ensure_cookie;
use crate::mail::{page_error, MailService};
use crate::pages::{encode_query_value, is_htmx, page_ctx, WebUser};
use crate::ui::{render, render_with_status, PageCtx, PageError};

const HX_TRIGGER: HeaderName = HeaderName::from_static("hx-trigger");
const HX_REDIRECT: HeaderName = HeaderName::from_static("hx-redirect");
const PAGE_SIZE: u32 = 25;
const MAX_FOLDER_PARAM: usize = 220;
const MAX_SEARCH_CHARS: usize = 200;
const MAX_TEXT_CHARS: usize = 200_000;
/// Largest request body for sending: attachments plus MIME overhead headroom.
pub const SEND_BODY_LIMIT: usize = 36 * 1024 * 1024;

// ─── views ──────────────────────────────────────────────────────────────────

struct FolderView {
    label: String,
    url: String,
    unread: u32,
    depth: usize,
    active: bool,
    selectable: bool,
}

struct RowView {
    uid: u32,
    subject: String,
    from: String,
    when: String,
    iso: String,
    unread: bool,
    starred: bool,
    attachment: bool,
    snippet: String,
    open_url: String,
    preview_url: String,
    selected: bool,
}

struct ListView {
    folder_name: String,
    folder_label: String,
    rows: Vec<RowView>,
    total: i64,
    page: i64,
    prev_url: Option<String>,
    next_url: Option<String>,
    search: String,
    searching: bool,
    refresh_url: String,
    index_note: bool,
}

struct AttachmentView {
    name: String,
    size: String,
    url: String,
}

struct PreviewView {
    uid: u32,
    folder_name: String,
    subject: String,
    from: String,
    to: String,
    cc: String,
    when: String,
    iso: String,
    in_junk: bool,
    starred: bool,
    html_url: Option<String>,
    images_blocked: bool,
    load_images_url: String,
    text_url: String,
    html_toggle_url: String,
    showing_text_of_html: bool,
    text: String,
    attachments: Vec<AttachmentView>,
    reply_url: String,
    reply_all_url: String,
    forward_url: String,
    move_targets: Vec<(String, String)>,
}

struct Notice {
    class: &'static str,
    key: &'static str,
}

#[derive(Template)]
#[template(path = "mail/index.html")]
struct MailIndexPage {
    ctx: PageCtx,
    folders: Vec<FolderView>,
    folders_url: String,
    list: ListView,
    preview: Option<PreviewView>,
    notice: Option<Notice>,
    folder_enc: String,
}

#[derive(Template)]
#[template(path = "mail/_list.html")]
struct ListPartial {
    ctx: PageCtx,
    list: ListView,
}

#[derive(Template)]
#[template(path = "mail/_folders.html")]
struct FolderNavPartial {
    ctx: PageCtx,
    folders: Vec<FolderView>,
    folders_url: String,
}

#[derive(Template)]
#[template(path = "mail/_preview.html")]
struct PreviewPartial {
    ctx: PageCtx,
    preview: Option<PreviewView>,
}

#[derive(Template)]
#[template(path = "mail/_alert.html")]
struct AlertPartial {
    class: &'static str,
    text: String,
}

#[derive(Template)]
#[template(path = "mail/unconfigured.html")]
struct UnconfiguredPage {
    ctx: PageCtx,
}

struct ComposeView {
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    in_reply_to: String,
    references: String,
    answered_folder: String,
    answered_uid: String,
    from: String,
    show_cc: bool,
    heading_key: &'static str,
}

#[derive(Template)]
#[template(path = "mail/compose.html")]
struct ComposePage {
    ctx: PageCtx,
    c: ComposeView,
}

struct FolderRow {
    label: String,
    name: String,
    total: u32,
    unread: u32,
    depth: usize,
    standard: bool,
    selectable: bool,
}

#[derive(Template)]
#[template(path = "mail/folders.html")]
struct FoldersPage {
    ctx: PageCtx,
    rows: Vec<FolderRow>,
    notice: Option<Notice>,
}

// ─── parameters ─────────────────────────────────────────────────────────────

/// Query parameters shared by the mail pages.
#[derive(Debug, Default, Deserialize)]
pub struct MailQuery {
    folder: Option<String>,
    page: Option<u32>,
    q: Option<String>,
    open: Option<u32>,
    images: Option<u8>,
    plain: Option<u8>,
    done: Option<String>,
}

/// Parameters of the compose page.
#[derive(Debug, Default, Deserialize)]
pub struct ComposeQuery {
    folder: Option<String>,
    reply: Option<u32>,
    all: Option<u8>,
    forward: Option<u32>,
    to: Option<String>,
}

/// A folder name from a query string or form, checked before it is used.
fn folder_param(raw: Option<&str>) -> Result<String, PageError> {
    let folder = raw
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .unwrap_or("INBOX");
    if folder.chars().count() > MAX_FOLDER_PARAM || folder.chars().any(char::is_control) {
        return Err(PageError::BadRequest);
    }
    Ok(folder.to_owned())
}

fn enc(value: &str) -> String {
    encode_query_value(value)
}

fn list_url(folder: &str, page: u32, q: &str, open: Option<u32>) -> String {
    let mut url = format!("/mail?folder={}", enc(folder));
    if page > 1 {
        url.push_str(&format!("&page={page}"));
    }
    if !q.is_empty() {
        url.push_str(&format!("&q={}", enc(q)));
    }
    if let Some(uid) = open {
        url.push_str(&format!("&open={uid}"));
    }
    url
}

fn message_url(folder: &str, uid: u32) -> String {
    format!("/mail/message/{uid}?folder={}", enc(folder))
}

fn human_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// `14:05` for today, `Nov 11` for this year, `2024-11-11` otherwise (UTC).
fn short_date(when: Option<DateTime<Utc>>, now: DateTime<Utc>) -> (String, String) {
    let Some(when) = when else {
        return (String::new(), String::new());
    };
    let label = if when.date_naive() == now.date_naive() {
        when.format("%H:%M").to_string()
    } else if when.year() == now.year() {
        when.format("%b %e").to_string().replace("  ", " ")
    } else {
        when.format("%Y-%m-%d").to_string()
    };
    (label, when.to_rfc3339())
}

fn long_date(when: Option<DateTime<Utc>>) -> (String, String) {
    when.map_or_else(Default::default, |w| {
        (
            w.format("%a, %d %b %Y %H:%M UTC").to_string(),
            w.to_rfc3339(),
        )
    })
}

fn addresses_line(list: &[Address]) -> String {
    list.iter()
        .map(Address::display)
        .collect::<Vec<_>>()
        .join(", ")
}

fn folder_label(ctx: &PageCtx, folder: &Folder) -> String {
    match folder.special {
        Some(special) => ctx.t(&format!("mail-folder-{}", special.key())),
        None => folder.leaf().to_owned(),
    }
}

fn folder_views(ctx: &PageCtx, folders: &[Folder], current: &str) -> Vec<FolderView> {
    folders
        .iter()
        .map(|f| FolderView {
            label: folder_label(ctx, f),
            url: list_url(&f.name, 1, "", None),
            unread: f.unread,
            depth: f.depth(),
            active: f.name == current,
            selectable: f.selectable,
        })
        .collect()
}

fn service(state: &AppState) -> Option<Arc<MailService>> {
    state.mail.clone()
}

fn unconfigured(state: &AppState, jar: &CookieJar, headers: &HeaderMap, user: &User) -> Response {
    let ctx = page_ctx(state, jar, headers, Some(user), "/mail");
    render_with_status(StatusCode::SERVICE_UNAVAILABLE, &UnconfiguredPage { ctx })
}

fn trigger(event: &'static str) -> [(HeaderName, HeaderValue); 1] {
    [(HX_TRIGGER, HeaderValue::from_static(event))]
}

fn alert(status: StatusCode, class: &'static str, text: impl Into<String>) -> Response {
    render_with_status(
        status,
        &AlertPartial {
            class,
            text: text.into(),
        },
    )
}

// ─── building the pages ─────────────────────────────────────────────────────

fn row_from_summary(
    folder: &str,
    s: &message::MessageSummary,
    open: Option<u32>,
    page: u32,
    q: &str,
    now: DateTime<Utc>,
) -> RowView {
    let (when, iso) = short_date(s.received.or(s.date), now);
    RowView {
        uid: s.uid,
        subject: if s.subject.is_empty() {
            String::new()
        } else {
            s.subject.clone()
        },
        from: s.from.as_ref().map(Address::short).unwrap_or_default(),
        when,
        iso,
        unread: !s.flags.seen,
        starred: s.flags.flagged,
        attachment: s.has_attachments,
        snippet: String::new(),
        open_url: list_url(folder, page, q, Some(s.uid)),
        preview_url: message_url(folder, s.uid),
        selected: open == Some(s.uid),
    }
}

fn indexed_from_parsed(
    folder: &str,
    raw: &RawMessage,
    size: usize,
    parsed: &ParsedMessage,
    uid: u32,
) -> IndexedMessage {
    IndexedMessage {
        folder: folder.to_owned(),
        uid_validity: i64::from(raw.uid_validity),
        imap_uid: i64::from(uid),
        message_id: parsed.message_id.clone(),
        in_reply_to: parsed.in_reply_to.clone(),
        subject: parsed.subject.clone(),
        from_addr: parsed
            .from
            .as_ref()
            .map(Address::display)
            .unwrap_or_default(),
        to_addrs: addresses_line(&parsed.to),
        cc_addrs: addresses_line(&parsed.cc),
        snippet: message::snippet(parsed),
        size_bytes: i64::try_from(size).unwrap_or(i64::MAX),
        flags: Flags {
            seen: true,
            ..raw.flags
        }
        .names(),
        has_attachments: !parsed.attachments.is_empty(),
        date_header: parsed.date,
        received_at: parsed.date.unwrap_or_else(Utc::now),
    }
}

/// What building a preview needs besides the session.
struct Env<'a> {
    service: &'a MailService,
    ctx: &'a PageCtx,
    mailbox_id: Option<uuid::Uuid>,
    folders: &'a [Folder],
}

async fn build_preview(
    env: &Env<'_>,
    session: &mut MailSession,
    folder: &str,
    uid: u32,
    query: &MailQuery,
    mark_read: bool,
) -> Result<PreviewView, PageError> {
    let (service, ctx, mailbox_id, all_folders) =
        (env.service, env.ctx, env.mailbox_id, env.folders);
    let raw = session
        .message(folder, uid, mark_read)
        .await
        .map_err(|e| page_error(&e))?;
    let parsed = message::parse(&raw.bytes).map_err(|e| page_error(&e))?;
    if let Some(mailbox_id) = mailbox_id {
        // Opening a message teaches the index its text; failures here do not matter.
        let row = indexed_from_parsed(folder, &raw, raw.bytes.len(), &parsed, uid);
        if let Err(error) = service.index.upsert(mailbox_id, &[row]).await {
            tracing::debug!(%error, "could not update the search index");
        }
    }
    let images = query.images == Some(1);
    let plain = query.plain == Some(1);
    let folder_enc = enc(folder);
    let base = message_url(folder, uid);
    let (when, iso) = long_date(parsed.date);
    let has_html = parsed.html.as_deref().is_some_and(|h| !h.trim().is_empty());
    let show_html = has_html && !plain;
    let images_blocked = show_html
        && !images
        && parsed
            .html
            .as_deref()
            .is_some_and(message::has_remote_images);
    let mut text = parsed.text.clone();
    if text.chars().count() > MAX_TEXT_CHARS {
        text = text.chars().take(MAX_TEXT_CHARS).collect();
    }
    let junk = all_folders
        .iter()
        .find(|f| f.special == Some(SpecialUse::Junk))
        .map(|f| f.name.clone());
    Ok(PreviewView {
        uid,
        subject: if parsed.subject.is_empty() {
            ctx.t("mail-no-subject")
        } else {
            parsed.subject.clone()
        },
        from: parsed
            .from
            .as_ref()
            .map(Address::display)
            .unwrap_or_default(),
        to: addresses_line(&parsed.to),
        cc: addresses_line(&parsed.cc),
        when,
        iso,
        in_junk: junk.as_deref() == Some(folder),
        folder_name: folder.to_owned(),
        starred: raw.flags.flagged,
        html_url: show_html.then(|| {
            format!(
                "/mail/message/{uid}/body?folder={folder_enc}&images={}",
                u8::from(images)
            )
        }),
        images_blocked,
        load_images_url: format!("{base}&images=1"),
        text_url: format!("{base}&plain=1"),
        html_toggle_url: base.clone(),
        showing_text_of_html: has_html && plain,
        text,
        attachments: parsed
            .attachments
            .iter()
            .map(|a| AttachmentView {
                name: a.filename.clone(),
                size: human_size(a.size),
                url: format!(
                    "/mail/message/{uid}/attachment/{}?folder={folder_enc}",
                    a.index
                ),
            })
            .collect(),
        reply_url: format!("/mail/compose?folder={folder_enc}&reply={uid}"),
        reply_all_url: format!("/mail/compose?folder={folder_enc}&reply={uid}&all=1"),
        forward_url: format!("/mail/compose?folder={folder_enc}&forward={uid}"),
        move_targets: all_folders
            .iter()
            .filter(|f| f.selectable && f.name != folder)
            .map(|f| (f.name.clone(), folder_label(ctx, f)))
            .collect(),
    })
}

// ─── handlers: reading ──────────────────────────────────────────────────────

pub(crate) async fn index(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(query): Query<MailQuery>,
) -> Response {
    let Some(service) = service(&state) else {
        return unconfigured(&state, &jar, &headers, &web.user);
    };
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail");
    let jar = ensure_cookie(&state.auth, jar);
    match load_index(&service, &web.user, &ctx, &query).await {
        Ok(page) => (jar, render(&page)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// Loads everything the three-pane page shows, in one IMAP session.
async fn load_index(
    service: &Arc<MailService>,
    user: &User,
    ctx: &PageCtx,
    query: &MailQuery,
) -> Result<MailIndexPage, PageError> {
    let folder = folder_param(query.folder.as_deref())?;
    let mut session = service.session(&user.email).await?;
    let result = load_index_with(&mut session, service, user, ctx, query, &folder).await;
    session.close().await;
    result
}

async fn load_index_with(
    session: &mut MailSession,
    service: &Arc<MailService>,
    user: &User,
    ctx: &PageCtx,
    query: &MailQuery,
    folder: &str,
) -> Result<MailIndexPage, PageError> {
    let folders = session.folders().await.map_err(|e| page_error(&e))?;
    if !folders.iter().any(|f| f.name == folder && f.selectable) {
        return Err(PageError::NotFound);
    }
    let mailbox_id = service
        .index
        .mailbox_id(user.id)
        .await
        .map_err(|e| page_error(&MailError::from(e)))?;
    if let Some(id) = mailbox_id {
        service.spawn_sync(user.email.clone(), id);
    }
    let list = load_list(session, service, ctx, mailbox_id, &folders, folder, query).await?;
    let preview = match query.open {
        Some(uid) => match build_preview(
            &Env {
                service,
                ctx,
                mailbox_id,
                folders: &folders,
            },
            session,
            folder,
            uid,
            query,
            true,
        )
        .await
        {
            Ok(p) => Some(p),
            // A message that is gone (moved elsewhere, deleted) just means no preview.
            Err(PageError::NotFound) => None,
            Err(other) => return Err(other),
        },
        None => None,
    };
    let notice = match query.done.as_deref() {
        Some("sent") => Some(Notice {
            class: "success",
            key: "mail-notice-sent",
        }),
        _ => None,
    };
    Ok(MailIndexPage {
        ctx: ctx.clone(),
        folders: folder_views(ctx, &folders, folder),
        folders_url: "/mail/folders".to_owned(),
        list,
        preview,
        notice,
        folder_enc: enc(folder),
    })
}

async fn load_list(
    session: &mut MailSession,
    service: &MailService,
    ctx: &PageCtx,
    mailbox_id: Option<uuid::Uuid>,
    folders: &[Folder],
    folder: &str,
    query: &MailQuery,
) -> Result<ListView, PageError> {
    let page = query.page.unwrap_or(1).clamp(1, 10_000);
    let q: String = query
        .q
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(MAX_SEARCH_CHARS)
        .collect();
    let now = Utc::now();
    let current = folders.iter().find(|f| f.name == folder);
    let folder_label = current.map_or_else(|| folder.to_owned(), |f| folder_label(ctx, f));
    let refresh_url = format!(
        "/mail/list?folder={}&page={page}&q={}{}",
        enc(folder),
        enc(&q),
        query.open.map_or(String::new(), |u| format!("&open={u}"))
    );

    if !q.is_empty() {
        let offset = i64::from(page - 1) * i64::from(PAGE_SIZE);
        let (hits, total, index_note) = match mailbox_id {
            Some(id) => match service
                .index
                .search(id, &q, i64::from(PAGE_SIZE), offset)
                .await
            {
                Ok(found) => {
                    let empty_index =
                        found.total == 0 && service.index.count(id).await.unwrap_or(0) == 0;
                    (found.hits, found.total, empty_index)
                }
                Err(rustmail_core::Error::Validation(_)) => (Vec::new(), 0, false),
                Err(error) => return Err(page_error(&MailError::from(error))),
            },
            None => (Vec::new(), 0, true),
        };
        let rows = hits
            .into_iter()
            .map(|h| {
                let (when, iso) = short_date(Some(h.received_at), now);
                let uid = u32::try_from(h.imap_uid).unwrap_or(0);
                RowView {
                    uid,
                    subject: h.subject,
                    from: h
                        .from_addr
                        .split('<')
                        .next()
                        .unwrap_or(&h.from_addr)
                        .trim()
                        .to_owned(),
                    when,
                    iso,
                    unread: !h.flags.iter().any(|f| f == "\\Seen"),
                    starred: h.flags.iter().any(|f| f == "\\Flagged"),
                    attachment: h.has_attachments,
                    snippet: h.snippet,
                    open_url: list_url(&h.folder, 1, "", Some(uid)),
                    preview_url: message_url(&h.folder, uid),
                    selected: false,
                }
            })
            .collect();
        return Ok(ListView {
            folder_name: folder.to_owned(),
            folder_label,
            rows,
            total,
            page: i64::from(page),
            prev_url: (page > 1).then(|| list_url(folder, page - 1, &q, None)),
            next_url: (i64::from(page) * i64::from(PAGE_SIZE) < total)
                .then(|| list_url(folder, page + 1, &q, None)),
            search: q,
            searching: true,
            refresh_url,
            index_note,
        });
    }

    let listing = session
        .list(folder, page, PAGE_SIZE)
        .await
        .map_err(|e| page_error(&e))?;
    let total = i64::from(listing.total);
    Ok(ListView {
        folder_name: folder.to_owned(),
        folder_label,
        rows: listing
            .items
            .iter()
            .map(|s| row_from_summary(folder, s, query.open, page, "", now))
            .collect(),
        total,
        page: i64::from(page),
        prev_url: (page > 1).then(|| list_url(folder, page - 1, "", None)),
        next_url: (i64::from(page) * i64::from(PAGE_SIZE) < total)
            .then(|| list_url(folder, page + 1, "", None)),
        search: String::new(),
        searching: false,
        refresh_url,
        index_note: false,
    })
}

pub(crate) async fn list_partial(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(query): Query<MailQuery>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::Unavailable.into_response();
    };
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail");
    let result = async {
        let folder = folder_param(query.folder.as_deref())?;
        let mut session = service.session(&web.user.email).await?;
        let folders = session.folders().await.map_err(|e| page_error(&e));
        let out = match folders {
            Ok(folders) => {
                let mailbox_id = service
                    .index
                    .mailbox_id(web.user.id)
                    .await
                    .map_err(|e| page_error(&MailError::from(e)))?;
                load_list(
                    &mut session,
                    &service,
                    &ctx,
                    mailbox_id,
                    &folders,
                    &folder,
                    &query,
                )
                .await
            }
            Err(e) => Err(e),
        };
        session.close().await;
        out
    }
    .await;
    match result {
        Ok(list) => render(&ListPartial { ctx, list }),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn folder_nav_partial(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(query): Query<MailQuery>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::Unavailable.into_response();
    };
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail");
    let result = async {
        let folder = folder_param(query.folder.as_deref())?;
        let mut session = service.session(&web.user.email).await?;
        let folders = session.folders().await.map_err(|e| page_error(&e));
        session.close().await;
        Ok::<_, PageError>((folder, folders?))
    }
    .await;
    match result {
        Ok((folder, folders)) => render(&FolderNavPartial {
            folders: folder_views(&ctx, &folders, &folder),
            folders_url: "/mail/folders".to_owned(),
            ctx,
        }),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn inbox_redirect() -> Redirect {
    Redirect::to("/mail?folder=INBOX")
}

pub(crate) async fn preview(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Path(uid): Path<u32>,
    Query(query): Query<MailQuery>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::Unavailable.into_response();
    };
    let folder = match folder_param(query.folder.as_deref()) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    // Typed into the address bar or followed from outside htmx: show the full page.
    if !is_htmx(&headers) {
        return Redirect::to(&list_url(&folder, 1, "", Some(uid))).into_response();
    }
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail");
    match preview_view(&service, &web.user, &ctx, &folder, uid, &query, true).await {
        // Opening marks it read: refresh the list and the folder counters.
        Ok(p) => (
            trigger("mail-refresh"),
            render(&PreviewPartial {
                ctx,
                preview: Some(p),
            }),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

/// Fetches one message and builds its preview in a fresh session.
async fn preview_view(
    service: &Arc<MailService>,
    user: &User,
    ctx: &PageCtx,
    folder: &str,
    uid: u32,
    query: &MailQuery,
    mark_read: bool,
) -> Result<PreviewView, PageError> {
    let mut session = service.session(&user.email).await?;
    let out = async {
        let folders = session.folders().await.map_err(|e| page_error(&e))?;
        let mailbox_id = service
            .index
            .mailbox_id(user.id)
            .await
            .map_err(|e| page_error(&MailError::from(e)))?;
        build_preview(
            &Env {
                service,
                ctx,
                mailbox_id,
                folders: &folders,
            },
            &mut session,
            folder,
            uid,
            query,
            mark_read,
        )
        .await
    }
    .await;
    session.close().await;
    out
}

/// Wraps sanitised mail HTML in a minimal document.
fn html_document(body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <style>body{{font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;font-size:15px;line-height:1.45;margin:12px;overflow-wrap:anywhere}}\
         img{{max-width:100%;height:auto}}table{{max-width:100%}}blockquote{{margin:0 0 0 .5rem;padding-left:.75rem;border-left:3px solid #ccc;color:#555}}</style>\
         </head><body>{body}</body></html>"
    )
}

pub(crate) async fn body(
    State(state): State<AppState>,
    web: WebUser,
    Path(uid): Path<u32>,
    Query(query): Query<MailQuery>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::Unavailable.into_response();
    };
    let folder = match folder_param(query.folder.as_deref()) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let images = query.images == Some(1);
    let raw = async {
        let mut session = service.session(&web.user.email).await?;
        let out = session
            .message(&folder, uid, false)
            .await
            .map_err(|e| page_error(&e));
        session.close().await;
        out
    }
    .await;
    let html = match raw.and_then(|r| message::parse(&r.bytes).map_err(|e| page_error(&e))) {
        Ok(parsed) => parsed.html.unwrap_or_default(),
        Err(error) => return error.into_response(),
    };
    let document = html_document(&message::sanitize_html(&html, images));
    // The page is shown in a sandboxed iframe, and says so itself too: no scripts,
    // no forms, no framing by anyone else, and remote images only when asked for.
    let img = if images { "https: http:" } else { "'none'" };
    let csp = format!(
        "sandbox allow-popups allow-popups-to-escape-sandbox; default-src 'none'; style-src 'unsafe-inline'; \
         img-src {img}; base-uri 'none'; form-action 'none'; frame-ancestors 'self'"
    );
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_owned()),
            (header::CONTENT_SECURITY_POLICY, csp),
            (header::X_FRAME_OPTIONS, "SAMEORIGIN".to_owned()),
            (header::REFERRER_POLICY, "no-referrer".to_owned()),
            (header::CACHE_CONTROL, "private, no-store".to_owned()),
        ],
        document,
    )
        .into_response()
}

/// Content types that are served as themselves (always as a download).
fn download_type(declared: &str) -> &'static str {
    match declared.to_ascii_lowercase().as_str() {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "application/pdf" => "application/pdf",
        "text/plain" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// An RFC 6266 `Content-Disposition` for a download.
fn attachment_disposition(filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .map(|c| {
            if matches!(c, '"' | '\\' | '%' | ';') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

pub(crate) async fn attachment(
    State(state): State<AppState>,
    web: WebUser,
    Path((uid, index)): Path<(u32, usize)>,
    Query(query): Query<MailQuery>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::Unavailable.into_response();
    };
    let folder = match folder_param(query.folder.as_deref()) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let raw = async {
        let mut session = service.session(&web.user.email).await?;
        let out = session
            .message(&folder, uid, false)
            .await
            .map_err(|e| page_error(&e));
        session.close().await;
        out
    }
    .await;
    let data =
        match raw.and_then(|r| message::attachment(&r.bytes, index).map_err(|e| page_error(&e))) {
            Ok(d) => d,
            Err(error) => return error.into_response(),
        };
    (
        [
            (
                header::CONTENT_TYPE,
                download_type(&data.content_type).to_owned(),
            ),
            (
                header::CONTENT_DISPOSITION,
                attachment_disposition(&data.filename),
            ),
            // Even if a browser decided to show it, nothing in it may run.
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'".to_owned(),
            ),
            (header::CACHE_CONTROL, "private, no-store".to_owned()),
        ],
        data.data,
    )
        .into_response()
}

// ─── handlers: acting on messages ───────────────────────────────────────────

/// The form posted by the message buttons.
#[derive(Debug, Deserialize)]
pub struct ActionForm {
    folder: Option<String>,
    flag: Option<String>,
    target: Option<String>,
}

fn empty_preview(ctx: PageCtx) -> Response {
    render(&PreviewPartial { ctx, preview: None })
}

pub(crate) async fn with_session<T>(
    state: &AppState,
    email: &str,
    action: impl AsyncFnOnce(&mut MailSession) -> Result<T, MailError>,
) -> Result<T, PageError> {
    let service = service(state).ok_or(PageError::Unavailable)?;
    let mut session = service.session(email).await?;
    let result = action(&mut session).await;
    session.close().await;
    result.map_err(|e| page_error(&e))
}

async fn forget(state: &AppState, user: &User, folder: &str, uid: u32) {
    if let Some(service) = service(state) {
        if let Ok(Some(id)) = service.index.mailbox_id(user.id).await {
            let _ = service.index.remove(id, folder, i64::from(uid)).await;
        }
    }
}

pub(crate) async fn flag(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Path(uid): Path<u32>,
    Form(form): Form<ActionForm>,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::Unavailable.into_response();
    };
    let folder = match folder_param(form.folder.as_deref()) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let (kind, on) = match form.flag.as_deref() {
        Some("seen") => (FlagKind::Seen, true),
        Some("unseen") => (FlagKind::Seen, false),
        Some("star") => (FlagKind::Flagged, true),
        Some("unstar") => (FlagKind::Flagged, false),
        _ => return PageError::BadRequest.into_response(),
    };
    if let Err(error) = with_session(&state, &web.user.email, async |s| {
        s.set_flag(&folder, uid, kind, on).await
    })
    .await
    {
        return error.into_response();
    }
    // Show the message again with the new state, without marking it read (the
    // point of "mark unread" is that it stays unread).
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail");
    match preview_view(
        &service,
        &web.user,
        &ctx,
        &folder,
        uid,
        &MailQuery::default(),
        false,
    )
    .await
    {
        Ok(p) => (
            trigger("mail-refresh"),
            render(&PreviewPartial {
                ctx,
                preview: Some(p),
            }),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn move_to(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Path(uid): Path<u32>,
    Form(form): Form<ActionForm>,
) -> Response {
    let folder = match folder_param(form.folder.as_deref()) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let Some(target) = form
        .target
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return PageError::BadRequest.into_response();
    };
    let target = match folder_param(Some(target)) {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    match with_session(&state, &web.user.email, async |s| {
        s.move_message(&folder, uid, &target).await
    })
    .await
    {
        Ok(()) => {
            forget(&state, &web.user, &folder, uid).await;
            let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail");
            (trigger("mail-refresh"), empty_preview(ctx)).into_response()
        }
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Path(uid): Path<u32>,
    Form(form): Form<ActionForm>,
) -> Response {
    let folder = match folder_param(form.folder.as_deref()) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    match with_session(&state, &web.user.email, async |s| {
        s.delete_message(&folder, uid).await
    })
    .await
    {
        Ok(_) => {
            forget(&state, &web.user, &folder, uid).await;
            let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail");
            (trigger("mail-refresh"), empty_preview(ctx)).into_response()
        }
        Err(error) => error.into_response(),
    }
}

/// Moves a message to Junk, or back to the Inbox if it is in Junk already. The
/// move into Junk is what trains the spam filter (Phase 5).
pub(crate) async fn spam(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Path(uid): Path<u32>,
    Form(form): Form<ActionForm>,
) -> Response {
    let folder = match folder_param(form.folder.as_deref()) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let result = with_session(&state, &web.user.email, async |s| {
        let junk = s
            .special_folder(SpecialUse::Junk)
            .await?
            .ok_or_else(|| MailError::Invalid("there is no Junk folder".into()))?;
        let target = if folder == junk {
            "INBOX".to_owned()
        } else {
            junk
        };
        s.move_message(&folder, uid, &target).await
    })
    .await;
    match result {
        Ok(()) => {
            forget(&state, &web.user, &folder, uid).await;
            let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail");
            (trigger("mail-refresh"), empty_preview(ctx)).into_response()
        }
        Err(error) => error.into_response(),
    }
}

// ─── compose and send ───────────────────────────────────────────────────────

pub(crate) async fn compose_page(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(query): Query<ComposeQuery>,
) -> Response {
    let Some(_) = service(&state) else {
        return unconfigured(&state, &jar, &headers, &web.user);
    };
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail/compose");
    let jar = ensure_cookie(&state.auth, jar);
    let folder = match folder_param(query.folder.as_deref()) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let me = web.user.email.clone();
    let from = Address {
        name: Some(web.user.display_name.clone()).filter(|n| !n.trim().is_empty()),
        email: me.clone(),
    }
    .display();
    let mut c = ComposeView {
        to: query
            .to
            .clone()
            .unwrap_or_default()
            .chars()
            .take(500)
            .filter(|ch| !ch.is_control())
            .collect(),
        cc: String::new(),
        bcc: String::new(),
        subject: String::new(),
        body: String::new(),
        in_reply_to: String::new(),
        references: String::new(),
        answered_folder: String::new(),
        answered_uid: String::new(),
        from,
        show_cc: false,
        heading_key: "mail-compose-title",
    };
    if let Some(uid) = query.reply.or(query.forward) {
        let loaded =
            with_session(&state, &me, async |s| s.message(&folder, uid, false).await).await;
        let parsed =
            match loaded.and_then(|raw| message::parse(&raw.bytes).map_err(|e| page_error(&e))) {
                Ok(p) => p,
                Err(error) => return error.into_response(),
            };
        let sender_line = parsed
            .from
            .as_ref()
            .map(Address::display)
            .unwrap_or_default();
        let (when, _) = long_date(parsed.date);
        if query.reply.is_some() {
            c.heading_key = "mail-reply-title";
            let target = parsed.reply_to.first().or(parsed.from.as_ref()).cloned();
            c.subject = reply_subject(&parsed.subject);
            c.body = quote_for_reply(&format!("{when}, {sender_line}:"), &parsed.text);
            c.answered_folder = folder.clone();
            c.answered_uid = uid.to_string();
            if let Some(id) = &parsed.message_id {
                c.in_reply_to = id.clone();
                let mut refs = parsed.references.clone();
                refs.push(id.clone());
                c.references = refs.join(" ");
            }
            if query.all == Some(1) {
                let sender = target.clone().unwrap_or(Address {
                    name: None,
                    email: String::new(),
                });
                let (to, cc) = reply_all_recipients(&sender, &parsed.to, &parsed.cc, &me);
                let mut all_to = vec![sender];
                all_to.extend(to);
                c.to = addresses_line(
                    &all_to
                        .into_iter()
                        .filter(|a| !a.email.is_empty())
                        .collect::<Vec<_>>(),
                );
                c.cc = addresses_line(&cc);
                c.show_cc = !c.cc.is_empty();
            } else if let Some(t) = target {
                c.to = t.display();
            }
        } else {
            c.heading_key = "mail-forward-title";
            c.subject = forward_subject(&parsed.subject);
            c.body = forward_body(
                &sender_line,
                &addresses_line(&parsed.to),
                &when,
                &parsed.subject,
                &parsed.text,
            );
        }
    }
    (jar, render(&ComposePage { ctx, c })).into_response()
}

/// The text fields of the compose form.
#[derive(Debug, Default)]
struct SendForm {
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    in_reply_to: String,
    references: String,
    answered_folder: String,
    answered_uid: String,
    files: Vec<OutAttachment>,
}

/// Longest text field accepted in the compose form, in bytes.
const MAX_FIELD_BYTES: usize = 3 * 1024 * 1024;

async fn read_form(mut multipart: Multipart) -> Result<SendForm, MailError> {
    let mut form = SendForm::default();
    let bad =
        |_| MailError::Invalid("the form could not be read (is the attachment too large?)".into());
    while let Some(field) = multipart.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "attachments" {
            let filename = field.file_name().unwrap_or_default().to_owned();
            let declared = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_owned();
            let data = field.bytes().await.map_err(bad)?;
            if filename.is_empty() && data.is_empty() {
                continue; // an empty file input
            }
            if data.is_empty() {
                continue;
            }
            form.files.push(OutAttachment {
                filename,
                content_type: clean_content_type(&declared),
                data: data.to_vec(),
            });
            continue;
        }
        let text = field.bytes().await.map_err(bad)?;
        if text.len() > MAX_FIELD_BYTES {
            return Err(MailError::TooLarge("a text field is too long".into()));
        }
        let text = String::from_utf8_lossy(&text).into_owned();
        match name.as_str() {
            "to" => form.to = text,
            "cc" => form.cc = text,
            "bcc" => form.bcc = text,
            "subject" => form.subject = text,
            "body" => form.body = text,
            "in_reply_to" => form.in_reply_to = text,
            "references" => form.references = text,
            "answered_folder" => form.answered_folder = text,
            "answered_uid" => form.answered_uid = text,
            _ => {}
        }
    }
    Ok(form)
}

/// A client-supplied content type reduced to `type/subtype` of safe characters.
fn clean_content_type(declared: &str) -> String {
    let main = declared
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let ok = main.split_once('/').is_some_and(|(t, s)| {
        !t.is_empty()
            && !s.is_empty()
            && main.len() <= 100
            && main
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '+' | '.'))
    });
    if ok {
        main
    } else {
        "application/octet-stream".to_owned()
    }
}

pub(crate) async fn send(
    State(state): State<AppState>,
    web: WebUser,
    multipart: Multipart,
) -> Response {
    let Some(service) = service(&state) else {
        return PageError::Unavailable.into_response();
    };
    let user = &web.user;
    if !service.allow_send(user.id) {
        return alert(StatusCode::TOO_MANY_REQUESTS, "warning", "You have sent a lot of messages in the last few minutes. Please wait a little before sending more.");
    }
    let form = match read_form(multipart).await {
        Ok(f) => f,
        Err(error) => {
            return alert(
                StatusCode::UNPROCESSABLE_ENTITY,
                "danger",
                error.to_string(),
            )
        }
    };
    let draft = (|| -> Result<Draft, MailError> {
        let name = Some(user.display_name.trim().to_owned()).filter(|n| !n.is_empty());
        Ok(Draft {
            from: Address {
                name,
                email: user.email.clone(),
            },
            to: parse_recipients(&form.to)?,
            cc: parse_recipients(&form.cc)?,
            bcc: parse_recipients(&form.bcc)?,
            subject: form.subject.trim().to_owned(),
            body: form.body.replace("\r\n", "\n"),
            in_reply_to: Some(form.in_reply_to.trim().to_owned()).filter(|s| !s.is_empty()),
            references: form
                .references
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            attachments: form.files.clone(),
        })
    })();
    let built = match draft.and_then(|d| build(&d, &service.config.helo_name)) {
        Ok(b) => b,
        Err(error) => {
            return alert(
                StatusCode::UNPROCESSABLE_ENTITY,
                "danger",
                error.to_string(),
            )
        }
    };
    // The envelope sender is always the signed-in user, whatever the form said.
    if let Err(error) = submit::send(&service.config, &user.email, &user.email, &built).await {
        tracing::warn!(%error, "sending mail failed");
        let text = match error {
            MailError::Send(text) | MailError::Invalid(text) => text,
            _ => "The mail server could not be reached. Nothing was sent.".to_owned(),
        };
        return alert(StatusCode::UNPROCESSABLE_ENTITY, "danger", text);
    }
    // Housekeeping after a successful send must never turn it into an error.
    let answered = form
        .answered_uid
        .trim()
        .parse::<u32>()
        .ok()
        .zip(Some(form.answered_folder.clone()));
    let email = user.email.clone();
    let raw = built.bytes.clone();
    let after = with_session(&state, &email, async |s| {
        if let Ok(Some(sent)) = s.special_folder(SpecialUse::Sent).await {
            s.append(
                &sent,
                Flags {
                    seen: true,
                    ..Flags::default()
                },
                &raw,
            )
            .await?;
        }
        if let Some((uid, folder)) = &answered {
            if folder_param(Some(folder)).is_ok() {
                s.set_flag(folder, *uid, FlagKind::Answered, true).await?;
            }
        }
        Ok(())
    })
    .await;
    if after.is_err() {
        tracing::warn!("the message was sent, but saving the copy or marking the reply failed");
    }
    (
        StatusCode::OK,
        [(
            HX_REDIRECT,
            HeaderValue::from_static("/mail?folder=INBOX&done=sent"),
        )],
    )
        .into_response()
}

// ─── folders ────────────────────────────────────────────────────────────────

pub(crate) async fn folders_page(
    State(state): State<AppState>,
    web: WebUser,
    jar: CookieJar,
    headers: HeaderMap,
    Query(query): Query<MailQuery>,
) -> Response {
    if service(&state).is_none() {
        return unconfigured(&state, &jar, &headers, &web.user);
    }
    let ctx = page_ctx(&state, &jar, &headers, Some(&web.user), "/mail/folders");
    let jar = ensure_cookie(&state.auth, jar);
    let folders = match with_session(&state, &web.user.email, async |s| s.folders().await).await {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let rows = folders
        .iter()
        .map(|f| FolderRow {
            label: folder_label(&ctx, f),
            name: f.name.clone(),
            total: f.total,
            unread: f.unread,
            depth: f.depth(),
            standard: f.special.is_some() || f.name.eq_ignore_ascii_case("inbox"),
            selectable: f.selectable,
        })
        .collect();
    let notice = match query.done.as_deref() {
        Some("created") => Some(Notice {
            class: "success",
            key: "mail-folders-created",
        }),
        Some("renamed") => Some(Notice {
            class: "success",
            key: "mail-folders-renamed",
        }),
        Some("deleted") => Some(Notice {
            class: "success",
            key: "mail-folders-deleted",
        }),
        _ => None,
    };
    (jar, render(&FoldersPage { ctx, rows, notice })).into_response()
}

/// The form posted by the folder management page.
#[derive(Debug, Deserialize)]
pub struct FolderForm {
    name: Option<String>,
    new_name: Option<String>,
    parent: Option<String>,
}

fn folder_done(location: &'static str) -> Response {
    (
        StatusCode::OK,
        [(HX_REDIRECT, HeaderValue::from_static(location))],
    )
        .into_response()
}

fn folder_error(error: &MailError) -> Response {
    match error {
        MailError::Invalid(text) => alert(StatusCode::UNPROCESSABLE_ENTITY, "danger", text.clone()),
        MailError::NotFound => PageError::NotFound.into_response(),
        other => page_error(other).into_response(),
    }
}

pub(crate) async fn folder_create(
    State(state): State<AppState>,
    web: WebUser,
    Form(form): Form<FolderForm>,
) -> Response {
    let leaf = form.name.unwrap_or_default().trim().to_owned();
    let parent = form.parent.unwrap_or_default().trim().to_owned();
    let service = match service(&state) {
        Some(s) => s,
        None => return PageError::Unavailable.into_response(),
    };
    let mut session = match service.session(&web.user.email).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let full = if parent.is_empty() {
        leaf
    } else {
        format!("{parent}{}{leaf}", session.delimiter())
    };
    let result = session.create_folder(&full).await;
    session.close().await;
    match result {
        Ok(()) => folder_done("/mail/folders?done=created"),
        Err(error) => folder_error(&error),
    }
}

pub(crate) async fn folder_rename(
    State(state): State<AppState>,
    web: WebUser,
    Form(form): Form<FolderForm>,
) -> Response {
    let (Some(from), Some(leaf)) = (form.name, form.new_name) else {
        return PageError::BadRequest.into_response();
    };
    let service = match service(&state) {
        Some(s) => s,
        None => return PageError::Unavailable.into_response(),
    };
    let mut session = match service.session(&web.user.email).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // Renaming keeps the folder where it is in the hierarchy.
    let delimiter = session.delimiter().to_owned();
    let to = match from.rsplit_once(&delimiter) {
        Some((parent, _)) => format!("{parent}{delimiter}{}", leaf.trim()),
        None => leaf.trim().to_owned(),
    };
    let result = session.rename_folder(&from, &to).await;
    session.close().await;
    match result {
        Ok(()) => {
            if let Ok(Some(id)) = service.index.mailbox_id(web.user.id).await {
                let _ = service.index.clear_folder(id, &from).await;
            }
            folder_done("/mail/folders?done=renamed")
        }
        Err(error) => folder_error(&error),
    }
}

pub(crate) async fn folder_delete(
    State(state): State<AppState>,
    web: WebUser,
    Form(form): Form<FolderForm>,
) -> Response {
    let Some(name) = form.name else {
        return PageError::BadRequest.into_response();
    };
    let service = match service(&state) {
        Some(s) => s,
        None => return PageError::Unavailable.into_response(),
    };
    let mut session = match service.session(&web.user.email).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let result = session.delete_folder(&name).await;
    session.close().await;
    match result {
        Ok(()) => {
            if let Ok(Some(id)) = service.index.mailbox_id(web.user.id).await {
                let _ = service.index.clear_folder(id, &name).await;
            }
            folder_done("/mail/folders?done=deleted")
        }
        Err(error) => folder_error(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_parameters_are_checked() {
        assert_eq!(folder_param(None).unwrap(), "INBOX");
        assert_eq!(folder_param(Some("  ")).unwrap(), "INBOX");
        assert_eq!(
            folder_param(Some("Work/Projects")).unwrap(),
            "Work/Projects"
        );
        assert_eq!(folder_param(Some("Entwürfe")).unwrap(), "Entwürfe");
        assert!(folder_param(Some("a\r\nb")).is_err());
        assert!(folder_param(Some("a\0b")).is_err());
        assert!(folder_param(Some(&"x".repeat(300))).is_err());
    }

    #[test]
    fn urls_encode_folder_names_and_skip_defaults() {
        assert_eq!(list_url("INBOX", 1, "", None), "/mail?folder=INBOX");
        assert_eq!(
            list_url("Work/A&B", 2, "a b", Some(7)),
            "/mail?folder=Work%2FA%26B&page=2&q=a%20b&open=7"
        );
        assert_eq!(
            message_url("Entwürfe", 3),
            "/mail/message/3?folder=Entw%C3%BCrfe"
        );
    }

    #[test]
    fn dates_are_short_when_recent() {
        let now = DateTime::parse_from_rfc3339("2025-11-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let at = |s: &str| Some(DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc));
        assert_eq!(short_date(at("2025-11-13T09:05:00Z"), now).0, "09:05");
        assert_eq!(short_date(at("2025-03-02T09:05:00Z"), now).0, "Mar 2");
        assert_eq!(short_date(at("2023-03-02T09:05:00Z"), now).0, "2023-03-02");
        assert_eq!(short_date(None, now), (String::new(), String::new()));
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn downloads_are_served_as_attachments_with_safe_types() {
        assert_eq!(download_type("text/html"), "application/octet-stream");
        assert_eq!(download_type("image/svg+xml"), "application/octet-stream");
        assert_eq!(
            download_type("application/javascript"),
            "application/octet-stream"
        );
        assert_eq!(download_type("IMAGE/PNG"), "image/png");
        assert_eq!(download_type("application/pdf"), "application/pdf");
    }

    #[test]
    fn content_disposition_cannot_be_broken_out_of() {
        let d = attachment_disposition("evil\"; filename=\"x.exe\r\nSet-Cookie: a=b");
        assert!(!d.contains('\r') && !d.contains('\n'), "{d}");
        assert_eq!(
            d.matches('"').count(),
            2,
            "only the two quotes we wrote: {d}"
        );
        let d = attachment_disposition("Bericht Größe.pdf");
        assert!(
            d.starts_with("attachment; filename=\"Bericht Gr__e.pdf\""),
            "{d}"
        );
        assert!(
            d.ends_with("filename*=UTF-8''Bericht%20Gr%C3%B6%C3%9Fe.pdf"),
            "{d}"
        );
    }

    #[test]
    fn client_content_types_are_reduced_to_a_safe_form() {
        assert_eq!(
            clean_content_type("Application/PDF; name=x"),
            "application/pdf"
        );
        assert_eq!(
            clean_content_type("text/plain; charset=utf-8"),
            "text/plain"
        );
        for bad in ["", "garbage", "a/b\r\nX: y", "/x", "x/", "a b/c"] {
            assert_eq!(
                clean_content_type(bad),
                "application/octet-stream",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_html_wrapper_adds_nothing_active() {
        let doc = html_document("<p>x</p>");
        assert!(!doc.contains("<script") && !doc.contains("<base") && !doc.contains("<link"));
        assert!(doc.contains("<p>x</p>"));
    }
}
