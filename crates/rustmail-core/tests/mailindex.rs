//! Integration tests for the webmail search index. Requires `DATABASE_URL`.
mod common;

use chrono::Utc;
use rustmail_core::domain::{NewMailbox, UserRole};
use rustmail_core::mailindex::{IndexedMessage, MailIndex};
use rustmail_core::repository::{MailboxRepository, PgMailboxRepository};
use sqlx::PgPool;
use uuid::Uuid;

async fn mailbox(pool: &PgPool, domain: Uuid, local: &str) -> Uuid {
    let user = common::user(pool, domain, local, UserRole::User).await;
    PgMailboxRepository::new(pool.clone())
        .create(&NewMailbox {
            user_id: user.id,
            maildir_path: format!("example.com/{local}/"),
            quota_bytes: None,
        })
        .await
        .unwrap()
        .id
}

fn message(folder: &str, uid: i64, subject: &str, from: &str, snippet: &str) -> IndexedMessage {
    IndexedMessage {
        folder: folder.into(),
        uid_validity: 1,
        imap_uid: uid,
        message_id: Some(format!("<{uid}@x>")),
        in_reply_to: None,
        subject: subject.into(),
        from_addr: from.into(),
        to_addrs: "me@example.com".into(),
        cc_addrs: String::new(),
        snippet: snippet.into(),
        size_bytes: 100,
        flags: vec!["\\Seen".into()],
        has_attachments: false,
        date_header: Some(Utc::now()),
        received_at: Utc::now(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn searching_finds_parts_of_addresses_and_words_in_the_snippet(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let mb = mailbox(&pool, domain.id, "alice").await;
    let index = MailIndex::new(pool.clone());
    index
        .upsert(
            mb,
            &[
                message(
                    "INBOX",
                    1,
                    "Quarterly report",
                    "Bob Builder <bob.builder@acme-corp.com>",
                    "numbers attached",
                ),
                message(
                    "INBOX",
                    2,
                    "Lunch?",
                    "carol@example.org",
                    "pizza on thursday",
                ),
            ],
        )
        .await
        .unwrap();

    for (query, expected) in [
        ("quarterly", 1),
        ("builder", 1),
        ("acme", 1),
        ("bob@acme-corp.com", 1),
        ("pizza", 1),
        ("thursday pizza", 1),
        ("example", 2), // carol's domain, and the recipient address on both
        ("nonexistent", 0),
    ] {
        let page = index.search(mb, query, 10, 0).await.unwrap();
        assert_eq!(page.total, expected, "query {query:?}");
    }
    assert!(
        index.search(mb, "@.-", 10, 0).await.is_err(),
        "nothing searchable"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn mailboxes_never_see_each_others_mail(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let alice = mailbox(&pool, domain.id, "alice").await;
    let bob = mailbox(&pool, domain.id, "bob").await;
    let index = MailIndex::new(pool.clone());
    index
        .upsert(alice, &[message("INBOX", 1, "secret plans", "x@y.z", "")])
        .await
        .unwrap();
    assert_eq!(index.search(alice, "secret", 10, 0).await.unwrap().total, 1);
    assert_eq!(index.search(bob, "secret", 10, 0).await.unwrap().total, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reindexing_is_idempotent_and_keeps_an_existing_snippet(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let mb = mailbox(&pool, domain.id, "alice").await;
    let index = MailIndex::new(pool.clone());
    index
        .upsert(
            mb,
            &[message("INBOX", 1, "Hello", "a@b.c", "the full text")],
        )
        .await
        .unwrap();
    // A header-only refresh (empty snippet) must not erase what was learned before.
    index
        .upsert(mb, &[message("INBOX", 1, "Hello", "a@b.c", "")])
        .await
        .unwrap();
    assert_eq!(index.count(mb).await.unwrap(), 1);
    assert_eq!(index.search(mb, "full text", 10, 0).await.unwrap().total, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn paging_and_removal(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let mb = mailbox(&pool, domain.id, "alice").await;
    let index = MailIndex::new(pool.clone());
    let batch: Vec<_> = (1..=7)
        .map(|i| message("INBOX", i, "invoice", "a@b.c", ""))
        .collect();
    index.upsert(mb, &batch).await.unwrap();
    let page = index.search(mb, "invoice", 3, 6).await.unwrap();
    assert_eq!((page.total, page.hits.len()), (7, 1));
    index.remove(mb, "INBOX", 7).await.unwrap();
    assert_eq!(index.search(mb, "invoice", 10, 0).await.unwrap().total, 6);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sync_state_is_remembered_and_clearing_a_folder_forgets_everything(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let mb = mailbox(&pool, domain.id, "alice").await;
    let index = MailIndex::new(pool.clone());
    assert!(index.sync_state(mb, "INBOX").await.unwrap().is_none());
    index
        .upsert(
            mb,
            &[
                message("INBOX", 1, "one", "a@b.c", ""),
                message("Sent", 1, "two", "a@b.c", ""),
            ],
        )
        .await
        .unwrap();
    index.set_sync_state(mb, "INBOX", 5, 42).await.unwrap();
    let state = index.sync_state(mb, "INBOX").await.unwrap().unwrap();
    assert_eq!((state.uid_validity, state.last_uid), (5, 42));
    index.set_sync_state(mb, "INBOX", 5, 50).await.unwrap();
    assert_eq!(
        index
            .sync_state(mb, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .last_uid,
        50
    );

    index.clear_folder(mb, "INBOX").await.unwrap();
    assert!(index.sync_state(mb, "INBOX").await.unwrap().is_none());
    assert_eq!(
        index.count(mb).await.unwrap(),
        1,
        "other folders are untouched"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn hostile_search_text_is_just_text(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let mb = mailbox(&pool, domain.id, "alice").await;
    let index = MailIndex::new(pool.clone());
    index
        .upsert(mb, &[message("INBOX", 1, "hello", "a@b.c", "")])
        .await
        .unwrap();
    for q in [
        "'; DROP TABLE messages_meta; --",
        "hello | & ! ( ) <->",
        "\\",
        "\"unterminated",
        "a OR b AND -c",
    ] {
        // A (possibly empty) result or a clean validation error, never a crash.
        let _ = index.search(mb, q, 10, 0).await;
    }
    assert_eq!(index.count(mb).await.unwrap(), 1);
}
