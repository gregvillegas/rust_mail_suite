//! Tests for migration 0009: `quota_usage` addressed by user name as well as by
//! mailbox id. The SQL here mirrors what Dovecot's SQL dictionary sends.
//! Requires `DATABASE_URL` (see `make dev`).

mod common;

use rustmail_core::domain::{NewMailbox, UserRole};
use rustmail_core::repository::{
    MailboxRepository, PgMailboxRepository, PgQuotaRepository, PgUserRepository, QuotaRepository,
    UserRepository,
};
use sqlx::PgPool;
use uuid::Uuid;

/// Creates `example.com`, a user and their mailbox; returns `(mailbox_id, email)`.
async fn mailbox(pool: &PgPool, local: &str) -> (Uuid, String) {
    let domain =
        match sqlx::query_scalar::<_, Uuid>("SELECT id FROM domains WHERE name = 'example.com'")
            .fetch_optional(pool)
            .await
            .unwrap()
        {
            Some(id) => id,
            None => common::domain(pool, "example.com").await.id,
        };
    let user = common::user(pool, domain, local, UserRole::User).await;
    let mailbox = PgMailboxRepository::new(pool.clone())
        .create(&NewMailbox {
            user_id: user.id,
            maildir_path: format!("example.com/{local}/"),
            quota_bytes: None,
        })
        .await
        .unwrap();
    (mailbox.id, user.email)
}

/// What Dovecot's dict-sql runs to add to a counter.
async fn dovecot_increment(
    pool: &PgPool,
    username: &str,
    bytes: i64,
    messages: i64,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "INSERT INTO quota_usage (username, bytes_used, messages_count) VALUES ($1, $2, $3)
         ON CONFLICT (username) DO UPDATE
            SET bytes_used = quota_usage.bytes_used + $2, messages_count = quota_usage.messages_count + $3",
    )
    .bind(username)
    .bind(bytes)
    .bind(messages)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
}

#[sqlx::test(migrations = "../../migrations")]
async fn dovecot_can_insert_and_increment_by_username(pool: PgPool) {
    let (mailbox_id, email) = mailbox(&pool, "alice").await;

    dovecot_increment(&pool, &email, 1_000, 1).await.unwrap();
    dovecot_increment(&pool, &email, 500, 1).await.unwrap();
    dovecot_increment(&pool, &email, -200, -1).await.unwrap();

    // RustMail reads the same row through the mailbox id.
    let usage = PgQuotaRepository::new(pool.clone())
        .get(mailbox_id)
        .await
        .unwrap()
        .expect("row");
    assert_eq!((usage.bytes_used, usage.messages_count), (1_300, 1));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM quota_usage")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "one row, however it is addressed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn usernames_are_stored_lower_case(pool: PgPool) {
    let (mailbox_id, email) = mailbox(&pool, "alice").await;
    dovecot_increment(&pool, &email.to_uppercase(), 10, 1)
        .await
        .unwrap();

    let stored: String =
        sqlx::query_scalar("SELECT username FROM quota_usage WHERE mailbox_id = $1")
            .bind(mailbox_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, email);
    // A second spelling lands on the same row.
    dovecot_increment(&pool, "Alice@EXAMPLE.com", 5, 0)
        .await
        .unwrap();
    let bytes: i64 = sqlx::query_scalar("SELECT bytes_used FROM quota_usage WHERE username = $1")
        .bind(&email)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(bytes, 15);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rustmail_writes_fill_in_the_username(pool: PgPool) {
    let (mailbox_id, email) = mailbox(&pool, "alice").await;
    let repo = PgQuotaRepository::new(pool.clone());

    repo.apply_delta(mailbox_id, 700, 2).await.unwrap();
    let stored: String =
        sqlx::query_scalar("SELECT username FROM quota_usage WHERE mailbox_id = $1")
            .bind(mailbox_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, email);

    // And Dovecot's increments continue from RustMail's figures.
    dovecot_increment(&pool, &email, 300, 1).await.unwrap();
    let usage = repo.get(mailbox_id).await.unwrap().unwrap();
    assert_eq!((usage.bytes_used, usage.messages_count), (1_000, 3));
}

#[sqlx::test(migrations = "../../migrations")]
async fn users_without_a_mailbox_are_refused(pool: PgPool) {
    mailbox(&pool, "alice").await;
    let err = dovecot_increment(&pool, "ghost@example.com", 1, 1)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, sqlx::Error::Database(e) if e.is_foreign_key_violation()),
        "expected a foreign key violation, got {err:?}"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM quota_usage")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "no orphan row");
}

#[sqlx::test(migrations = "../../migrations")]
async fn conflicting_keys_are_refused(pool: PgPool) {
    let (alice_mailbox, _) = mailbox(&pool, "alice").await;
    let (_, bob_email) = mailbox(&pool, "bob").await;
    let err = sqlx::query("INSERT INTO quota_usage (mailbox_id, username) VALUES ($1, $2)")
        .bind(alice_mailbox)
        .bind(&bob_email)
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, sqlx::Error::Database(e) if e.is_foreign_key_violation()),
        "got {err:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn usage_is_clamped_at_zero_instead_of_failing_the_decrement(pool: PgPool) {
    let (mailbox_id, email) = mailbox(&pool, "alice").await;
    dovecot_increment(&pool, &email, 100, 1).await.unwrap();

    // More removed than recorded (e.g. after a manual recount): Dovecot must not
    // get an error, and the figures must not go negative.
    dovecot_increment(&pool, &email, -101, -2).await.unwrap();
    let usage = PgQuotaRepository::new(pool.clone())
        .get(mailbox_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((usage.bytes_used, usage.messages_count), (0, 0));

    // A first-ever write that is a decrement is also harmless.
    let (other_mailbox, other_email) = mailbox(&pool, "bob").await;
    dovecot_increment(&pool, &other_email, -50, -1)
        .await
        .unwrap();
    let usage = PgQuotaRepository::new(pool.clone())
        .get(other_mailbox)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((usage.bytes_used, usage.messages_count), (0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn usage_follows_the_user_away(pool: PgPool) {
    let (mailbox_id, email) = mailbox(&pool, "alice").await;
    dovecot_increment(&pool, &email, 100, 1).await.unwrap();

    let users = PgUserRepository::new(pool.clone());
    let user = users.find_by_email(&email).await.unwrap().unwrap();
    users.delete(user.id).await.unwrap();
    assert!(
        PgQuotaRepository::new(pool)
            .get(mailbox_id)
            .await
            .unwrap()
            .is_none(),
        "deleted with the user"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn updates_bump_the_timestamp(pool: PgPool) {
    let (_, email) = mailbox(&pool, "alice").await;
    dovecot_increment(&pool, &email, 1, 1).await.unwrap();
    let before: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM quota_usage")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("SELECT pg_sleep(0.05)")
        .execute(&pool)
        .await
        .unwrap();
    dovecot_increment(&pool, &email, 1, 1).await.unwrap();
    let after: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM quota_usage")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(after > before);
}
