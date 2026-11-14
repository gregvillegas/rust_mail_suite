//! Commands for the mail filtering pipeline: log ingestion, quarantine upkeep and
//! spam/virus policy.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand, ValueEnum};
use rustmail_core::db::PgPool;
use rustmail_core::domain::UserRole;
use rustmail_core::pipeline::follower::{read_batch, Origin, MAX_BATCH_BYTES};
use rustmail_core::pipeline::parser::{LogParser, LogTimeZone};
use rustmail_core::pipeline::store::PipelineStore;
use rustmail_core::policy::{PgPolicyRepository, PolicyUpdate};
use rustmail_core::quarantine::{
    Actor, QuarantineConfig, QuarantineFilter, QuarantineService, QuarantineStatus,
};
use rustmail_core::repository::{PgUserRepository, UserRepository};
use uuid::Uuid;

/// `rustmail-cli pipeline ...`
#[derive(Debug, Subcommand)]
pub enum PipelineCommand {
    /// Read the Postfix and Amavis logs once, store what is new, and exit.
    Ingest(LogArgs),
    /// Keep reading the logs until interrupted (run this as a service).
    Follow(FollowArgs),
    /// Show the most recent stored events.
    Recent {
        /// How many to show.
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Delete events older than a number of days.
    Prune {
        /// Keep this many days.
        #[arg(long, default_value_t = 90)]
        days: i32,
    },
}

/// `rustmail-cli quarantine ...`
#[derive(Debug, Subcommand)]
pub enum QuarantineCommand {
    /// List held messages.
    List {
        /// How many to show.
        #[arg(long, default_value_t = 25)]
        limit: i64,
    },
    /// Deliver a held message to its recipient, skipping the filters.
    Release {
        /// The item id shown by `quarantine list`.
        id: Uuid,
        /// Address of the administrator doing this (recorded in the audit log).
        #[arg(long)]
        by: String,
    },
    /// Delete one held message.
    Delete {
        /// The item id shown by `quarantine list`.
        id: Uuid,
        /// Address of the administrator doing this (recorded in the audit log).
        #[arg(long)]
        by: String,
    },
    /// Delete held messages (and their files) older than a number of days.
    Purge {
        /// Keep this many days.
        #[arg(long, default_value_t = 30)]
        days: i32,
    },
}

/// `rustmail-cli policy ...`
#[derive(Debug, Subcommand)]
pub enum PolicyCommand {
    /// Show all spam and virus policies.
    List,
    /// Create a policy or change some of its settings; others are kept.
    Set(PolicySetArgs),
    /// Remove an override so the scope inherits again.
    Remove {
        /// `@example.com` or `user@example.com`.
        scope: String,
    },
}

/// Which log files to read and how to interpret them.
#[derive(Debug, Args)]
pub struct LogArgs {
    /// Log files to read (repeat, or comma separated). Typically
    /// /var/log/mail.log, or the Postfix and Amavis files from deploy/mta.env.
    #[arg(
        long = "log",
        env = "PIPELINE_LOG_FILES",
        value_delimiter = ',',
        required = true
    )]
    logs: Vec<PathBuf>,
    /// Time zone of the timestamps in the logs: syslog lines carry none.
    #[arg(
        long,
        env = "PIPELINE_LOG_TIMEZONE",
        value_enum,
        default_value = "local"
    )]
    log_timezone: Zone,
    /// For a file never read before, skip its history and read only new lines.
    #[arg(long)]
    from_end: bool,
}

/// Arguments of `pipeline follow`.
#[derive(Debug, Args)]
pub struct FollowArgs {
    #[command(flatten)]
    logs: LogArgs,
    /// Seconds between checks for new lines.
    #[arg(long, default_value_t = 2)]
    interval_secs: u64,
}

/// See [`LogTimeZone`].
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Zone {
    /// UTC (containers, many servers).
    Utc,
    /// This machine's local time.
    Local,
}

impl From<Zone> for LogTimeZone {
    fn from(zone: Zone) -> Self {
        match zone {
            Zone::Utc => LogTimeZone::Utc,
            Zone::Local => LogTimeZone::Local,
        }
    }
}

/// Arguments of `policy set`.
#[derive(Debug, Args)]
pub struct PolicySetArgs {
    /// `@.` (server-wide), `@example.com` or `user@example.com`.
    scope: String,
    /// Score from which spam headers are added.
    #[arg(long)]
    tag_level: Option<f64>,
    /// Score from which a message is marked as spam (and filed in Junk).
    #[arg(long)]
    spam_level: Option<f64>,
    /// Score from which a message is quarantined instead of delivered.
    #[arg(long)]
    quarantine_level: Option<f64>,
    /// Skip spam checks (true/false).
    #[arg(long)]
    bypass_spam: Option<bool>,
    /// Skip virus checks (true/false).
    #[arg(long)]
    bypass_virus: Option<bool>,
    /// Deliver even if flagged as spam (true/false).
    #[arg(long)]
    spam_lover: Option<bool>,
}

/// Runs a `pipeline` subcommand.
pub async fn run_pipeline(pool: &PgPool, command: PipelineCommand) -> Result<()> {
    let store = PipelineStore::new(pool.clone());
    match command {
        PipelineCommand::Ingest(args) => {
            let total = ingest_once(&store, &args).await?;
            println!(
                "Stored {} events and {} new quarantine entries from {} lines.",
                total.events, total.quarantined, total.lines
            );
        }
        PipelineCommand::Follow(args) => follow(&store, &args).await?,
        PipelineCommand::Recent { limit } => {
            for e in store.recent(limit).await? {
                println!(
                    "{}  {:<7} {:<9} {:<12} {} -> {}  {}",
                    e.occurred_at.format("%Y-%m-%d %H:%M:%S"),
                    e.source,
                    e.kind,
                    e.verdict.or(e.status).unwrap_or_default(),
                    e.sender,
                    e.recipient,
                    e.virus_name
                        .or_else(|| e.spam_score.map(|s| format!("score {s:.1}")))
                        .unwrap_or_default()
                );
            }
        }
        PipelineCommand::Prune { days } => {
            let removed = store.prune(days).await?;
            println!("Removed {removed} events older than {days} days.");
        }
    }
    Ok(())
}

/// Runs a `quarantine` subcommand.
pub async fn run_quarantine(pool: &PgPool, command: QuarantineCommand) -> Result<()> {
    let service = QuarantineService::new(
        pool.clone(),
        QuarantineConfig::from_env().context("quarantine settings")?,
    );
    match command {
        QuarantineCommand::List { limit } => {
            let page = service
                .list(&QuarantineFilter {
                    status: Some(QuarantineStatus::Held),
                    limit,
                    ..Default::default()
                })
                .await?;
            for i in &page.items {
                println!(
                    "{}  {}  {:<10} {} -> {}  {}",
                    i.id,
                    i.received_at.format("%Y-%m-%d %H:%M:%S"),
                    i.kind.as_str(),
                    i.sender,
                    i.recipient,
                    i.reason
                );
            }
            println!("{} held in total.", page.total);
        }
        QuarantineCommand::Release { id, by } => {
            let actor = actor_for(pool, &by).await?;
            let item = service.release(id, &actor).await?;
            println!("Released the message for {}.", item.recipient);
        }
        QuarantineCommand::Delete { id, by } => {
            let actor = actor_for(pool, &by).await?;
            service.delete(id, &actor).await?;
            println!("Deleted.");
        }
        QuarantineCommand::Purge { days } => {
            let removed = service.purge_older_than(days).await?;
            println!("Removed {removed} held messages older than {days} days.");
        }
    }
    Ok(())
}

/// Looks up the administrator named on the command line, who must be a super administrator.
async fn actor_for(pool: &PgPool, email: &str) -> Result<Actor> {
    let user = PgUserRepository::new(pool.clone())
        .find_by_email(&email.trim().to_lowercase())
        .await?
        .with_context(|| format!("no user {email}"))?;
    if user.role != UserRole::SuperAdmin {
        anyhow::bail!("{email} is not a super administrator");
    }
    Ok(Actor {
        user_id: user.id,
        email: user.email,
        ip_address: None,
    })
}

/// Runs a `policy` subcommand.
pub async fn run_policy(pool: &PgPool, command: PolicyCommand) -> Result<()> {
    let repo = PgPolicyRepository::new(pool.clone());
    let show = |v: Option<f64>| v.map_or_else(|| "inherit".to_owned(), |v| v.to_string());
    let flag = |v: Option<bool>| v.map_or("inherit", |b| if b { "yes" } else { "no" });
    match command {
        PolicyCommand::List => {
            println!(
                "{:<28} {:>8} {:>8} {:>11}  {:<8} {:<8} {:<9}",
                "scope", "tag", "spam", "quarantine", "no-spam", "no-virus", "spam-lover"
            );
            for p in repo.list().await? {
                println!(
                    "{:<28} {:>8} {:>8} {:>11}  {:<8} {:<8} {:<9}",
                    p.scope,
                    show(p.spam_tag_level),
                    show(p.spam_tag2_level),
                    show(p.spam_kill_level),
                    flag(p.bypass_spam_checks),
                    flag(p.bypass_virus_checks),
                    flag(p.spam_lover)
                );
            }
        }
        PolicyCommand::Set(args) => {
            let saved = repo
                .set(
                    &args.scope,
                    &PolicyUpdate {
                        spam_tag_level: args.tag_level,
                        spam_tag2_level: args.spam_level,
                        spam_kill_level: args.quarantine_level,
                        bypass_spam_checks: args.bypass_spam,
                        bypass_virus_checks: args.bypass_virus,
                        spam_lover: args.spam_lover,
                        virus_lover: None,
                    },
                )
                .await?;
            println!("Policy for {} saved.", saved.scope);
        }
        PolicyCommand::Remove { scope } => {
            repo.remove(&scope).await?;
            println!("Policy for {scope} removed; it inherits again.");
        }
    }
    Ok(())
}

/// Totals for one pass over the logs.
#[derive(Debug, Default)]
struct Totals {
    lines: usize,
    events: usize,
    quarantined: usize,
}

async fn ingest_once(store: &PipelineStore, args: &LogArgs) -> Result<Totals> {
    let mut totals = Totals::default();
    for path in &args.logs {
        let t = ingest_file(store, path, args).await?;
        totals.lines += t.lines;
        totals.events += t.events;
        totals.quarantined += t.quarantined;
    }
    Ok(totals)
}

/// Reads a file to its current end, one bounded batch at a time.
async fn ingest_file(store: &PipelineStore, path: &Path, args: &LogArgs) -> Result<Totals> {
    let key = path.to_string_lossy().into_owned();
    let origin = if args.from_end {
        Origin::End
    } else {
        Origin::Beginning
    };
    let parser = LogParser::new(Utc::now(), args.log_timezone.into());
    let mut totals = Totals::default();
    loop {
        let start = store.position(&key).await?;
        let Some(batch) = read_batch(path, start, origin, MAX_BATCH_BYTES)
            .await
            .with_context(|| format!("reading {}", path.display()))?
        else {
            tracing::debug!(path = %path.display(), "log file does not exist yet");
            return Ok(totals);
        };
        if batch.rotated {
            tracing::info!(path = %path.display(), "log file was rotated; reading from the top");
        }
        let events: Vec<_> = batch.lines.iter().flat_map(|l| parser.parse(l)).collect();
        let progressed = Some(batch.position) != start;
        // Always store the position, even with no events, so the next run starts here
        // (and so --from-end remembers where "the end" was).
        let summary = store.record(&key, batch.position, &events).await?;
        totals.lines += batch.lines.len();
        totals.events += summary.events;
        totals.quarantined += summary.quarantined;
        if !progressed {
            return Ok(totals);
        }
    }
}

async fn follow(store: &PipelineStore, args: &FollowArgs) -> Result<()> {
    let interval = Duration::from_secs(args.interval_secs.max(1));
    tracing::info!(
        files = args.logs.logs.len(),
        "following the mail logs; press Ctrl-C to stop"
    );
    loop {
        match ingest_once(store, &args.logs).await {
            Ok(t) if t.events > 0 => {
                tracing::info!(
                    events = t.events,
                    quarantined = t.quarantined,
                    "stored events"
                );
            }
            Ok(_) => {}
            // A database blip must not end the service; try again next round.
            Err(error) => tracing::warn!(%error, "pass failed; will retry"),
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("stopping");
                return Ok(());
            }
        }
    }
}
