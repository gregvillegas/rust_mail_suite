//! Hosted domains, user accounts and mailboxes.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::enums::{AccountStatus, UserRole};

/// A hosted mail domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct MailDomain {
    /// Primary key.
    pub id: Uuid,
    /// Lower-case DNS name. Immutable once created.
    pub name: String,
    /// Lifecycle state.
    pub status: AccountStatus,
    /// Default mailbox quota in bytes; `None` means unlimited.
    pub default_quota_bytes: Option<i64>,
    /// Maximum number of mailboxes; `None` means unlimited.
    pub max_mailboxes: Option<i32>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a [`MailDomain`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NewDomain {
    /// Domain name; normalised and validated on insert.
    pub name: String,
    /// Default mailbox quota in bytes; `None` means unlimited.
    pub default_quota_bytes: Option<i64>,
    /// Maximum number of mailboxes; `None` means unlimited.
    pub max_mailboxes: Option<i32>,
}

/// A user account (login identity).
///
/// The password hash is never serialised and is redacted from `Debug` output.
#[derive(Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct User {
    /// Primary key.
    pub id: Uuid,
    /// Owning domain.
    pub domain_id: Uuid,
    /// Lower-case part before the `@`.
    pub local_part: String,
    /// Full lower-case address, `local_part@domain`.
    pub email: String,
    /// Argon2id PHC string.
    #[serde(skip_serializing)]
    pub password_hash: String,
    /// Name shown in the UI.
    pub display_name: String,
    /// Authorisation level.
    pub role: UserRole,
    /// Lifecycle state.
    pub status: AccountStatus,
    /// UI locale tag, e.g. `en`.
    pub locale: String,
    /// Last successful login.
    pub last_login_at: Option<DateTime<Utc>>,
    /// Consecutive failed logins since the last success.
    pub failed_login_count: i32,
    /// Login is refused until this time, if set.
    pub locked_until: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("password_hash", &"<redacted>")
            .field("role", &self.role)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Input for creating a [`User`]. The caller hashes the password.
#[derive(Clone, PartialEq, Eq)]
pub struct NewUser {
    /// Owning domain.
    pub domain_id: Uuid,
    /// Part before the `@`; normalised and validated on insert.
    pub local_part: String,
    /// Argon2id PHC string (see [`crate::password::hash_password`]).
    pub password_hash: String,
    /// Name shown in the UI.
    pub display_name: String,
    /// Authorisation level.
    pub role: UserRole,
    /// UI locale tag, e.g. `en`.
    pub locale: String,
}

impl fmt::Debug for NewUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewUser")
            .field("domain_id", &self.domain_id)
            .field("local_part", &self.local_part)
            .field("password_hash", &"<redacted>")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

/// Login-failure state of a user after recording one more failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct LoginFailure {
    /// Failed attempts counted towards the next lock (reset to 0 when a lock
    /// is applied).
    pub failed_login_count: i32,
    /// The account is locked until this time, if a lock is in force.
    pub locked_until: Option<DateTime<Utc>>,
}

/// A user's primary mailbox (storage).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Mailbox {
    /// Primary key.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Maildir location relative to the vmail base directory.
    pub maildir_path: String,
    /// Quota override in bytes; `None` inherits the domain default.
    pub quota_bytes: Option<i64>,
    /// Lifecycle state.
    pub status: AccountStatus,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a [`Mailbox`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMailbox {
    /// Owning user.
    pub user_id: Uuid,
    /// Maildir location relative to the vmail base directory.
    pub maildir_path: String,
    /// Quota override in bytes; `None` inherits the domain default.
    pub quota_bytes: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_user() -> User {
        let now = Utc::now();
        User {
            id: Uuid::nil(),
            domain_id: Uuid::nil(),
            local_part: "admin".into(),
            email: "admin@example.com".into(),
            password_hash: "$argon2id$secret-material".into(),
            display_name: String::new(),
            role: UserRole::SuperAdmin,
            status: AccountStatus::Active,
            locale: "en".into(),
            last_login_at: None,
            failed_login_count: 0,
            locked_until: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn user_json_never_contains_the_password_hash() {
        let json = serde_json::to_string(&sample_user()).expect("serialise");
        assert!(!json.contains("password_hash"));
        assert!(!json.contains("secret-material"));
    }

    #[test]
    fn user_debug_redacts_the_password_hash() {
        let debug = format!("{:?}", sample_user());
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret-material"));
    }

    #[test]
    fn new_user_debug_redacts_the_password_hash() {
        let new = NewUser {
            domain_id: Uuid::nil(),
            local_part: "a".into(),
            password_hash: "secret-material".into(),
            display_name: String::new(),
            role: UserRole::User,
            locale: "en".into(),
        };
        assert!(!format!("{new:?}").contains("secret-material"));
    }
}
