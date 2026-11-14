//! An IMAP session acting for one user.
//!
//! Sessions are short-lived: the web server opens one per request (a TLS
//! handshake and a master-user login on the same host cost a few milliseconds)
//! and closes it afterwards. Nothing here caches or pools connections.
//!
//! Every folder name crosses this API decoded (`Entwürfe`) and is converted to
//! IMAP's modified UTF-7 on the way out. Every public operation has a time limit
//! so a stuck server cannot hold a web request forever.

use std::future::Future;
use std::time::Duration;

use async_imap::types::{Fetch, Flag, NameAttribute};
use async_imap::Session;
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;

use crate::config::MailConfig;
use crate::error::{MailError, MailResult};
use crate::folder::{special_from_name, validate_name, Folder, SpecialUse};
use crate::message::{
    summary_from_headers, Flags, MessageSummary, MAX_MESSAGE_BYTES, SUMMARY_HEADERS,
};
use crate::{tls, utf7};

/// Time limit for one operation (not for IDLE).
pub const OP_TIMEOUT: Duration = Duration::from_secs(30);

type Conn = TlsStream<TcpStream>;

/// One page of a folder, newest message first.
#[derive(Debug, Clone, PartialEq)]
pub struct MessagePage {
    /// The folder's UIDVALIDITY.
    pub uid_validity: u32,
    /// Messages in the whole folder.
    pub total: u32,
    /// The messages on this page.
    pub items: Vec<MessageSummary>,
}

/// A raw message with what the server knows about it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawMessage {
    /// The folder's UIDVALIDITY.
    pub uid_validity: u32,
    /// Flags at the time of the fetch (before any change this call made).
    pub flags: Flags,
    /// The complete message.
    pub bytes: Vec<u8>,
}

/// Which flag to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagKind {
    /// `\Seen`
    Seen,
    /// `\Flagged`
    Flagged,
    /// `\Answered`
    Answered,
}

impl FlagKind {
    fn name(self) -> &'static str {
        match self {
            Self::Seen => "\\Seen",
            Self::Flagged => "\\Flagged",
            Self::Answered => "\\Answered",
        }
    }
}

/// What a wait on a folder ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The server reported activity (new mail, expunge, flag change).
    Activity,
    /// Nothing happened before the time limit.
    Quiet,
}

/// An authenticated IMAP session.
pub struct MailSession {
    session: Session<Conn>,
    delimiter: String,
}

fn flags_of(fetch: &Fetch) -> Flags {
    let mut flags = Flags::default();
    for flag in fetch.flags() {
        match flag {
            Flag::Seen => flags.seen = true,
            Flag::Answered => flags.answered = true,
            Flag::Flagged => flags.flagged = true,
            Flag::Draft => flags.draft = true,
            Flag::Deleted => flags.deleted = true,
            _ => {}
        }
    }
    flags
}

fn summary_of(fetch: &Fetch) -> Option<MessageSummary> {
    let uid = fetch.uid?;
    let internal: Option<DateTime<Utc>> = fetch.internal_date().map(|d| d.with_timezone(&Utc));
    Some(summary_from_headers(
        uid,
        flags_of(fetch),
        fetch.size.unwrap_or(0),
        internal,
        fetch.header().unwrap_or_default(),
    ))
}

async fn limited<T>(op: impl Future<Output = MailResult<T>>) -> MailResult<T> {
    timeout(OP_TIMEOUT, op)
        .await
        .map_err(|_| MailError::Connect("the mail server did not answer in time".into()))?
}

impl MailSession {
    /// Opens a session acting as `user` (an e-mail address) via the master login.
    ///
    /// # Errors
    /// [`MailError::Connect`] / [`MailError::Tls`] if the server cannot be
    /// reached securely, [`MailError::Auth`] if it refuses the master login,
    /// [`MailError::Invalid`] for an unusable user name.
    pub async fn connect(config: &MailConfig, user: &str) -> MailResult<Self> {
        let login = config.imap_login_for(user)?;
        limited(async {
            let stream = tls::connect(config, &config.imap_host, config.imap_port).await?;
            let mut client = async_imap::Client::new(stream);
            client
                .read_response()
                .await?
                .ok_or_else(|| MailError::Connect("the server closed the connection".into()))?;
            let mut session = client.login(login, &config.master_password).await.map_err(
                |(error, _)| match error {
                    async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                        MailError::Auth
                    }
                    other => MailError::from(other),
                },
            )?;
            // The hierarchy separator, needed to validate and build folder names.
            let delimiter = {
                let names: Vec<_> = session
                    .list(Some(""), Some(""))
                    .await?
                    .try_collect()
                    .await?;
                names
                    .first()
                    .and_then(|n| n.delimiter().map(str::to_owned))
                    .unwrap_or_else(|| "/".to_owned())
            };
            Ok(Self { session, delimiter })
        })
        .await
    }

    /// The folder hierarchy separator.
    #[must_use]
    pub fn delimiter(&self) -> &str {
        &self.delimiter
    }

    /// Ends the session politely. Failures are ignored: the connection is dropped anyway.
    pub async fn close(mut self) {
        let _ = timeout(Duration::from_secs(5), self.session.logout()).await;
    }

    /// Lists all folders with message and unread counts, standard ones first.
    ///
    /// # Errors
    /// Connection or protocol errors.
    pub async fn folders(&mut self) -> MailResult<Vec<Folder>> {
        limited(async {
            let names: Vec<_> = self
                .session
                .list(Some(""), Some("*"))
                .await?
                .try_collect()
                .await?;
            let mut folders = Vec::with_capacity(names.len());
            for name in &names {
                let Some(decoded) = utf7::decode(name.name()) else {
                    continue; // a name we cannot represent safely is not offered
                };
                let selectable = !name
                    .attributes()
                    .iter()
                    .any(|a| matches!(a, NameAttribute::NoSelect));
                let special = name
                    .attributes()
                    .iter()
                    .find_map(|a| match a {
                        NameAttribute::Drafts => Some(SpecialUse::Drafts),
                        NameAttribute::Sent => Some(SpecialUse::Sent),
                        NameAttribute::Trash => Some(SpecialUse::Trash),
                        NameAttribute::Junk => Some(SpecialUse::Junk),
                        NameAttribute::Archive => Some(SpecialUse::Archive),
                        _ => None,
                    })
                    .or_else(|| special_from_name(&decoded));
                let (total, unread) = if selectable {
                    match self.session.status(name.name(), "(MESSAGES UNSEEN)").await {
                        Ok(status) => (status.exists, status.unseen.unwrap_or(0)),
                        Err(_) => (0, 0),
                    }
                } else {
                    (0, 0)
                };
                folders.push(Folder {
                    name: decoded,
                    delimiter: name.delimiter().map(str::to_owned),
                    special,
                    total,
                    unread,
                    selectable,
                });
            }
            folders.sort_by(|a, b| {
                let rank = |f: &Folder| f.special.map_or(u8::MAX, SpecialUse::rank);
                rank(a)
                    .cmp(&rank(b))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(folders)
        })
        .await
    }

    /// The name of the folder that plays `role`, if the user has one.
    ///
    /// # Errors
    /// Connection or protocol errors.
    pub async fn special_folder(&mut self, role: SpecialUse) -> MailResult<Option<String>> {
        Ok(self
            .folders()
            .await?
            .into_iter()
            .find(|f| f.special == Some(role))
            .map(|f| f.name))
    }

    async fn select_ro(&mut self, folder: &str) -> MailResult<async_imap::types::Mailbox> {
        self.session
            .examine(utf7::encode(folder))
            .await
            .map_err(|_| MailError::NotFound)
    }

    async fn select_rw(&mut self, folder: &str) -> MailResult<async_imap::types::Mailbox> {
        self.session
            .select(utf7::encode(folder))
            .await
            .map_err(|_| MailError::NotFound)
    }

    /// One page of a folder, newest first. `page` counts from 1.
    ///
    /// # Errors
    /// [`MailError::NotFound`] if the folder does not exist.
    pub async fn list(
        &mut self,
        folder: &str,
        page: u32,
        per_page: u32,
    ) -> MailResult<MessagePage> {
        let per_page = per_page.clamp(1, 200);
        let page = page.max(1);
        limited(async {
            let mailbox = self.select_ro(folder).await?;
            let total = mailbox.exists;
            let uid_validity = mailbox.uid_validity.unwrap_or(0);
            let skip = (page - 1).saturating_mul(per_page);
            if total == 0 || skip >= total {
                return Ok(MessagePage {
                    uid_validity,
                    total,
                    items: Vec::new(),
                });
            }
            let end = total - skip;
            let start = end.saturating_sub(per_page - 1).max(1);
            let query = format!("(UID FLAGS RFC822.SIZE INTERNALDATE {SUMMARY_HEADERS})");
            let fetched: Vec<Fetch> = self
                .session
                .fetch(format!("{start}:{end}"), query)
                .await?
                .try_collect()
                .await?;
            let mut items: Vec<_> = fetched.iter().filter_map(summary_of).collect();
            items.sort_by_key(|s| std::cmp::Reverse(s.uid));
            Ok(MessagePage {
                uid_validity,
                total,
                items,
            })
        })
        .await
    }

    /// The UIDVALIDITY, next UID and message count of a folder.
    ///
    /// # Errors
    /// [`MailError::NotFound`] if the folder does not exist.
    pub async fn folder_state(&mut self, folder: &str) -> MailResult<(u32, u32, u32)> {
        limited(async {
            let status = self
                .session
                .status(utf7::encode(folder), "(UIDVALIDITY UIDNEXT MESSAGES)")
                .await
                .map_err(|_| MailError::NotFound)?;
            Ok((
                status.uid_validity.unwrap_or(0),
                status.uid_next.unwrap_or(1),
                status.exists,
            ))
        })
        .await
    }

    /// Summaries of up to `limit` messages with a UID above `after_uid`, oldest first.
    ///
    /// # Errors
    /// [`MailError::NotFound`] if the folder does not exist.
    pub async fn summaries_after(
        &mut self,
        folder: &str,
        after_uid: u32,
        limit: usize,
    ) -> MailResult<Vec<MessageSummary>> {
        limited(async {
            self.select_ro(folder).await?;
            let query = format!("(UID FLAGS RFC822.SIZE INTERNALDATE {SUMMARY_HEADERS})");
            let fetched: Vec<Fetch> = self
                .session
                .uid_fetch(format!("{}:*", after_uid.saturating_add(1)), query)
                .await?
                .try_collect()
                .await?;
            let mut items: Vec<_> = fetched
                .iter()
                .filter_map(summary_of)
                // `n:*` always returns the last message even when it is older than `n`.
                .filter(|s| s.uid > after_uid)
                .collect();
            items.sort_by_key(|s| s.uid);
            items.truncate(limit);
            Ok(items)
        })
        .await
    }

    /// Fetches a whole message. With `mark_read`, sets `\Seen` afterwards.
    ///
    /// # Errors
    /// [`MailError::NotFound`] if the folder or message is gone,
    /// [`MailError::TooLarge`] above 25 MB.
    pub async fn message(
        &mut self,
        folder: &str,
        uid: u32,
        mark_read: bool,
    ) -> MailResult<RawMessage> {
        if uid == 0 {
            return Err(MailError::NotFound);
        }
        limited(async {
            let mailbox = if mark_read {
                self.select_rw(folder).await?
            } else {
                self.select_ro(folder).await?
            };
            let uid_validity = mailbox.uid_validity.unwrap_or(0);
            let meta: Vec<Fetch> = self
                .session
                .uid_fetch(uid.to_string(), "(UID FLAGS RFC822.SIZE)")
                .await?
                .try_collect()
                .await?;
            let meta = meta
                .iter()
                .find(|f| f.uid == Some(uid))
                .ok_or(MailError::NotFound)?;
            if meta.size.unwrap_or(0) > MAX_MESSAGE_BYTES {
                return Err(MailError::TooLarge(
                    "this message is too large to display".into(),
                ));
            }
            let flags = flags_of(meta);
            let body: Vec<Fetch> = self
                .session
                .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
                .await?
                .try_collect()
                .await?;
            let bytes = body
                .iter()
                .find(|f| f.uid == Some(uid))
                .and_then(|f| f.body())
                .ok_or(MailError::NotFound)?
                .to_vec();
            if mark_read && !flags.seen {
                self.store(uid, FlagKind::Seen, true).await?;
            }
            Ok(RawMessage {
                uid_validity,
                flags,
                bytes,
            })
        })
        .await
    }

    async fn store(&mut self, uid: u32, flag: FlagKind, on: bool) -> MailResult<()> {
        let sign = if on { "+" } else { "-" };
        let _: Vec<Fetch> = self
            .session
            .uid_store(
                uid.to_string(),
                format!("{sign}FLAGS.SILENT ({})", flag.name()),
            )
            .await?
            .try_collect()
            .await?;
        Ok(())
    }

    /// Sets or clears a flag on one message.
    ///
    /// # Errors
    /// [`MailError::NotFound`] if the folder is gone.
    pub async fn set_flag(
        &mut self,
        folder: &str,
        uid: u32,
        flag: FlagKind,
        on: bool,
    ) -> MailResult<()> {
        if uid == 0 {
            return Err(MailError::NotFound);
        }
        limited(async {
            self.select_rw(folder).await?;
            self.store(uid, flag, on).await
        })
        .await
    }

    /// Moves a message to another folder.
    ///
    /// # Errors
    /// [`MailError::NotFound`] if either folder is gone.
    pub async fn move_message(&mut self, folder: &str, uid: u32, target: &str) -> MailResult<()> {
        if uid == 0 || folder == target {
            return Err(MailError::Invalid("choose a different folder".into()));
        }
        limited(async {
            self.select_rw(folder).await?;
            let to = utf7::encode(target);
            match self.session.uid_mv(uid.to_string(), &to).await {
                Ok(()) => Ok(()),
                Err(async_imap::error::Error::No(_)) => Err(MailError::NotFound),
                Err(_) => {
                    // A server without MOVE: copy, flag, expunge.
                    self.session
                        .uid_copy(uid.to_string(), &to)
                        .await
                        .map_err(|_| MailError::NotFound)?;
                    self.store_deleted_and_expunge(uid).await
                }
            }
        })
        .await
    }

    async fn store_deleted_and_expunge(&mut self, uid: u32) -> MailResult<()> {
        let _: Vec<Fetch> = self
            .session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
            .await?
            .try_collect()
            .await?;
        let _: Vec<u32> = self
            .session
            .uid_expunge(uid.to_string())
            .await?
            .try_collect()
            .await?;
        Ok(())
    }

    /// Deletes a message: moved to Trash, or removed for good if it is already in
    /// Trash (or there is no Trash).
    ///
    /// Returns `true` if the message was removed permanently.
    ///
    /// # Errors
    /// [`MailError::NotFound`] if the folder is gone.
    pub async fn delete_message(&mut self, folder: &str, uid: u32) -> MailResult<bool> {
        let trash = self.special_folder(SpecialUse::Trash).await?;
        match trash {
            Some(trash) if trash != folder => {
                self.move_message(folder, uid, &trash).await?;
                Ok(false)
            }
            _ => {
                limited(async {
                    self.select_rw(folder).await?;
                    self.store_deleted_and_expunge(uid).await
                })
                .await?;
                Ok(true)
            }
        }
    }

    /// Stores a message in a folder (for example a copy in Sent).
    ///
    /// # Errors
    /// [`MailError::NotFound`] if the folder does not exist.
    pub async fn append(&mut self, folder: &str, flags: Flags, bytes: &[u8]) -> MailResult<()> {
        let names = flags.names();
        let flag_list = (!names.is_empty()).then(|| format!("({})", names.join(" ")));
        limited(async {
            self.session
                .append(utf7::encode(folder), flag_list.as_deref(), None, bytes)
                .await
                .map_err(|_| MailError::NotFound)
        })
        .await
    }

    /// Creates a folder.
    ///
    /// # Errors
    /// [`MailError::Invalid`] for a bad name or one that already exists.
    pub async fn create_folder(&mut self, name: &str) -> MailResult<()> {
        validate_name(name, &self.delimiter)?;
        limited(async {
            self.session.create(utf7::encode(name)).await.map_err(|_| {
                MailError::Invalid(
                    "that folder could not be created (does it already exist?)".into(),
                )
            })?;
            let _ = self.session.subscribe(utf7::encode(name)).await;
            Ok(())
        })
        .await
    }

    /// Renames a folder. Standard folders (Inbox, Sent, ...) cannot be renamed.
    ///
    /// # Errors
    /// [`MailError::Invalid`] for a bad or protected name.
    pub async fn rename_folder(&mut self, from: &str, to: &str) -> MailResult<()> {
        validate_name(to, &self.delimiter)?;
        self.refuse_standard(from).await?;
        limited(async {
            self.session
                .rename(utf7::encode(from), utf7::encode(to))
                .await
                .map_err(|_| MailError::Invalid("that folder could not be renamed".into()))
        })
        .await
    }

    /// Deletes a folder, its subfolders and everything in them. Standard folders
    /// cannot be deleted.
    ///
    /// # Errors
    /// [`MailError::Invalid`] for a protected name.
    pub async fn delete_folder(&mut self, name: &str) -> MailResult<()> {
        self.refuse_standard(name).await?;
        let prefix = format!("{name}{}", self.delimiter);
        let all = self.folders().await?;
        // A name that only exists as a parent of other folders (not selectable)
        // disappears by itself once its children are gone.
        let own_is_real = all.iter().any(|f| f.name == name && f.selectable);
        let mut doomed: Vec<String> = all
            .into_iter()
            .filter(|f| f.name.starts_with(&prefix))
            .map(|f| {
                if f.special.is_some() {
                    Err(MailError::Invalid(
                        "a standard folder inside it cannot be deleted".into(),
                    ))
                } else {
                    Ok(f.name)
                }
            })
            .collect::<MailResult<_>>()?;
        // Deepest first: a parent cannot be removed while it still has children.
        doomed.sort_by_key(|n| std::cmp::Reverse(n.len()));
        if own_is_real {
            doomed.push(name.to_owned());
        }
        limited(async {
            for folder in &doomed {
                self.session
                    .delete(utf7::encode(folder))
                    .await
                    .map_err(|_| MailError::Invalid("that folder could not be deleted".into()))?;
            }
            Ok(())
        })
        .await
    }

    async fn refuse_standard(&mut self, name: &str) -> MailResult<()> {
        if name.eq_ignore_ascii_case("inbox") {
            return Err(MailError::Invalid("the Inbox cannot be changed".into()));
        }
        let folders = self.folders().await?;
        match folders.iter().find(|f| f.name == name) {
            None => Err(MailError::NotFound),
            Some(f) if f.special.is_some() => Err(MailError::Invalid(
                "standard folders cannot be renamed or deleted".into(),
            )),
            Some(_) => Ok(()),
        }
    }

    /// Waits until something happens in `folder`, or `max_wait` passes, using IMAP IDLE.
    /// Consumes the session and returns it with the outcome.
    ///
    /// # Errors
    /// Connection or protocol errors; the session is lost in that case.
    pub async fn wait_for_change(
        mut self,
        folder: &str,
        max_wait: Duration,
    ) -> MailResult<(Self, Change)> {
        self.select_ro(folder).await?;
        let delimiter = self.delimiter;
        let mut handle = self.session.idle();
        handle.init().await?;
        let outcome = {
            let (wait, _stop) = handle.wait_with_timeout(max_wait);
            wait.await?
        };
        let session = handle.done().await?;
        let change = match outcome {
            async_imap::extensions::idle::IdleResponse::NewData(_) => Change::Activity,
            _ => Change::Quiet,
        };
        Ok((Self { session, delimiter }, change))
    }
}
