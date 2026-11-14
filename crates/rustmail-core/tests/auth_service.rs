//! Integration tests for `AuthService` against real PostgreSQL. Each test gets
//! its own freshly migrated database. Requires `DATABASE_URL` (see `make dev`).

mod common;

use std::time::Duration as StdDuration;

use chrono::Duration;
use rustmail_core::auth::tokens::hash_refresh_token;
use rustmail_core::auth::{AuthConfig, AuthError, AuthService, ClientInfo, LoginRequest};
use rustmail_core::domain::{AccountStatus, AuditOutcome, AuditQuery, User, UserRole};
use rustmail_core::repository::{
    AuditLogRepository, DomainRepository, PgAuditLogRepository, PgDomainRepository,
    PgUserRepository, UserRepository,
};
use sqlx::PgPool;
use uuid::Uuid;

fn config() -> AuthConfig {
    AuthConfig::new("integration-test-secret-32-bytes!!").expect("config")
}

fn client() -> ClientInfo {
    ClientInfo {
        ip_address: Some("203.0.113.9".into()),
        user_agent: Some("tests/1.0".into()),
    }
}

fn login_request(email: &str, password: &str) -> LoginRequest {
    LoginRequest {
        email: email.into(),
        password: password.into(),
        client: client(),
    }
}

/// Creates `example.com` and a regular user `alice`, plus a service for them.
async fn setup(pool: &PgPool, config: AuthConfig) -> (AuthService, User, Uuid) {
    let domain = common::domain(pool, "example.com").await;
    let alice = common::user_with_password(pool, domain.id, "alice", UserRole::User).await;
    (
        AuthService::postgres(pool.clone(), config),
        alice,
        domain.id,
    )
}

/// All audit entries, oldest first, as `(action, outcome, details)`.
async fn audit(pool: &PgPool) -> Vec<(String, AuditOutcome, serde_json::Value)> {
    let mut entries = PgAuditLogRepository::new(pool.clone())
        .list(&AuditQuery {
            limit: 500,
            ..Default::default()
        })
        .await
        .expect("list audit");
    entries.reverse();
    entries
        .into_iter()
        .map(|e| (e.action, e.outcome, e.details))
        .collect()
}

fn reasons(entries: &[(String, AuditOutcome, serde_json::Value)], action: &str) -> Vec<String> {
    entries
        .iter()
        .filter(|(a, ..)| a == action)
        .map(|(_, _, d)| d["reason"].as_str().unwrap_or("").to_owned())
        .collect()
}

// ─── login ──────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn login_issues_working_tokens_and_stores_only_a_hash(pool: PgPool) {
    let (auth, alice, _) = setup(&pool, config()).await;

    let outcome = auth
        .login(login_request(" ALICE@Example.com ", common::PASSWORD))
        .await
        .unwrap();
    assert_eq!(outcome.user.id, alice.id);
    assert!(outcome.tokens.access_expires_at < outcome.tokens.refresh_expires_at);

    let ctx = auth
        .authenticate(&outcome.tokens.access_token)
        .await
        .unwrap();
    assert_eq!((ctx.user_id, ctx.role), (alice.id, UserRole::User));
    assert_eq!(ctx.session_id, outcome.tokens.session_id);

    // Only the hash of the refresh token is stored.
    let stored: String =
        sqlx::query_scalar("SELECT refresh_token_hash FROM sessions WHERE id = $1")
            .bind(outcome.tokens.session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, hash_refresh_token(&outcome.tokens.refresh_token));
    assert_ne!(stored, outcome.tokens.refresh_token);

    let user = PgUserRepository::new(pool.clone())
        .get(alice.id)
        .await
        .unwrap();
    assert!(user.last_login_at.is_some());

    let entries = audit(&pool).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (entries[0].0.as_str(), entries[0].1),
        ("auth.login", AuditOutcome::Success)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn wrong_password_and_unknown_user_are_indistinguishable(pool: PgPool) {
    let (auth, ..) = setup(&pool, config()).await;

    let wrong = auth
        .login(login_request("alice@example.com", "not-the-password"))
        .await;
    let unknown = auth
        .login(login_request("nobody@example.com", "not-the-password"))
        .await;
    let malformed = auth
        .login(login_request("not an address", "not-the-password"))
        .await;
    for result in [&wrong, &unknown, &malformed] {
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "got {result:?}"
        );
    }
    assert_eq!(
        wrong.unwrap_err().to_string(),
        unknown.unwrap_err().to_string(),
        "the caller-visible error must be identical"
    );

    // The audit trail still tells operators what happened.
    let entries = audit(&pool).await;
    assert_eq!(
        reasons(&entries, "auth.login"),
        ["bad_password", "unknown_user", "unknown_user"]
    );
    assert!(entries
        .iter()
        .all(|(_, outcome, _)| *outcome == AuditOutcome::Failure));
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_log_never_contains_secrets(pool: PgPool) {
    let (auth, ..) = setup(&pool, config()).await;
    let secret_attempt = "hunter2-this-is-a-secret-attempt";

    let _ = auth
        .login(login_request("alice@example.com", secret_attempt))
        .await;
    let _ = auth
        .login(login_request("ghost@example.com", secret_attempt))
        .await;
    let ok = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    let refreshed = auth
        .refresh(&ok.tokens.refresh_token, client())
        .await
        .unwrap();
    auth.logout(refreshed.tokens.session_id, client())
        .await
        .unwrap();

    let dump = serde_json::to_string(&audit(&pool).await).unwrap();
    for secret in [
        secret_attempt,
        common::PASSWORD,
        ok.tokens.refresh_token.as_str(),
        ok.tokens.access_token.as_str(),
        refreshed.tokens.refresh_token.as_str(),
        refreshed.tokens.access_token.as_str(),
    ] {
        assert!(
            !dump.contains(secret),
            "audit log leaked a secret: {secret}"
        );
    }
}

// ─── lockout ────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn account_locks_after_repeated_failures_and_recovers(pool: PgPool) {
    let config = config().with_lockout(3, Duration::seconds(2));
    let (auth, alice, _) = setup(&pool, config).await;
    let users = PgUserRepository::new(pool.clone());

    for _ in 0..3 {
        let result = auth
            .login(login_request("alice@example.com", "wrong-password-1"))
            .await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "got {result:?}"
        );
    }
    let locked_until = users
        .get(alice.id)
        .await
        .unwrap()
        .locked_until
        .expect("locked");

    // The right password now reports the lock...
    let result = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await;
    assert!(
        matches!(result, Err(AuthError::AccountLocked { .. })),
        "got {result:?}"
    );

    // ...but a wrong password does not reveal it, and does not extend it.
    let result = auth
        .login(login_request("alice@example.com", "wrong-password-2"))
        .await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "got {result:?}"
    );
    assert_eq!(
        users.get(alice.id).await.unwrap().locked_until,
        Some(locked_until)
    );

    tokio::time::sleep(StdDuration::from_millis(2300)).await;
    auth.login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    let alice = users.get(alice.id).await.unwrap();
    assert_eq!((alice.failed_login_count, alice.locked_until), (0, None));

    let entries = audit(&pool).await;
    assert_eq!(
        reasons(&entries, "auth.login"),
        [
            "bad_password",
            "bad_password",
            "bad_password",
            "locked",
            "locked",
            "ok"
        ]
    );
    assert_eq!(
        entries[2].2["locked_now"], true,
        "the third failure applied the lock"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_successful_login_resets_the_failure_counter(pool: PgPool) {
    let (auth, alice, _) = setup(&pool, config().with_lockout(3, Duration::seconds(60))).await;
    let users = PgUserRepository::new(pool);

    for _ in 0..2 {
        let _ = auth
            .login(login_request("alice@example.com", "wrong-password-1"))
            .await;
    }
    assert_eq!(users.get(alice.id).await.unwrap().failed_login_count, 2);
    auth.login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    assert_eq!(users.get(alice.id).await.unwrap().failed_login_count, 0);
}

// ─── disabled accounts ──────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn disabled_users_and_suspended_domains_cannot_log_in(pool: PgPool) {
    let (auth, alice, domain_id) = setup(&pool, config()).await;
    let users = PgUserRepository::new(pool.clone());
    let domains = PgDomainRepository::new(pool.clone());

    users
        .set_status(alice.id, AccountStatus::Disabled)
        .await
        .unwrap();
    let result = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await;
    assert!(
        matches!(result, Err(AuthError::AccountDisabled)),
        "got {result:?}"
    );
    // Without the password, nothing about the account's state is revealed.
    let result = auth
        .login(login_request("alice@example.com", "wrong-password-1"))
        .await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "got {result:?}"
    );

    users
        .set_status(alice.id, AccountStatus::Active)
        .await
        .unwrap();
    domains
        .set_status(domain_id, AccountStatus::Suspended)
        .await
        .unwrap();
    let result = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await;
    assert!(
        matches!(result, Err(AuthError::AccountDisabled)),
        "got {result:?}"
    );

    domains
        .set_status(domain_id, AccountStatus::Active)
        .await
        .unwrap();
    auth.login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
}

// ─── refresh ────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn refresh_rotates_the_session_and_retires_the_old_tokens(pool: PgPool) {
    let (auth, alice, _) = setup(&pool, config()).await;
    let first = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();

    let second = auth
        .refresh(&first.tokens.refresh_token, client())
        .await
        .unwrap();
    assert_eq!(second.user.id, alice.id);
    assert_ne!(second.tokens.refresh_token, first.tokens.refresh_token);
    assert_ne!(second.tokens.session_id, first.tokens.session_id);

    // The old access token died with the old session; the new one works.
    assert!(matches!(
        auth.authenticate(&first.tokens.access_token).await,
        Err(AuthError::InvalidToken)
    ));
    auth.authenticate(&second.tokens.access_token)
        .await
        .unwrap();

    let replaced_by: Option<Uuid> =
        sqlx::query_scalar("SELECT replaced_by FROM sessions WHERE id = $1")
            .bind(first.tokens.session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(replaced_by, Some(second.tokens.session_id));

    // And the chain can continue.
    auth.refresh(&second.tokens.refresh_token, client())
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn refresh_picks_up_a_changed_role(pool: PgPool) {
    let (auth, alice, _) = setup(&pool, config()).await;
    let first = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    assert_eq!(
        auth.authenticate(&first.tokens.access_token)
            .await
            .unwrap()
            .role,
        UserRole::User
    );

    PgUserRepository::new(pool)
        .set_role(alice.id, UserRole::DomainAdmin)
        .await
        .unwrap();
    let second = auth
        .refresh(&first.tokens.refresh_token, client())
        .await
        .unwrap();
    assert_eq!(
        auth.authenticate(&second.tokens.access_token)
            .await
            .unwrap()
            .role,
        UserRole::DomainAdmin
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn replaying_a_rotated_token_after_the_grace_window_revokes_every_session(pool: PgPool) {
    // Zero grace: any replay of a rotated token counts as theft.
    let (auth, ..) = setup(&pool, config().with_reuse_grace(Duration::zero())).await;
    let laptop = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    let phone = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();

    let rotated = auth
        .refresh(&laptop.tokens.refresh_token, client())
        .await
        .unwrap();
    auth.authenticate(&rotated.tokens.access_token)
        .await
        .unwrap();

    // The attacker replays the token the laptop already rotated away from.
    let replay = auth.refresh(&laptop.tokens.refresh_token, client()).await;
    assert!(
        matches!(replay, Err(AuthError::InvalidToken)),
        "got {replay:?}"
    );

    // Everything is dead: the laptop's new session and the unrelated phone.
    for token in [&rotated.tokens.access_token, &phone.tokens.access_token] {
        assert!(matches!(
            auth.authenticate(token).await,
            Err(AuthError::InvalidToken)
        ));
    }
    assert!(matches!(
        auth.refresh(&rotated.tokens.refresh_token, client()).await,
        Err(AuthError::InvalidToken)
    ));

    let entries = audit(&pool).await;
    let reuse = entries
        .iter()
        .find(|(a, ..)| a == "auth.refresh_reuse")
        .expect("reuse audited");
    assert_eq!(reuse.1, AuditOutcome::Failure);
    assert_eq!(reuse.2["sessions_revoked"], 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn replay_inside_the_grace_window_is_rejected_without_revoking_anything(pool: PgPool) {
    let (auth, ..) = setup(&pool, config().with_reuse_grace(Duration::seconds(30))).await;
    let first = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    let second = auth
        .refresh(&first.tokens.refresh_token, client())
        .await
        .unwrap();

    let replay = auth.refresh(&first.tokens.refresh_token, client()).await;
    assert!(
        matches!(replay, Err(AuthError::InvalidToken)),
        "got {replay:?}"
    );
    auth.authenticate(&second.tokens.access_token)
        .await
        .expect("winner's session survives");

    let entries = audit(&pool).await;
    assert!(!entries.iter().any(|(a, ..)| a == "auth.refresh_reuse"));
    assert!(reasons(&entries, "auth.refresh").contains(&"reuse_within_grace".to_owned()));
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_refreshes_yield_exactly_one_winner(pool: PgPool) {
    let (auth, ..) = setup(&pool, config()).await;
    let first = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();

    let (a, b) = tokio::join!(
        auth.refresh(&first.tokens.refresh_token, client()),
        auth.refresh(&first.tokens.refresh_token, client()),
    );
    let (winner, loser) = match (a, b) {
        (Ok(w), Err(l)) | (Err(l), Ok(w)) => (w, l),
        other => panic!("expected exactly one success, got {other:?}"),
    };
    assert!(matches!(loser, AuthError::InvalidToken));
    auth.authenticate(&winner.tokens.access_token)
        .await
        .expect("winner stays logged in");

    let live: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE revoked_at IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(live, 1, "a race must never create two live sessions");
}

#[sqlx::test(migrations = "../../migrations")]
async fn refresh_rejects_unknown_expired_and_disabled(pool: PgPool) {
    let config = config().with_refresh_ttl(Duration::seconds(1));
    let (auth, alice, _) = setup(&pool, config).await;

    let result = auth.refresh("this-token-was-never-issued", client()).await;
    assert!(
        matches!(result, Err(AuthError::InvalidToken)),
        "got {result:?}"
    );
    let result = auth.refresh("", client()).await;
    assert!(
        matches!(result, Err(AuthError::InvalidToken)),
        "got {result:?}"
    );

    let short = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    tokio::time::sleep(StdDuration::from_millis(1200)).await;
    let result = auth.refresh(&short.tokens.refresh_token, client()).await;
    assert!(
        matches!(result, Err(AuthError::InvalidToken)),
        "expired: got {result:?}"
    );

    // A disabled account cannot refresh, and its session is revoked.
    let auth = AuthService::postgres(pool.clone(), self::config());
    let live = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    PgUserRepository::new(pool)
        .set_status(alice.id, AccountStatus::Disabled)
        .await
        .unwrap();
    let result = auth.refresh(&live.tokens.refresh_token, client()).await;
    assert!(
        matches!(result, Err(AuthError::AccountDisabled)),
        "got {result:?}"
    );
    assert!(matches!(
        auth.authenticate(&live.tokens.access_token).await,
        Err(AuthError::InvalidToken)
    ));
}

// ─── authenticate ───────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn authenticate_rejects_bad_expired_and_orphaned_tokens(pool: PgPool) {
    let config = config().with_access_ttl(Duration::seconds(1));
    let (auth, alice, _) = setup(&pool, config).await;

    for bad in ["", "garbage", "a.b.c"] {
        assert!(
            matches!(auth.authenticate(bad).await, Err(AuthError::InvalidToken)),
            "{bad:?}"
        );
    }

    let outcome = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    auth.authenticate(&outcome.tokens.access_token)
        .await
        .unwrap();
    tokio::time::sleep(StdDuration::from_millis(1300)).await;
    assert!(matches!(
        auth.authenticate(&outcome.tokens.access_token).await,
        Err(AuthError::InvalidToken)
    ));

    // A token whose user (and therefore session) was deleted stops working.
    let auth = AuthService::postgres(pool.clone(), self::config());
    let outcome = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    PgUserRepository::new(pool).delete(alice.id).await.unwrap();
    assert!(matches!(
        auth.authenticate(&outcome.tokens.access_token).await,
        Err(AuthError::InvalidToken)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn role_checks_follow_the_privilege_order(pool: PgPool) {
    let (auth, _, domain_id) = setup(&pool, config()).await;
    common::user_with_password(&pool, domain_id, "dana", UserRole::DomainAdmin).await;
    common::user_with_password(&pool, domain_id, "root", UserRole::SuperAdmin).await;

    let ctx = |email: &'static str| {
        let auth = auth.clone();
        async move {
            let out = auth
                .login(login_request(email, common::PASSWORD))
                .await
                .unwrap();
            auth.authenticate(&out.tokens.access_token).await.unwrap()
        }
    };
    let (user, domain_admin, super_admin) = (
        ctx("alice@example.com").await,
        ctx("dana@example.com").await,
        ctx("root@example.com").await,
    );

    assert!(user.require_role(UserRole::User).is_ok());
    assert!(matches!(
        user.require_role(UserRole::DomainAdmin),
        Err(AuthError::Forbidden)
    ));
    assert!(domain_admin.require_role(UserRole::DomainAdmin).is_ok());
    assert!(matches!(
        domain_admin.require_role(UserRole::SuperAdmin),
        Err(AuthError::Forbidden)
    ));
    assert!(super_admin.require_role(UserRole::SuperAdmin).is_ok());
}

// ─── logout ─────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn logout_takes_effect_immediately_and_is_idempotent(pool: PgPool) {
    let (auth, ..) = setup(&pool, config()).await;
    let laptop = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    let phone = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();

    auth.logout(laptop.tokens.session_id, client())
        .await
        .unwrap();
    auth.logout(laptop.tokens.session_id, client())
        .await
        .unwrap();
    auth.logout(Uuid::new_v4(), client()).await.unwrap();

    assert!(matches!(
        auth.authenticate(&laptop.tokens.access_token).await,
        Err(AuthError::InvalidToken)
    ));
    auth.authenticate(&phone.tokens.access_token)
        .await
        .expect("other devices stay signed in");

    // Replaying a logged-out refresh token fails but is NOT treated as theft.
    let replay = auth.refresh(&laptop.tokens.refresh_token, client()).await;
    assert!(
        matches!(replay, Err(AuthError::InvalidToken)),
        "got {replay:?}"
    );
    auth.authenticate(&phone.tokens.access_token)
        .await
        .expect("no collateral revocation");

    let entries = audit(&pool).await;
    assert_eq!(
        entries.iter().filter(|(a, ..)| a == "auth.logout").count(),
        1,
        "idempotent"
    );
    assert!(!entries.iter().any(|(a, ..)| a == "auth.refresh_reuse"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn logout_by_refresh_token_and_logout_all(pool: PgPool) {
    let (auth, alice, _) = setup(&pool, config()).await;
    let a = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    let b = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();
    let c = auth
        .login(login_request("alice@example.com", common::PASSWORD))
        .await
        .unwrap();

    auth.logout_by_refresh_token(&a.tokens.refresh_token, client())
        .await
        .unwrap();
    auth.logout_by_refresh_token(&a.tokens.refresh_token, client())
        .await
        .unwrap();
    auth.logout_by_refresh_token("never-issued", client())
        .await
        .unwrap();
    assert!(auth.authenticate(&a.tokens.access_token).await.is_err());
    assert!(auth.authenticate(&b.tokens.access_token).await.is_ok());

    assert_eq!(auth.logout_all(alice.id, client()).await.unwrap(), 2);
    for out in [&b, &c] {
        assert!(auth.authenticate(&out.tokens.access_token).await.is_err());
    }
    assert_eq!(auth.logout_all(alice.id, client()).await.unwrap(), 0);
}

// ─── audit metadata ─────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn audit_entries_carry_client_details_bounded_in_size(pool: PgPool) {
    let (auth, ..) = setup(&pool, config()).await;
    let hostile = ClientInfo {
        ip_address: Some("198.51.100.7".into()),
        user_agent: Some("x".repeat(5_000)),
    };
    let long_email = format!("{}@example.com", "a".repeat(400));
    let _ = auth
        .login(LoginRequest {
            email: long_email,
            password: "whatever-password".into(),
            client: hostile,
        })
        .await;

    let entries = PgAuditLogRepository::new(pool)
        .list(&AuditQuery::default())
        .await
        .unwrap();
    let entry = entries.first().expect("one entry");
    assert_eq!(entry.ip_address.as_deref(), Some("198.51.100.7"));
    assert_eq!(entry.user_agent.as_deref().map(str::len), Some(512));
    assert_eq!(entry.actor_email.as_deref().map(str::len), Some(254));
}
