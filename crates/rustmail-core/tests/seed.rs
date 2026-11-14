//! Tests for the first-run seed. Requires `DATABASE_URL` (see `make dev`).

use rustmail_core::domain::{AuditQuery, UserRole};
use rustmail_core::password::verify_password;
use rustmail_core::repository::{
    AuditLogRepository, DomainRepository, MailboxRepository, PgAuditLogRepository,
    PgDomainRepository, PgMailboxRepository, PgUserRepository, UserRepository,
};
use rustmail_core::seed::{seed, SeedRequest};
use rustmail_core::Error;
use sqlx::PgPool;

fn request(password: Option<&str>) -> SeedRequest {
    SeedRequest {
        domain: "Example.COM".into(),
        admin_local_part: "Admin".into(),
        admin_display_name: "Administrator".into(),
        admin_password: password.map(str::to_owned),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn first_run_creates_domain_super_admin_mailbox_and_audit_entry(pool: PgPool) {
    let outcome = seed(&pool, &request(Some("a-long-enough-password")))
        .await
        .unwrap();

    assert!(outcome.domain_created && outcome.admin_created && outcome.mailbox_created);
    assert_eq!(outcome.domain.name, "example.com");
    assert_eq!(outcome.admin.email, "admin@example.com");
    assert_eq!(outcome.admin.role, UserRole::SuperAdmin);
    assert!(
        outcome.generated_password.is_none(),
        "caller supplied the password"
    );

    // The stored hash verifies, and the plaintext is not what is stored.
    let stored = PgUserRepository::new(pool.clone())
        .get(outcome.admin.id)
        .await
        .unwrap();
    assert!(stored.password_hash.starts_with("$argon2id$"));
    assert!(verify_password("a-long-enough-password", &stored.password_hash).unwrap());
    assert!(!verify_password("something-else-entirely", &stored.password_hash).unwrap());

    let mailbox = PgMailboxRepository::new(pool.clone())
        .find_by_user(outcome.admin.id)
        .await
        .unwrap()
        .expect("mailbox row");
    assert_eq!(mailbox.maildir_path, "example.com/admin/");

    let audit = PgAuditLogRepository::new(pool)
        .list(&AuditQuery::default())
        .await
        .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "system.seed");
    assert_eq!(audit[0].details["admin_created"], true);
    assert!(!audit[0]
        .details
        .to_string()
        .contains("a-long-enough-password"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn generated_password_is_returned_once_and_verifies(pool: PgPool) {
    let outcome = seed(&pool, &request(None)).await.unwrap();
    let password = outcome.generated_password.expect("generated password");
    assert_eq!(password.len(), 32);
    assert!(verify_password(&password, &outcome.admin.password_hash).unwrap());
}

#[sqlx::test(migrations = "../../migrations")]
async fn rerunning_is_idempotent_and_never_resets_the_password(pool: PgPool) {
    let first = seed(&pool, &request(Some("original-password-1")))
        .await
        .unwrap();

    // A second run with a *different* password must not touch the account.
    let second = seed(&pool, &request(Some("different-password-2")))
        .await
        .unwrap();
    assert!(!second.domain_created && !second.admin_created && !second.mailbox_created);
    assert!(second.generated_password.is_none());
    assert_eq!(second.admin.id, first.admin.id);
    assert!(verify_password("original-password-1", &second.admin.password_hash).unwrap());
    assert!(!verify_password("different-password-2", &second.admin.password_hash).unwrap());

    // A third run without any password does not generate one either.
    let third = seed(&pool, &request(None)).await.unwrap();
    assert!(third.generated_password.is_none());

    assert_eq!(
        PgDomainRepository::new(pool.clone())
            .list()
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        PgUserRepository::new(pool)
            .count_by_role(UserRole::SuperAdmin)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_missing_mailbox_is_recreated_on_rerun(pool: PgPool) {
    let first = seed(&pool, &request(Some("a-long-enough-password")))
        .await
        .unwrap();
    let mailboxes = PgMailboxRepository::new(pool.clone());
    let mailbox = mailboxes
        .find_by_user(first.admin.id)
        .await
        .unwrap()
        .unwrap();
    mailboxes.delete(mailbox.id).await.unwrap();

    let healed = seed(&pool, &request(Some("a-long-enough-password")))
        .await
        .unwrap();
    assert!(!healed.admin_created);
    assert!(healed.mailbox_created);
    assert!(mailboxes
        .find_by_user(first.admin.id)
        .await
        .unwrap()
        .is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_input_is_rejected_before_anything_is_written(pool: PgPool) {
    let short = seed(&pool, &request(Some("short"))).await;
    assert!(matches!(short, Err(Error::Validation(_))), "got {short:?}");

    let bad_domain = seed(
        &pool,
        &SeedRequest {
            domain: "localhost".into(),
            ..request(None)
        },
    )
    .await;
    assert!(
        matches!(bad_domain, Err(Error::Validation(_))),
        "got {bad_domain:?}"
    );

    let bad_local = seed(
        &pool,
        &SeedRequest {
            admin_local_part: "a b".into(),
            ..request(None)
        },
    )
    .await;
    assert!(
        matches!(bad_local, Err(Error::Validation(_))),
        "got {bad_local:?}"
    );

    assert!(PgDomainRepository::new(pool.clone())
        .list()
        .await
        .unwrap()
        .is_empty());
    assert!(PgAuditLogRepository::new(pool)
        .list(&AuditQuery::default())
        .await
        .unwrap()
        .is_empty());
}

#[test]
fn seed_request_debug_redacts_the_password() {
    let debug = format!("{:?}", request(Some("super-secret-password")));
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("super-secret-password"));
}
