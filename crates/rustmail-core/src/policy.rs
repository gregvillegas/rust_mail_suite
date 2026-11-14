//! Per-domain and per-recipient spam and virus policy.
//!
//! The rows live in `filter_policies` and are read by Amavis for every
//! recipient (see `deploy/amavis/60-rustmail.j2`). A `None` setting means
//! "inherit from the next less specific scope": `alice@example.com` falls back to
//! `@example.com`, which falls back to the server-wide `@.`.

use serde::Serialize;
use sqlx::PgPool;

use crate::error::{Error, Result};

/// The server-wide default scope.
pub const GLOBAL_SCOPE: &str = "@.";

/// Highest and lowest scores accepted for a threshold.
const LEVEL_RANGE: std::ops::RangeInclusive<f64> = -1000.0..=10000.0;

/// One policy row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FilterPolicy {
    /// `@.`, `@domain` or `user@domain`.
    pub scope: String,
    /// Score from which headers are added.
    pub spam_tag_level: Option<f64>,
    /// Score from which a message is marked as spam.
    pub spam_tag2_level: Option<f64>,
    /// Score from which a message is quarantined.
    pub spam_kill_level: Option<f64>,
    /// Skip spam checks entirely.
    pub bypass_spam_checks: Option<bool>,
    /// Skip virus checks entirely.
    pub bypass_virus_checks: Option<bool>,
    /// Deliver even if flagged as spam.
    pub spam_lover: Option<bool>,
    /// Deliver even if infected. Dangerous; off unless an operator says so.
    pub virus_lover: Option<bool>,
}

/// Settings to change. Fields left `None` keep their current value; a new
/// scope starts with everything inherited.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PolicyUpdate {
    /// See [`FilterPolicy::spam_tag_level`].
    pub spam_tag_level: Option<f64>,
    /// See [`FilterPolicy::spam_tag2_level`].
    pub spam_tag2_level: Option<f64>,
    /// See [`FilterPolicy::spam_kill_level`].
    pub spam_kill_level: Option<f64>,
    /// See [`FilterPolicy::bypass_spam_checks`].
    pub bypass_spam_checks: Option<bool>,
    /// See [`FilterPolicy::bypass_virus_checks`].
    pub bypass_virus_checks: Option<bool>,
    /// See [`FilterPolicy::spam_lover`].
    pub spam_lover: Option<bool>,
    /// See [`FilterPolicy::virus_lover`].
    pub virus_lover: Option<bool>,
}

/// Normalises and validates a scope: lower-case, and one of `@.`, `@domain` or
/// `user@domain`.
///
/// # Errors
/// [`Error::Validation`] for anything else.
pub fn normalise_scope(scope: &str) -> Result<String> {
    let scope = scope.trim().to_lowercase();
    let ok_part = |s: &str| !s.is_empty() && !s.contains(|c: char| c.is_whitespace() || c == '@');
    let valid = if scope == GLOBAL_SCOPE {
        true
    } else if let Some(domain) = scope.strip_prefix('@') {
        ok_part(domain)
    } else if let Some((local, domain)) = scope.split_once('@') {
        ok_part(local) && ok_part(domain)
    } else {
        false
    };
    if valid && scope.len() <= 320 {
        Ok(scope)
    } else {
        Err(Error::Validation(
            "scope must be '@.', '@example.com' or 'user@example.com'".into(),
        ))
    }
}

impl PolicyUpdate {
    fn validate(&self) -> Result<()> {
        for level in [
            self.spam_tag_level,
            self.spam_tag2_level,
            self.spam_kill_level,
        ]
        .into_iter()
        .flatten()
        {
            if !level.is_finite() || !LEVEL_RANGE.contains(&level) {
                return Err(Error::Validation(
                    "score thresholds must be between -1000 and 10000".into(),
                ));
            }
        }
        let ordered = |low: Option<f64>, high: Option<f64>| match (low, high) {
            (Some(l), Some(h)) => l <= h,
            _ => true,
        };
        if !ordered(self.spam_tag_level, self.spam_tag2_level)
            || !ordered(self.spam_tag2_level, self.spam_kill_level)
            || !ordered(self.spam_tag_level, self.spam_kill_level)
        {
            return Err(Error::Validation(
                "thresholds must satisfy tag <= spam <= quarantine".into(),
            ));
        }
        Ok(())
    }
}

/// PostgreSQL access to `filter_policies`.
#[derive(Debug, Clone)]
pub struct PgPolicyRepository {
    pool: PgPool,
}

impl PgPolicyRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// All policies, server-wide first, then domains, then recipients.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn list(&self) -> Result<Vec<FilterPolicy>> {
        let rows = sqlx::query_as!(
            FilterPolicy,
            r#"SELECT scope, spam_tag_level::float8 AS "spam_tag_level?",
                      spam_tag2_level::float8 AS "spam_tag2_level?",
                      spam_kill_level::float8 AS "spam_kill_level?",
                      bypass_spam_checks, bypass_virus_checks, spam_lover, virus_lover
               FROM filter_policies
               ORDER BY CASE WHEN scope = '@.' THEN 0 WHEN left(scope, 1) = '@' THEN 1 ELSE 2 END, scope"#
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Creates the policy for `scope` or changes the given settings of an existing one.
    ///
    /// # Errors
    /// [`Error::Validation`] for a bad scope or thresholds.
    pub async fn set(&self, scope: &str, update: &PolicyUpdate) -> Result<FilterPolicy> {
        let scope = normalise_scope(scope)?;
        update.validate()?;
        let saved = sqlx::query_as!(
            FilterPolicy,
            r#"INSERT INTO filter_policies
                   (scope, spam_tag_level, spam_tag2_level, spam_kill_level,
                    bypass_spam_checks, bypass_virus_checks, spam_lover, virus_lover)
               VALUES ($1, $2::float8::numeric, $3::float8::numeric, $4::float8::numeric, $5, $6, $7, $8)
               ON CONFLICT (scope) DO UPDATE SET
                   spam_tag_level = COALESCE(EXCLUDED.spam_tag_level, filter_policies.spam_tag_level),
                   spam_tag2_level = COALESCE(EXCLUDED.spam_tag2_level, filter_policies.spam_tag2_level),
                   spam_kill_level = COALESCE(EXCLUDED.spam_kill_level, filter_policies.spam_kill_level),
                   bypass_spam_checks = COALESCE(EXCLUDED.bypass_spam_checks, filter_policies.bypass_spam_checks),
                   bypass_virus_checks = COALESCE(EXCLUDED.bypass_virus_checks, filter_policies.bypass_virus_checks),
                   spam_lover = COALESCE(EXCLUDED.spam_lover, filter_policies.spam_lover),
                   virus_lover = COALESCE(EXCLUDED.virus_lover, filter_policies.virus_lover)
               RETURNING scope, spam_tag_level::float8 AS "spam_tag_level?",
                         spam_tag2_level::float8 AS "spam_tag2_level?",
                         spam_kill_level::float8 AS "spam_kill_level?",
                         bypass_spam_checks, bypass_virus_checks, spam_lover, virus_lover"#,
            scope,
            update.spam_tag_level,
            update.spam_tag2_level,
            update.spam_kill_level,
            update.bypass_spam_checks,
            update.bypass_virus_checks,
            update.spam_lover,
            update.virus_lover,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(saved)
    }

    /// Removes an override so the scope inherits again. The server-wide default
    /// cannot be removed.
    ///
    /// # Errors
    /// [`Error::Validation`] for the global scope; [`Error::NotFound`] if absent.
    pub async fn remove(&self, scope: &str) -> Result<()> {
        let scope = normalise_scope(scope)?;
        if scope == GLOBAL_SCOPE {
            return Err(Error::Validation(
                "the server-wide default cannot be removed; change it instead".into(),
            ));
        }
        let done = sqlx::query!("DELETE FROM filter_policies WHERE scope = $1", scope)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound { entity: "policy" });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_normalised_and_validated() {
        assert_eq!(normalise_scope(" @Example.COM ").unwrap(), "@example.com");
        assert_eq!(
            normalise_scope("Alice@Example.com").unwrap(),
            "alice@example.com"
        );
        assert_eq!(normalise_scope("@.").unwrap(), "@.");
        for bad in ["", "example.com", "@", "@a@b", "a b@c", "a@", "@@", "a@b@c"] {
            assert!(normalise_scope(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn thresholds_must_be_ordered_and_in_range() {
        let ok = PolicyUpdate {
            spam_tag_level: Some(2.0),
            spam_tag2_level: Some(6.0),
            spam_kill_level: Some(12.0),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        let backwards = PolicyUpdate {
            spam_tag2_level: Some(10.0),
            spam_kill_level: Some(5.0),
            ..Default::default()
        };
        assert!(backwards.validate().is_err());
        for bad in [f64::NAN, f64::INFINITY, 1e9, -1e9] {
            let update = PolicyUpdate {
                spam_kill_level: Some(bad),
                ..Default::default()
            };
            assert!(update.validate().is_err(), "{bad}");
        }
    }
}
