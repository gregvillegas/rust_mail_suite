//! Searching and exporting the audit log.

use chrono::{DateTime, Utc};

use super::csv::write_row;
use super::{like_contains, Admin, AdminService};
use crate::domain::{AuditEntry, AuditOutcome};
use crate::error::Result;

/// Largest page of entries.
pub const MAX_PAGE_SIZE: i64 = 200;
/// Most entries one CSV export contains.
pub const MAX_EXPORT_ROWS: i64 = 10_000;

/// Filters for the audit log.
#[derive(Debug, Clone, Default)]
pub struct AuditSearch {
    /// Text in the actor's address.
    pub actor: Option<String>,
    /// An action name or prefix, e.g. `user.` or `auth.login`.
    pub action: Option<String>,
    /// Success or failure.
    pub outcome: Option<AuditOutcome>,
    /// At or after.
    pub from: Option<DateTime<Utc>>,
    /// Before.
    pub to: Option<DateTime<Utc>>,
    /// Text in the target or the details.
    pub text: Option<String>,
    /// Rows per page.
    pub limit: i64,
    /// Rows to skip.
    pub offset: i64,
}

/// A page of entries.
#[derive(Debug, Clone)]
pub struct AuditPage {
    /// The entries, newest first.
    pub entries: Vec<AuditEntry>,
    /// Matches in total.
    pub total: i64,
}

fn outcome_text(o: AuditOutcome) -> &'static str {
    match o {
        AuditOutcome::Success => "success",
        AuditOutcome::Failure => "failure",
    }
}

impl AdminService {
    /// Searches the audit log. Domain administrators see entries made by users of
    /// their domain and entries about their domain; super administrators see all.
    ///
    /// # Errors
    /// Database errors.
    pub async fn search_audit(&self, admin: &Admin, search: &AuditSearch) -> Result<AuditPage> {
        let scope = admin.scope.domain_filter();
        let actor = search.actor.as_deref().and_then(like_contains);
        let action = search
            .action
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(|a| {
                let escaped: String = a
                    .chars()
                    .take(100)
                    .flat_map(|c| {
                        if matches!(c, '%' | '_' | '\\') {
                            vec!['\\', c]
                        } else {
                            vec![c]
                        }
                    })
                    .collect();
                format!("{escaped}%")
            });
        let outcome = search.outcome.map(outcome_text);
        let text = search.text.as_deref().and_then(like_contains);
        let (limit, offset) = (
            search.limit.clamp(1, MAX_PAGE_SIZE.max(MAX_EXPORT_ROWS)),
            search.offset.max(0),
        );
        let total = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM audit_log a
               WHERE ($1::uuid IS NULL
                      OR (jsonb_exists(a.details, 'domain_id') AND a.details->>'domain_id' = $1::text)
                      OR (NOT jsonb_exists(a.details, 'domain_id')
                          AND a.actor_user_id IN (SELECT id FROM users WHERE domain_id = $1 AND role <> 'super_admin')))
                 AND ($2::text IS NULL OR a.actor_email ILIKE $2)
                 AND ($3::text IS NULL OR a.action LIKE $3)
                 AND ($4::text IS NULL OR a.outcome::text = $4)
                 AND ($5::timestamptz IS NULL OR a.occurred_at >= $5)
                 AND ($6::timestamptz IS NULL OR a.occurred_at < $6)
                 AND ($7::text IS NULL OR a.target_id ILIKE $7 OR a.details::text ILIKE $7)"#,
            scope,
            actor,
            action,
            outcome,
            search.from,
            search.to,
            text
        )
        .fetch_one(self.pool())
        .await?;
        let entries = sqlx::query_as!(
            AuditEntry,
            r#"SELECT a.id, a.occurred_at, a.actor_user_id, a.actor_email, a.action, a.target_type, a.target_id,
                      a.outcome AS "outcome: AuditOutcome", a.ip_address, a.user_agent, a.details
               FROM audit_log a
               WHERE ($1::uuid IS NULL
                      OR (jsonb_exists(a.details, 'domain_id') AND a.details->>'domain_id' = $1::text)
                      OR (NOT jsonb_exists(a.details, 'domain_id')
                          AND a.actor_user_id IN (SELECT id FROM users WHERE domain_id = $1 AND role <> 'super_admin')))
                 AND ($2::text IS NULL OR a.actor_email ILIKE $2)
                 AND ($3::text IS NULL OR a.action LIKE $3)
                 AND ($4::text IS NULL OR a.outcome::text = $4)
                 AND ($5::timestamptz IS NULL OR a.occurred_at >= $5)
                 AND ($6::timestamptz IS NULL OR a.occurred_at < $6)
                 AND ($7::text IS NULL OR a.target_id ILIKE $7 OR a.details::text ILIKE $7)
               ORDER BY a.occurred_at DESC, a.id DESC
               LIMIT $8 OFFSET $9"#,
            scope,
            actor,
            action,
            outcome,
            search.from,
            search.to,
            text,
            limit,
            offset
        )
        .fetch_all(self.pool())
        .await?;
        Ok(AuditPage { entries, total })
    }

    /// The distinct action names visible to the administrator, for the filter menu.
    ///
    /// # Errors
    /// Database errors.
    pub async fn audit_actions(&self, admin: &Admin) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar!(
            r#"SELECT DISTINCT a.action AS "action!" FROM audit_log a
               WHERE ($1::uuid IS NULL
                      OR (jsonb_exists(a.details, 'domain_id') AND a.details->>'domain_id' = $1::text)
                      OR (NOT jsonb_exists(a.details, 'domain_id')
                          AND a.actor_user_id IN (SELECT id FROM users WHERE domain_id = $1 AND role <> 'super_admin')))
               ORDER BY 1"#,
            admin.scope.domain_filter()
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// The matching entries as CSV (at most [`MAX_EXPORT_ROWS`]), safe to open in a
    /// spreadsheet. The export itself is audited.
    ///
    /// # Errors
    /// Database errors.
    pub async fn export_audit_csv(&self, admin: &Admin, search: &AuditSearch) -> Result<String> {
        let page = self
            .search_audit(
                admin,
                &AuditSearch {
                    limit: MAX_EXPORT_ROWS,
                    offset: 0,
                    ..search.clone()
                },
            )
            .await?;
        let mut out = write_row(&[
            "time",
            "actor",
            "action",
            "outcome",
            "target_type",
            "target_id",
            "ip_address",
            "details",
        ]);
        for e in &page.entries {
            out.push_str(&write_row(&[
                e.occurred_at.to_rfc3339(),
                e.actor_email.clone().unwrap_or_default(),
                e.action.clone(),
                outcome_text(e.outcome).to_owned(),
                e.target_type.clone().unwrap_or_default(),
                e.target_id.clone().unwrap_or_default(),
                e.ip_address.clone().unwrap_or_default(),
                e.details.to_string(),
            ]));
        }
        self.audit(
            admin,
            "audit.export",
            None,
            admin.scope.domain_filter(),
            AuditOutcome::Success,
            serde_json::json!({"rows": page.entries.len()}),
        )
        .await;
        Ok(out)
    }
}
