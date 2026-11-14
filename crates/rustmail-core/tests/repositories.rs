//! Integration tests for the PostgreSQL repositories. Each test runs against
//! its own freshly migrated database (`#[sqlx::test]`), so they are isolated.
//! Requires `DATABASE_URL` to point at a Postgres server (see `make dev`).

mod common;

use chrono::{Duration, Utc};
use rustmail_core::domain::{
    AccountStatus, AuditOutcome, AuditQuery, NewAlias, NewAuditEntry, NewDomain, NewGroup,
    NewMailbox, NewSession, NewUser, UserRole,
};
use rustmail_core::repository::{
    AliasRepository, AuditLogRepository, DomainRepository, GroupRepository, MailboxRepository,
    PgAliasRepository, PgAuditLogRepository, PgDomainRepository, PgGroupRepository,
    PgMailboxRepository, PgQuotaRepository, PgSessionRepository, PgUserRepository, QuotaRepository,
    SessionRepository, UserRepository,
};
use rustmail_core::Error;
use sqlx::PgPool;
use uuid::Uuid;

const MIGRATIONS: &str = "../../migrations";

// ─── domains ────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn domain_lifecycle(pool: PgPool) {
    let repo = PgDomainRepository::new(pool.clone());

    let created = repo
        .create(&NewDomain {
            name: "  Example.COM ".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(created.name, "example.com");
    assert_eq!(created.status, AccountStatus::Active);

    let dup = repo
        .create(&NewDomain {
            name: "example.com".into(),
            ..Default::default()
        })
        .await;
    assert!(matches!(dup, Err(Error::Conflict(_))), "got {dup:?}");

    let bad = repo
        .create(&NewDomain {
            name: "localhost".into(),
            ..Default::default()
        })
        .await;
    assert!(matches!(bad, Err(Error::Validation(_))), "got {bad:?}");

    let found = repo.find_by_name("EXAMPLE.com").await.unwrap();
    assert_eq!(found.map(|d| d.id), Some(created.id));
    assert!(repo.find_by_name("nope.example").await.unwrap().is_none());
    assert!(repo.find_by_name("not a domain").await.unwrap().is_none());

    repo.set_status(created.id, AccountStatus::Suspended)
        .await
        .unwrap();
    assert_eq!(
        repo.get(created.id).await.unwrap().status,
        AccountStatus::Suspended
    );
    assert_eq!(repo.list().await.unwrap().len(), 1);

    repo.delete(created.id).await.unwrap();
    assert!(matches!(
        repo.get(created.id).await,
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        repo.delete(created.id).await,
        Err(Error::NotFound { .. })
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn negative_default_quota_is_rejected(pool: PgPool) {
    let repo = PgDomainRepository::new(pool);
    let result = repo
        .create(&NewDomain {
            name: "example.com".into(),
            default_quota_bytes: Some(-1),
            max_mailboxes: None,
        })
        .await;
    assert!(
        matches!(result, Err(Error::Validation(_))),
        "got {result:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn domain_with_users_cannot_be_deleted(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    common::user(&pool, domain.id, "alice", UserRole::User).await;

    let result = PgDomainRepository::new(pool).delete(domain.id).await;
    assert!(matches!(result, Err(Error::Conflict(_))), "got {result:?}");
}

// ─── users ──────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn user_address_is_built_from_domain_and_normalised(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let repo = PgUserRepository::new(pool.clone());

    let alice = common::user(&pool, domain.id, "Alice.Smith", UserRole::User).await;
    assert_eq!(alice.local_part, "alice.smith");
    assert_eq!(alice.email, "alice.smith@example.com");
    assert_eq!(alice.status, AccountStatus::Active);

    let found = repo
        .find_by_email(" ALICE.smith@Example.COM ")
        .await
        .unwrap();
    assert_eq!(found.map(|u| u.id), Some(alice.id));
    assert!(repo
        .find_by_email("missing@example.com")
        .await
        .unwrap()
        .is_none());
    assert!(repo.find_by_email("garbage").await.unwrap().is_none());

    assert_eq!(repo.list_by_domain(domain.id).await.unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn user_creation_errors(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let repo = PgUserRepository::new(pool.clone());
    common::user(&pool, domain.id, "alice", UserRole::User).await;

    let new = |domain_id: Uuid, local: &str, hash: &str| NewUser {
        domain_id,
        local_part: local.into(),
        password_hash: hash.into(),
        display_name: String::new(),
        role: UserRole::User,
        locale: "en".into(),
    };

    let dup = repo
        .create(&new(domain.id, "ALICE", common::FAKE_HASH))
        .await;
    assert!(matches!(dup, Err(Error::Conflict(_))), "got {dup:?}");

    let no_domain = repo
        .create(&new(Uuid::new_v4(), "bob", common::FAKE_HASH))
        .await;
    assert!(
        matches!(no_domain, Err(Error::NotFound { .. })),
        "got {no_domain:?}"
    );

    let bad_local = repo
        .create(&new(domain.id, "bad local", common::FAKE_HASH))
        .await;
    assert!(
        matches!(bad_local, Err(Error::Validation(_))),
        "got {bad_local:?}"
    );

    let empty_hash = repo.create(&new(domain.id, "carol", "")).await;
    assert!(
        matches!(empty_hash, Err(Error::Validation(_))),
        "got {empty_hash:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn user_updates_and_role_counts(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let repo = PgUserRepository::new(pool.clone());
    let admin = common::user(&pool, domain.id, "admin", UserRole::SuperAdmin).await;
    let alice = common::user(&pool, domain.id, "alice", UserRole::User).await;

    assert_eq!(repo.count_by_role(UserRole::SuperAdmin).await.unwrap(), 1);
    assert_eq!(repo.count_by_role(UserRole::User).await.unwrap(), 1);
    assert_eq!(repo.count_by_role(UserRole::DomainAdmin).await.unwrap(), 0);

    repo.set_role(alice.id, UserRole::DomainAdmin)
        .await
        .unwrap();
    repo.set_status(alice.id, AccountStatus::Disabled)
        .await
        .unwrap();
    let alice = repo.get(alice.id).await.unwrap();
    assert_eq!(alice.role, UserRole::DomainAdmin);
    assert_eq!(alice.status, AccountStatus::Disabled);

    repo.set_password_hash(admin.id, "$argon2id$new")
        .await
        .unwrap();
    assert_eq!(
        repo.get(admin.id).await.unwrap().password_hash,
        "$argon2id$new"
    );
    let empty = repo.set_password_hash(admin.id, "").await;
    assert!(matches!(empty, Err(Error::Validation(_))), "got {empty:?}");

    let missing = repo.set_role(Uuid::new_v4(), UserRole::User).await;
    assert!(
        matches!(missing, Err(Error::NotFound { .. })),
        "got {missing:?}"
    );

    repo.delete(alice.id).await.unwrap();
    assert!(matches!(
        repo.get(alice.id).await,
        Err(Error::NotFound { .. })
    ));
}

// ─── mailboxes and quota ────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn mailbox_rules_and_effective_quota(pool: PgPool) {
    let domain = PgDomainRepository::new(pool.clone())
        .create(&NewDomain {
            name: "example.com".into(),
            default_quota_bytes: Some(1_000),
            max_mailboxes: None,
        })
        .await
        .unwrap();
    let alice = common::user(&pool, domain.id, "alice", UserRole::User).await;
    let repo = PgMailboxRepository::new(pool.clone());

    let new = |path: &str| NewMailbox {
        user_id: alice.id,
        maildir_path: path.into(),
        quota_bytes: None,
    };

    for bad in ["/abs/path", "../escape", "a/../b", ""] {
        let result = repo.create(&new(bad)).await;
        assert!(
            matches!(result, Err(Error::Validation(_))),
            "{bad:?} gave {result:?}"
        );
    }

    let mailbox = repo.create(&new("example.com/alice/")).await.unwrap();
    assert_eq!(mailbox.status, AccountStatus::Active);
    assert_eq!(
        repo.find_by_user(alice.id).await.unwrap().map(|m| m.id),
        Some(mailbox.id)
    );

    let second = repo.create(&new("example.com/alice2/")).await;
    assert!(matches!(second, Err(Error::Conflict(_))), "got {second:?}");

    // Inherits the domain default until overridden.
    assert_eq!(
        repo.effective_quota_bytes(mailbox.id).await.unwrap(),
        Some(1_000)
    );
    repo.set_quota(mailbox.id, Some(50)).await.unwrap();
    assert_eq!(
        repo.effective_quota_bytes(mailbox.id).await.unwrap(),
        Some(50)
    );
    repo.set_quota(mailbox.id, None).await.unwrap();
    assert_eq!(
        repo.effective_quota_bytes(mailbox.id).await.unwrap(),
        Some(1_000)
    );

    repo.set_status(mailbox.id, AccountStatus::Suspended)
        .await
        .unwrap();
    assert_eq!(
        repo.get(mailbox.id).await.unwrap().status,
        AccountStatus::Suspended
    );

    // Deleting the user removes the mailbox with it.
    PgUserRepository::new(pool).delete(alice.id).await.unwrap();
    assert!(repo.find_by_user(alice.id).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn unlimited_quota_when_nothing_is_set(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let alice = common::user(&pool, domain.id, "alice", UserRole::User).await;
    let repo = PgMailboxRepository::new(pool);
    let mailbox = repo
        .create(&NewMailbox {
            user_id: alice.id,
            maildir_path: "example.com/alice/".into(),
            quota_bytes: None,
        })
        .await
        .unwrap();
    assert_eq!(repo.effective_quota_bytes(mailbox.id).await.unwrap(), None);
}

#[sqlx::test(migrations = "../../migrations")]
async fn quota_usage_tracking(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let alice = common::user(&pool, domain.id, "alice", UserRole::User).await;
    let mailbox = PgMailboxRepository::new(pool.clone())
        .create(&NewMailbox {
            user_id: alice.id,
            maildir_path: "example.com/alice/".into(),
            quota_bytes: None,
        })
        .await
        .unwrap();
    let repo = PgQuotaRepository::new(pool.clone());

    assert!(repo.get(mailbox.id).await.unwrap().is_none());

    let usage = repo.apply_delta(mailbox.id, 500, 2).await.unwrap();
    assert_eq!((usage.bytes_used, usage.messages_count), (500, 2));
    let usage = repo.apply_delta(mailbox.id, -200, -1).await.unwrap();
    assert_eq!((usage.bytes_used, usage.messages_count), (300, 1));

    // Usage never goes below zero.
    let usage = repo.apply_delta(mailbox.id, -10_000, -10).await.unwrap();
    assert_eq!((usage.bytes_used, usage.messages_count), (0, 0));

    let usage = repo.set_usage(mailbox.id, 42, 7).await.unwrap();
    assert_eq!((usage.bytes_used, usage.messages_count), (42, 7));
    assert_eq!(
        repo.get(mailbox.id).await.unwrap().map(|u| u.bytes_used),
        Some(42)
    );

    let negative = repo.set_usage(mailbox.id, -1, 0).await;
    assert!(
        matches!(negative, Err(Error::Validation(_))),
        "got {negative:?}"
    );

    let unknown = repo.apply_delta(Uuid::new_v4(), 1, 1).await;
    assert!(
        matches!(unknown, Err(Error::Conflict(_))),
        "got {unknown:?}"
    );

    // Removing the user cascades through the mailbox to its usage row.
    PgUserRepository::new(pool).delete(alice.id).await.unwrap();
    assert!(repo.get(mailbox.id).await.unwrap().is_none());
}

// ─── aliases ────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn alias_rules_and_resolution(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let other = common::domain(&pool, "other.org").await;
    let repo = PgAliasRepository::new(pool);

    let new = |domain_id: Uuid, address: &str, destination: &str| NewAlias {
        domain_id,
        address: address.into(),
        destination: destination.into(),
    };

    let sales_a = repo
        .create(&new(domain.id, "Sales@Example.com", "alice@example.com"))
        .await
        .unwrap();
    assert_eq!(sales_a.address, "sales@example.com");
    repo.create(&new(domain.id, "sales@example.com", "bob@elsewhere.net"))
        .await
        .unwrap();

    let dup = repo
        .create(&new(domain.id, "sales@example.com", "alice@example.com"))
        .await;
    assert!(matches!(dup, Err(Error::Conflict(_))), "got {dup:?}");

    let wrong_domain = repo
        .create(&new(other.id, "sales@example.com", "x@example.com"))
        .await;
    assert!(
        matches!(wrong_domain, Err(Error::Validation(_))),
        "got {wrong_domain:?}"
    );

    let self_loop = repo
        .create(&new(domain.id, "loop@example.com", "loop@example.com"))
        .await;
    assert!(
        matches!(self_loop, Err(Error::Validation(_))),
        "got {self_loop:?}"
    );

    let invalid = repo
        .create(&new(domain.id, "not-an-address", "a@b.com"))
        .await;
    assert!(
        matches!(invalid, Err(Error::Validation(_))),
        "got {invalid:?}"
    );

    assert_eq!(
        repo.resolve("SALES@example.com").await.unwrap(),
        vec![
            "alice@example.com".to_owned(),
            "bob@elsewhere.net".to_owned()
        ]
    );
    assert!(repo.resolve("nobody@example.com").await.unwrap().is_empty());
    assert!(repo.resolve("garbage").await.unwrap().is_empty());

    repo.set_enabled(sales_a.id, false).await.unwrap();
    assert_eq!(
        repo.resolve("sales@example.com").await.unwrap(),
        vec!["bob@elsewhere.net".to_owned()]
    );

    assert_eq!(repo.list_by_domain(domain.id).await.unwrap().len(), 2);
    repo.delete(sales_a.id).await.unwrap();
    assert!(matches!(
        repo.delete(sales_a.id).await,
        Err(Error::NotFound { .. })
    ));
}

// ─── groups ─────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn group_membership(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let alice = common::user(&pool, domain.id, "alice", UserRole::User).await;
    let bob = common::user(&pool, domain.id, "bob", UserRole::User).await;
    let repo = PgGroupRepository::new(pool.clone());

    let new = |name: &str, address: Option<&str>| NewGroup {
        domain_id: domain.id,
        name: name.into(),
        address: address.map(str::to_owned),
        description: String::new(),
    };

    let team = repo
        .create(&new("Team", Some("Team@Example.com")))
        .await
        .unwrap();
    assert_eq!(team.address.as_deref(), Some("team@example.com"));

    let dup_name = repo.create(&new("Team", None)).await;
    assert!(
        matches!(dup_name, Err(Error::Conflict(_))),
        "got {dup_name:?}"
    );

    let foreign = repo.create(&new("Foreign", Some("list@other.org"))).await;
    assert!(
        matches!(foreign, Err(Error::Validation(_))),
        "got {foreign:?}"
    );

    // Adding twice is harmless.
    repo.add_member(team.id, alice.id).await.unwrap();
    repo.add_member(team.id, alice.id).await.unwrap();
    repo.add_member(team.id, bob.id).await.unwrap();
    assert_eq!(repo.list_member_ids(team.id).await.unwrap().len(), 2);

    repo.remove_member(team.id, bob.id).await.unwrap();
    assert_eq!(repo.list_member_ids(team.id).await.unwrap(), vec![alice.id]);

    // Deleting a user drops their memberships.
    PgUserRepository::new(pool).delete(alice.id).await.unwrap();
    assert!(repo.list_member_ids(team.id).await.unwrap().is_empty());

    assert_eq!(repo.list_by_domain(domain.id).await.unwrap().len(), 1);
    repo.delete(team.id).await.unwrap();
    assert!(matches!(
        repo.get(team.id).await,
        Err(Error::NotFound { .. })
    ));
}

// ─── sessions ───────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn session_lifecycle(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let alice = common::user(&pool, domain.id, "alice", UserRole::User).await;
    let repo = PgSessionRepository::new(pool);

    let new = |hash: &str, ttl: Duration| NewSession {
        user_id: alice.id,
        refresh_token_hash: hash.into(),
        user_agent: Some("tests".into()),
        ip_address: Some("127.0.0.1".into()),
        expires_at: Utc::now() + ttl,
    };

    let first = repo
        .create(&new("hash-1", Duration::days(14)))
        .await
        .unwrap();
    let found = repo.find_active_by_token_hash("hash-1").await.unwrap();
    assert_eq!(found.map(|s| s.id), Some(first.id));
    assert!(repo
        .find_active_by_token_hash("unknown")
        .await
        .unwrap()
        .is_none());

    let dup = repo.create(&new("hash-1", Duration::days(1))).await;
    assert!(matches!(dup, Err(Error::Conflict(_))), "got {dup:?}");

    let already_expired = repo.create(&new("hash-x", Duration::seconds(-5))).await;
    assert!(
        matches!(already_expired, Err(Error::Validation(_))),
        "got {already_expired:?}"
    );

    repo.touch(first.id).await.unwrap();

    repo.revoke(first.id).await.unwrap();
    repo.revoke(first.id).await.unwrap(); // idempotent
    assert!(repo
        .find_active_by_token_hash("hash-1")
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        repo.touch(first.id).await,
        Err(Error::NotFound { .. })
    ));

    repo.create(&new("hash-2", Duration::days(1)))
        .await
        .unwrap();
    repo.create(&new("hash-3", Duration::days(1)))
        .await
        .unwrap();
    assert_eq!(repo.revoke_all_for_user(alice.id).await.unwrap(), 2);
    assert_eq!(repo.revoke_all_for_user(alice.id).await.unwrap(), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_sessions_are_inactive_and_purged(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let alice = common::user(&pool, domain.id, "alice", UserRole::User).await;
    let repo = PgSessionRepository::new(pool);

    repo.create(&NewSession {
        user_id: alice.id,
        refresh_token_hash: "short-lived".into(),
        user_agent: None,
        ip_address: None,
        expires_at: Utc::now() + Duration::milliseconds(700),
    })
    .await
    .unwrap();
    repo.create(&NewSession {
        user_id: alice.id,
        refresh_token_hash: "long-lived".into(),
        user_agent: None,
        ip_address: None,
        expires_at: Utc::now() + Duration::days(1),
    })
    .await
    .unwrap();

    assert!(repo
        .find_active_by_token_hash("short-lived")
        .await
        .unwrap()
        .is_some());
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    assert!(repo
        .find_active_by_token_hash("short-lived")
        .await
        .unwrap()
        .is_none());
    assert!(repo
        .find_active_by_token_hash("long-lived")
        .await
        .unwrap()
        .is_some());

    assert_eq!(repo.delete_expired().await.unwrap(), 1);
    assert_eq!(repo.delete_expired().await.unwrap(), 0);
}

// ─── audit log ──────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn audit_log_append_list_and_filters(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let alice = common::user(&pool, domain.id, "alice", UserRole::User).await;
    let repo = PgAuditLogRepository::new(pool.clone());

    let empty = repo
        .append(&NewAuditEntry::new("  ", AuditOutcome::Success))
        .await;
    assert!(matches!(empty, Err(Error::Validation(_))), "got {empty:?}");

    repo.append(&NewAuditEntry {
        actor_user_id: Some(alice.id),
        actor_email: Some(alice.email.clone()),
        ip_address: Some("10.0.0.1".into()),
        details: serde_json::json!({ "reason": "ok" }),
        ..NewAuditEntry::new("auth.login", AuditOutcome::Success)
    })
    .await
    .unwrap();
    repo.append(&NewAuditEntry::new("auth.login", AuditOutcome::Failure))
        .await
        .unwrap();
    let last = repo
        .append(&NewAuditEntry {
            actor_user_id: Some(alice.id),
            target_type: Some("user".into()),
            target_id: Some(alice.id.to_string()),
            ..NewAuditEntry::new("user.update", AuditOutcome::Success)
        })
        .await
        .unwrap();

    let all = repo.list(&AuditQuery::default()).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].id, last.id, "newest first");
    assert!(all.windows(2).all(|w| w[0].id > w[1].id));

    let logins = repo
        .list(&AuditQuery {
            action: Some("auth.login".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(logins.len(), 2);

    let by_actor = repo
        .list(&AuditQuery {
            actor_user_id: Some(alice.id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(by_actor.len(), 2);
    let success = by_actor.iter().find(|e| e.action == "auth.login").unwrap();
    assert_eq!(success.details["reason"], "ok");
    assert_eq!(success.outcome, AuditOutcome::Success);

    let page = repo
        .list(&AuditQuery {
            limit: 2,
            offset: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].id, all[1].id);

    // Limits are clamped rather than rejected.
    assert_eq!(
        repo.list(&AuditQuery {
            limit: 0,
            ..Default::default()
        })
        .await
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        repo.list(&AuditQuery {
            offset: -5,
            ..Default::default()
        })
        .await
        .unwrap()
        .len(),
        3
    );

    // History survives user deletion; only the live link is cleared.
    PgUserRepository::new(pool).delete(alice.id).await.unwrap();
    let after = repo.list(&AuditQuery::default()).await.unwrap();
    assert_eq!(after.len(), 3);
    let kept = after.iter().find(|e| e.actor_email.is_some()).unwrap();
    assert_eq!(kept.actor_user_id, None);
    assert_eq!(kept.actor_email.as_deref(), Some("alice@example.com"));
}

#[test]
fn migrations_path_constant_is_in_sync() {
    // `#[sqlx::test(migrations = ...)]` needs a string literal, so the path is
    // repeated above. Guard against the directory moving.
    assert!(std::path::Path::new(MIGRATIONS).is_dir());
}
