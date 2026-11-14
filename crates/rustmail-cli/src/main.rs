//! RustMail Suite operator CLI.
//!
//! Subcommands:
//! - `migrate`: apply pending database migrations.
//! - `seed`: create the first domain and super-admin (idempotent).
//! - `domain create`: add a hosted domain (and its mail directory).
//! - `user create`: add a user with their mailbox and Maildir.
//! - `mailbox check`: compare mailboxes in the database with the disk.
//! - `pipeline`: turn the Postfix/Amavis logs into events and quarantine entries.
//! - `quarantine`: list and purge held messages.
//! - `policy`: spam and virus thresholds per server, domain or recipient.
//! - `mail index`: build the webmail search index for a user.
//!
//! Web-based user management arrives in Phase 7; until then this is the way to
//! create accounts.
//!
//! The database URL comes from `--database-url` or `DATABASE_URL` (a `.env`
//! file is loaded if present). It contains credentials, so it is never logged.
//! Mail is stored under `--vmail-base` / `VMAIL_BASE`; see
//! `rustmail_core::provisioning` for the permissions model.

mod mail_cmd;
mod pipeline_cmd;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rustmail_core::db::PgPool;
use rustmail_core::domain::{NewDomain, UserRole};
use rustmail_core::password::generate_password;
use rustmail_core::provisioning::{
    AccountService, CreateUser, MailboxProvisioner, ProvisionerConfig,
};
use rustmail_core::seed::{seed, SeedRequest};

const MIB: i64 = 1024 * 1024;

/// Operator tooling for RustMail Suite.
#[derive(Debug, Parser)]
#[command(name = "rustmail-cli", version, about)]
struct Cli {
    /// PostgreSQL connection URL.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true, global = true)]
    database_url: Option<String>,

    /// Mail base directory (Dovecot's vmail base). Default: /var/mail/vhosts.
    #[arg(long, env = "VMAIL_BASE", global = true)]
    vmail_base: Option<PathBuf>,

    /// Octal permission bits for new mail directories (default 0700; use 0770
    /// when RustMail and Dovecot run as different users sharing a group).
    #[arg(long, env = "VMAIL_DIR_MODE", global = true)]
    vmail_dir_mode: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Apply pending database migrations.
    Migrate,
    /// Create the first domain and super-admin. Safe to run repeatedly.
    Seed(SeedArgs),
    /// Manage hosted domains.
    #[command(subcommand)]
    Domain(DomainCommand),
    /// Manage users.
    #[command(subcommand)]
    User(UserCommand),
    /// Inspect and repair mailboxes on disk.
    #[command(subcommand)]
    Mailbox(MailboxCommand),
    /// Mail filtering pipeline: log ingestion and events.
    #[command(subcommand)]
    Pipeline(pipeline_cmd::PipelineCommand),
    /// Quarantined messages.
    #[command(subcommand)]
    Quarantine(pipeline_cmd::QuarantineCommand),
    /// Spam and virus policy.
    #[command(subcommand)]
    Policy(pipeline_cmd::PolicyCommand),
    /// Webmail maintenance.
    #[command(subcommand)]
    Mail(mail_cmd::MailCommand),
}

#[derive(Debug, Subcommand)]
enum DomainCommand {
    /// Add a hosted domain and create its directory.
    Create(DomainCreateArgs),
}

#[derive(Debug, Subcommand)]
enum UserCommand {
    /// Add a user, their mailbox and their Maildir.
    Create(UserCreateArgs),
}

#[derive(Debug, Subcommand)]
enum MailboxCommand {
    /// Report mailboxes whose Maildir is missing on disk; with --repair, create them.
    Check {
        /// Create any missing Maildirs.
        #[arg(long)]
        repair: bool,
    },
}

#[derive(Debug, Args)]
struct SeedArgs {
    /// Domain to create, e.g. example.com.
    #[arg(long)]
    domain: String,

    /// Local part of the admin address (before the @).
    #[arg(long, default_value = "admin")]
    admin_local_part: String,

    /// Display name for the admin.
    #[arg(long, default_value = "Administrator")]
    admin_name: String,

    /// Admin password (at least 12 characters). Prefer the environment variable
    /// over the flag, which is visible in the process list. If omitted, a random
    /// password is generated and printed once.
    #[arg(long, env = "RUSTMAIL_ADMIN_PASSWORD", hide_env_values = true)]
    admin_password: Option<String>,
}

#[derive(Debug, Args)]
struct DomainCreateArgs {
    /// Domain name, e.g. example.com.
    name: String,

    /// Default mailbox quota in MiB (omit for unlimited).
    #[arg(long)]
    default_quota_mb: Option<i64>,

    /// Maximum number of mailboxes (omit for unlimited).
    #[arg(long)]
    max_mailboxes: Option<i32>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RoleArg {
    User,
    DomainAdmin,
    SuperAdmin,
}

impl From<RoleArg> for UserRole {
    fn from(role: RoleArg) -> Self {
        match role {
            RoleArg::User => UserRole::User,
            RoleArg::DomainAdmin => UserRole::DomainAdmin,
            RoleArg::SuperAdmin => UserRole::SuperAdmin,
        }
    }
}

#[derive(Debug, Args)]
struct UserCreateArgs {
    /// The full address, e.g. alice@example.com. The domain must already exist.
    email: String,

    /// Name shown in the UI.
    #[arg(long, default_value = "")]
    name: String,

    /// Authorisation level.
    #[arg(long, value_enum, default_value = "user")]
    role: RoleArg,

    /// Mailbox quota in MiB, overriding the domain default.
    #[arg(long)]
    quota_mb: Option<i64>,

    /// Initial password (at least 12 characters). Prefer the environment variable
    /// over the flag, which is visible in the process list. If omitted, a random
    /// password is generated and printed once.
    #[arg(long, env = "RUSTMAIL_USER_PASSWORD", hide_env_values = true)]
    password: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // A missing .env file is normal (CI, production); ignore that case.
    let _ = dotenvy::dotenv();
    init_tracing();

    let cli = Cli::parse();
    let database_url = cli
        .database_url
        .clone()
        .context("DATABASE_URL is not set (use --database-url or a .env file)")?;
    let pool = rustmail_core::db::connect(&database_url, 5)
        .await
        .context("could not connect to the database")?;
    let provisioner = MailboxProvisioner::new(
        ProvisionerConfig::from_parts(cli.vmail_base.clone(), cli.vmail_dir_mode.as_deref())
            .context("invalid mail directory settings")?,
    );

    match cli.command {
        Command::Migrate => {
            rustmail_core::db::migrate(&pool)
                .await
                .context("migration failed")?;
            println!("Migrations are up to date.");
        }
        Command::Seed(args) => run_seed(&pool, provisioner, args).await?,
        Command::Domain(DomainCommand::Create(args)) => {
            run_domain_create(&pool, provisioner, args).await?;
        }
        Command::User(UserCommand::Create(args)) => {
            run_user_create(&pool, provisioner, args).await?;
        }
        Command::Mailbox(MailboxCommand::Check { repair }) => {
            run_mailbox_check(&pool, provisioner, repair).await?;
        }
        Command::Pipeline(command) => pipeline_cmd::run_pipeline(&pool, command).await?,
        Command::Quarantine(command) => pipeline_cmd::run_quarantine(&pool, command).await?,
        Command::Policy(command) => pipeline_cmd::run_policy(&pool, command).await?,
        Command::Mail(command) => mail_cmd::run_mail(&pool, command).await?,
    }
    Ok(())
}

async fn run_seed(pool: &PgPool, provisioner: MailboxProvisioner, args: SeedArgs) -> Result<()> {
    let outcome = seed(
        pool,
        &SeedRequest {
            domain: args.domain,
            admin_local_part: args.admin_local_part,
            admin_display_name: args.admin_name,
            admin_password: args.admin_password,
        },
    )
    .await
    .context("seeding failed")?;

    println!(
        "Domain  : {} ({})",
        outcome.domain.name,
        if outcome.domain_created {
            "created"
        } else {
            "already existed"
        }
    );
    println!(
        "Admin   : {} ({})",
        outcome.admin.email,
        if outcome.admin_created {
            "created"
        } else {
            "already existed, left unchanged"
        }
    );

    // The rows exist now. Creating the directories is a separate step that can
    // legitimately fail (no mail base on a dev machine), so it only warns.
    let accounts = AccountService::postgres(pool, provisioner.clone());
    let disk = async {
        provisioner.ensure_domain_dir(&outcome.domain.name)?;
        accounts.ensure_maildir_for(&outcome.admin.email).await
    }
    .await;
    match disk {
        Ok(maildir) => println!(
            "Maildir : {} ({})",
            maildir.path.display(),
            if maildir.created {
                "created"
            } else {
                "already existed"
            }
        ),
        Err(error) => eprintln!(
            "warning: the Maildir was not created: {error}\n         \
             Fix VMAIL_BASE and run `rustmail-cli mailbox check --repair`."
        ),
    }

    if !outcome.admin_created && outcome.admin.role != UserRole::SuperAdmin {
        eprintln!(
            "warning: {} already existed with role '{}', not super_admin; it was not modified",
            outcome.admin.email, outcome.admin.role
        );
    }
    if let Some(password) = outcome.generated_password {
        println!();
        println!("Generated admin password (shown once, store it now):");
        println!("  {password}");
    }
    Ok(())
}

async fn run_domain_create(
    pool: &PgPool,
    provisioner: MailboxProvisioner,
    args: DomainCreateArgs,
) -> Result<()> {
    let accounts = AccountService::postgres(pool, provisioner);
    let domain = accounts
        .create_domain(&NewDomain {
            name: args.name,
            default_quota_bytes: mib_to_bytes(args.default_quota_mb, "--default-quota-mb")?,
            max_mailboxes: args.max_mailboxes,
        })
        .await
        .context("could not create the domain")?;

    println!("Domain  : {} (created)", domain.name);
    println!("Quota   : {}", describe_quota(domain.default_quota_bytes));
    println!(
        "Limit   : {}",
        domain.max_mailboxes.map_or_else(
            || "unlimited mailboxes".to_owned(),
            |n| format!("{n} mailboxes")
        )
    );
    Ok(())
}

async fn run_user_create(
    pool: &PgPool,
    provisioner: MailboxProvisioner,
    args: UserCreateArgs,
) -> Result<()> {
    let generated = args.password.is_none().then(generate_password);
    let password = args
        .password
        .or_else(|| generated.clone())
        .context("no password available")?;

    let accounts = AccountService::postgres(pool, provisioner);
    let created = accounts
        .create_user(CreateUser {
            email: args.email,
            password,
            display_name: args.name,
            role: args.role.into(),
            quota_bytes: mib_to_bytes(args.quota_mb, "--quota-mb")?,
        })
        .await
        .context("could not create the user")?;

    println!("User    : {} ({})", created.user.email, created.user.role);
    println!("Mailbox : {}", created.mailbox.maildir_path);
    println!("Quota   : {}", describe_quota(created.mailbox.quota_bytes));
    println!(
        "Maildir : {}",
        if created.maildir_created {
            "created"
        } else {
            "already existed"
        }
    );
    if let Some(password) = generated {
        println!();
        println!("Generated password (shown once, store it now):");
        println!("  {password}");
    }
    Ok(())
}

async fn run_mailbox_check(
    pool: &PgPool,
    provisioner: MailboxProvisioner,
    repair: bool,
) -> Result<()> {
    let accounts = AccountService::postgres(pool, provisioner);
    let report = accounts
        .check_mailboxes(repair)
        .await
        .context("could not check the mailboxes")?;

    println!("Checked {} mailbox(es).", report.checked);
    if report.missing.is_empty() {
        println!("All Maildirs are present.");
        return Ok(());
    }
    for problem in &report.missing {
        println!("  missing: {} ({})", problem.email, problem.maildir_path);
    }
    if repair {
        println!("Repaired {} Maildir(s).", report.repaired);
        Ok(())
    } else {
        bail!(
            "{} Maildir(s) are missing; run again with --repair to create them",
            report.missing.len()
        )
    }
}

/// Converts a MiB count to bytes, rejecting negatives and overflow.
fn mib_to_bytes(mib: Option<i64>, flag: &str) -> Result<Option<i64>> {
    mib.map(|n| {
        if n < 0 {
            bail!("{flag} must not be negative");
        }
        n.checked_mul(MIB)
            .with_context(|| format!("{flag} is too large"))
    })
    .transpose()
}

fn describe_quota(bytes: Option<i64>) -> String {
    bytes.map_or_else(
        || "unlimited (or the domain default)".to_owned(),
        |b| format!("{} MiB", b / MIB),
    )
}

/// Initializes the global tracing subscriber, honoring `RUST_LOG`.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn megabytes_convert_to_bytes() {
        assert_eq!(mib_to_bytes(None, "--q").expect("none"), None);
        assert_eq!(mib_to_bytes(Some(0), "--q").expect("zero"), Some(0));
        assert_eq!(
            mib_to_bytes(Some(50), "--q").expect("50"),
            Some(50 * 1024 * 1024)
        );
    }

    #[test]
    fn negative_and_overflowing_quotas_are_rejected() {
        assert!(mib_to_bytes(Some(-1), "--quota-mb").is_err());
        assert!(mib_to_bytes(Some(i64::MAX), "--quota-mb").is_err());
    }

    #[test]
    fn roles_map_to_the_domain_roles() {
        assert_eq!(UserRole::from(RoleArg::User), UserRole::User);
        assert_eq!(UserRole::from(RoleArg::DomainAdmin), UserRole::DomainAdmin);
        assert_eq!(UserRole::from(RoleArg::SuperAdmin), UserRole::SuperAdmin);
    }

    #[test]
    fn the_command_line_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
