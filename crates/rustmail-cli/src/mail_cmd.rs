//! Webmail maintenance commands.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use rustmail_core::db::PgPool;
use rustmail_core::mailindex::MailIndex;
use rustmail_core::repository::{PgUserRepository, UserRepository};
use rustmail_imap::sync::sync_mailbox;
use rustmail_imap::{MailConfig, MailSession};

/// `rustmail-cli mail ...`
#[derive(Debug, Subcommand)]
pub enum MailCommand {
    /// Index a user's mailbox for webmail search (headers and a short snippet).
    ///
    /// The webmail does this in the background as people use it; this command is
    /// for a first full run, or to repair the index. Needs the same
    /// IMAP_* / MAIL_MASTER_* settings as the web server.
    Index {
        /// The user's address.
        email: String,
        /// Stop after this many messages (run again to continue).
        #[arg(long, default_value_t = 100_000)]
        budget: usize,
    },
}

/// Runs a `mail` subcommand.
pub async fn run_mail(pool: &PgPool, command: MailCommand) -> Result<()> {
    match command {
        MailCommand::Index { email, budget } => {
            let config = MailConfig::from_env()
                .context("webmail settings")?
                .context("MAIL_MASTER_PASSWORD is not set; see deploy/mta.env.example")?;
            let user = PgUserRepository::new(pool.clone())
                .find_by_email(&email.trim().to_lowercase())
                .await?
                .with_context(|| format!("no user {email}"))?;
            let index = MailIndex::new(pool.clone());
            let Some(mailbox_id) = index.mailbox_id(user.id).await? else {
                bail!("{email} has no mailbox");
            };
            let mut session = MailSession::connect(&config, &user.email)
                .await
                .context("could not open the mailbox")?;
            let mut total = 0;
            let mut complete = false;
            // The library stops after a fixed number per folder; keep going until the
            // budget is used or everything is caught up.
            while total < budget && !complete {
                let report = sync_mailbox(&mut session, &index, mailbox_id, budget - total).await?;
                total += report.messages;
                complete = report.complete;
                if report.messages == 0 {
                    break;
                }
            }
            session.close().await;
            println!(
                "Indexed {total} messages for {}.{}",
                user.email,
                if complete {
                    ""
                } else {
                    " More remain; run the command again."
                }
            );
        }
    }
    Ok(())
}
