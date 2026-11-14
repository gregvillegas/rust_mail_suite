//! Schema-level tests: constraints, indexes, triggers and `FromRow` decoding
//! for models that do not have repositories yet. Each test gets its own freshly
//! migrated database. Requires `DATABASE_URL` (see `make dev`).

mod common;

use rustmail_core::domain::{
    Autoresponder, CalendarEvent, Contact, MailFilter, MessageMeta, NewMailbox, SieveScript,
    UserRole,
};
use rustmail_core::repository::{MailboxRepository, PgMailboxRepository};
use sqlx::PgPool;
use uuid::Uuid;

fn is_unique(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(e) if e.is_unique_violation())
}

fn is_check(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(e) if e.is_check_violation())
}

/// Creates `example.com`, a user and (optionally) their mailbox.
async fn fixture(pool: &PgPool) -> (Uuid, Uuid) {
    let domain = common::domain(pool, "example.com").await;
    let user = common::user(pool, domain.id, "alice", UserRole::User).await;
    let mailbox = PgMailboxRepository::new(pool.clone())
        .create(&NewMailbox {
            user_id: user.id,
            maildir_path: "example.com/alice/".into(),
            quota_bytes: None,
        })
        .await
        .unwrap();
    (user.id, mailbox.id)
}

#[sqlx::test(migrations = "../../migrations")]
async fn migrations_create_every_expected_table(pool: PgPool) {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' AND tablename NOT LIKE '\\_%' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for expected in [
        "domains",
        "users",
        "mailboxes",
        "aliases",
        "groups",
        "group_members",
        "messages_meta",
        "sessions",
        "audit_log",
        "quota_usage",
        "sieve_scripts",
        "autoresponders",
        "mail_filters",
        "calendar_events",
        "contacts",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}; have {tables:?}"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn domain_name_check_rejects_uppercase_and_updated_at_trigger_fires(pool: PgPool) {
    let bad = sqlx::query("INSERT INTO domains (name) VALUES ('Example.COM')")
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(is_check(&bad), "got {bad:?}");

    let domain = common::domain(&pool, "example.com").await;
    sqlx::query("SELECT pg_sleep(0.05)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE domains SET status = 'suspended' WHERE id = $1")
        .bind(domain.id)
        .execute(&pool)
        .await
        .unwrap();
    let updated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM domains WHERE id = $1")
            .bind(domain.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        updated_at > domain.updated_at,
        "trigger should bump updated_at"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn messages_meta_full_text_search_and_decoding(pool: PgPool) {
    let (_, mailbox_id) = fixture(&pool).await;

    for (uid, subject, from, snippet) in [
        (
            1_i64,
            "Quarterly invoice attached",
            "billing@vendor.test",
            "Please find the invoice",
        ),
        (2, "Lunch on Friday?", "bob@example.com", "Pizza or sushi"),
    ] {
        sqlx::query(
            "INSERT INTO messages_meta
                 (mailbox_id, folder, uid_validity, imap_uid, message_id, subject, from_addr,
                  to_addrs, snippet, size_bytes, flags, has_attachments)
             VALUES ($1, 'INBOX', 1, $2, $3, $4, $5, 'alice@example.com', $6, 2048,
                     ARRAY['\\Seen'], $7)",
        )
        .bind(mailbox_id)
        .bind(uid)
        .bind(format!("<{uid}@test>"))
        .bind(subject)
        .bind(from)
        .bind(snippet)
        .bind(uid == 1)
        .execute(&pool)
        .await
        .unwrap();
    }

    let hits: Vec<MessageMeta> = sqlx::query_as(
        "SELECT * FROM messages_meta WHERE search_vector @@ plainto_tsquery('simple', $1)",
    )
    .bind("invoice")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.subject, "Quarterly invoice attached");
    assert_eq!(hit.flags, vec!["\\Seen".to_owned()]);
    assert!(hit.has_attachments);
    assert_eq!(hit.size_bytes, 2048);

    // Sender addresses are searchable too.
    let by_sender: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM messages_meta WHERE search_vector @@ plainto_tsquery('simple', 'sushi')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(by_sender, 1);

    // A (folder, uidvalidity, uid) triple identifies exactly one message.
    let dup = sqlx::query(
        "INSERT INTO messages_meta (mailbox_id, folder, uid_validity, imap_uid)
         VALUES ($1, 'INBOX', 1, 1)",
    )
    .bind(mailbox_id)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(is_unique(&dup), "got {dup:?}");

    // The index is removed with the mailbox.
    sqlx::query("DELETE FROM mailboxes WHERE id = $1")
        .bind(mailbox_id)
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM messages_meta")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn only_one_sieve_script_can_be_active_per_user(pool: PgPool) {
    let (user_id, _) = fixture(&pool).await;
    let insert = |name: &'static str, active: bool| {
        let pool = pool.clone();
        async move {
            sqlx::query("INSERT INTO sieve_scripts (user_id, name, content, is_active) VALUES ($1, $2, 'keep;', $3)")
                .bind(user_id)
                .bind(name)
                .bind(active)
                .execute(&pool)
                .await
        }
    };

    insert("main", true).await.unwrap();
    insert("backup", false).await.unwrap();
    let second_active = insert("other", true).await.unwrap_err();
    assert!(is_unique(&second_active), "got {second_active:?}");
    let same_name = insert("main", false).await.unwrap_err();
    assert!(is_unique(&same_name), "got {same_name:?}");

    let scripts: Vec<SieveScript> =
        sqlx::query_as("SELECT * FROM sieve_scripts WHERE user_id = $1 ORDER BY name")
            .bind(user_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(scripts.iter().filter(|s| s.is_active).count(), 1);
    assert_eq!(scripts[0].content, "keep;");
}

#[sqlx::test(migrations = "../../migrations")]
async fn autoresponder_constraints(pool: PgPool) {
    let (user_id, _) = fixture(&pool).await;

    sqlx::query(
        "INSERT INTO autoresponders (user_id, enabled, subject, body, starts_at, ends_at)
         VALUES ($1, true, 'Away', 'Back soon', now(), now() + interval '7 days')",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    let again = sqlx::query("INSERT INTO autoresponders (user_id) VALUES ($1)")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(is_unique(&again), "one responder per user; got {again:?}");

    let responder: Autoresponder =
        sqlx::query_as("SELECT * FROM autoresponders WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(responder.enabled);
    assert_eq!(responder.reply_interval_days, 7);
    assert!(responder.ends_at.unwrap() > responder.starts_at.unwrap());

    let reversed = sqlx::query(
        "UPDATE autoresponders SET starts_at = now(), ends_at = now() - interval '1 day' WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(is_check(&reversed), "got {reversed:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn mail_filters_round_trip_json(pool: PgPool) {
    let (user_id, _) = fixture(&pool).await;
    let conditions = serde_json::json!([{ "header": "from", "contains": "boss@" }]);
    let actions = serde_json::json!([{ "move_to": "Boss" }]);

    sqlx::query(
        "INSERT INTO mail_filters (user_id, name, position, conditions, actions, stop_processing)
         VALUES ($1, 'Boss mail', 1, $2, $3, true)",
    )
    .bind(user_id)
    .bind(&conditions)
    .bind(&actions)
    .execute(&pool)
    .await
    .unwrap();

    let filter: MailFilter = sqlx::query_as("SELECT * FROM mail_filters WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(filter.conditions, conditions);
    assert_eq!(filter.actions, actions);
    assert!(filter.enabled && filter.stop_processing);
}

#[sqlx::test(migrations = "../../migrations")]
async fn calendar_and_contact_stubs(pool: PgPool) {
    let (user_id, _) = fixture(&pool).await;

    sqlx::query(
        "INSERT INTO calendar_events (owner_user_id, uid, title, starts_at, ends_at)
         VALUES ($1, 'evt-1', 'Standup', now(), now() + interval '15 minutes')",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    let backwards = sqlx::query(
        "INSERT INTO calendar_events (owner_user_id, uid, starts_at, ends_at)
         VALUES ($1, 'evt-2', now(), now() - interval '1 hour')",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(is_check(&backwards), "got {backwards:?}");

    let dup_uid = sqlx::query(
        "INSERT INTO calendar_events (owner_user_id, uid, starts_at, ends_at) VALUES ($1, 'evt-1', now(), now())",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(is_unique(&dup_uid), "got {dup_uid:?}");

    let event: CalendarEvent = sqlx::query_as("SELECT * FROM calendar_events WHERE uid = 'evt-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(event.title, "Standup");
    assert!(!event.all_day && event.rrule.is_none());

    sqlx::query("INSERT INTO contacts (owner_user_id, uid, full_name, email) VALUES ($1, 'c-1', 'Bob', 'bob@example.com')")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    let contact: Contact = sqlx::query_as("SELECT * FROM contacts WHERE uid = 'c-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(contact.email, "bob@example.com");
    assert!(contact.vcard.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleting_a_user_removes_personal_data(pool: PgPool) {
    let (user_id, _) = fixture(&pool).await;
    sqlx::query("INSERT INTO sieve_scripts (user_id, name, content) VALUES ($1, 's', 'keep;')")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO contacts (owner_user_id, uid) VALUES ($1, 'c')")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();

    for table in ["mailboxes", "sieve_scripts", "contacts"] {
        let remaining: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0, "{table} should be empty after user deletion");
    }
}
