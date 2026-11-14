//! Webmail state shared by the mail pages and the new-mail WebSocket.
//!
//! One [`MailService`] lives in the application state when webmail is configured
//! (`MAIL_MASTER_PASSWORD` is set). It opens IMAP sessions on behalf of the
//! signed-in user, decides when to refresh the search index in the background,
//! and limits how many WebSockets a user and the server hold open.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustmail_core::mailindex::MailIndex;
use rustmail_imap::sync::sync_mailbox;
use rustmail_imap::{MailConfig, MailError, MailSession};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::ui::PageError;

/// Do not start another background index run for a mailbox within this time.
const SYNC_INTERVAL: Duration = Duration::from_secs(90);
/// Messages one background run indexes at most.
const SYNC_BUDGET: usize = 400;
/// A user may send this many messages in [`SEND_WINDOW`]. A compromised account is
/// otherwise a free spam relay for whoever holds the password.
pub const SENDS_PER_WINDOW: usize = 30;
/// See [`SENDS_PER_WINDOW`].
pub const SEND_WINDOW: Duration = Duration::from_secs(600);
/// WebSockets one user may hold open (several tabs).
pub const SOCKETS_PER_USER: usize = 5;
/// WebSockets the whole server will hold open for webmail.
pub const SOCKETS_TOTAL: usize = 500;

/// A held WebSocket slot; released when dropped.
pub struct SocketSlot {
    user: Uuid,
    service: Arc<MailService>,
    _global: OwnedSemaphorePermit,
}

impl Drop for SocketSlot {
    fn drop(&mut self) {
        let mut open = self
            .service
            .sockets
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = open.get_mut(&self.user) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                open.remove(&self.user);
            }
        }
    }
}

/// Webmail configuration plus the little shared state it needs.
pub struct MailService {
    /// Connection settings.
    pub config: MailConfig,
    /// The search index.
    pub index: MailIndex,
    last_sync: Mutex<HashMap<Uuid, Instant>>,
    sends: Mutex<HashMap<Uuid, VecDeque<Instant>>>,
    sockets: Mutex<HashMap<Uuid, usize>>,
    socket_limit: Arc<Semaphore>,
}

impl MailService {
    /// Creates the service.
    #[must_use]
    pub fn new(config: MailConfig, index: MailIndex) -> Self {
        Self {
            config,
            index,
            last_sync: Mutex::new(HashMap::new()),
            sends: Mutex::new(HashMap::new()),
            sockets: Mutex::new(HashMap::new()),
            socket_limit: Arc::new(Semaphore::new(SOCKETS_TOTAL)),
        }
    }

    /// Opens an IMAP session as `email`.
    ///
    /// # Errors
    /// A [`PageError`] after logging the cause: the user sees "mail is
    /// unavailable", the operator sees why.
    pub async fn session(&self, email: &str) -> Result<MailSession, PageError> {
        MailSession::connect(&self.config, email)
            .await
            .map_err(|e| page_error(&e))
    }

    /// Starts a background index run for the mailbox unless one ran recently.
    pub fn spawn_sync(self: &Arc<Self>, email: String, mailbox_id: Uuid) {
        {
            let mut last = self
                .last_sync
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            if last
                .get(&mailbox_id)
                .is_some_and(|t| now.duration_since(*t) < SYNC_INTERVAL)
            {
                return;
            }
            // Keep the map small: forget entries that no longer block anything.
            last.retain(|_, t| now.duration_since(*t) < SYNC_INTERVAL);
            last.insert(mailbox_id, now);
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let result = async {
                let mut session = MailSession::connect(&service.config, &email).await?;
                let report =
                    sync_mailbox(&mut session, &service.index, mailbox_id, SYNC_BUDGET).await;
                session.close().await;
                report
            }
            .await;
            match result {
                Ok(report) if report.messages > 0 => {
                    tracing::debug!(
                        messages = report.messages,
                        complete = report.complete,
                        "indexed mail"
                    );
                    if !report.complete {
                        // More to do: allow the next visit to continue straight away.
                        service
                            .last_sync
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .remove(&mailbox_id);
                    }
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "background mail indexing failed"),
            }
        });
    }

    /// Counts a send attempt for `user` and says whether it is within the limit.
    #[must_use]
    pub fn allow_send(&self, user: Uuid) -> bool {
        let mut sends = self.sends.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let recent = sends.entry(user).or_default();
        while recent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= SEND_WINDOW)
        {
            recent.pop_front();
        }
        if recent.len() >= SENDS_PER_WINDOW {
            return false;
        }
        recent.push_back(now);
        // Do not keep entries for users who stopped sending.
        if sends.len() > 1000 {
            sends.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.duration_since(*t) < SEND_WINDOW)
            });
        }
        true
    }

    /// Reserves a WebSocket slot for `user`, or `None` if a limit is reached.
    #[must_use]
    pub fn acquire_socket(self: &Arc<Self>, user: Uuid) -> Option<SocketSlot> {
        let global = Arc::clone(&self.socket_limit).try_acquire_owned().ok()?;
        let mut open = self.sockets.lock().unwrap_or_else(PoisonError::into_inner);
        let count = open.entry(user).or_insert(0);
        if *count >= SOCKETS_PER_USER {
            return None;
        }
        *count += 1;
        drop(open);
        Some(SocketSlot {
            user,
            service: Arc::clone(self),
            _global: global,
        })
    }
}

/// Maps a mail error to the page the user sees, logging the cause.
#[must_use]
pub fn page_error(error: &MailError) -> PageError {
    match error {
        MailError::NotFound => PageError::NotFound,
        MailError::Invalid(_) | MailError::TooLarge(_) => PageError::BadRequest,
        MailError::Parse(_) | MailError::Index(_) => {
            tracing::error!(%error, "webmail failed");
            PageError::Internal
        }
        MailError::Auth
        | MailError::Connect(_)
        | MailError::Tls(_)
        | MailError::Imap(_)
        | MailError::Send(_)
        | MailError::Config(_) => {
            tracing::error!(%error, "the mail server could not be used");
            PageError::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> Arc<MailService> {
        let config = MailConfig {
            imap_host: "127.0.0.1".into(),
            imap_port: 1,
            smtp_host: "127.0.0.1".into(),
            smtp_port: 1,
            tls_server_name: None,
            tls: rustmail_imap::TlsMode::Insecure,
            master_user: "webmail".into(),
            master_password: "x".into(),
            helo_name: "rustmail.local".into(),
        };
        // The pool is never used by these tests.
        let pool = sqlx::PgPool::connect_lazy("postgres://x:x@127.0.0.1:1/x").expect("lazy pool");
        Arc::new(MailService::new(config, MailIndex::new(pool)))
    }

    #[tokio::test]
    async fn sockets_are_limited_per_user_and_released_on_drop() {
        let service = service();
        let alice = Uuid::new_v4();
        let slots: Vec<_> = (0..SOCKETS_PER_USER)
            .map(|_| service.acquire_socket(alice).expect("slot"))
            .collect();
        assert!(
            service.acquire_socket(alice).is_none(),
            "one user cannot hold more than the limit"
        );
        assert!(
            service.acquire_socket(Uuid::new_v4()).is_some(),
            "other users are unaffected"
        );
        drop(slots);
        assert!(service.acquire_socket(alice).is_some(), "slots come back");
    }

    #[tokio::test]
    async fn sending_is_rate_limited_per_user() {
        let service = service();
        let alice = Uuid::new_v4();
        for _ in 0..SENDS_PER_WINDOW {
            assert!(service.allow_send(alice));
        }
        assert!(!service.allow_send(alice), "the next one is refused");
        assert!(
            service.allow_send(Uuid::new_v4()),
            "other users are unaffected"
        );
    }

    #[test]
    fn mail_errors_map_to_the_right_pages() {
        assert_eq!(page_error(&MailError::NotFound), PageError::NotFound);
        assert_eq!(
            page_error(&MailError::Invalid("x".into())),
            PageError::BadRequest
        );
        assert_eq!(page_error(&MailError::Auth), PageError::Unavailable);
        assert_eq!(
            page_error(&MailError::Connect("x".into())),
            PageError::Unavailable
        );
        assert_eq!(
            page_error(&MailError::Parse("x".into())),
            PageError::Internal
        );
    }
}
