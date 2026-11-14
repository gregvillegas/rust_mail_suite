//! `AccountService` against real PostgreSQL and a real (scratch) mail directory.
//! Requires `DATABASE_URL` (see `make dev`).

mod common;

use std::fs;

use common::Scratch;
use rustmail_core::domain::{AccountStatus, NewDomain, UserRole};
use rustmail_core::password::verify_password;
use rustmail_core::provisioning::{
    AccountService, CreateUser, MailboxProvisioner, ProvisionerConfig,
};
use rustmail_core::repository::{
    DomainRepository, MailboxRepository, PgDomainRepository, PgMailboxRepository, PgUserRepository,
    UserRepository,
};
use rustmail_core::Error;
use sqlx::PgPool;

const PASSWORD: &str = "correct-horse-battery-staple";

fn service(pool: &PgPool, scratch: &Scratch) -> AccountService {
    AccountService::postgres(
        pool,
        MailboxProvisioner::new(ProvisionerConfig::new(scratch.path())),
    )
}

fn request(email: &str) -> CreateUser {
    CreateUser {
        email: email.into(),
        password: PASSWORD.into(),
        display_name: "Test User".into(),
        role: UserRole::User,
        quota_bytes: None,
    }
}

fn domain(name: &str) -> NewDomain {
    NewDomain {
        name: name.into(),
        ..Default::default()
    }
}

fn maildir_complete(scratch: &Scratch, home: &str) -> bool {
    ["cur", "new", "tmp"]
        .iter()
        .all(|sub| scratch.path().join(home).join("Maildir").join(sub).is_dir())
}

// ─── domains ────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn creating_a_domain_creates_its_directory(pool: PgPool) {
    let scratch = Scratch::new();
    let created = service(&pool, &scratch)
        .create_domain(&domain("Example.COM"))
        .await
        .unwrap();

    assert_eq!(created.name, "example.com");
    assert!(scratch.path().join("example.com").is_dir());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_rejected_domain_leaves_no_directory(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&domain("example.com")).await.unwrap();

    let dup = svc.create_domain(&domain("example.com")).await;
    assert!(matches!(dup, Err(Error::Conflict(_))), "got {dup:?}");
    let bad = svc.create_domain(&domain("not a domain")).await;
    assert!(matches!(bad, Err(Error::Validation(_))), "got {bad:?}");

    let dirs: Vec<_> = fs::read_dir(scratch.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(dirs, ["example.com"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_domain_whose_directory_cannot_be_created_is_rolled_back(pool: PgPool) {
    let missing_base = std::env::temp_dir().join("rustmail-does-not-exist-base");
    let svc = AccountService::postgres(
        &pool,
        MailboxProvisioner::new(ProvisionerConfig::new(&missing_base)),
    );

    let result = svc.create_domain(&domain("example.com")).await;
    assert!(
        matches!(&result, Err(Error::Provisioning(m)) if m.contains("VMAIL_BASE")),
        "got {result:?}"
    );
    assert!(
        PgDomainRepository::new(pool)
            .list()
            .await
            .unwrap()
            .is_empty(),
        "no half-created domain"
    );
}

// ─── users ──────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn creating_a_user_creates_the_rows_and_the_maildir(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&domain("example.com")).await.unwrap();

    let created = svc
        .create_user(CreateUser {
            quota_bytes: Some(50_000_000),
            role: UserRole::DomainAdmin,
            ..request(" Alice.Smith@Example.COM ")
        })
        .await
        .unwrap();

    assert_eq!(created.user.email, "alice.smith@example.com");
    assert_eq!(created.user.role, UserRole::DomainAdmin);
    assert_eq!(created.user.display_name, "Test User");
    assert_eq!(created.user.status, AccountStatus::Active);
    assert_eq!(created.mailbox.maildir_path, "example.com/alice.smith/");
    assert_eq!(created.mailbox.quota_bytes, Some(50_000_000));
    assert!(created.maildir_created);
    assert!(maildir_complete(&scratch, "example.com/alice.smith"));

    // The password is stored only as an Argon2id hash that verifies.
    assert!(created.user.password_hash.starts_with("$argon2id$"));
    assert!(!created.user.password_hash.contains(PASSWORD));
    assert!(verify_password(PASSWORD, &created.user.password_hash).unwrap());

    // The database agrees with what was returned.
    let found = PgUserRepository::new(pool.clone())
        .find_by_email("ALICE.SMITH@example.com")
        .await
        .unwrap();
    assert_eq!(found.map(|u| u.id), Some(created.user.id));
    let mailbox = PgMailboxRepository::new(pool)
        .find_by_user(created.user.id)
        .await
        .unwrap();
    assert_eq!(mailbox.map(|m| m.id), Some(created.mailbox.id));
}

#[sqlx::test(migrations = "../../migrations")]
async fn bad_requests_change_nothing(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&domain("example.com")).await.unwrap();
    let domains = PgDomainRepository::new(pool.clone());
    let suspended = domains.create(&domain("suspended.org")).await.unwrap();
    domains
        .set_status(suspended.id, AccountStatus::Suspended)
        .await
        .unwrap();

    let cases: Vec<(&str, CreateUser)> = vec![
        ("unknown domain", request("alice@nowhere.example")),
        (
            "short password",
            CreateUser {
                password: "short".into(),
                ..request("alice@example.com")
            },
        ),
        (
            "negative quota",
            CreateUser {
                quota_bytes: Some(-1),
                ..request("alice@example.com")
            },
        ),
        ("malformed address", request("not-an-address")),
        ("path-like local part", request("../etc@example.com")),
        ("suspended domain", request("alice@suspended.org")),
    ];
    for (label, req) in cases {
        let result = svc.create_user(req).await;
        assert!(
            matches!(result, Err(Error::Validation(_))),
            "{label}: got {result:?}"
        );
    }

    assert!(PgUserRepository::new(pool)
        .find_by_email("alice@example.com")
        .await
        .unwrap()
        .is_none());
    assert!(!scratch.path().join("example.com/alice").exists());
    assert!(!scratch.path().join("suspended.org").exists());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_duplicate_address_is_a_conflict_and_leaves_the_first_user_intact(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&domain("example.com")).await.unwrap();
    let first = svc.create_user(request("alice@example.com")).await.unwrap();
    fs::write(
        scratch.path().join("example.com/alice/Maildir/new/1.msg"),
        b"mail",
    )
    .unwrap();

    let second = svc.create_user(request("ALICE@example.com")).await;
    assert!(matches!(second, Err(Error::Conflict(_))), "got {second:?}");

    assert!(
        scratch
            .path()
            .join("example.com/alice/Maildir/new/1.msg")
            .exists(),
        "existing mail untouched"
    );
    let found = PgUserRepository::new(pool)
        .find_by_email("alice@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, first.user.id);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_domains_mailbox_limit_is_enforced(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&NewDomain {
        name: "example.com".into(),
        default_quota_bytes: None,
        max_mailboxes: Some(2),
    })
    .await
    .unwrap();

    svc.create_user(request("a@example.com")).await.unwrap();
    svc.create_user(request("b@example.com")).await.unwrap();
    let third = svc.create_user(request("c@example.com")).await;
    assert!(
        matches!(&third, Err(Error::Conflict(m)) if m.contains("limit of 2")),
        "got {third:?}"
    );
    assert!(!scratch.path().join("example.com/c").exists());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_disk_failure_removes_the_user_again(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&domain("example.com")).await.unwrap();
    let alice = svc.create_user(request("alice@example.com")).await.unwrap();
    // A regular file where bob's home directory should go.
    fs::write(scratch.path().join("example.com/bob"), b"in the way").unwrap();

    let result = svc.create_user(request("bob@example.com")).await;
    assert!(
        matches!(&result, Err(Error::Provisioning(m)) if m.contains("not a directory")),
        "got {result:?}"
    );

    // Neither the user nor the mailbox row survived; alice is unaffected.
    let users = PgUserRepository::new(pool.clone());
    assert!(users
        .find_by_email("bob@example.com")
        .await
        .unwrap()
        .is_none());
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM mailboxes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
    assert!(users
        .find_by_email("alice@example.com")
        .await
        .unwrap()
        .is_some());
    assert!(maildir_complete(&scratch, "example.com/alice"));
    assert_eq!(alice.user.email, "alice@example.com");
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_creation_of_the_same_user_has_one_winner(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&domain("example.com")).await.unwrap();

    let (a, b) = tokio::join!(
        svc.create_user(request("alice@example.com")),
        svc.create_user(request("alice@example.com"))
    );
    let outcomes = [a.is_ok(), b.is_ok()];
    assert_eq!(
        outcomes.iter().filter(|ok| **ok).count(),
        1,
        "exactly one succeeds: {outcomes:?}"
    );
    let loser = if a.is_err() { a } else { b };
    assert!(
        matches!(loser, Err(Error::Conflict(_))),
        "the loser gets a conflict"
    );
    assert!(maildir_complete(&scratch, "example.com/alice"));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(users, 1);
}

// ─── maintenance ────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn a_missing_maildir_can_be_recreated_by_address(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&domain("example.com")).await.unwrap();
    svc.create_user(request("alice@example.com")).await.unwrap();

    assert!(
        !svc.ensure_maildir_for("alice@example.com")
            .await
            .unwrap()
            .created,
        "already complete"
    );
    fs::remove_dir_all(scratch.path().join("example.com/alice")).unwrap();
    assert!(
        svc.ensure_maildir_for("Alice@Example.com")
            .await
            .unwrap()
            .created
    );
    assert!(maildir_complete(&scratch, "example.com/alice"));

    let missing = svc.ensure_maildir_for("nobody@example.com").await;
    assert!(
        matches!(missing, Err(Error::NotFound { entity: "user" })),
        "got {missing:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_check_finds_and_repairs_missing_maildirs(pool: PgPool) {
    let scratch = Scratch::new();
    let svc = service(&pool, &scratch);
    svc.create_domain(&domain("example.com")).await.unwrap();
    svc.create_domain(&domain("other.org")).await.unwrap();
    for email in ["alice@example.com", "bob@example.com", "carol@other.org"] {
        svc.create_user(request(email)).await.unwrap();
    }

    let clean = svc.check_mailboxes(false).await.unwrap();
    assert_eq!(
        (clean.checked, clean.missing.len(), clean.repaired),
        (3, 0, 0)
    );

    // Simulate restoring the database without the mail, and a half-built Maildir.
    fs::remove_dir_all(scratch.path().join("example.com/alice")).unwrap();
    fs::remove_dir(scratch.path().join("other.org/carol/Maildir/tmp")).unwrap();

    let report = svc.check_mailboxes(false).await.unwrap();
    assert_eq!(report.checked, 3);
    let mut found: Vec<_> = report.missing.iter().map(|m| m.email.as_str()).collect();
    found.sort_unstable();
    assert_eq!(found, ["alice@example.com", "carol@other.org"]);
    assert_eq!(report.repaired, 0, "a plain check changes nothing");
    assert!(!scratch.path().join("example.com/alice").exists());

    let repaired = svc.check_mailboxes(true).await.unwrap();
    assert_eq!((repaired.missing.len(), repaired.repaired), (2, 2));
    assert_eq!(svc.check_mailboxes(false).await.unwrap().missing.len(), 0);
    assert!(
        maildir_complete(&scratch, "example.com/alice")
            && maildir_complete(&scratch, "other.org/carol")
    );
}

#[test]
fn the_create_request_never_prints_the_password() {
    let debug = format!("{:?}", request("alice@example.com"));
    assert!(!debug.contains(PASSWORD) && debug.contains("<redacted>"));
}
