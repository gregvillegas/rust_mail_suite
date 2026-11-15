//! Database access for filters and the autoresponder.
//!
//! Rules live in `mail_filters` (one row each, in `position` order), a hand-written
//! script in `sieve_scripts`, the autoresponder in `autoresponders`, and whether the
//! last push to the mail server worked in `sieve_sync_state`.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::sieve::FILTERS_SCRIPT;
use super::{validate_rules, Action, Autoresponder, Condition, Rule, MAX_RAW_BYTES};
use crate::error::{Error, Result};

/// Which script a sync status is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    /// The rules or the hand-written script.
    Filters,
    /// The autoresponder.
    Vacation,
}

impl Script {
    fn as_str(self) -> &'static str {
        match self {
            Self::Filters => "filters",
            Self::Vacation => "vacation",
        }
    }
}

/// How the filters script is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// Generated from the rule list.
    Rules,
    /// Written by the user in the advanced editor.
    Raw,
}

/// Whether the last push of a script to the mail server worked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStatus {
    /// When it last succeeded; `None` if it never has, or a change is waiting.
    pub installed_at: Option<DateTime<Utc>>,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

/// A user's saved filters.
#[derive(Debug, Clone)]
pub struct Filters {
    /// Rule list or raw script.
    pub mode: FilterMode,
    /// The rules (kept even in raw mode, so switching back loses nothing).
    pub rules: Vec<Rule>,
    /// The raw script (empty until the user writes one).
    pub raw_script: String,
    /// Push status.
    pub status: SyncStatus,
}

/// An autoresponder together with its push status.
#[derive(Debug, Clone)]
pub struct StoredAutoresponder {
    /// The settings.
    pub settings: Autoresponder,
    /// Push status.
    pub status: SyncStatus,
}

/// Reads and writes filter settings. Every call is for one user: callers pass the
/// signed-in user's id, never an id taken from a request.
#[derive(Clone)]
pub struct FilterStore {
    pool: PgPool,
}

fn bad(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Validation(format!("{what}: {e}"))
}

impl FilterStore {
    /// Creates the store.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The user's filters (an empty rule list if they never saved any).
    ///
    /// # Errors
    /// Database errors, or stored rules that no longer parse.
    pub async fn filters(&self, user_id: Uuid) -> Result<Filters> {
        let rows = sqlx::query!(
            r#"SELECT name, enabled, match_all, conditions, actions, stop_processing
               FROM mail_filters WHERE user_id = $1 ORDER BY position"#,
            user_id
        )
        .fetch_all(&self.pool)
        .await?;
        let mut rules = Vec::with_capacity(rows.len());
        for r in rows {
            let conditions: Vec<Condition> = serde_json::from_value(r.conditions)
                .map_err(|e| bad("stored rule conditions are unreadable", e))?;
            let mut actions: Vec<Action> = serde_json::from_value(r.actions)
                .map_err(|e| bad("stored rule actions are unreadable", e))?;
            if r.stop_processing {
                actions.push(Action::Stop);
            }
            rules.push(Rule {
                name: r.name,
                enabled: r.enabled,
                match_all: r.match_all,
                conditions,
                actions,
            });
        }
        let raw = sqlx::query!(
            r#"SELECT content, is_active FROM sieve_scripts WHERE user_id = $1 AND name = $2"#,
            user_id,
            FILTERS_SCRIPT
        )
        .fetch_optional(&self.pool)
        .await?;
        let (mode, raw_script) = match raw {
            Some(r) if r.is_active => (FilterMode::Raw, r.content),
            Some(r) => (FilterMode::Rules, r.content),
            None => (FilterMode::Rules, String::new()),
        };
        Ok(Filters {
            mode,
            rules,
            raw_script,
            status: self.status(user_id, Script::Filters).await?,
        })
    }

    /// Replaces the rule list and switches to rule mode. The push status is reset.
    ///
    /// # Errors
    /// [`Error::Validation`] for invalid rules, or database errors.
    pub async fn save_rules(&self, user_id: Uuid, rules: &[Rule]) -> Result<()> {
        validate_rules(rules)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query!("DELETE FROM mail_filters WHERE user_id = $1", user_id)
            .execute(&mut *tx)
            .await?;
        for (position, rule) in rules.iter().enumerate() {
            let stop = rule.actions.contains(&Action::Stop);
            let actions: Vec<&Action> = rule
                .actions
                .iter()
                .filter(|a| **a != Action::Stop)
                .collect();
            let conditions =
                serde_json::to_value(&rule.conditions).map_err(|e| bad("rule conditions", e))?;
            let actions = serde_json::to_value(actions).map_err(|e| bad("rule actions", e))?;
            sqlx::query!(
                r#"INSERT INTO mail_filters (user_id, name, position, enabled, match_all, conditions, actions, stop_processing)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
                user_id,
                rule.name,
                i32::try_from(position).unwrap_or(i32::MAX),
                rule.enabled,
                rule.match_all,
                conditions,
                actions,
                stop
            )
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query!(
            "UPDATE sieve_scripts SET is_active = false WHERE user_id = $1 AND name = $2",
            user_id,
            FILTERS_SCRIPT
        )
        .execute(&mut *tx)
        .await?;
        reset_status(&mut tx, user_id, Script::Filters).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Saves a hand-written script and switches to raw mode.
    ///
    /// The text is only size-checked here; the mail server's parser has the last word
    /// when it is installed.
    ///
    /// # Errors
    /// [`Error::Validation`] for an oversized or binary script, or database errors.
    pub async fn save_raw(&self, user_id: Uuid, script: &str) -> Result<()> {
        if script.len() > MAX_RAW_BYTES {
            return Err(Error::Validation(format!(
                "the script is larger than {} KB",
                MAX_RAW_BYTES / 1024
            )));
        }
        if script
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err(Error::Validation(
                "the script contains control characters".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            r#"INSERT INTO sieve_scripts (user_id, name, content, is_active) VALUES ($1, $2, $3, true)
               ON CONFLICT (user_id, name) DO UPDATE SET content = $3, is_active = true"#,
            user_id,
            FILTERS_SCRIPT,
            script
        )
        .execute(&mut *tx)
        .await?;
        reset_status(&mut tx, user_id, Script::Filters).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Switches back to the rule list without deleting the hand-written script.
    ///
    /// # Errors
    /// Database errors.
    pub async fn use_rules(&self, user_id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "UPDATE sieve_scripts SET is_active = false WHERE user_id = $1 AND name = $2",
            user_id,
            FILTERS_SCRIPT
        )
        .execute(&mut *tx)
        .await?;
        reset_status(&mut tx, user_id, Script::Filters).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The user's autoresponder (switched off if never saved).
    ///
    /// # Errors
    /// Database errors.
    pub async fn autoresponder(&self, user_id: Uuid) -> Result<StoredAutoresponder> {
        let row = sqlx::query!(
            r#"SELECT enabled, starts_on, ends_on, subject, body, reply_interval_days FROM autoresponders WHERE user_id = $1"#,
            user_id
        )
        .fetch_optional(&self.pool)
        .await?;
        let settings = row.map_or_else(Autoresponder::default, |r| Autoresponder {
            enabled: r.enabled,
            starts_on: r.starts_on,
            ends_on: r.ends_on,
            subject: r.subject,
            body: r.body,
            days: r.reply_interval_days,
        });
        Ok(StoredAutoresponder {
            settings,
            status: self.status(user_id, Script::Vacation).await?,
        })
    }

    /// Saves the autoresponder. The push status is reset.
    ///
    /// # Errors
    /// [`Error::Validation`] for invalid settings, or database errors.
    pub async fn save_autoresponder(&self, user_id: Uuid, a: &Autoresponder) -> Result<()> {
        a.validate()?;
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            r#"INSERT INTO autoresponders (user_id, enabled, starts_on, ends_on, subject, body, reply_interval_days)
               VALUES ($1, $2, $3, $4, $5, $6, $7)
               ON CONFLICT (user_id) DO UPDATE
               SET enabled = $2, starts_on = $3, ends_on = $4, subject = $5, body = $6, reply_interval_days = $7"#,
            user_id,
            a.enabled,
            a.starts_on,
            a.ends_on,
            a.subject,
            a.body,
            a.days
        )
        .execute(&mut *tx)
        .await?;
        reset_status(&mut tx, user_id, Script::Vacation).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The addresses mail for this user arrives under: their login and the enabled
    /// aliases that point at it. The autoresponder answers only mail sent to these.
    ///
    /// # Errors
    /// Database errors.
    pub async fn own_addresses(&self, user_id: Uuid) -> Result<Vec<String>> {
        let email = sqlx::query_scalar!("SELECT email FROM users WHERE id = $1", user_id)
            .fetch_one(&self.pool)
            .await?;
        let mut all = vec![email.clone()];
        all.extend(
            sqlx::query_scalar!(
                "SELECT address FROM aliases WHERE enabled AND destination = $1 ORDER BY address",
                email
            )
            .fetch_all(&self.pool)
            .await?,
        );
        Ok(all)
    }

    /// Whether the last push of `script` worked.
    ///
    /// # Errors
    /// Database errors.
    pub async fn status(&self, user_id: Uuid, script: Script) -> Result<SyncStatus> {
        let row = sqlx::query!(
            "SELECT installed_at, error FROM sieve_sync_state WHERE user_id = $1 AND script = $2",
            user_id,
            script.as_str()
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .map(|r| SyncStatus {
                installed_at: r.installed_at,
                error: r.error,
            })
            .unwrap_or_default())
    }

    /// Records the outcome of a push: `None` for success, or why it failed.
    ///
    /// # Errors
    /// Database errors.
    pub async fn mark(&self, user_id: Uuid, script: Script, error: Option<&str>) -> Result<()> {
        sqlx::query!(
            r#"INSERT INTO sieve_sync_state (user_id, script, installed_at, error)
               VALUES ($1, $2, CASE WHEN $3::text IS NULL THEN now() END, $3)
               ON CONFLICT (user_id, script) DO UPDATE
               SET installed_at = CASE WHEN $3::text IS NULL THEN now() END, error = $3, updated_at = now()"#,
            user_id,
            script.as_str(),
            error
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

async fn reset_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    script: Script,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO sieve_sync_state (user_id, script) VALUES ($1, $2)
           ON CONFLICT (user_id, script) DO UPDATE SET installed_at = NULL, error = NULL, updated_at = now()"#,
        user_id,
        script.as_str()
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}
