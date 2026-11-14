//! New-mail push over a WebSocket (`/mail/ws`).
//!
//! The browser opens one socket per mail page. For each socket the server holds
//! an IMAP IDLE connection on the user's inbox and sends the single word `new`
//! whenever the server reports activity; the page then refreshes its list. The
//! page also polls every minute, so this is an accelerator: if it fails, nothing
//! is lost but a little promptness.
//!
//! Safety: the upgrade needs the signed-in user's cookies **and** an `Origin`
//! that matches the `Host` (browsers do not apply the same-origin policy to
//! WebSockets, so a page on another site could otherwise open one with the
//! visitor's cookies). Sockets are limited per user and in total.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use rustmail_imap::session::Change;
use rustmail_imap::MailSession;
use tokio::sync::oneshot;

use crate::app::AppState;
use crate::mail::{MailService, SocketSlot};
use crate::pages::WebUser;
use crate::ui::PageError;

/// How long one IDLE lasts before the socket is pinged and IDLE restarted.
const IDLE_WINDOW: Duration = Duration::from_secs(240);
/// How many connection failures in a row end the socket.
const MAX_FAILURES: u32 = 3;
const RETRY_PAUSE: Duration = Duration::from_secs(10);

/// Whether the `Origin` header names the same site as the `Host` header.
/// A missing `Origin` is refused: every browser sends one on WebSocket upgrades.
#[must_use]
pub fn origin_matches_host(headers: &HeaderMap) -> bool {
    let (Some(origin), Some(host)) = (
        headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()),
        headers.get(header::HOST).and_then(|v| v.to_str().ok()),
    ) else {
        return false;
    };
    let authority = origin.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    !authority.is_empty() && !authority.contains('/') && authority.eq_ignore_ascii_case(host)
}

pub(crate) async fn socket(
    State(state): State<AppState>,
    web: WebUser,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(service) = state.mail.clone() else {
        return PageError::Unavailable.into_response();
    };
    if !origin_matches_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(slot) = service.acquire_socket(web.user.id) else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let email = web.user.email.clone();
    ws.on_upgrade(move |socket| watch(socket, service, email, slot))
}

/// Runs until the browser leaves or the mail server cannot be reached repeatedly.
async fn watch(
    socket: WebSocket,
    service: std::sync::Arc<MailService>,
    email: String,
    _slot: SocketSlot,
) {
    let (mut tx, mut rx) = socket.split();
    // Notice the browser going away while an IDLE wait is in progress.
    let (gone_tx, mut gone) = oneshot::channel::<()>();
    tokio::spawn(async move {
        while let Some(Ok(message)) = rx.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
        let _ = gone_tx.send(());
    });

    let mut failures = 0;
    let mut session: Option<MailSession> = None;
    loop {
        let current = match session.take() {
            Some(s) => s,
            None => match MailSession::connect(&service.config, &email).await {
                Ok(s) => s,
                Err(error) => {
                    failures += 1;
                    tracing::debug!(%error, failures, "new-mail watcher could not connect");
                    if failures >= MAX_FAILURES {
                        break;
                    }
                    tokio::select! {
                        () = tokio::time::sleep(RETRY_PAUSE) => continue,
                        _ = &mut gone => break,
                    }
                }
            },
        };
        tokio::select! {
            outcome = current.wait_for_change("INBOX", IDLE_WINDOW) => match outcome {
                Ok((next, change)) => {
                    failures = 0;
                    session = Some(next);
                    let message = match change {
                        Change::Activity => Message::Text("new".into()),
                        // Also keeps proxies from closing an idle connection.
                        Change::Quiet => Message::Ping(Vec::new().into()),
                    };
                    if tx.send(message).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    failures += 1;
                    tracing::debug!(%error, failures, "new-mail watcher lost its connection");
                    if failures >= MAX_FAILURES {
                        break;
                    }
                }
            },
            _ = &mut gone => break,
        }
    }
    let _ = tx.close().await;
    if let Some(s) = session {
        s.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(origin: Option<&str>, host: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(o) = origin {
            h.insert(header::ORIGIN, HeaderValue::from_str(o).unwrap());
        }
        if let Some(v) = host {
            h.insert(header::HOST, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn only_same_site_origins_may_open_the_socket() {
        assert!(origin_matches_host(&headers(
            Some("https://mail.example.com"),
            Some("mail.example.com")
        )));
        assert!(origin_matches_host(&headers(
            Some("http://127.0.0.1:8080"),
            Some("127.0.0.1:8080")
        )));
        assert!(origin_matches_host(&headers(
            Some("https://MAIL.example.com"),
            Some("mail.example.com")
        )));
        for (origin, host) in [
            (Some("https://evil.example"), Some("mail.example.com")),
            (
                Some("https://mail.example.com.evil.example"),
                Some("mail.example.com"),
            ),
            (
                Some("https://evil.example/mail.example.com"),
                Some("mail.example.com"),
            ),
            (Some("null"), Some("mail.example.com")),
            (
                Some("https://mail.example.com:444"),
                Some("mail.example.com"),
            ),
            (None, Some("mail.example.com")),
            (Some("https://mail.example.com"), None),
            (None, None),
        ] {
            assert!(
                !origin_matches_host(&headers(origin, host)),
                "{origin:?} vs {host:?}"
            );
        }
    }
}
