//! Filling the search index (`messages_meta`) from IMAP.
//!
//! The index stores headers and a short snippet per message. A sync reads only
//! what is new since the last one (tracked per folder by UID), starts over for a
//! folder whose UIDVALIDITY changed, and stops after a budget of messages so one
//! request never turns into a mailbox-wide crawl. Run it again (or let the web
//! server do so on the next visit) to continue.

use chrono::Utc;
use rustmail_core::mailindex::{IndexedMessage, MailIndex};
use uuid::Uuid;

use crate::error::MailResult;
use crate::message::{parse, snippet, MessageSummary};
use crate::session::MailSession;

/// Messages fetched per round trip.
pub const BATCH: usize = 200;
/// Messages indexed per folder per call, at most.
pub const FOLDER_BUDGET: usize = 1000;
/// Only messages up to this size are opened to take a snippet.
pub const SNIPPET_MAX_SIZE: u32 = 64 * 1024;

/// What a sync accomplished.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Folders looked at.
    pub folders: usize,
    /// Messages newly indexed.
    pub messages: usize,
    /// Whether every folder is now fully indexed.
    pub complete: bool,
}

/// Turns a list summary into an index row.
#[must_use]
pub fn indexed(
    folder: &str,
    uid_validity: u32,
    summary: &MessageSummary,
    snippet_text: String,
) -> IndexedMessage {
    let join = |list: &[crate::message::Address]| {
        list.iter()
            .map(|a| a.display())
            .collect::<Vec<_>>()
            .join(", ")
    };
    IndexedMessage {
        folder: folder.to_owned(),
        uid_validity: i64::from(uid_validity),
        imap_uid: i64::from(summary.uid),
        message_id: summary.message_id.clone(),
        in_reply_to: summary.in_reply_to.clone(),
        subject: summary.subject.clone(),
        from_addr: summary
            .from
            .as_ref()
            .map(|a| a.display())
            .unwrap_or_default(),
        to_addrs: join(&summary.to),
        cc_addrs: join(&summary.cc),
        snippet: snippet_text,
        size_bytes: i64::from(summary.size),
        flags: summary.flags.names(),
        has_attachments: summary.has_attachments,
        date_header: summary.date,
        received_at: summary.received.unwrap_or_else(Utc::now),
    }
}

/// Indexes one folder: at most `budget` new messages. Returns how many were
/// indexed and whether the folder is now fully caught up.
///
/// # Errors
/// IMAP or database errors. Progress made before the error is kept.
pub async fn sync_folder(
    session: &mut MailSession,
    index: &MailIndex,
    mailbox_id: Uuid,
    folder: &str,
    budget: usize,
) -> MailResult<(usize, bool)> {
    let (validity, _next, _exists) = session.folder_state(folder).await?;
    let mut last_uid = match index.sync_state(mailbox_id, folder).await? {
        Some(state) if state.uid_validity == i64::from(validity) => {
            u32::try_from(state.last_uid).unwrap_or(0)
        }
        Some(_) => {
            // The server renumbered the folder: every stored UID now points elsewhere.
            index.clear_folder(mailbox_id, folder).await?;
            0
        }
        None => 0,
    };
    let mut done = 0;
    while done < budget {
        let batch = session
            .summaries_after(folder, last_uid, BATCH.min(budget - done))
            .await?;
        if batch.is_empty() {
            index
                .set_sync_state(mailbox_id, folder, i64::from(validity), i64::from(last_uid))
                .await?;
            return Ok((done, true));
        }
        let mut rows = Vec::with_capacity(batch.len());
        for summary in &batch {
            let text = if summary.size <= SNIPPET_MAX_SIZE {
                match session.message(folder, summary.uid, false).await {
                    Ok(raw) => parse(&raw.bytes).map(|p| snippet(&p)).unwrap_or_default(),
                    Err(error) => {
                        tracing::debug!(%error, uid = summary.uid, "could not read a message for its snippet");
                        String::new()
                    }
                }
            } else {
                String::new()
            };
            rows.push(indexed(folder, validity, summary, text));
        }
        index.upsert(mailbox_id, &rows).await?;
        last_uid = batch.iter().map(|s| s.uid).max().unwrap_or(last_uid);
        index
            .set_sync_state(mailbox_id, folder, i64::from(validity), i64::from(last_uid))
            .await?;
        done += batch.len();
    }
    Ok((done, false))
}

/// Indexes every selectable folder of the mailbox, sharing one budget.
///
/// # Errors
/// IMAP or database errors on the folder list; per-folder failures are logged
/// and skipped so one broken folder does not block the rest.
pub async fn sync_mailbox(
    session: &mut MailSession,
    index: &MailIndex,
    mailbox_id: Uuid,
    budget: usize,
) -> MailResult<SyncReport> {
    let folders = session.folders().await?;
    let mut report = SyncReport {
        complete: true,
        ..SyncReport::default()
    };
    let mut left = budget;
    for folder in folders.iter().filter(|f| f.selectable) {
        if left == 0 {
            report.complete = false;
            break;
        }
        match sync_folder(
            session,
            index,
            mailbox_id,
            &folder.name,
            left.min(FOLDER_BUDGET),
        )
        .await
        {
            Ok((n, caught_up)) => {
                report.folders += 1;
                report.messages += n;
                left = left.saturating_sub(n);
                report.complete &= caught_up;
            }
            Err(error) => {
                tracing::warn!(%error, "could not index a folder");
                report.complete = false;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Address, Flags};

    #[test]
    fn a_summary_becomes_an_index_row() {
        let summary = MessageSummary {
            uid: 9,
            flags: Flags {
                seen: true,
                ..Flags::default()
            },
            from: Some(Address {
                name: Some("Ann".into()),
                email: "ann@x.co".into(),
            }),
            to: vec![Address {
                name: None,
                email: "bob@x.co".into(),
            }],
            cc: vec![],
            subject: "Hi".into(),
            date: None,
            received: None,
            size: 1234,
            has_attachments: true,
            message_id: Some("m@x".into()),
            in_reply_to: None,
        };
        let row = indexed("Work/2024", 77, &summary, "hello".into());
        assert_eq!(
            (row.folder.as_str(), row.uid_validity, row.imap_uid),
            ("Work/2024", 77, 9)
        );
        assert_eq!(row.from_addr, "Ann <ann@x.co>");
        assert_eq!(row.to_addrs, "bob@x.co");
        assert_eq!(row.flags, ["\\Seen"]);
        assert!(row.has_attachments);
        assert_eq!(row.snippet, "hello");
    }
}
