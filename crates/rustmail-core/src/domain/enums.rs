//! Enumerations mirrored from PostgreSQL enum types.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A user's authorisation level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "user_role", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    /// Manages the whole installation.
    SuperAdmin,
    /// Manages the domains they are responsible for.
    DomainAdmin,
    /// A regular mailbox owner.
    User,
}

impl UserRole {
    /// Privilege rank: `SuperAdmin` (3) > `DomainAdmin` (2) > `User` (1).
    #[must_use]
    pub fn level(self) -> u8 {
        match self {
            Self::SuperAdmin => 3,
            Self::DomainAdmin => 2,
            Self::User => 1,
        }
    }

    /// Whether this role is at least as privileged as `minimum`.
    ///
    /// Note this only compares roles. A `DomainAdmin` is further restricted to
    /// the domains they administer, which is enforced where domains are managed
    /// (Phase 7).
    #[must_use]
    pub fn at_least(self, minimum: UserRole) -> bool {
        self.level() >= minimum.level()
    }

    /// Parses the stored value (`super_admin`, `domain_admin`, `user`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "super_admin" => Some(Self::SuperAdmin),
            "domain_admin" => Some(Self::DomainAdmin),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    /// The stable snake_case identifier, identical to the database value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SuperAdmin => "super_admin",
            Self::DomainAdmin => "domain_admin",
            Self::User => "user",
        }
    }
}

impl fmt::Display for UserRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lifecycle state shared by domains, users and mailboxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "account_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    /// Fully operational.
    Active,
    /// Temporarily blocked; data is kept.
    Suspended,
    /// Permanently switched off; data is kept until deleted.
    Disabled,
}

impl AccountStatus {
    /// Whether the account may log in / receive mail.
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    /// Parses the stored value (`active`, `suspended`, `disabled`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "suspended" => Some(Self::Suspended),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }

    /// The stable snake_case identifier, identical to the database value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Disabled => "disabled",
        }
    }
}

impl fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "audit_outcome", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// The action completed.
    Success,
    /// The action was attempted and failed or was denied.
    Failure,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serialises_as_snake_case() {
        let json = serde_json::to_string(&UserRole::SuperAdmin).expect("serialise");
        assert_eq!(json, "\"super_admin\"");
        let back: UserRole = serde_json::from_str("\"domain_admin\"").expect("deserialise");
        assert_eq!(back, UserRole::DomainAdmin);
    }

    #[test]
    fn display_matches_database_value() {
        assert_eq!(UserRole::SuperAdmin.to_string(), "super_admin");
        assert_eq!(AccountStatus::Suspended.to_string(), "suspended");
    }

    #[test]
    fn roles_are_ordered_by_privilege() {
        assert!(UserRole::SuperAdmin.at_least(UserRole::DomainAdmin));
        assert!(UserRole::SuperAdmin.at_least(UserRole::SuperAdmin));
        assert!(UserRole::DomainAdmin.at_least(UserRole::User));
        assert!(!UserRole::User.at_least(UserRole::DomainAdmin));
        assert!(!UserRole::DomainAdmin.at_least(UserRole::SuperAdmin));
    }

    #[test]
    fn only_active_status_is_active() {
        assert!(AccountStatus::Active.is_active());
        assert!(!AccountStatus::Suspended.is_active());
        assert!(!AccountStatus::Disabled.is_active());
    }
}
