//! [`AuthService`]: login, token refresh, logout and request authentication.
//!
//! Security properties worth knowing:
//!
//! - **No account enumeration.** An unknown e-mail runs a full password hash
//!   comparison against a dummy hash, and returns the same
//!   [`AuthError::InvalidCredentials`] as a wrong password. "Locked" and
//!   "disabled" are only revealed after the correct password was supplied.
//! - **Lockout.** After `max_failed_logins` wrong passwords the account is
//!   locked for `lockout`. Attempts made while locked do not extend the lock.
//! - **Refresh rotation with reuse detection.** Every refresh revokes the
//!   presented session and issues a new one. Presenting an already-rotated
//!   token after the grace window (`reuse_grace`) is treated as theft and
//!   revokes all of the user's sessions; inside the window it is just rejected,
//!   so two tabs refreshing at once do not log the user out.
//! - **Immediate logout.** [`AuthService::authenticate`] checks that the
//!   session is still active, so a revoked session's access token stops
//!   working at once instead of lingering until it expires.
//! - **Audit trail.** Every outcome is recorded. Writes are best-effort: a
//!   failure is logged but never turns a successful login into an error, and
//!   secrets are never written.

use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::config::AuthConfig;
use super::error::AuthError;
use super::tokens::{
    generate_refresh_token, hash_refresh_token, issue_access_token, verify_access_token, TokenPair,
};
use crate::domain::validation::normalize_email;
use crate::domain::{AuditOutcome, NewAuditEntry, NewSession, User, UserRole};
use crate::error::Error;
use crate::password::{hash_password, verify_password};
use crate::repository::{
    AuditLogRepository, DomainRepository, PgAuditLogRepository, PgDomainRepository,
    PgSessionRepository, PgUserRepository, SessionRepository, UserRepository,
};

const MAX_EMAIL_AUDIT_CHARS: usize = 254;
const MAX_USER_AGENT_CHARS: usize = 512;
const MAX_IP_CHARS: usize = 64;

/// Details about the calling client, recorded on sessions and in the audit log.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    /// The client's IP address.
    pub ip_address: Option<String>,
    /// The client's `User-Agent` header.
    pub user_agent: Option<String>,
}

/// Credentials presented to [`AuthService::login`].
#[derive(Clone)]
pub struct LoginRequest {
    /// The account's e-mail address (case-insensitive).
    pub email: String,
    /// The plaintext password. Never logged or stored.
    pub password: String,
    /// Where the request came from.
    pub client: ClientInfo,
}

impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("email", &self.email)
            .field("password", &"<redacted>")
            .field("client", &self.client)
            .finish()
    }
}

/// The result of a successful login or refresh.
#[derive(Debug, Clone)]
pub struct AuthOutcome {
    /// The authenticated user (as loaded just now).
    pub user: User,
    /// Fresh credentials for the new session.
    pub tokens: TokenPair,
}

/// An authenticated request's identity, produced by [`AuthService::authenticate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// The authenticated user.
    pub user_id: Uuid,
    /// The active session backing the access token.
    pub session_id: Uuid,
    /// The role carried by the token.
    pub role: UserRole,
    /// When the access token expires.
    pub expires_at: DateTime<Utc>,
}

impl AuthContext {
    /// Requires the caller to hold at least `minimum`.
    ///
    /// # Errors
    /// Returns [`AuthError::Forbidden`] if the role is too low.
    pub fn require_role(&self, minimum: UserRole) -> Result<(), AuthError> {
        if self.role.at_least(minimum) {
            Ok(())
        } else {
            Err(AuthError::Forbidden)
        }
    }
}

/// Orchestrates authentication on top of the repositories.
#[derive(Clone)]
pub struct AuthService {
    config: Arc<AuthConfig>,
    users: Arc<dyn UserRepository>,
    domains: Arc<dyn DomainRepository>,
    sessions: Arc<dyn SessionRepository>,
    audit: Arc<dyn AuditLogRepository>,
}

impl std::fmt::Debug for AuthService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthService")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AuthService {
    /// Builds a service from explicit repositories (useful for tests and fakes).
    #[must_use]
    pub fn new(
        config: AuthConfig,
        users: Arc<dyn UserRepository>,
        domains: Arc<dyn DomainRepository>,
        sessions: Arc<dyn SessionRepository>,
        audit: Arc<dyn AuditLogRepository>,
    ) -> Self {
        // Pay the one-off cost of the dummy hash now, not on the first login.
        let _ = dummy_hash();
        Self {
            config: Arc::new(config),
            users,
            domains,
            sessions,
            audit,
        }
    }

    /// Builds a service backed by PostgreSQL.
    #[must_use]
    pub fn postgres(pool: PgPool, config: AuthConfig) -> Self {
        Self::new(
            config,
            Arc::new(PgUserRepository::new(pool.clone())),
            Arc::new(PgDomainRepository::new(pool.clone())),
            Arc::new(PgSessionRepository::new(pool.clone())),
            Arc::new(PgAuditLogRepository::new(pool)),
        )
    }

    /// The settings in force.
    #[must_use]
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Verifies credentials and starts a session.
    ///
    /// # Errors
    /// - [`AuthError::InvalidCredentials`] for an unknown account or wrong password
    /// - [`AuthError::AccountLocked`] / [`AuthError::AccountDisabled`] once the
    ///   password was correct
    /// - [`AuthError::Internal`] for infrastructure failures
    pub async fn login(&self, request: LoginRequest) -> Result<AuthOutcome, AuthError> {
        let client = &request.client;
        let normalized = normalize_email(&request.email).ok();
        let user = match &normalized {
            Some(email) => self.users.find_by_email(email).await?,
            None => None,
        };

        let Some(user) = user else {
            // Do the same expensive work as for a real account so response
            // time does not reveal whether the address exists.
            if let Some(dummy) = dummy_hash() {
                let _ = verify_blocking(request.password.clone(), dummy.to_owned()).await;
            }
            self.record(login_entry(
                None,
                Some(&request.email),
                client,
                AuditOutcome::Failure,
                "unknown_user",
                None,
            ))
            .await;
            return Err(AuthError::InvalidCredentials);
        };

        let password_ok =
            verify_blocking(request.password.clone(), user.password_hash.clone()).await?;
        let now = Utc::now();
        let active_lock = user.locked_until.filter(|until| *until > now);

        if !password_ok {
            if active_lock.is_some() {
                // Already locked: do not extend the lock for attempts made during it.
                self.record(login_entry(
                    Some(&user),
                    None,
                    client,
                    AuditOutcome::Failure,
                    "locked",
                    None,
                ))
                .await;
            } else {
                let state = self
                    .users
                    .record_login_failure(
                        user.id,
                        self.config.max_failed_logins,
                        self.config.lockout.num_seconds(),
                    )
                    .await?;
                let details = serde_json::json!({
                    "failed_attempts": state.failed_login_count,
                    "locked_now": state.locked_until.is_some(),
                });
                self.record(login_entry(
                    Some(&user),
                    None,
                    client,
                    AuditOutcome::Failure,
                    "bad_password",
                    Some(details),
                ))
                .await;
            }
            return Err(AuthError::InvalidCredentials);
        }

        if let Some(until) = active_lock {
            self.record(login_entry(
                Some(&user),
                None,
                client,
                AuditOutcome::Failure,
                "locked",
                None,
            ))
            .await;
            return Err(AuthError::AccountLocked { until });
        }
        if !self.account_is_usable(&user).await? {
            self.record(login_entry(
                Some(&user),
                None,
                client,
                AuditOutcome::Failure,
                "account_inactive",
                None,
            ))
            .await;
            return Err(AuthError::AccountDisabled);
        }

        self.users.record_login_success(user.id).await?;
        let tokens = self.start_session(&user, client).await?;
        let details = serde_json::json!({ "session_id": tokens.session_id });
        self.record(login_entry(
            Some(&user),
            None,
            client,
            AuditOutcome::Success,
            "ok",
            Some(details),
        ))
        .await;
        Ok(AuthOutcome { user, tokens })
    }

    /// Exchanges a refresh token for a new token pair, rotating the session.
    ///
    /// # Errors
    /// - [`AuthError::InvalidToken`] if the token is unknown, expired, revoked
    ///   or was already rotated (the last also revokes every session of the user)
    /// - [`AuthError::AccountDisabled`] if the account was disabled meanwhile
    pub async fn refresh(
        &self,
        refresh_token: &str,
        client: ClientInfo,
    ) -> Result<AuthOutcome, AuthError> {
        let hash = hash_refresh_token(refresh_token);
        let Some(old) = self.sessions.find_by_token_hash(&hash).await? else {
            self.record(refresh_entry(
                None,
                &client,
                AuditOutcome::Failure,
                "unknown_token",
                None,
            ))
            .await;
            return Err(AuthError::InvalidToken);
        };

        if let Some(revoked_at) = old.revoked_at {
            let rotated = old.replaced_by.is_some();
            let within_grace = Utc::now() - revoked_at < self.config.reuse_grace;
            if rotated && !within_grace {
                // A rotated token came back well after rotation: someone has a
                // copy. Burn every session of the user.
                let revoked = self.sessions.revoke_all_for_user(old.user_id).await?;
                let mut entry = refresh_entry(
                    Some(old.user_id),
                    &client,
                    AuditOutcome::Failure,
                    "token_reuse_detected",
                    Some(serde_json::json!({ "sessions_revoked": revoked })),
                );
                entry.action = "auth.refresh_reuse".to_owned();
                self.record(entry).await;
            } else {
                // Ordinary logout, or a rotation race inside the grace window.
                let reason = if rotated {
                    "reuse_within_grace"
                } else {
                    "revoked"
                };
                self.record(refresh_entry(
                    Some(old.user_id),
                    &client,
                    AuditOutcome::Failure,
                    reason,
                    None,
                ))
                .await;
            }
            return Err(AuthError::InvalidToken);
        }
        if old.expires_at <= Utc::now() {
            self.record(refresh_entry(
                Some(old.user_id),
                &client,
                AuditOutcome::Failure,
                "expired",
                None,
            ))
            .await;
            return Err(AuthError::InvalidToken);
        }

        let user = match self.users.get(old.user_id).await {
            Ok(user) => user,
            Err(Error::NotFound { .. }) => return Err(AuthError::InvalidToken),
            Err(e) => return Err(e.into()),
        };
        if !self.account_is_usable(&user).await? {
            self.sessions.revoke(old.id).await?;
            self.record(refresh_entry(
                Some(user.id),
                &client,
                AuditOutcome::Failure,
                "account_inactive",
                None,
            ))
            .await;
            return Err(AuthError::AccountDisabled);
        }

        let now = Utc::now();
        let new_refresh = generate_refresh_token();
        let rotated = self
            .sessions
            .rotate(
                old.id,
                &NewSession {
                    user_id: user.id,
                    refresh_token_hash: hash_refresh_token(&new_refresh),
                    user_agent: client
                        .user_agent
                        .as_deref()
                        .map(|s| truncate(s, MAX_USER_AGENT_CHARS)),
                    ip_address: client
                        .ip_address
                        .as_deref()
                        .map(|s| truncate(s, MAX_IP_CHARS)),
                    expires_at: now + self.config.refresh_ttl,
                },
            )
            .await?;
        let Some(session) = rotated else {
            // Lost a race with a concurrent refresh using the same token.
            self.record(refresh_entry(
                Some(user.id),
                &client,
                AuditOutcome::Failure,
                "lost_race",
                None,
            ))
            .await;
            return Err(AuthError::InvalidToken);
        };

        let (access_token, access_expires_at) =
            issue_access_token(&self.config, user.id, session.id, user.role, now)?;
        let tokens = TokenPair {
            access_token,
            access_expires_at,
            refresh_token: new_refresh,
            refresh_expires_at: session.expires_at,
            session_id: session.id,
        };
        let details =
            serde_json::json!({ "session_id": session.id, "previous_session_id": old.id });
        self.record(refresh_entry(
            Some(user.id),
            &client,
            AuditOutcome::Success,
            "ok",
            Some(details),
        ))
        .await;
        Ok(AuthOutcome { user, tokens })
    }

    /// Validates an access token and confirms its session is still active.
    ///
    /// # Errors
    /// Returns [`AuthError::InvalidToken`] if the token is invalid, expired, or
    /// its session was revoked or has expired.
    pub async fn authenticate(&self, access_token: &str) -> Result<AuthContext, AuthError> {
        let claims = verify_access_token(&self.config, access_token, Utc::now())?;
        let session = self
            .sessions
            .find_active_by_id(claims.sid)
            .await?
            .filter(|s| s.user_id == claims.sub)
            .ok_or(AuthError::InvalidToken)?;
        Ok(AuthContext {
            user_id: claims.sub,
            session_id: session.id,
            role: claims.role,
            expires_at: DateTime::from_timestamp(claims.exp, 0).unwrap_or(session.expires_at),
        })
    }

    /// Deletes expired sessions so the table does not grow without bound.
    /// Returns how many were removed. Meant to run periodically.
    ///
    /// # Errors
    /// Returns [`AuthError::Internal`] on infrastructure failure.
    pub async fn purge_expired_sessions(&self) -> Result<u64, AuthError> {
        Ok(self.sessions.delete_expired().await?)
    }

    /// Loads the user an authenticated request belongs to.
    ///
    /// # Errors
    /// Returns [`AuthError::InvalidToken`] if the user no longer exists.
    pub async fn current_user(&self, context: &AuthContext) -> Result<User, AuthError> {
        match self.users.get(context.user_id).await {
            Ok(user) => Ok(user),
            Err(Error::NotFound { .. }) => Err(AuthError::InvalidToken),
            Err(e) => Err(e.into()),
        }
    }

    /// Ends one session. Idempotent: unknown or already ended sessions are fine.
    ///
    /// # Errors
    /// Returns [`AuthError::Internal`] on infrastructure failure.
    pub async fn logout(&self, session_id: Uuid, client: ClientInfo) -> Result<(), AuthError> {
        if let Some(session) = self.sessions.find_active_by_id(session_id).await? {
            self.sessions.revoke(session.id).await?;
            self.record(logout_entry(
                "auth.logout",
                Some(session.user_id),
                &client,
                serde_json::json!({
                    "session_id": session.id,
                }),
            ))
            .await;
        }
        Ok(())
    }

    /// Ends the session a refresh token belongs to. Idempotent.
    ///
    /// Unlike [`AuthService::refresh`], presenting a token that was already
    /// revoked is harmless here and never triggers reuse handling.
    ///
    /// # Errors
    /// Returns [`AuthError::Internal`] on infrastructure failure.
    pub async fn logout_by_refresh_token(
        &self,
        refresh_token: &str,
        client: ClientInfo,
    ) -> Result<(), AuthError> {
        let hash = hash_refresh_token(refresh_token);
        if let Some(session) = self.sessions.find_by_token_hash(&hash).await? {
            if session.revoked_at.is_none() {
                self.sessions.revoke(session.id).await?;
                self.record(logout_entry(
                    "auth.logout",
                    Some(session.user_id),
                    &client,
                    serde_json::json!({
                        "session_id": session.id,
                    }),
                ))
                .await;
            }
        }
        Ok(())
    }

    /// Ends every session of a user ("log out everywhere"). Returns how many.
    ///
    /// # Errors
    /// Returns [`AuthError::Internal`] on infrastructure failure.
    pub async fn logout_all(&self, user_id: Uuid, client: ClientInfo) -> Result<u64, AuthError> {
        let revoked = self.sessions.revoke_all_for_user(user_id).await?;
        self.record(logout_entry(
            "auth.logout_all",
            Some(user_id),
            &client,
            serde_json::json!({
                "sessions_revoked": revoked,
            }),
        ))
        .await;
        Ok(revoked)
    }

    /// Whether the user and their domain are both active.
    async fn account_is_usable(&self, user: &User) -> Result<bool, AuthError> {
        if !user.status.is_active() {
            return Ok(false);
        }
        let domain = self.domains.get(user.domain_id).await?;
        Ok(domain.status.is_active())
    }

    /// Creates a session and its first token pair.
    async fn start_session(
        &self,
        user: &User,
        client: &ClientInfo,
    ) -> Result<TokenPair, AuthError> {
        let now = Utc::now();
        let refresh_token = generate_refresh_token();
        let session = self
            .sessions
            .create(&NewSession {
                user_id: user.id,
                refresh_token_hash: hash_refresh_token(&refresh_token),
                user_agent: client
                    .user_agent
                    .as_deref()
                    .map(|s| truncate(s, MAX_USER_AGENT_CHARS)),
                ip_address: client
                    .ip_address
                    .as_deref()
                    .map(|s| truncate(s, MAX_IP_CHARS)),
                expires_at: now + self.config.refresh_ttl,
            })
            .await?;
        let (access_token, access_expires_at) =
            issue_access_token(&self.config, user.id, session.id, user.role, now)?;
        Ok(TokenPair {
            access_token,
            access_expires_at,
            refresh_token,
            refresh_expires_at: session.expires_at,
            session_id: session.id,
        })
    }

    /// Best-effort audit write: failures are logged, never propagated.
    async fn record(&self, entry: NewAuditEntry) {
        if let Err(error) = self.audit.append(&entry).await {
            tracing::error!(%error, action = %entry.action, "failed to write audit log entry");
        }
    }
}

/// Builds the audit entry for a login attempt.
fn login_entry(
    user: Option<&User>,
    attempted_email: Option<&str>,
    client: &ClientInfo,
    outcome: AuditOutcome,
    reason: &str,
    extra: Option<serde_json::Value>,
) -> NewAuditEntry {
    let mut details = serde_json::json!({ "reason": reason });
    merge(&mut details, extra);
    NewAuditEntry {
        actor_user_id: user.map(|u| u.id),
        // For unknown accounts record what was typed (bounded), never a password.
        actor_email: user
            .map(|u| u.email.clone())
            .or_else(|| attempted_email.map(|e| truncate(e, MAX_EMAIL_AUDIT_CHARS))),
        ip_address: client
            .ip_address
            .as_deref()
            .map(|s| truncate(s, MAX_IP_CHARS)),
        user_agent: client
            .user_agent
            .as_deref()
            .map(|s| truncate(s, MAX_USER_AGENT_CHARS)),
        details,
        ..NewAuditEntry::new("auth.login", outcome)
    }
}

/// Builds the audit entry for a refresh attempt.
fn refresh_entry(
    user_id: Option<Uuid>,
    client: &ClientInfo,
    outcome: AuditOutcome,
    reason: &str,
    extra: Option<serde_json::Value>,
) -> NewAuditEntry {
    let mut details = serde_json::json!({ "reason": reason });
    merge(&mut details, extra);
    NewAuditEntry {
        actor_user_id: user_id,
        ip_address: client
            .ip_address
            .as_deref()
            .map(|s| truncate(s, MAX_IP_CHARS)),
        user_agent: client
            .user_agent
            .as_deref()
            .map(|s| truncate(s, MAX_USER_AGENT_CHARS)),
        details,
        ..NewAuditEntry::new("auth.refresh", outcome)
    }
}

/// Builds the audit entry for a logout.
fn logout_entry(
    action: &str,
    user_id: Option<Uuid>,
    client: &ClientInfo,
    details: serde_json::Value,
) -> NewAuditEntry {
    NewAuditEntry {
        actor_user_id: user_id,
        ip_address: client
            .ip_address
            .as_deref()
            .map(|s| truncate(s, MAX_IP_CHARS)),
        user_agent: client
            .user_agent
            .as_deref()
            .map(|s| truncate(s, MAX_USER_AGENT_CHARS)),
        details,
        ..NewAuditEntry::new(action, AuditOutcome::Success)
    }
}

/// Copies the keys of `extra` into `base` (both JSON objects).
fn merge(base: &mut serde_json::Value, extra: Option<serde_json::Value>) {
    if let (Some(base), Some(serde_json::Value::Object(extra))) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
}

/// Truncates to at most `max` characters, on a character boundary.
fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Runs password verification on the blocking pool so Argon2 cannot stall the
/// async runtime.
async fn verify_blocking(password: String, hash: String) -> Result<bool, AuthError> {
    tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .map_err(|e| {
            AuthError::Internal(Error::PasswordHash(format!(
                "verification task failed: {e}"
            )))
        })?
        .map_err(AuthError::from)
}

/// A real Argon2 hash of a throwaway password, used to equalise timing for
/// unknown accounts. `None` only if hashing itself failed.
fn dummy_hash() -> Option<&'static str> {
    static DUMMY: OnceLock<Option<String>> = OnceLock::new();
    DUMMY
        .get_or_init(|| hash_password("rustmail-timing-equalisation").ok())
        .as_deref()
}
