//! The quota dashboard.

use serde::Serialize;

use super::{Admin, AdminService};
use crate::error::Result;

/// How many of the fullest mailboxes the dashboard lists.
pub const TOP_MAILBOXES: i64 = 10;
/// A mailbox this full (percent) counts as "near its limit".
pub const NEAR_LIMIT_PERCENT: f64 = 80.0;

/// Storage figures for one domain.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DomainQuota {
    /// Domain name.
    pub domain: String,
    /// Mailboxes.
    pub mailboxes: i64,
    /// Bytes used by all of them.
    pub used_bytes: i64,
    /// Sum of the quotas of mailboxes that have one.
    pub limited_quota_bytes: i64,
    /// Mailboxes with no quota.
    pub unlimited_mailboxes: i64,
}

/// One mailbox's figures.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MailboxQuota {
    /// The user's address.
    pub email: String,
    /// The user's id (for links).
    pub user_id: uuid::Uuid,
    /// Bytes used.
    pub used_bytes: i64,
    /// The quota; `None` = unlimited.
    pub quota_bytes: Option<i64>,
    /// Messages stored.
    pub messages: i64,
}

impl MailboxQuota {
    /// Percent of the quota used; `None` if unlimited.
    #[must_use]
    pub fn percent(&self) -> Option<f64> {
        self.quota_bytes
            .filter(|q| *q > 0)
            .map(|q| (self.used_bytes as f64 / q as f64) * 100.0)
    }
}

/// Everything on the quota dashboard.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaDashboard {
    /// Per domain.
    pub domains: Vec<DomainQuota>,
    /// The fullest mailboxes by bytes.
    pub top: Vec<MailboxQuota>,
    /// Mailboxes at or above [`NEAR_LIMIT_PERCENT`], fullest first.
    pub near_limit: Vec<MailboxQuota>,
    /// Bytes used in total.
    pub total_used_bytes: i64,
    /// Mailboxes in total.
    pub total_mailboxes: i64,
}

impl AdminService {
    /// Figures for the dashboard, limited to the administrator's scope.
    ///
    /// # Errors
    /// Database errors.
    pub async fn quota_dashboard(&self, admin: &Admin) -> Result<QuotaDashboard> {
        let scope = admin.scope.domain_filter();
        let domains: Vec<DomainQuota> = sqlx::query!(
            r#"SELECT d.name AS domain,
                      count(m.id) AS "mailboxes!",
                      COALESCE(sum(q.bytes_used), 0)::bigint AS "used!",
                      COALESCE(sum(COALESCE(m.quota_bytes, d.default_quota_bytes))
                               FILTER (WHERE COALESCE(m.quota_bytes, d.default_quota_bytes) > 0), 0)::bigint AS "limited!",
                      count(m.id) FILTER (WHERE COALESCE(m.quota_bytes, d.default_quota_bytes, 0) = 0) AS "unlimited!"
               FROM domains d
               LEFT JOIN users u ON u.domain_id = d.id
               LEFT JOIN mailboxes m ON m.user_id = u.id
               LEFT JOIN quota_usage q ON q.mailbox_id = m.id
               WHERE ($1::uuid IS NULL OR d.id = $1)
               GROUP BY d.name ORDER BY d.name"#,
            scope
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|r| DomainQuota {
            domain: r.domain,
            mailboxes: r.mailboxes,
            used_bytes: r.used,
            limited_quota_bytes: r.limited,
            unlimited_mailboxes: r.unlimited,
        })
        .collect();

        let mailboxes = sqlx::query!(
            r#"SELECT u.id, u.email, COALESCE(q.bytes_used, 0)::bigint AS "used!",
                      COALESCE(q.messages_count, 0)::bigint AS "messages!",
                      NULLIF(COALESCE(m.quota_bytes, d.default_quota_bytes, 0), 0) AS quota
               FROM users u
               JOIN domains d ON d.id = u.domain_id
               JOIN mailboxes m ON m.user_id = u.id
               LEFT JOIN quota_usage q ON q.mailbox_id = m.id
               WHERE ($1::uuid IS NULL OR u.domain_id = $1)"#,
            scope
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|r| MailboxQuota {
            email: r.email,
            user_id: r.id,
            used_bytes: r.used,
            quota_bytes: r.quota,
            messages: r.messages,
        })
        .collect::<Vec<_>>();

        let mut top = mailboxes.clone();
        top.sort_by_key(|m| std::cmp::Reverse(m.used_bytes));
        top.truncate(usize::try_from(TOP_MAILBOXES).unwrap_or(10));
        let mut near_limit: Vec<_> = mailboxes
            .iter()
            .filter(|m| m.percent().is_some_and(|p| p >= NEAR_LIMIT_PERCENT))
            .cloned()
            .collect();
        near_limit.sort_by(|a, b| {
            b.percent()
                .partial_cmp(&a.percent())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(QuotaDashboard {
            total_used_bytes: domains.iter().map(|d| d.used_bytes).sum(),
            total_mailboxes: domains.iter().map(|d| d.mailboxes).sum(),
            domains,
            top,
            near_limit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages() {
        let m = |used, quota| MailboxQuota {
            email: String::new(),
            user_id: uuid::Uuid::nil(),
            used_bytes: used,
            quota_bytes: quota,
            messages: 0,
        };
        assert_eq!(m(50, Some(200)).percent(), Some(25.0));
        assert_eq!(m(50, None).percent(), None);
        assert_eq!(m(50, Some(0)).percent(), None, "zero means unlimited");
        assert_eq!(m(300, Some(200)).percent(), Some(150.0));
    }
}
