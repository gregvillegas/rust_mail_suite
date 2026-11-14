//! Figures for the health page that live in the database.

use chrono::{DateTime, Utc};

use super::{Admin, AdminService};
use crate::error::Result;

/// What the mail pipeline has been doing lately.
#[derive(Debug, Clone)]
pub struct PipelineSnapshot {
    /// When the Amavis/Postfix log was last read, if ever.
    pub last_read: Option<DateTime<Utc>>,
    /// Pipeline events recorded in the last hour.
    pub events_last_hour: i64,
    /// Messages currently held in quarantine.
    pub held: i64,
}

impl AdminService {
    /// Pipeline freshness for the health page. Super administrators only.
    ///
    /// # Errors
    /// [`crate::Error::Forbidden`] for other roles, or database errors.
    pub async fn pipeline_snapshot(&self, admin: &Admin) -> Result<PipelineSnapshot> {
        admin.require_super()?;
        let row = sqlx::query!(
            r#"SELECT (SELECT max(updated_at) FROM pipeline_log_state) AS last_read,
                      (SELECT count(*) FROM pipeline_events WHERE occurred_at > now() - interval '1 hour') AS "events!",
                      (SELECT count(*) FROM quarantine_items WHERE status = 'held') AS "held!""#
        )
        .fetch_one(self.pool())
        .await?;
        Ok(PipelineSnapshot {
            last_read: row.last_read,
            events_last_hour: row.events,
            held: row.held,
        })
    }
}
