//! Integration tests for the administration service. Requires `DATABASE_URL`.
//!
//! The recurring theme: what each kind of administrator can and cannot do.
mod common;

use std::sync::Arc;

use rustmail_core::admin::audit::AuditSearch;
use rustmail_core::admin::domains::DomainUpdate;
use rustmail_core::admin::users::{NewAccount, UserFilter, UserUpdate};
use rustmail_core::admin::{Admin, AdminService};
use rustmail_core::domain::{AccountStatus, AuditOutcome, User, UserRole};
use rustmail_core::password::verify_password;
use rustmail_core::provisioning::{MailboxProvisioner, ProvisionerConfig};
use rustmail_core::repository::{
    MailboxRepository, PgMailboxRepository, PgQuotaRepository, QuotaRepository,
};
use rustmail_core::Error;
use sqlx::PgPool;

struct World {
    service: AdminService,
    scratch: common::Scratch,
    root: Admin,
    root_user: User,
    dana: Admin, // domain admin of example.com
    other: rustmail_core::domain::MailDomain,
    example: rustmail_core::domain::MailDomain,
}

async fn world(pool: &PgPool) -> World {
    let scratch = common::Scratch::new();
    let provisioner = Arc::new(MailboxProvisioner::new(ProvisionerConfig::new(
        scratch.path(),
    )));
    let service = AdminService::new(pool.clone(), Some(provisioner));
    let example = common::domain(pool, "example.com").await;
    let other = common::domain(pool, "other.org").await;
    let root_user =
        common::user_with_password(pool, example.id, "root", UserRole::SuperAdmin).await;
    let dana_user =
        common::user_with_password(pool, example.id, "dana", UserRole::DomainAdmin).await;
    let root = Admin::from_user(&root_user, Some("203.0.113.9".into())).unwrap();
    let dana = Admin::from_user(&dana_user, None).unwrap();
    World {
        service,
        scratch,
        root,
        root_user,
        dana,
        other,
        example,
    }
}

fn account(email: &str) -> NewAccount {
    NewAccount {
        email: email.into(),
        display_name: "Test".into(),
        role: UserRole::User,
        quota_bytes: None,
        password: None,
    }
}

fn forbidden<T: std::fmt::Debug>(result: Result<T, Error>) -> bool {
    matches!(result, Err(Error::Forbidden(_)))
}

#[sqlx::test(migrations = "../../migrations")]
async fn ordinary_users_cannot_be_administrators(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let user = common::user(&pool, domain.id, "alice", UserRole::User).await;
    assert!(forbidden(Admin::from_user(&user, None)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_super_admin_creates_a_domain_and_three_users_with_one_time_passwords(pool: PgPool) {
    let w = world(&pool).await;
    let domain = w
        .service
        .create_domain(&w.root, "new.test", Some(5 * 1024 * 1024), Some(10))
        .await
        .unwrap();
    assert!(
        w.scratch.path().join("new.test").is_dir(),
        "the domain directory exists"
    );
    let mut passwords = Vec::new();
    for name in ["a", "b", "c"] {
        let created = w
            .service
            .create_user(&w.root, account(&format!("{name}@new.test")))
            .await
            .unwrap();
        let password = created.generated_password.clone().expect("generated");
        assert!(verify_password(&password, &created.user.password_hash).unwrap());
        assert!(w
            .scratch
            .path()
            .join(format!("new.test/{name}/Maildir/cur"))
            .is_dir());
        passwords.push(password);
    }
    passwords.sort();
    passwords.dedup();
    assert_eq!(passwords.len(), 3, "each user gets their own password");
    let row = w.service.get_domain(&w.root, domain.id).await.unwrap();
    assert_eq!(
        (row.users, row.default_quota_bytes, row.max_mailboxes),
        (3, Some(5 * 1024 * 1024), Some(10))
    );
    // The created users inherit the domain's quota.
    let page = w
        .service
        .list_users(
            &w.root,
            &UserFilter {
                domain_id: Some(domain.id),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(page
        .rows
        .iter()
        .all(|r| r.quota_bytes == Some(5 * 1024 * 1024)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn domain_admins_are_confined_to_their_domain(pool: PgPool) {
    let w = world(&pool).await;
    w.service
        .create_user(&w.root, account("alice@example.com"))
        .await
        .unwrap();
    let bob = w
        .service
        .create_user(&w.root, account("bob@other.org"))
        .await
        .unwrap()
        .user;

    // Lists show only their own domain, whatever the filter says.
    let mine = w
        .service
        .list_users(
            &w.dana,
            &UserFilter {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        mine.rows.iter().all(|r| r.domain == "example.com"),
        "{:?}",
        mine.rows.iter().map(|r| &r.email).collect::<Vec<_>>()
    );
    assert!(forbidden(
        w.service
            .list_users(
                &w.dana,
                &UserFilter {
                    domain_id: Some(w.other.id),
                    limit: 50,
                    ..Default::default()
                }
            )
            .await
    ));
    assert_eq!(w.service.list_domains(&w.dana).await.unwrap().len(), 1);

    // They cannot reach into the other domain.
    assert!(forbidden(w.service.get_user(&w.dana, bob.id).await));
    assert!(forbidden(w.service.get_domain(&w.dana, w.other.id).await));
    assert!(forbidden(
        w.service
            .create_user(&w.dana, account("eve@other.org"))
            .await
    ));
    assert!(forbidden(
        w.service
            .set_user_status(&w.dana, bob.id, AccountStatus::Disabled)
            .await
    ));
    assert!(forbidden(
        w.service.reset_password(&w.dana, bob.id, None).await
    ));
    assert!(forbidden(
        w.service
            .delete_user(&w.dana, bob.id, "bob@other.org")
            .await
    ));
    assert!(forbidden(
        w.service
            .create_alias(&w.dana, w.other.id, "x", "y@z.co")
            .await
    ));
    assert!(forbidden(
        w.service
            .create_group(&w.dana, w.other.id, "g", None, "")
            .await
    ));

    // Domain administration and administrator accounts are for super administrators.
    assert!(forbidden(
        w.service
            .create_domain(&w.dana, "mine.test", None, None)
            .await
    ));
    assert!(forbidden(
        w.service
            .update_domain(
                &w.dana,
                w.example.id,
                &DomainUpdate {
                    status: AccountStatus::Active,
                    default_quota_bytes: None,
                    max_mailboxes: None
                }
            )
            .await
    ));
    assert!(forbidden(
        w.service
            .delete_domain(&w.dana, w.example.id, "example.com")
            .await
    ));
    let mut admin_request = account("boss@example.com");
    admin_request.role = UserRole::DomainAdmin;
    assert!(forbidden(
        w.service.create_user(&w.dana, admin_request).await
    ));

    // They can manage ordinary users of their own domain...
    let alice = w
        .service
        .list_users(
            &w.dana,
            &UserFilter {
                search: Some("alice".into()),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .rows[0]
        .id;
    w.service
        .set_user_status(&w.dana, alice, AccountStatus::Suspended)
        .await
        .unwrap();
    // ...but not other administrators, nor change anyone's role.
    assert!(forbidden(
        w.service
            .set_user_status(&w.dana, w.root_user.id, AccountStatus::Disabled)
            .await
    ));
    assert!(forbidden(
        w.service
            .reset_password(&w.dana, w.root_user.id, None)
            .await
    ));
    let update = UserUpdate {
        display_name: "Al".into(),
        role: UserRole::DomainAdmin,
        quota_bytes: None,
    };
    assert!(forbidden(
        w.service.update_user(&w.dana, alice, &update).await
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn addresses_cannot_be_used_twice(pool: PgPool) {
    let w = world(&pool).await;
    w.service
        .create_user(&w.root, account("alice@example.com"))
        .await
        .unwrap();
    assert!(matches!(
        w.service
            .create_user(&w.root, account("alice@example.com"))
            .await,
        Err(Error::Conflict(_))
    ));
    w.service
        .create_alias(&w.root, w.example.id, "info", "alice@example.com")
        .await
        .unwrap();
    // A user may not take an alias's address, an alias may not take a mailbox's.
    assert!(matches!(
        w.service
            .create_user(&w.root, account("info@example.com"))
            .await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        w.service
            .create_alias(&w.root, w.example.id, "alice", "x@y.co")
            .await,
        Err(Error::Conflict(_))
    ));
    w.service
        .create_group(&w.root, w.example.id, "Team", Some("team"), "")
        .await
        .unwrap();
    assert!(matches!(
        w.service
            .create_user(&w.root, account("team@example.com"))
            .await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        w.service
            .create_alias(&w.root, w.example.id, "team", "x@y.co")
            .await,
        Err(Error::Conflict(_))
    ));
    // The failed attempts were audited.
    let page = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                action: Some("user.create".into()),
                outcome: Some(AuditOutcome::Failure),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(page.total >= 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn passwords_can_be_reset_and_lockouts_cleared(pool: PgPool) {
    let w = world(&pool).await;
    let alice = w
        .service
        .create_user(&w.root, account("alice@example.com"))
        .await
        .unwrap()
        .user;
    sqlx::query("UPDATE users SET failed_login_count = 9, locked_until = now() + interval '1 hour' WHERE id = $1").bind(alice.id).execute(&pool).await.unwrap();
    let detail = w.service.get_user(&w.root, alice.id).await.unwrap();
    assert!(detail.locked_until.is_some() && detail.failed_logins == 9);

    let fresh = w
        .service
        .reset_password(&w.root, alice.id, None)
        .await
        .unwrap();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
        .bind(alice.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(verify_password(&fresh, &hash).unwrap());
    let detail = w.service.get_user(&w.root, alice.id).await.unwrap();
    assert!(detail.locked_until.is_none() && detail.failed_logins == 0);
    assert!(matches!(
        w.service
            .reset_password(&w.root, alice.id, Some("short".into()))
            .await,
        Err(Error::Validation(_))
    ));
    w.service
        .reset_password(&w.root, alice.id, Some("a-chosen-password-123".into()))
        .await
        .unwrap();

    // The audit trail records the reset but never a password.
    let page = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                action: Some("user.password_reset".into()),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.total, 2);
    let dump = serde_json::to_string(&page.entries.iter().map(|e| &e.details).collect::<Vec<_>>())
        .unwrap();
    assert!(
        !dump.contains(&fresh) && !dump.contains("a-chosen-password-123"),
        "{dump}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn administrators_are_protected_from_locking_everyone_out(pool: PgPool) {
    let w = world(&pool).await;
    // You cannot disable, delete or demote yourself.
    assert!(forbidden(
        w.service
            .set_user_status(&w.root, w.root.user_id, AccountStatus::Disabled)
            .await
    ));
    assert!(forbidden(
        w.service
            .delete_user(&w.root, w.root.user_id, "root@example.com")
            .await
    ));
    let demote = UserUpdate {
        display_name: "Root".into(),
        role: UserRole::User,
        quota_bytes: None,
    };
    assert!(forbidden(
        w.service
            .update_user(&w.root, w.root.user_id, &demote)
            .await
    ));
    // The only other super admin cannot be removed once they are the last one.
    let second =
        common::user_with_password(&pool, w.example.id, "second", UserRole::SuperAdmin).await;
    let second_admin = Admin::from_user(&second, None).unwrap();
    // Now there are two: disabling one is fine...
    w.service
        .set_user_status(&w.root, second.id, AccountStatus::Disabled)
        .await
        .unwrap();
    // ...and with only root left active, nobody can disable root (they would have to be another active super admin).
    w.service
        .set_user_status(&w.root, second.id, AccountStatus::Active)
        .await
        .unwrap();
    w.service
        .set_user_status(&second_admin, w.root.user_id, AccountStatus::Disabled)
        .await
        .unwrap();
    assert!(matches!(
        w.service
            .set_user_status(&w.root, second.id, AccountStatus::Disabled)
            .await,
        Err(Error::Forbidden(_) | Error::Conflict(_))
    ));
    let last = w
        .service
        .set_user_status(&second_admin, second.id, AccountStatus::Disabled)
        .await;
    assert!(
        last.is_err(),
        "the last active super administrator is protected"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn user_details_edits_and_quota_overrides(pool: PgPool) {
    let w = world(&pool).await;
    let alice = w
        .service
        .create_user(&w.root, account("alice@example.com"))
        .await
        .unwrap()
        .user;
    w.service
        .update_user(
            &w.root,
            alice.id,
            &UserUpdate {
                display_name: "  Alice A  ".into(),
                role: UserRole::User,
                quota_bytes: Some(2 * 1024 * 1024),
            },
        )
        .await
        .unwrap();
    let detail = w.service.get_user(&w.root, alice.id).await.unwrap();
    assert_eq!(detail.row.display_name, "Alice A");
    assert_eq!(
        (detail.quota_override, detail.row.quota_bytes),
        (Some(2 * 1024 * 1024), Some(2 * 1024 * 1024))
    );
    w.service
        .update_user(
            &w.root,
            alice.id,
            &UserUpdate {
                display_name: "Alice A".into(),
                role: UserRole::User,
                quota_bytes: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        w.service
            .get_user(&w.root, alice.id)
            .await
            .unwrap()
            .quota_override,
        None,
        "back to inheriting"
    );
    assert!(w
        .service
        .update_user(
            &w.root,
            alice.id,
            &UserUpdate {
                display_name: "x".into(),
                role: UserRole::User,
                quota_bytes: Some(-1)
            }
        )
        .await
        .is_err());
    assert!(w
        .service
        .update_user(
            &w.root,
            alice.id,
            &UserUpdate {
                display_name: "bad\nname".into(),
                role: UserRole::User,
                quota_bytes: None
            }
        )
        .await
        .is_err());
    // Search and filters.
    w.service
        .create_user(&w.root, account("bob@other.org"))
        .await
        .unwrap();
    let all = w
        .service
        .list_users(
            &w.root,
            &UserFilter {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(all.total, 4);
    let found = w
        .service
        .list_users(
            &w.root,
            &UserFilter {
                search: Some("%".into()),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(found.total, 0, "wildcards are literal");
    let admins = w
        .service
        .list_users(
            &w.root,
            &UserFilter {
                role: Some(UserRole::DomainAdmin),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(admins.total, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleting_a_user_for_good_removes_their_mail_directory_and_aliases(pool: PgPool) {
    let w = world(&pool).await;
    let alice = w
        .service
        .create_user(&w.root, account("alice@example.com"))
        .await
        .unwrap()
        .user;
    w.service
        .create_alias(&w.root, w.example.id, "info", "alice@example.com")
        .await
        .unwrap();
    let home = w.scratch.path().join("example.com/alice");
    std::fs::write(home.join("Maildir/cur/1:2,S"), "mail").unwrap();
    assert!(matches!(
        w.service
            .delete_user(&w.root, alice.id, "wrong@example.com")
            .await,
        Err(Error::Validation(_))
    ));
    assert!(home.exists(), "a wrong confirmation changes nothing");
    w.service
        .delete_user(&w.root, alice.id, "Alice@Example.com")
        .await
        .unwrap();
    assert!(!home.exists(), "the mail directory is gone");
    assert!(
        w.scratch.path().join("example.com").is_dir(),
        "the domain directory is not"
    );
    let aliases = w
        .service
        .list_aliases(&w.root, None, None, 50, 0)
        .await
        .unwrap();
    assert_eq!(aliases.total, 0, "aliases pointing at the user are removed");
    assert!(matches!(
        w.service.get_user(&w.root, alice.id).await,
        Err(Error::NotFound { .. })
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn domains_are_edited_and_deleted_carefully(pool: PgPool) {
    let w = world(&pool).await;
    let domain = w
        .service
        .create_domain(&w.root, "temp.test", None, Some(1))
        .await
        .unwrap();
    w.service
        .create_user(&w.root, account("a@temp.test"))
        .await
        .unwrap();
    assert!(
        matches!(
            w.service.create_user(&w.root, account("b@temp.test")).await,
            Err(Error::Conflict(_))
        ),
        "the mailbox limit holds"
    );
    w.service
        .update_domain(
            &w.root,
            domain.id,
            &DomainUpdate {
                status: AccountStatus::Suspended,
                default_quota_bytes: Some(1024),
                max_mailboxes: Some(5),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        w.service
            .get_domain(&w.root, domain.id)
            .await
            .unwrap()
            .status,
        AccountStatus::Suspended
    );
    assert!(
        matches!(
            w.service.create_user(&w.root, account("c@temp.test")).await,
            Err(Error::Validation(_))
        ),
        "no new users in a suspended domain"
    );
    assert!(w
        .service
        .update_domain(
            &w.root,
            domain.id,
            &DomainUpdate {
                status: AccountStatus::Active,
                default_quota_bytes: Some(-1),
                max_mailboxes: None
            }
        )
        .await
        .is_err());
    // Not while users remain; not without the typed name.
    assert!(matches!(
        w.service
            .delete_domain(&w.root, domain.id, "temp.test")
            .await,
        Err(Error::Conflict(_))
    ));
    let user = w
        .service
        .list_users(
            &w.root,
            &UserFilter {
                domain_id: Some(domain.id),
                limit: 5,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .rows[0]
        .id;
    w.service
        .delete_user(&w.root, user, "a@temp.test")
        .await
        .unwrap();
    assert!(matches!(
        w.service.delete_domain(&w.root, domain.id, "wrong").await,
        Err(Error::Validation(_))
    ));
    w.service
        .delete_domain(&w.root, domain.id, "TEMP.test")
        .await
        .unwrap();
    assert!(matches!(
        w.service.get_domain(&w.root, domain.id).await,
        Err(Error::NotFound { .. })
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn aliases_and_groups(pool: PgPool) {
    let w = world(&pool).await;
    let alice = w
        .service
        .create_user(&w.root, account("alice@example.com"))
        .await
        .unwrap()
        .user;
    let bob = w
        .service
        .create_user(&w.root, account("bob@example.com"))
        .await
        .unwrap()
        .user;
    let carol = w
        .service
        .create_user(&w.root, account("carol@other.org"))
        .await
        .unwrap()
        .user;

    let alias = w
        .service
        .create_alias(&w.dana, w.example.id, "Sales", "alice@example.com")
        .await
        .unwrap();
    assert_eq!(alias.address, "sales@example.com");
    w.service
        .create_alias(
            &w.dana,
            w.example.id,
            "sales@example.com",
            "bob@example.com",
        )
        .await
        .unwrap();
    assert!(
        w.service
            .create_alias(&w.dana, w.example.id, "sales", "bob@example.com")
            .await
            .is_err(),
        "duplicate pair"
    );
    assert!(
        w.service
            .create_alias(&w.dana, w.example.id, "x@other.org", "bob@example.com")
            .await
            .is_err(),
        "wrong domain"
    );
    assert!(
        w.service
            .create_alias(&w.dana, w.example.id, "loop", "loop@example.com")
            .await
            .is_err(),
        "self alias"
    );
    assert!(w
        .service
        .create_alias(&w.dana, w.example.id, "bad address", "bob@example.com")
        .await
        .is_err());
    w.service
        .set_alias_enabled(&w.dana, alias.id, false)
        .await
        .unwrap();
    let list = w
        .service
        .list_aliases(&w.dana, None, Some("sales"), 50, 0)
        .await
        .unwrap();
    assert_eq!(
        (list.total, list.rows.iter().filter(|a| a.enabled).count()),
        (2, 1)
    );
    let detail = w.service.get_user(&w.dana, bob.id).await.unwrap();
    assert_eq!(detail.aliases, ["sales@example.com"]);
    w.service.delete_alias(&w.dana, alias.id).await.unwrap();

    let group = w
        .service
        .create_group(&w.dana, w.example.id, "Everyone", Some("all"), "all staff")
        .await
        .unwrap();
    w.service
        .add_group_member(&w.dana, group, "alice@example.com")
        .await
        .unwrap();
    w.service
        .add_group_member(&w.dana, group, "alice@example.com")
        .await
        .unwrap(); // idempotent
    w.service
        .add_group_member(&w.dana, group, "bob@example.com")
        .await
        .unwrap();
    assert!(
        matches!(
            w.service
                .add_group_member(&w.dana, group, "carol@other.org")
                .await,
            Err(Error::Validation(_))
        ),
        "members must be in the group's domain"
    );
    let detail = w.service.get_group(&w.dana, group).await.unwrap();
    assert_eq!(detail.members.len(), 2);
    assert_eq!(
        w.service.get_user(&w.dana, alice.id).await.unwrap().groups[0].1,
        "Everyone"
    );
    w.service
        .remove_group_member(&w.dana, group, alice.id)
        .await
        .unwrap();
    assert_eq!(
        w.service
            .get_group(&w.dana, group)
            .await
            .unwrap()
            .members
            .len(),
        1
    );
    assert!(forbidden(
        w.service
            .get_group(
                &w.dana,
                w.service
                    .create_group(&w.root, w.other.id, "Theirs", None, "")
                    .await
                    .unwrap()
            )
            .await
    ));
    assert!(forbidden(w.service.get_user(&w.dana, carol.id).await));
    w.service.delete_group(&w.dana, group).await.unwrap();
    assert!(w
        .service
        .list_groups(&w.dana, None)
        .await
        .unwrap()
        .is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_quota_dashboard_adds_up_and_respects_scope(pool: PgPool) {
    let w = world(&pool).await;
    let mut ids = Vec::new();
    for (email, quota, used) in [
        ("a@example.com", Some(1000), 900),
        ("b@example.com", Some(1000), 100),
        ("c@example.com", None, 5000),
        ("d@other.org", Some(100), 50),
    ] {
        let mut request = account(email);
        request.quota_bytes = quota;
        let user = w.service.create_user(&w.root, request).await.unwrap().user;
        let mailbox = PgMailboxRepository::new(pool.clone())
            .find_by_user(user.id)
            .await
            .unwrap()
            .unwrap();
        PgQuotaRepository::new(pool.clone())
            .set_usage(mailbox.id, used, 3)
            .await
            .unwrap();
        ids.push(user.id);
    }
    let all = w.service.quota_dashboard(&w.root).await.unwrap();
    assert_eq!(
        (all.total_mailboxes, all.total_used_bytes),
        (4, 900 + 100 + 5000 + 50),
        "only users with a mailbox count"
    );
    assert_eq!(all.top[0].email, "c@example.com");
    assert_eq!(all.near_limit.len(), 1);
    assert_eq!(all.near_limit[0].email, "a@example.com");
    assert_eq!(all.near_limit[0].percent(), Some(90.0));
    let example = all
        .domains
        .iter()
        .find(|d| d.domain == "example.com")
        .unwrap();
    assert_eq!(
        (example.used_bytes, example.unlimited_mailboxes),
        (6000, 1),
        "only c has no quota"
    );

    let mine = w.service.quota_dashboard(&w.dana).await.unwrap();
    assert!(mine.domains.iter().all(|d| d.domain == "example.com"));
    assert!(mine.top.iter().all(|m| m.email.ends_with("@example.com")));
    assert_eq!(mine.total_used_bytes, 6000);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_audit_log_records_admin_actions_filters_and_scopes_them(pool: PgPool) {
    let w = world(&pool).await;
    let other = w
        .service
        .create_user(&w.root, account("bob@other.org"))
        .await
        .unwrap()
        .user;
    w.service
        .create_user(&w.dana, account("alice@example.com"))
        .await
        .unwrap();
    w.service
        .set_user_status(&w.root, other.id, AccountStatus::Disabled)
        .await
        .unwrap();

    let all = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        all.entries
            .iter()
            .any(|e| e.action == "user.status"
                && e.actor_email.as_deref() == Some("root@example.com"))
    );
    assert!(all
        .entries
        .iter()
        .any(|e| e.ip_address.as_deref() == Some("203.0.113.9")));

    // A domain administrator sees their own domain's entries only.
    let mine = w
        .service
        .search_audit(
            &w.dana,
            &AuditSearch {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!mine.entries.is_empty());
    let dump = serde_json::to_string(
        &mine
            .entries
            .iter()
            .map(|e| (&e.details, &e.target_id))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(
        !dump.contains("bob@other.org"),
        "other domains' entries are hidden: {dump}"
    );
    assert!(mine.entries.iter().any(|e| e.action == "user.create"));

    // Filters.
    let by_action = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                action: Some("user.".into()),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(by_action
        .entries
        .iter()
        .all(|e| e.action.starts_with("user.")));
    let by_actor = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                actor: Some("DANA".into()),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        by_actor
            .entries
            .iter()
            .all(|e| e.actor_email.as_deref() == Some("dana@example.com"))
            && by_actor.total >= 1
    );
    let by_text = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                text: Some("bob@other.org".into()),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(by_text.total >= 2);
    let future = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                from: Some(chrono::Utc::now() + chrono::Duration::days(1)),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(future.total, 0);
    let wildcard = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                action: Some("%".into()),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(wildcard.total, 0, "wildcards are literal");
    let actions = w.service.audit_actions(&w.dana).await.unwrap();
    assert!(actions.contains(&"user.create".to_owned()));

    // The CSV export is audited, bounded and spreadsheet-safe.
    let csv = w
        .service
        .export_audit_csv(&w.root, &AuditSearch::default())
        .await
        .unwrap();
    assert!(csv.starts_with("time,actor,action,outcome"));
    assert!(csv.lines().count() > 3);
    let exported = w
        .service
        .search_audit(
            &w.root,
            &AuditSearch {
                action: Some("audit.export".into()),
                limit: 5,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(exported.total, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn csv_import_previews_then_creates_what_is_valid(pool: PgPool) {
    let w = world(&pool).await;
    w.service
        .create_domain(&w.root, "small.test", None, Some(2))
        .await
        .unwrap();
    w.service
        .create_user(&w.root, account("existing@example.com"))
        .await
        .unwrap();
    w.service
        .create_alias(&w.root, w.example.id, "info", "existing@example.com")
        .await
        .unwrap();
    let file = "email,name,role,quota_mb,password\n\
        ann@example.com,Ann,user,50,\n\
        ben@example.com,Ben,user,,chosen-password-123\n\
        ann@example.com,Ann again,user,,\n\
        existing@example.com,Dupe,user,,\n\
        info@example.com,Alias clash,user,,\n\
        nobody@nowhere.invalid,Lost,user,,\n\
        not-an-address,Broken,user,,\n\
        s1@small.test,S1,user,,\n\
        s2@small.test,S2,user,,\n\
        s3@small.test,S3,user,,\n\
        cat@example.com,Cat,wizard,,\n";
    let preview = w.service.preview_import(&w.root, file).await.unwrap();
    let valid: Vec<_> = preview.valid.iter().map(|r| r.email.as_str()).collect();
    assert_eq!(
        valid,
        [
            "ann@example.com",
            "ben@example.com",
            "s1@small.test",
            "s2@small.test"
        ]
    );
    assert_eq!(preview.problems.len(), 7, "{:?}", preview.problems);
    let reason = |email: &str| {
        preview
            .problems
            .iter()
            .find(|p| p.email == email)
            .map(|p| p.message.clone())
            .unwrap_or_default()
    };
    assert!(reason("existing@example.com").contains("already exists"));
    assert!(reason("info@example.com").contains("already exists"));
    assert!(reason("nobody@nowhere.invalid").contains("does not exist"));
    assert!(reason("s3@small.test").contains("limit"));
    assert!(reason("cat@example.com").contains("unknown role"));
    assert_eq!(
        preview
            .problems
            .iter()
            .filter(|p| p.message.contains("twice"))
            .count(),
        1
    );
    // Previewing changed nothing.
    assert_eq!(
        w.service
            .list_users(
                &w.root,
                &UserFilter {
                    limit: 50,
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .total,
        3
    );

    let results = w.service.apply_import(&w.root, file).await.unwrap();
    assert_eq!(results.iter().filter(|r| r.created).count(), 4);
    let ann = results
        .iter()
        .find(|r| r.email == "ann@example.com")
        .unwrap();
    let ben = results
        .iter()
        .find(|r| r.email == "ben@example.com")
        .unwrap();
    assert!(
        ann.generated_password.is_some(),
        "no password in the file: one is generated"
    );
    assert!(
        ben.generated_password.is_none(),
        "a chosen password is not echoed back"
    );
    assert!(w
        .scratch
        .path()
        .join("example.com/ann/Maildir/new")
        .is_dir());
    let csv = rustmail_core::admin::import::results_csv(&results);
    assert!(
        csv.contains(ann.generated_password.as_deref().unwrap())
            && !csv.contains("chosen-password-123")
    );
    // The quota column was applied.
    let found = w
        .service
        .list_users(
            &w.root,
            &UserFilter {
                search: Some("ann@".into()),
                limit: 5,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(found.rows[0].quota_bytes, Some(50 * 1024 * 1024));
    // Running it again creates nothing: everything already exists.
    let again = w.service.apply_import(&w.root, file).await.unwrap();
    assert_eq!(again.iter().filter(|r| r.created).count(), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn csv_import_respects_a_domain_admins_limits(pool: PgPool) {
    let w = world(&pool).await;
    let file =
        "email,role\nok@example.com,user\nboss@example.com,domain_admin\nfar@other.org,user\n";
    let preview = w.service.preview_import(&w.dana, file).await.unwrap();
    assert_eq!(preview.valid.len(), 1);
    assert!(preview
        .problems
        .iter()
        .any(|p| p.email == "boss@example.com" && p.message.contains("super administrator")));
    assert!(preview
        .problems
        .iter()
        .any(|p| p.email == "far@other.org" && p.message.contains("may not add")));
    // Whole-file problems are errors, not row problems.
    for bad in [
        "",
        "name\nx",
        "email,shoe\na@b.co,1",
        &"a@b.co\n".repeat(2000),
    ] {
        assert!(
            w.service.preview_import(&w.dana, bad).await.is_err(),
            "{:?}",
            bad.chars().take(20).collect::<String>()
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn without_a_mail_directory_only_database_work_is_possible(pool: PgPool) {
    let domain = common::domain(&pool, "example.com").await;
    let root = common::user_with_password(&pool, domain.id, "root", UserRole::SuperAdmin).await;
    let service = AdminService::new(pool.clone(), None);
    let admin = Admin::from_user(&root, None).unwrap();
    assert_eq!(service.list_domains(&admin).await.unwrap().len(), 1);
    let result = service.create_user(&admin, account("a@example.com")).await;
    assert!(matches!(result, Err(Error::Provisioning(_))), "{result:?}");
}
